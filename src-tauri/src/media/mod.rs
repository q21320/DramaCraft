//! 媒体处理：ffprobe 探测 + ffmpeg 切片。
//!
//! 所有外部进程调用集中在这里，便于统一处理超时、错误信息截断与临时文件清理。

pub mod plan;

use crate::error::{AppError, AppResult};
use crate::models::{MediaProbe, SlicePlan};
use crate::proc::{no_window, tail};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub fn ms_to_secs(ms: i64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
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

// ---------------- 能不能播：解码自检 ----------------

/// 坏素材解码时的典型报错。
///
/// 下载没下全 / CDN 给的片段缺损时，ffmpeg 用 `-c copy` 转封装**照样报成功**，
/// 坏文件就这么落盘了；等到去重、切片时才炸，而那时刷出来的是上万行解码错误，
/// 用户看不出「这是下载的问题」。下面这几条是所有坏素材共同的特征。
const DECODE_ERRORS: [&str; 8] = [
    "Invalid NAL unit size",
    "Error splitting the input into NAL units",
    "Error submitting packet to decoder",
    "Invalid data found when processing input",
    "channel element",
    "Reserved bit set",
    "Prediction is not allowed",
    "error while decoding",
];

/// 从 ffmpeg 的 stderr 里挑出**第一条**能说明「文件坏了」的行。
///
/// 坏文件会刷上万行（每帧一条），所以只取第一条 —— 够定位，也不会淹掉日志。
pub fn decode_error_line(stderr: &str) -> Option<&str> {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| DECODE_ERRORS.iter().any(|key| line.contains(key)))
}

/// 去掉 ffmpeg 日志里的对象地址：`[aac @ 000001c3df778280] xxx` → `[aac] xxx`。
///
/// 地址每次运行都不一样，留着既吵又像关键信息 ——
/// 日志里「播不了（解码报错：[aac @ 000001c3...] channel element ...）」根本读不下去。
pub fn strip_pointer(line: &str) -> String {
    let Some(close) = line.find(']') else {
        return line.to_string();
    };
    let Some(at) = line[..close].find(" @ ") else {
        return line.to_string();
    };
    format!("{}{}", &line[..at], &line[close..])
}

/// 完整解码一遍（不编码、不落盘），回答「这个文件到底能不能播」。
///
/// **能解密、能转封装，不等于能播。** 下载链路里用的就是 `-c copy`，
/// 源片段坏了它照样成功，于是坏文件一路走到去重才失败。这里真解码一遍，
/// 坏的在下载阶段就能发现并重下。
///
/// 实测一集 60 秒的 480p 素材约 0.6 秒 —— 比重新下载一次便宜得多。
pub fn decode_check(ffmpeg: &Path, file: &Path) -> AppResult<()> {
    if !file.is_file() {
        return Err(AppError::Msg("文件不存在".into()));
    }

    let out = no_window(
        Command::new(ffmpeg)
            .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-i"])
            .arg(file)
            .args(["-map", "0:v:0", "-map", "0:a:0?", "-f", "null", "-"]),
    )
    .stdin(Stdio::null())
    .output()
    .map_err(|e| AppError::Process(format!("无法启动 ffmpeg：{e}")))?;

    let stderr = String::from_utf8_lossy(&out.stderr);
    // **解码报错就算不过**，哪怕 ffmpeg 退出码是 0 ——
    // 半坏的文件能「跑完」，但画面会花、会卡，拿去发布是废片。
    if let Some(line) = decode_error_line(&stderr) {
        return Err(AppError::Process(format!("解码报错：{}", strip_pointer(line))));
    }
    if !out.status.success() {
        return Err(AppError::Process(format!(
            "解码失败（退出码 {:?}）：{}",
            out.status.code(),
            tail(&stderr)
        )));
    }
    Ok(())
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
/// 可用的 H.264 编码器。
///
/// 为什么需要它：源素材常是 **HEVC（H.265）**，而抖音/快手/视频号的上传规范
/// 要 **H.264**。流拷贝会把 HEVC 原样保留，传上去被拒、或者被平台二次转码掉画质。
/// 所以「兼容模式」必须真转码，而转码就该尽量用硬件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H264Encoder {
    Nvenc,
    Qsv,
    Amf,
    X264,
}

impl H264Encoder {
    pub fn name(self) -> &'static str {
        match self {
            H264Encoder::Nvenc => "h264_nvenc",
            H264Encoder::Qsv => "h264_qsv",
            H264Encoder::Amf => "h264_amf",
            H264Encoder::X264 => "libx264",
        }
    }

    pub fn is_hardware(self) -> bool {
        !matches!(self, H264Encoder::X264)
    }

    /// 质量参数各家叫法不同，分开给。
    pub fn quality_args(self) -> Vec<String> {
        match self {
            // nvenc 用 -cq 恒定质量；preset 用 p4（速度与质量的中间档）
            H264Encoder::Nvenc => vec!["-preset".into(), "p4".into(), "-cq".into(), "23".into()],
            H264Encoder::Qsv => vec!["-global_quality".into(), "23".into()],
            H264Encoder::Amf => vec!["-quality".into(), "balanced".into(), "-rc".into(), "cqp".into(), "-qp_i".into(), "23".into(), "-qp_p".into(), "23".into()],
            H264Encoder::X264 => vec!["-preset".into(), "veryfast".into(), "-crf".into(), "20".into()],
        }
    }
}

