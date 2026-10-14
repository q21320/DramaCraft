//! 成片导出：烧字幕与竖屏适配。
//!
//! 两个设计决定：
//! 1. **不转画幅也不烧字幕时走流拷贝**，最快且无损；
//! 2. **烧字幕时把 ffmpeg 的工作目录设为字幕所在目录**，滤镜里只写文件名 ——
//!    这样彻底避开 Windows 盘符冒号在 filtergraph 里的转义地狱。

use crate::error::{AppError, AppResult};
use crate::media::ms_to_secs;
use crate::models::{AspectMode, ExportOptions, ExportOutcome};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const TARGET_W: u32 = 1080;
pub const TARGET_H: u32 = 1920;
const STDERR_KEEP: usize = 800;

const PRESETS: [&str; 9] = [
    "ultrafast",
    "superfast",
    "veryfast",
    "faster",
    "fast",
    "medium",
    "slow",
    "slower",
    "veryslow",
];

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

fn tail(text: &str) -> String {
    let t = text.trim();
    if t.chars().count() <= STDERR_KEEP {
        return t.to_string();
    }
    let skip = t.chars().count() - STDERR_KEEP;
    format!("…{}", t.chars().skip(skip).collect::<String>())
}

/// 只接受白名单 preset，避免把用户输入直接拼进命令行。
pub fn normalize_preset(raw: &str) -> String {
    let value = raw.trim().to_lowercase();
    if PRESETS.contains(&value.as_str()) {
        value
    } else {
        "veryfast".to_string()
    }
}

pub fn normalize_crf(raw: u32) -> u32 {
    raw.clamp(0, 51)
}

/// filtergraph 里的路径转义。文件名已在工作目录内，只需处理引号、冒号、逗号与方括号。
fn escape_subtitle_name(name: &str) -> String {
    let mut escaped = String::with_capacity(name.len() + 2);
    for ch in name.chars() {
        match ch {
            '\\' => escaped.push('/'),
            '\'' => escaped.push_str("\\'"),
            ':' => escaped.push_str("\\:"),
            ',' => escaped.push_str("\\,"),
            '[' => escaped.push_str("\\["),
            ']' => escaped.push_str("\\]"),
            other => escaped.push(other),
        }
    }
    format!("'{escaped}'")
}

/// 构造 `-vf` 滤镜串。纯函数，便于把画幅与字幕的组合测干净。
pub fn build_filter(aspect: &AspectMode, subtitle_name: Option<&str>) -> String {
    let base = match aspect {
        AspectMode::Keep => String::new(),
        AspectMode::VerticalCrop => format!(
            "scale={TARGET_W}:{TARGET_H}:force_original_aspect_ratio=increase,crop={TARGET_W}:{TARGET_H}"
        ),
        AspectMode::VerticalBlur => format!(
            "split=2[bg][fg];\
             [bg]scale={TARGET_W}:{TARGET_H}:force_original_aspect_ratio=increase,\
             crop={TARGET_W}:{TARGET_H},gblur=sigma=24[bgb];\
             [fg]scale={TARGET_W}:{TARGET_H}:force_original_aspect_ratio=decrease[fgs];\
             [bgb][fgs]overlay=(W-w)/2:(H-h)/2"
        ),
    };

    let subtitle = subtitle_name
        .map(|name| format!("subtitles={}", escape_subtitle_name(name)))
        .unwrap_or_default();

    match (base.is_empty(), subtitle.is_empty()) {
        (true, true) => String::new(),
        (false, true) => base,
        (true, false) => subtitle,
        (false, false) => format!("{base},{subtitle}"),
    }
}

fn temp_path(out: &Path) -> AppResult<PathBuf> {
    let stem = out
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "export".into());
    let ext = out
        .extension()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "mp4".into());
    let dir = out.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    Ok(dir.join(format!("{stem}.part.{ext}")))
}

