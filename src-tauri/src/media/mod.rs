//! 媒体处理：ffprobe 探测 + ffmpeg 切片。
//!
//! 所有外部进程调用集中在这里，便于统一处理超时、错误信息截断与临时文件清理。

pub mod plan;

use crate::error::{AppError, AppResult};
use crate::models::{MediaProbe, SlicePlan};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

/// stderr 只保留这么长，避免把整段日志灌进数据库和界面。
const STDERR_KEEP: usize = 600;

pub fn ms_to_secs(ms: i64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

fn tail(text: &str) -> String {
    let t = text.trim();
    if t.chars().count() <= STDERR_KEEP {
        return t.to_string();
    }
    let skip = t.chars().count() - STDERR_KEEP;
    format!("…{}", t.chars().skip(skip).collect::<String>())
}

#[cfg(windows)]
fn no_window(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW)
}

#[cfg(not(windows))]
fn no_window(cmd: &mut Command) -> &mut Command {
    cmd
}

// ---------------- ffprobe ----------------

#[derive(Deserialize)]
struct FfprobeOut {
    format: Option<FfprobeFormat>,
    streams: Option<Vec<FfprobeStream>>,
}

#[derive(Deserialize)]
struct FfprobeFormat {
    duration: Option<String>,
    size: Option<String>,
    format_name: Option<String>,
}

#[derive(Deserialize)]
struct FfprobeStream {
    codec_type: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
}

/// 探测媒体信息。ffprobe 缺失或输出异常时返回可读错误。
pub fn probe(ffprobe: &Path, input: &Path) -> AppResult<MediaProbe> {
    if !input.is_file() {
        return Err(AppError::Msg(format!(
            "素材文件不存在：{}",
            input.to_string_lossy()
        )));
    }

    let out = no_window(
        Command::new(ffprobe)
            .arg("-v")
            .arg("quiet")
            .arg("-print_format")
            .arg("json")
            .arg("-show_format")
            .arg("-show_streams")
            .arg(input),
    )
    .output()
    .map_err(|e| AppError::Process(format!("无法启动 ffprobe：{e}")))?;

    if !out.status.success() {
        return Err(AppError::Process(format!(
            "ffprobe 退出码 {:?}：{}",
            out.status.code(),
            tail(&String::from_utf8_lossy(&out.stderr))
        )));
    }

    let parsed: FfprobeOut = serde_json::from_slice(&out.stdout)
        .map_err(|e| AppError::Process(format!("ffprobe 输出无法解析：{e}")))?;

    let duration_ms = parsed
        .format
        .as_ref()
        .and_then(|f| f.duration.as_deref())
        .and_then(|d| d.parse::<f64>().ok())
        .map(|secs| (secs * 1000.0).round() as i64)
        .unwrap_or(0);

    let mut width = 0i64;
    let mut height = 0i64;
    let mut has_audio = false;
    for s in parsed.streams.unwrap_or_default() {
        match s.codec_type.as_deref() {
            Some("video") if width == 0 => {
                width = s.width.unwrap_or(0);
                height = s.height.unwrap_or(0);
            }
            Some("audio") => has_audio = true,
            _ => {}
        }
    }

    let size_bytes = parsed
        .format
        .as_ref()
        .and_then(|f| f.size.as_deref())
        .and_then(|s| s.parse::<i64>().ok())
        .or_else(|| std::fs::metadata(input).ok().map(|m| m.len() as i64))
        .unwrap_or(0);

    Ok(MediaProbe {
        duration_ms,
        width,
        height,
        has_audio,
        size_bytes,
        format_name: parsed
            .format
            .and_then(|f| f.format_name)
            .unwrap_or_default(),
    })
}

// ---------------- ffmpeg 切片 ----------------

#[derive(Debug)]
pub struct SliceOutcome {
    pub out_path: PathBuf,
    pub size_bytes: i64,
    pub message: String,
}