static H264_ENCODER: std::sync::OnceLock<H264Encoder> = std::sync::OnceLock::new();

/// 探测本机可用的 H.264 编码器，优先硬件。
///
/// 探测方式是**真的编一帧**，而不是看 `-encoders` 列表 ——
/// 列表里有 `h264_nvenc` 不代表这台机器真有 N 卡（没有卡时会直接报错）。
pub fn pick_h264_encoder(ffmpeg: &Path) -> H264Encoder {
    *H264_ENCODER.get_or_init(|| {
        for candidate in [H264Encoder::Nvenc, H264Encoder::Qsv, H264Encoder::Amf] {
            let mut cmd = Command::new(ffmpeg);
            crate::proc::no_window(&mut cmd);
            cmd.args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=64x64:d=0.1",
                "-frames:v",
                "1",
                "-c:v",
            ])
            .arg(candidate.name())
            .arg("-f")
            .arg("null")
            .arg("-");
            if let Ok(out) = cmd.output() {
                if out.status.success() {
                    return candidate;
                }
            }
        }
        H264Encoder::X264
    })
}


/// 切一片。
///
/// `transcode` 为真时**强制转成 H.264**（平台上传规范要求），
/// 为假时先试流拷贝（快、无损），失败再退回转码。
pub fn slice(
    ffmpeg: &Path,
    input: &Path,
    out: &Path,
    p: &SlicePlan,
    transcode: bool,
) -> AppResult<SliceOutcome> {
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

    // 兼容模式：直接转码，不走拷贝
    if transcode {
        return reencode(ffmpeg, input, &temp, out, p, &duration);
    }

    match run_ffmpeg(ffmpeg, &copy_args) {
        Ok(()) => finish(out, &temp, "流拷贝"),
        Err(_copy_err) => {
            // 常见原因：源容器里有 mp4 不支持的流、或时间戳异常
            reencode(ffmpeg, input, &temp, out, p, &duration)
        }
    }
}