/// 执行导出。`subtitle` 为 `None` 表示不烧字幕。
pub fn export(
    ffmpeg: &Path,
    input: &Path,
    opts: &ExportOptions,
    subtitle: Option<&Path>,
) -> AppResult<ExportOutcome> {
    if !input.is_file() {
        return Err(AppError::Msg(format!(
            "素材文件不存在：{}",
            input.to_string_lossy()
        )));
    }
    let out = PathBuf::from(opts.out_path.trim());
    if out.as_os_str().is_empty() {
        return Err(AppError::Msg("请先设置导出路径".into()));
    }
    let temp = temp_path(&out)?;

    let (subtitle_name, work_dir) = match subtitle {
        Some(path) if path.is_file() => {
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if name.is_empty() {
                return Err(AppError::Msg("字幕文件名称为空".into()));
            }
            (Some(name), path.parent().map(|d| d.to_path_buf()))
        }
        Some(path) => {
            return Err(AppError::Msg(format!(
                "字幕文件不存在：{}",
                path.to_string_lossy()
            )))
        }
        None => (None, None),
    };

    let filter = build_filter(&opts.aspect, subtitle_name.as_deref());
    let range = opts.end_ms > opts.start_ms && opts.start_ms >= 0;

    let mut args: Vec<String> = vec!["-hide_banner".into(), "-nostdin".into(), "-y".into()];
    if opts.start_ms > 0 {
        args.push("-ss".into());
        args.push(ms_to_secs(opts.start_ms));
    }
    args.push("-i".into());
    args.push(input.to_string_lossy().to_string());
    if range {
        args.push("-t".into());
        args.push(ms_to_secs(opts.end_ms - opts.start_ms));
    }

    if filter.is_empty() {
        for a in [
            "-map",
            "0:v:0?",
            "-map",
            "0:a:0?",
            "-c",
            "copy",
            "-avoid_negative_ts",
            "make_zero",
        ] {
            args.push(a.into());
        }
    } else {
        args.push("-vf".into());
        args.push(filter.clone());
        for a in ["-c:v", "libx264", "-preset"] {
            args.push(a.into());
        }
        args.push(normalize_preset(&opts.preset));
        args.push("-crf".into());
        args.push(normalize_crf(opts.crf).to_string());
        for a in ["-pix_fmt", "yuv420p", "-c:a", "aac", "-b:a", "192k"] {
            args.push(a.into());
        }
    }

    for a in ["-movflags", "+faststart"] {
        args.push(a.into());
    }
    args.push(temp.to_string_lossy().to_string());

    let mut cmd = Command::new(ffmpeg);
    cmd.args(&args);
    if let Some(dir) = &work_dir {
        cmd.current_dir(dir);
    }

    let output = no_window(&mut cmd)
        .output()
        .map_err(|e| AppError::Process(format!("无法启动 ffmpeg：{e}")))?;

    if !output.status.success() {
        let _ = std::fs::remove_file(&temp);
        return Err(AppError::Process(format!(
            "导出失败（退出码 {:?}）：{}",
            output.status.code(),
            tail(&String::from_utf8_lossy(&output.stderr))
        )));
    }
    if !temp.is_file() {
        return Err(AppError::Process("ffmpeg 报告成功但没有产出文件".into()));
    }

    if out.exists() {
        std::fs::remove_file(&out)?;
    }
    std::fs::rename(&temp, &out)?;

    Ok(ExportOutcome {
        out_path: out.to_string_lossy().to_string(),
        size_bytes: std::fs::metadata(&out).map(|m| m.len() as i64).unwrap_or(0),
        duration_ms: if range { opts.end_ms - opts.start_ms } else { 0 },
        filter: if filter.is_empty() {
            "(流拷贝，未重编码)".into()
        } else {
            filter
        },
        log: tail(&String::from_utf8_lossy(&output.stderr)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_without_subtitles_needs_no_filter() {
        assert_eq!(build_filter(&AspectMode::Keep, None), "");
    }

    #[test]
    fn keep_with_subtitles_only_burns() {
        assert_eq!(
            build_filter(&AspectMode::Keep, Some("001.srt")),
            "subtitles='001.srt'"
        );
    }

    #[test]
    fn vertical_crop_scales_and_crops_to_1080x1920() {
        let f = build_filter(&AspectMode::VerticalCrop, None);
        assert!(f.contains("scale=1080:1920:force_original_aspect_ratio=increase"));
        assert!(f.contains("crop=1080:1920"));
        assert!(!f.contains("overlay"));
    }

    #[test]
    fn vertical_blur_builds_a_two_branch_graph() {
        let f = build_filter(&AspectMode::VerticalBlur, None);
        assert!(f.contains("split=2[bg][fg]"), "{f}");
        assert!(f.contains("gblur=sigma=24"), "{f}");
        assert!(f.contains("overlay=(W-w)/2:(H-h)/2"), "{f}");
    }

    #[test]
    fn subtitles_are_appended_after_the_graph() {
        let f = build_filter(&AspectMode::VerticalBlur, Some("a.srt"));
        assert!(f.starts_with("split=2[bg][fg]"), "{f}");
        assert!(f.ends_with("overlay=(W-w)/2:(H-h)/2,subtitles='a.srt'"), "{f}");
    }

    #[test]
    fn escapes_dangerous_characters_in_subtitle_names() {
        // 冒号、单引号、逗号、方括号都要转义，反斜杠统一成正斜杠
        assert_eq!(
            escape_subtitle_name("a:b'c,d[e]f"),
            "'a\\:b\\'c\\,d\\[e\\]f'"
        );
        assert_eq!(escape_subtitle_name("子目录\\片名.srt"), "'子目录/片名.srt'");
    }

    #[test]
    fn preset_and_crf_are_whitelisted_and_clamped() {
        assert_eq!(normalize_preset("veryfast"), "veryfast");
        assert_eq!(normalize_preset("  MEDIUM "), "medium");
        // 非法值回落到安全默认，不会原样拼进命令行
        assert_eq!(normalize_preset("; rm -rf /"), "veryfast");
        assert_eq!(normalize_preset(""), "veryfast");
        assert_eq!(normalize_crf(999), 51);
        assert_eq!(normalize_crf(23), 23);
    }

    #[test]
    fn temp_path_keeps_extension_for_container_guessing() {
        let t = temp_path(Path::new("E:/out/final.mp4")).unwrap();
        assert_eq!(t.file_name().unwrap().to_string_lossy(), "final.part.mp4");
    }

    #[test]
    fn missing_input_is_reported_before_spawning_ffmpeg() {
        let opts = ExportOptions {
            media_id: 1,
            out_path: "E:/nope/out.mp4".into(),
            burn_subtitles: false,
            aspect: AspectMode::Keep,
            start_ms: 0,
            end_ms: 0,
            crf: 23,
            preset: "veryfast".into(),
        };
        let err = export(
            Path::new("Z:/nope/ffmpeg.exe"),
            Path::new("Z:/nope/missing.mp4"),
            &opts,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("素材文件不存在"), "实际：{err}");
    }

    // ---------------- 端到端（需要真实 ffmpeg，默认跳过） ----------------

    fn test_ffmpeg() -> Option<PathBuf> {
        let path = std::env::var("DRAMADIST_TEST_FFMPEG").ok()?;
        let path = PathBuf::from(path);
        path.is_file().then_some(path)
    }

    fn test_ffprobe() -> Option<PathBuf> {
        let path = std::env::var("DRAMADIST_TEST_FFPROBE").ok()?;
        let path = PathBuf::from(path);
        path.is_file().then_some(path)
    }

    fn opts(out: PathBuf, aspect: AspectMode, burn: bool) -> ExportOptions {
        ExportOptions {
            media_id: 1,
            out_path: out.to_string_lossy().to_string(),
            burn_subtitles: burn,
            aspect,
            start_ms: 0,
            end_ms: 0,
            crf: 28,
            preset: "ultrafast".into(),
        }
    }

    #[test]
    fn end_to_end_export_stream_copy_and_vertical_with_subtitles() {
        let (Some(ffmpeg), Some(ffprobe)) = (test_ffmpeg(), test_ffprobe()) else {
            eprintln!("跳过端到端导出测试：未设置 DRAMADIST_TEST_FFMPEG / DRAMADIST_TEST_FFPROBE");
            return;
        };

        let dir = std::env::temp_dir().join("dramadist_export_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 1) 造素材：6 秒 320x240 带音轨
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
                "6",
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
            "生成素材失败：{}",
            tail(&String::from_utf8_lossy(&generated.stderr))
        );

        // 2) 不转画幅也不烧字幕 → 流拷贝
        let copied = dir.join("copy.mp4");
        let outcome = export(&ffmpeg, &input, &opts(copied.clone(), AspectMode::Keep, false), None)
            .expect("流拷贝导出失败");
        assert!(outcome.filter.contains("流拷贝"));
        assert!(outcome.size_bytes > 0);
        let info = crate::media::probe(&ffprobe, &copied).unwrap();
        assert_eq!((info.width, info.height), (320, 240));

        // 3) 竖屏模糊 + 烧字幕
        let srt = dir.join("sub.srt");
        std::fs::write(
            &srt,
            "1\n00:00:00,200 --> 00:00:02,500\n第一句字幕\n\n2\n00:00:03,000 --> 00:00:05,000\n第二句字幕\n",
        )
        .unwrap();
        let burned = dir.join("vertical.mp4");
        let outcome = export(
            &ffmpeg,
            &input,
            &opts(burned.clone(), AspectMode::VerticalBlur, true),
            Some(&srt),
        )
        .expect("竖屏烧字幕导出失败");
        assert!(outcome.filter.contains("subtitles="), "{}", outcome.filter);
        assert!(outcome.size_bytes > 0);
        let info = crate::media::probe(&ffprobe, &burned).unwrap();
        assert_eq!(
            (info.width, info.height),
            (TARGET_W as i64, TARGET_H as i64),
            "竖屏输出应为 1080x1920"
        );
        assert!(info.has_audio, "导出应保留音轨");

        // 4) 不应留下 .part 临时文件
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".part."))
            .count();
        assert_eq!(leftovers, 0, "输出目录残留了临时文件");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