/// 切出一片。优先流拷贝（无损且快），失败则回退转码。
///
/// 写入策略：先写 `xxx.part.mp4`，成功后原子重命名 —— 中途失败不会在输出目录留下半成品。
pub fn slice(ffmpeg: &Path, input: &Path, out: &Path, p: &SlicePlan) -> AppResult<SliceOutcome> {
    if p.end_ms <= p.start_ms {
        return Err(AppError::Msg(format!("非法区间：{}", p.index)));
    }

    let temp = temp_path(out)?;
    let duration = ms_to_secs(p.duration_ms());

    let copy_args: Vec<String> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-y".into(),
        "-ss".into(),
        ms_to_secs(p.start_ms),
        "-i".into(),
        input.to_string_lossy().to_string(),
        "-t".into(),
        duration.clone(),
        "-map".into(),
        "0:v:0?".into(),
        "-map".into(),
        "0:a:0?".into(),
        "-c".into(),
        "copy".into(),
        "-avoid_negative_ts".into(),
        "make_zero".into(),
        "-movflags".into(),
        "+faststart".into(),
        temp.to_string_lossy().to_string(),
    ];

    match run_ffmpeg(ffmpeg, &copy_args) {
        Ok(()) => finish(out, &temp, "流拷贝"),
        Err(copy_err) => {
            // 常见原因：源容器里有 mp4 不支持的流、或时间戳异常
            let reencode_args: Vec<String> = vec![
                "-hide_banner".into(),
                "-nostdin".into(),
                "-y".into(),
                "-ss".into(),
                ms_to_secs(p.start_ms),
                "-i".into(),
                input.to_string_lossy().to_string(),
                "-t".into(),
                duration,
                "-map".into(),
                "0:v:0?".into(),
                "-map".into(),
                "0:a:0?".into(),
                "-c:v".into(),
                "libx264".into(),
                "-preset".into(),
                "veryfast".into(),
                "-crf".into(),
                "20".into(),
                "-c:a".into(),
                "aac".into(),
                "-b:a".into(),
                "192k".into(),
                "-movflags".into(),
                "+faststart".into(),
                temp.to_string_lossy().to_string(),
            ];
            match run_ffmpeg(ffmpeg, &reencode_args) {
                Ok(()) => finish(
                    out,
                    &temp,
                    &format!("流拷贝失败已转码：{copy_err}"),
                ),
                Err(re_err) => {
                    let _ = std::fs::remove_file(&temp);
                    Err(AppError::Process(format!(
                        "切片失败（流拷贝与转码均失败）。转码错误：{re_err}"
                    )))
                }
            }
        }
    }
}

fn temp_path(out: &Path) -> AppResult<PathBuf> {
    let stem = out
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "segment".into());
    let ext = out
        .extension()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "mp4".into());
    let dir = out.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    Ok(dir.join(format!("{stem}.part.{ext}")))
}

fn run_ffmpeg(ffmpeg: &Path, args: &[String]) -> Result<(), String> {
    let out = no_window(Command::new(ffmpeg).args(args))
        .output()
        .map_err(|e| format!("无法启动 ffmpeg：{e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "退出码 {:?}：{}",
            out.status.code(),
            tail(&String::from_utf8_lossy(&out.stderr))
        ))
    }
}