/// 转码那一段抽出来，好让「拷贝失败」和「兼容模式」共用同一份参数。
fn reencode(
    ffmpeg: &Path,
    input: &Path,
    temp: &Path,
    out: &Path,
    p: &SlicePlan,
    duration: &str,
) -> AppResult<SliceOutcome> {
    let encoder = pick_h264_encoder(ffmpeg);
    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-y".into(),
        "-ss".into(),
        ms_to_secs(p.start_ms),
        "-i".into(),
        input.to_string_lossy().to_string(),
        "-t".into(),
        duration.to_string(),
        "-map".into(),
        "0:v:0?".into(),
        "-map".into(),
        "0:a:0?".into(),
        "-c:v".into(),
        encoder.name().into(),
    ];
    args.extend(encoder.quality_args());
    args.extend([
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "aac".into(),
        "-b:a".into(),
        "192k".into(),
        "-movflags".into(),
        "+faststart".into(),
    ]);
    args.push(temp.to_string_lossy().to_string());

    run_ffmpeg(ffmpeg, &args).map_err(AppError::Process)?;
    let detail = if encoder.is_hardware() {
        format!("转码 → H.264（{} 硬件加速）", encoder.name())
    } else {
        "转码 → H.264（CPU）".to_string()
    };
    finish(out, temp, &detail)
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

    /// 坏文件的典型报错要能认出来。
    ///
    /// 下载阶段就是靠这几条判断「这一集能不能播」—— 认不出来，坏文件会
    /// 一路走到去重才炸，用户看到的是一屏 "Conversion failed"。
    #[test]
    fn corruption_markers_are_recognised() {
        let broken = "[aac @ 000002635632b5c0] channel element 3.11 is not allocated\n\
[hevc @ 00000263563cd9c0] Invalid NAL unit size (1746351624 > 7589).\n\
[hevc @ 00000263563cd9c0] Error splitting the input into NAL units.\n\
Conversion failed!";
        let hit = decode_error_line(broken).expect("应当认出坏文件");
        assert!(hit.contains("channel element"), "{hit}");

        // 正常的编码器统计不能误伤
        let fine = "[libx264 @ 000001] kb/s:5931.57\n[aac @ 000001] Qavg: 1351.030";
        assert!(decode_error_line(fine).is_none(), "{fine}");
    }

    /// 好片子要能过自检，坏片子（截掉一半）要能当场拦住。
    ///
    /// 做法就是把一段真编出来的片截短：mdat 断在中间 → 解码器必然报
    /// "Invalid NAL unit size"/"Error splitting the input into NAL units"。
    /// 这正是「下载没下全」产生的文件。
    #[test]
    fn decode_check_accepts_good_and_rejects_truncated() {
        let Some(ffmpeg) = std::env::var("DRAMADIST_TEST_FFMPEG")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
        else {
            eprintln!("跳过：未设置 DRAMADIST_TEST_FFMPEG");
            return;
        };

        let dir = std::env::temp_dir().join("dramadist_decode_check");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("good.mp4");
        let st = crate::proc::no_window(
            Command::new(&ffmpeg).args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x96:rate=10",
                "-t",
                "3",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
            ]),
        )
        .arg(&good)
        .status()
        .expect("生成测试视频失败");
        assert!(st.success());

        decode_check(&ffmpeg, &good).expect("好片子应当通过自检");

        // 截到一半：模拟「下载没下全」
        let bytes = std::fs::read(&good).unwrap();
        let cut = dir.join("cut.mp4");
        std::fs::write(&cut, &bytes[..bytes.len() / 2]).unwrap();
        assert!(
            decode_check(&ffmpeg, &cut).is_err(),
            "截断的片子必须被拦住，否则坏文件会一路走到去重"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 兼容模式：源是 HEVC 时必须转成 H.264。
    ///
    /// 为什么这条重要：抖音/快手/视频号的上传规范要 H.264，
    /// 而流拷贝会把 HEVC 原样保留。真实的短剧素材包正好就是 HEVC。
    #[test]
    fn compatible_mode_turns_hevc_into_h264() {
        let Some(ffmpeg) = std::env::var("DRAMADIST_TEST_FFMPEG")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
        else {
            eprintln!("跳过：未设置 DRAMADIST_TEST_FFMPEG");
            return;
        };
        let ffprobe = std::env::var("DRAMADIST_TEST_FFPROBE")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_file());
        let Some(ffprobe) = ffprobe else {
            eprintln!("跳过：未设置 DRAMADIST_TEST_FFPROBE");
            return;
        };

        let dir = std::env::temp_dir().join("dramadist_hevc_compat");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("hevc.mp4");

        // 先造一个 HEVC 源（内置 ffmpeg 带 libx265）
        let status = crate::proc::no_window(
            Command::new(&ffmpeg).args([
                "-hide_banner", "-loglevel", "error", "-y",
                "-f", "lavfi", "-i", "testsrc=size=320x240:rate=15",
                "-t", "4", "-c:v", "libx265", "-pix_fmt", "yuv420p",
                "-tag:v", "hvc1",
            ]),
        )
        .arg(&src)
        .status()
        .expect("无法启动 ffmpeg");
        assert!(status.success(), "生成 HEVC 测试素材失败");

        let probe = crate::media::probe(&ffprobe, &src).unwrap();
        assert_eq!(probe.format_name.is_empty(), false);
        // 确认源确实是 HEVC
        let enc = crate::proc::no_window(
            Command::new(&ffprobe).args(["-v", "error", "-select_streams", "v:0",
                "-show_entries", "stream=codec_name", "-of", "csv=p=0"]),
        )
        .arg(&src)
        .output()
        .unwrap();
        assert_eq!(String::from_utf8_lossy(&enc.stdout).trim(), "hevc");

        let plan = SlicePlan {
            index: 1,
            start_ms: 0,
            end_ms: 2000,
        };

        // 兼容模式：应当是 H.264
        let out_compat = dir.join("compat.mp4");
        let outcome = slice(&ffmpeg, &src, &out_compat, &plan, true).expect("兼容模式切片失败");
        assert!(outcome.size_bytes > 0);
        let codec = codec_of(&ffprobe, &out_compat);
        assert_eq!(codec, "h264", "兼容模式必须转成 H.264，实际 {codec}");

        // 非兼容模式：流拷贝，保持 HEVC（快，适合本地预览）
        let out_copy = dir.join("copy.mp4");
        slice(&ffmpeg, &src, &out_copy, &plan, false).expect("流拷贝失败");
        let codec = codec_of(&ffprobe, &out_copy);
        assert_eq!(codec, "hevc", "非兼容模式应当保持源编码，实际 {codec}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn codec_of(ffprobe: &Path, file: &Path) -> String {
        let out = crate::proc::no_window(
            Command::new(ffprobe).args(["-v", "error", "-select_streams", "v:0",
                "-show_entries", "stream=codec_name", "-of", "csv=p=0"]),
        )
        .arg(file)
        .output()
        .expect("ffprobe 失败");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }
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
            true,
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
            let outcome = slice(&ffmpeg, &input, &out, p, false).expect("切片失败");
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