fn finish(out: &Path, temp: &Path, how: &str) -> AppResult<SliceOutcome> {
    if !temp.is_file() {
        return Err(AppError::Process("ffmpeg 报告成功但没有产出文件".into()));
    }
    // 目标已存在时先删，Windows 上 rename 不覆盖
    if out.exists() {
        std::fs::remove_file(out)?;
    }
    std::fs::rename(temp, out)?;
    let size_bytes = std::fs::metadata(out).map(|m| m.len() as i64).unwrap_or(0);
    Ok(SliceOutcome {
        out_path: out.to_path_buf(),
        size_bytes,
        message: how.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ms_formats_to_three_decimals() {
        assert_eq!(ms_to_secs(0), "0.000");
        assert_eq!(ms_to_secs(1), "0.001");
        assert_eq!(ms_to_secs(1_500), "1.500");
        assert_eq!(ms_to_secs(3_600_000), "3600.000");
    }

    #[test]
    fn temp_path_keeps_extension_so_ffmpeg_can_guess_container() {
        let t = temp_path(Path::new("E:/out/001.mp4")).unwrap();
        assert_eq!(t.file_name().unwrap().to_string_lossy(), "001.part.mp4");
        assert_eq!(t.extension().unwrap().to_string_lossy(), "mp4");
    }

    #[test]
    fn probe_rejects_missing_file_before_spawning() {
        let err = probe(Path::new("Z:/nope/ffprobe.exe"), Path::new("Z:/nope/missing.mp4"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("素材文件不存在"), "实际：{err}");
    }

    #[test]
    fn slice_rejects_inverted_range() {
        let bad = SlicePlan {
            index: 1,
            start_ms: 5_000,
            end_ms: 5_000,
        };
        let err = slice(
            Path::new("Z:/nope/ffmpeg.exe"),
            Path::new("E:/nope/in.mp4"),
            Path::new("E:/nope/out.mp4"),
            &bad,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("非法区间"), "实际：{err}");
    }

    /// 端到端冒烟：需要真实 ffmpeg / ffprobe，默认跳过。
    ///
    /// 用法（PowerShell）：
    /// ```text
    /// $env:DRAMADIST_TEST_FFMPEG  = "C:\ffmpeg\bin\ffmpeg.exe"
    /// $env:DRAMADIST_TEST_FFPROBE = "C:\ffmpeg\bin\ffprobe.exe"
    /// cargo test end_to_end -- --nocapture
    /// ```
    /// 没装 ffmpeg 的机器上这条测试直接返回，不会把整个测试套件拖红。
    #[test]
    fn end_to_end_probe_and_slice_with_real_ffmpeg() {
        let (Ok(ffmpeg), Ok(ffprobe)) = (
            std::env::var("DRAMADIST_TEST_FFMPEG"),
            std::env::var("DRAMADIST_TEST_FFPROBE"),
        ) else {
            eprintln!("跳过端到端测试：未设置 DRAMADIST_TEST_FFMPEG / DRAMADIST_TEST_FFPROBE");
            return;
        };
        let ffmpeg = PathBuf::from(ffmpeg);
        let ffprobe = PathBuf::from(ffprobe);

        let dir = std::env::temp_dir().join("dramadist_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 1) 造一段 12 秒带音轨的测试素材
        let input = dir.join("input.mp4");
        let generated = no_window(
            Command::new(&ffmpeg).args([
                "-hide_banner",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=15",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440",
                "-t",
                "12",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-c:a",
                "aac",
                "-shortest",
            ]),
        )
        .arg(&input)
        .output()
        .expect("无法启动 ffmpeg");
        assert!(
            generated.status.success(),
            "生成测试素材失败：{}",
            tail(&String::from_utf8_lossy(&generated.stderr))
        );

        // 2) 探测
        let info = probe(&ffprobe, &input).expect("探测失败");
        assert_eq!((info.width, info.height), (320, 240));
        assert!(info.has_audio, "应当探测到音轨");
        assert!(
            (11_000..=13_400).contains(&info.duration_ms),
            "时长异常：{}",
            info.duration_ms
        );

        // 3) 计划 + 逐个切片
        let plans = plan::plan_every_seconds(info.duration_ms, 4.0);
        assert_eq!(plans.len(), 3, "12 秒按 4 秒切应得 3 片");
        for p in &plans {
            let out = dir.join(format!("{:03}.mp4", p.index));
            let outcome = slice(&ffmpeg, &input, &out, p).expect("切片失败");
            assert!(out.is_file(), "第 {} 片没有产出文件", p.index);
            assert!(outcome.size_bytes > 0, "第 {} 片是空文件", p.index);
        }

        // 4) 不应留下任何 .part 临时文件
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".part."))
            .count();
        assert_eq!(leftovers, 0, "输出目录残留了临时文件");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
