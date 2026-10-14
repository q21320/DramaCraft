//! 用户自备素材库：音效/BGM 与 LUT 滤镜，**全部由用户导入**。
//!
//! ## 为什么一个素材都不内置
//!
//! 原软件打包了 88 个音效 mp3、3 个结尾模板视频和一整套面板素材。那些资产的
//! 授权状况不明，照搬过来会把风险转嫁给使用者。所以这里**一个都不带**：
//! 你要用什么就导入什么，版权始终在你手上。
//!
//! ## 素材是怎么被用起来的
//!
//! - **音频**：混进视频当 BGM 或音效。混音链是纯函数，包含「原片没有音轨」
//!   「BGM 比片子短要不要循环」这些真实边界。
//! - **LUT**：`.cube` 调色。导入时会**解析文件头并核对数据行数** ——
//!   截断的 LUT 在 ffmpeg 里报错很难看懂，在这里就能说清是哪一行不对。
//!
//! ## 应用素材是「另存一份」
//!
//! 和画面处理一致：产出 `<原名>_bgm.mp4` / `<原名>_lut.mp4`，不改动原文件。

use crate::error::{AppError, AppResult};
use crate::models::{AssetApplyReport, CubeInfo, DiscoveredAsset, MediaProbe};
use crate::proc::{filter_path, no_window, tail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const KINDS: [(&str, &str); 2] = [("audio", "音频"), ("lut", "LUT 滤镜")];

const AUDIO_EXTS: [&str; 8] = ["mp3", "wav", "m4a", "aac", "flac", "ogg", "wma", "opus"];
const LUT_EXTS: [&str; 1] = ["cube"];

/// ffmpeg 的 lut3d 支持的上限；超过只会得到一个看不懂的报错
pub const MAX_LUT_SIZE: i64 = 128;

pub fn kind_label(kind: &str) -> String {
    KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, label)| (*label).to_string())
        .unwrap_or_else(|| kind.to_string())
}

pub fn is_known_kind(kind: &str) -> bool {
    KINDS.iter().any(|(k, _)| *k == kind)
}

/// 按扩展名判断素材类型。不认识就返回 `None`（扫描时直接跳过）。
pub fn classify(path: &Path) -> Option<&'static str> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if AUDIO_EXTS.contains(&ext.as_str()) {
        return Some("audio");
    }
    if LUT_EXTS.contains(&ext.as_str()) {
        return Some("lut");
    }
    None
}

// ---------------- .cube 解析（纯函数） ----------------

fn parse_triple(raw: &str) -> AppResult<Vec<f64>> {
    let parts: Vec<&str> = raw.split_whitespace().collect();
    if parts.len() != 3 {
        return Err(AppError::Msg(format!(
            "DOMAIN 行应当是三个数字，实际是「{}」",
            raw.trim()
        )));
    }
    let mut out = Vec::with_capacity(3);
    for p in parts {
        out.push(
            p.parse::<f64>()
                .map_err(|_| AppError::Msg(format!("DOMAIN 行里有不是数字的内容：{p}")))?,
        );
    }
    Ok(out)
}

fn extract_quoted(line: &str) -> Option<String> {
    let start = line.find('"')?;
    let rest = &line[start + 1..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// 解析 `.cube` 文件内容。
///
/// 除了读元信息，还会**核对数据行数**：`LUT_3D_SIZE n` 就必须有 `n³` 行数据。
/// 截断的 LUT 在 ffmpeg 里只会给一句没头没尾的错，在这里能直接说清。
pub fn parse_cube(text: &str) -> AppResult<CubeInfo> {
    let mut title = String::new();
    let mut size_3d: i64 = 0;
    let mut size_1d: i64 = 0;
    let mut domain_min = vec![0.0, 0.0, 0.0];
    let mut domain_max = vec![1.0, 1.0, 1.0];
    let mut data_lines: i64 = 0;

    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let upper = line.to_ascii_uppercase();

        // 先匹配长的，避免 LUT_1D_SIZE 被 LUT_ 前缀吃掉
        if let Some(rest) = upper.strip_prefix("LUT_3D_SIZE") {
            size_3d = rest.trim().parse::<i64>().map_err(|_| {
                AppError::Msg(format!("第 {} 行的 LUT_3D_SIZE 不是整数", index + 1))
            })?;
            continue;
        }
        if let Some(rest) = upper.strip_prefix("LUT_1D_SIZE") {
            size_1d = rest.trim().parse::<i64>().map_err(|_| {
                AppError::Msg(format!("第 {} 行的 LUT_1D_SIZE 不是整数", index + 1))
            })?;
            continue;
        }
        if let Some(rest) = upper.strip_prefix("DOMAIN_MIN") {
            domain_min = parse_triple(rest)?;
            continue;
        }
        if let Some(rest) = upper.strip_prefix("DOMAIN_MAX") {
            domain_max = parse_triple(rest)?;
            continue;
        }
        if let Some(rest) = upper.strip_prefix("TITLE") {
            title = extract_quoted(line).unwrap_or_else(|| rest.trim().to_string());
            continue;
        }
        // 剩下的都应当是数据行
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() != 3 {
            return Err(AppError::Msg(format!(
                "第 {} 行不像 LUT 数据（应当是三个浮点数）：{}",
                index + 1,
                tail(line)
            )));
        }
        for p in &parts {
            if p.parse::<f64>().is_err() {
                return Err(AppError::Msg(format!(
                    "第 {} 行有不是数字的内容：{p}",
                    index + 1
                )));
            }
        }
        data_lines += 1;
    }

    if size_3d <= 0 && size_1d <= 0 {
        return Err(AppError::Msg(
            "这不是一个能用的 .cube：找不到 LUT_3D_SIZE 或 LUT_1D_SIZE".into(),
        ));
    }
    if size_3d > MAX_LUT_SIZE {
        return Err(AppError::Msg(format!(
            "LUT_3D_SIZE 是 {size_3d}，超过 ffmpeg 能用的上限 {MAX_LUT_SIZE}"
        )));
    }

    let expected = if size_3d > 0 {
        size_3d * size_3d * size_3d
    } else {
        size_1d
    };
    if data_lines != expected {
        return Err(AppError::Msg(format!(
            "LUT 数据不完整：声明的是 {expected} 行，实际只有 {data_lines} 行。文件可能被截断或没下载完。"
        )));
    }

    Ok(CubeInfo {
        title,
        size: size_3d,
        size_1d,
        domain_min,
        domain_max,
        data_lines,
        expected_lines: expected,
    })
}

pub fn read_cube(path: &Path) -> AppResult<CubeInfo> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| AppError::Msg(format!("读取 .cube 失败（{}）：{e}", path.display())))?;
    parse_cube(&text)
}

// ---------------- 扫描 ----------------

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 8 || out.len() > 5000 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out, depth + 1);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

/// 扫描一个目录，列出**可以导入**的素材。不写库，先让用户看清楚。
pub fn scan_dir(dir: &Path) -> AppResult<Vec<DiscoveredAsset>> {
    if !dir.is_dir() {
        return Err(AppError::Msg(format!(
            "目录不存在：{}",
            dir.to_string_lossy()
        )));
    }
    let mut files = Vec::new();
    walk(dir, &mut files, 0);
    files.sort();

    let mut out: Vec<DiscoveredAsset> = Vec::new();
    for path in files {
        let Some(kind) = classify(&path) else {
            continue;
        };
        // LUT 在扫描阶段就验一遍，别等到导入才报错
        let problem = if kind == "lut" {
            read_cube(&path).err().map(|e| e.to_string()).unwrap_or_default()
        } else {
            String::new()
        };
        out.push(DiscoveredAsset {
            file_name: path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            path: path.to_string_lossy().to_string(),
            kind: kind.to_string(),
            size_bytes: std::fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0),
            problem,
        });
    }
    Ok(out)
}

// ---------------- 滤镜构造（纯函数） ----------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BgmMode {
    /// 和原片声音混在一起
    Mix,
    /// 用 BGM 替换原片声音
    Replace,
}

pub fn parse_bgm_mode(raw: &str) -> BgmMode {
    match raw.trim() {
        "replace" => BgmMode::Replace,
        _ => BgmMode::Mix,
    }
}

fn clamp_volume(value: f64) -> f64 {
    if !value.is_finite() {
        return 1.0;
    }
    value.clamp(0.0, 4.0)
}

/// 构造音频处理链。
///
/// - 原片**没有音轨**时无论选哪种模式都只能直接用 BGM，不能去引用 `[0:a]`；
/// - 循环要在 `volume` 之前，`aloop` 是把输入接长，不是把音量接长；
/// - `amix` 用 `duration=first`，避免 BGM 比片子长时把成片拖长。
pub fn build_bgm_filter(
    has_original_audio: bool,
    mode: BgmMode,
    bgm_volume: f64,
    original_volume: f64,
    loop_bgm: bool,
) -> String {
    let mut bgm = String::new();
    if loop_bgm {
        bgm.push_str("aloop=loop=-1:size=2e9,");
    }
    bgm.push_str(&format!("volume={:.3}", clamp_volume(bgm_volume)));

    if mode == BgmMode::Replace || !has_original_audio {
        return format!("[1:a]{bgm}[aout]");
    }
    format!(
        "[0:a]volume={:.3}[a0];[1:a]{bgm}[a1];[a0][a1]amix=inputs=2:duration=first:dropout_transition=0[aout]",
        clamp_volume(original_volume)
    )
}

/// 构造 LUT 滤镜链。
///
/// 强度不是 1.0 时走「原图 + 调色图」混合 —— `lut3d` 自己没有强度参数，
/// 直接上就是全强度，做不到"淡一点"。
pub fn build_lut_filter(lut_path: &str, intensity: f64) -> String {
    let path = filter_path(lut_path);
    let strength = if intensity.is_finite() {
        intensity.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if strength >= 0.999 {
        return format!("lut3d=file={path}");
    }
    format!(
        "split[lut_base][lut_src];[lut_src]lut3d=file={path}[lut_done];[lut_base][lut_done]blend=all_mode=normal:all_opacity={strength:.3}"
    )
}

/// 产出路径：`原名_bgm.mp4` / `原名_lut.mp4`，不动原文件。
pub fn output_path(input: &Path, suffix: &str) -> PathBuf {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "output".to_string());
    let name = format!("{stem}_{suffix}.mp4");
    match input.parent() {
        Some(dir) => dir.join(name),
        None => PathBuf::from(name),
    }
}

// ---------------- 应用 ----------------

fn run_ffmpeg(cmd: &mut Command, what: &str) -> AppResult<()> {
    let output = no_window(cmd)
        .output()
        .map_err(|e| AppError::Process(format!("无法启动 ffmpeg：{e}")))?;
    if !output.status.success() {
        return Err(AppError::Process(format!(
            "{what}失败（退出码 {:?}）：{}",
            output.status.code(),
            tail(&String::from_utf8_lossy(&output.stderr))
        )));
    }
    Ok(())
}

fn file_size(path: &Path) -> i64 {
    std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0)
}

/// 给视频套一个 LUT。
pub fn apply_lut(
    ffmpeg: &Path,
    video: &Path,
    lut: &Path,
    out: &Path,
    intensity: f64,
) -> AppResult<AssetApplyReport> {
    if !video.is_file() {
        return Err(AppError::Msg(format!(
            "素材文件不存在：{}",
            video.to_string_lossy()
        )));
    }
    if !lut.is_file() {
        return Err(AppError::Msg(format!(
            "LUT 文件不存在：{}",
            lut.to_string_lossy()
        )));
    }
    // 先验一遍，别把截断的 LUT 丢给 ffmpeg
    let info = read_cube(lut)?;
    let filter = build_lut_filter(&lut.to_string_lossy(), intensity);

    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if out.exists() {
        let _ = std::fs::remove_file(out);
    }

    run_ffmpeg(
        Command::new(ffmpeg)
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-y")
            .arg("-i")
            .arg(video.as_os_str())
            .arg("-vf")
            .arg(&filter)
            .arg("-c:v")
            .arg("libx264")
            .arg("-crf")
            .arg("20")
            .arg("-preset")
            .arg("veryfast")
            .arg("-pix_fmt")
            .arg("yuv420p")
            .arg("-c:a")
            .arg("copy")
            .arg("-movflags")
            .arg("+faststart")
            .arg(out.as_os_str()),
        "套用 LUT",
    )?;

    Ok(AssetApplyReport {
        out_path: out.to_string_lossy().to_string(),
        size_bytes: file_size(out),
        duration_ms: 0,
        filter,
        warnings: vec![format!(
            "LUT：{}（{}³）",
            if info.title.is_empty() {
                "未命名"
            } else {
                &info.title
            },
            info.size
        )],
    })
}

/// 给视频配 BGM 或音效。
#[allow(clippy::too_many_arguments)]
pub fn apply_bgm(
    ffmpeg: &Path,
    ffprobe: &Path,
    video: &Path,
    audio: &Path,
    out: &Path,
    mode: BgmMode,
    bgm_volume: f64,
    original_volume: f64,
    loop_bgm: bool,
    video_probe: Option<&MediaProbe>,
) -> AppResult<AssetApplyReport> {
    if !video.is_file() {
        return Err(AppError::Msg(format!(
            "素材文件不存在：{}",
            video.to_string_lossy()
        )));
    }
    if !audio.is_file() {
        return Err(AppError::Msg(format!(
            "音频不存在：{}",
            audio.to_string_lossy()
        )));
    }

    let owned_probe;
    let probe = match video_probe {
        Some(p) => p,
        None => {
            owned_probe = crate::media::probe(ffprobe, video)?;
            &owned_probe
        }
    };
    if probe.duration_ms <= 0 {
        return Err(AppError::Msg("读不出素材时长，无法确定成片长度".into()));
    }

    let audio_probe = crate::media::probe(ffprobe, audio)?;
    if !audio_probe.has_audio {
        return Err(AppError::Msg(format!(
            "这个文件里没有音轨：{}",
            audio.to_string_lossy()
        )));
    }

    let filter = build_bgm_filter(probe.has_audio, mode, bgm_volume, original_volume, loop_bgm);

    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if out.exists() {
        let _ = std::fs::remove_file(out);
    }

    // -t 用素材时长兜底：BGM 比片子长时，否则成片会被拖长到 BGM 的长度
    let seconds = format!("{:.3}", probe.duration_ms as f64 / 1000.0);
    run_ffmpeg(
        Command::new(ffmpeg)
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-y")
            .arg("-i")
            .arg(video.as_os_str())
            .arg("-i")
            .arg(audio.as_os_str())
            .arg("-filter_complex")
            .arg(&filter)
            .arg("-map")
            .arg("0:v")
            .arg("-map")
            .arg("[aout]")
            .arg("-c:v")
            .arg("copy")
            .arg("-c:a")
            .arg("aac")
            .arg("-b:a")
            .arg("192k")
            .arg("-t")
            .arg(&seconds)
            .arg("-movflags")
            .arg("+faststart")
            .arg(out.as_os_str()),
        "配 BGM",
    )?;

    let mut warnings: Vec<String> = Vec::new();
    if !probe.has_audio {
        warnings.push("原片没有音轨，成片里只有你配的音频。".to_string());
    }
    if audio_probe.duration_ms > 0 && audio_probe.duration_ms < probe.duration_ms && !loop_bgm {
        warnings.push(format!(
            "音频比素材短（{:.1}s vs {:.1}s），后半段会没有声音。需要铺满请勾选「循环」。",
            audio_probe.duration_ms as f64 / 1000.0,
            probe.duration_ms as f64 / 1000.0
        ));
    }

    Ok(AssetApplyReport {
        out_path: out.to_string_lossy().to_string(),
        size_bytes: file_size(out),
        duration_ms: probe.duration_ms,
        filter,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_cube() -> String {
        // 2×2×2 = 8 行数据
        let mut text = String::from(
            "# Generated by hand\nTITLE \"My Look\"\nLUT_3D_SIZE 2\nDOMAIN_MIN 0.0 0.0 0.0\nDOMAIN_MAX 1.0 1.0 1.0\n",
        );
        for i in 0..8 {
            text.push_str(&format!("0.{} 0.{} 0.{}\n", i, i, i));
        }
        text
    }

    // ---------------- 分类 ----------------

    #[test]
    fn classify_by_extension() {
        assert_eq!(classify(Path::new("a/b/sfx.mp3")), Some("audio"));
        assert_eq!(classify(Path::new("a/B/SFX.MP3")), Some("audio"));
        assert_eq!(classify(Path::new("look.cube")), Some("lut"));
        assert_eq!(classify(Path::new("movie.mp4")), None);
        assert_eq!(classify(Path::new("noext")), None);
    }

    #[test]
    fn kind_labels_and_validation() {
        assert_eq!(kind_label("audio"), "音频");
        assert_eq!(kind_label("lut"), "LUT 滤镜");
        assert_eq!(kind_label("other"), "other");
        assert!(is_known_kind("audio"));
        assert!(!is_known_kind("video"));
    }

    // ---------------- .cube 解析 ----------------

    #[test]
    fn parses_a_valid_cube() {
        let info = parse_cube(&valid_cube()).unwrap();
        assert_eq!(info.title, "My Look");
        assert_eq!(info.size, 2);
        assert_eq!(info.size_1d, 0);
        assert_eq!(info.data_lines, 8);
        assert_eq!(info.expected_lines, 8);
        assert_eq!(info.domain_min, vec![0.0, 0.0, 0.0]);
        assert_eq!(info.domain_max, vec![1.0, 1.0, 1.0]);
    }

    #[test]
    fn truncated_cube_is_caught_with_a_clear_message() {
        let text = valid_cube().replace("0.7 0.7 0.7\n", "");
        let err = parse_cube(&text).unwrap_err().to_string();
        assert!(err.contains("数据不完整"), "{err}");
        assert!(err.contains("8 行"), "要说清声明了多少行：{err}");
        assert!(err.contains("7 行"), "也要说清实际多少行：{err}");
        assert!(err.contains("截断"), "{err}");
    }

    #[test]
    fn cube_without_size_is_rejected() {
        let err = parse_cube("0.0 0.0 0.0\n0.5 0.5 0.5\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("LUT_3D_SIZE"), "{err}");
    }

    #[test]
    fn cube_size_beyond_ffmpeg_limit_is_rejected() {
        let text = format!("LUT_3D_SIZE {}\n", MAX_LUT_SIZE + 1);
        let err = parse_cube(&text).unwrap_err().to_string();
        assert!(err.contains("上限"), "{err}");
    }

    #[test]
    fn malformed_data_lines_are_reported_with_line_numbers() {
        let mut text = String::from("LUT_3D_SIZE 2\n");
        text.push_str("0.0 0.0 0.0\n");
        text.push_str("garbage here now\n");
        let err = parse_cube(&text).unwrap_err().to_string();
        assert!(err.contains("第 3 行"), "{err}");

        let mut text = String::from("LUT_3D_SIZE 2\n");
        text.push_str("0.0 0.0\n");
        let err = parse_cube(&text).unwrap_err().to_string();
        assert!(err.contains("三个浮点数") || err.contains("三个数字"), "{err}");
    }

    #[test]
    fn one_dimensional_cube_is_supported() {
        let text = "LUT_1D_SIZE 4\n0.0 0.0 0.0\n0.3 0.3 0.3\n0.6 0.6 0.6\n1.0 1.0 1.0\n";
        let info = parse_cube(text).unwrap();
        assert_eq!(info.size_1d, 4);
        assert_eq!(info.size, 0);
        assert_eq!(info.expected_lines, 4);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let text = "\n# a comment\n\nLUT_3D_SIZE 2\n# another\n0 0 0\n0 0 1\n0 1 0\n0 1 1\n1 0 0\n1 0 1\n1 1 0\n1 1 1\n\n";
        let info = parse_cube(text).unwrap();
        assert_eq!(info.data_lines, 8);
        assert!(info.title.is_empty());
    }

    #[test]
    fn custom_domain_is_read() {
        let text = "LUT_3D_SIZE 2\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 255 255 255\n";
        let mut full = text.to_string();
        for _ in 0..8 {
            full.push_str("128 128 128\n");
        }
        let info = parse_cube(&full).unwrap();
        assert_eq!(info.domain_max, vec![255.0, 255.0, 255.0]);
    }

    #[test]
    fn read_cube_reports_missing_file() {
        let err = read_cube(Path::new("Z:/nope/missing.cube"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("读取 .cube 失败"), "{err}");
    }

    // ---------------- 滤镜构造 ----------------

    #[test]
    fn bgm_filter_mixes_when_original_has_audio() {
        let f = build_bgm_filter(true, BgmMode::Mix, 0.3, 1.0, false);
        assert!(f.contains("[0:a]volume=1.000[a0]"), "{f}");
        assert!(f.contains("[1:a]volume=0.300[a1]"), "{f}");
        assert!(f.contains("amix=inputs=2:duration=first"), "{f}");
        assert!(!f.contains("aloop"), "{f}");
    }

    #[test]
    fn bgm_filter_replaces_when_asked() {
        let f = build_bgm_filter(true, BgmMode::Replace, 0.5, 1.0, false);
        assert_eq!(f, "[1:a]volume=0.500[aout]");
    }

    #[test]
    fn bgm_filter_ignores_missing_original_track() {
        // 原片没有音轨时，即使选了"混合"也不能去引用 [0:a]
        let f = build_bgm_filter(false, BgmMode::Mix, 0.5, 1.0, false);
        assert_eq!(f, "[1:a]volume=0.500[aout]");
        assert!(!f.contains("[0:a]"), "不能引用不存在的音轨：{f}");
    }

    #[test]
    fn loop_is_applied_before_volume() {
        let f = build_bgm_filter(false, BgmMode::Replace, 0.4, 1.0, true);
        assert!(f.starts_with("[1:a]aloop=loop=-1:size=2e9,volume=0.400"), "{f}");
    }

    #[test]
    fn volume_is_clamped_and_nan_safe() {
        assert!(build_bgm_filter(false, BgmMode::Replace, 99.0, 1.0, false).contains("4.000"));
        assert!(build_bgm_filter(false, BgmMode::Replace, -5.0, 1.0, false).contains("0.000"));
        assert!(build_bgm_filter(false, BgmMode::Replace, f64::NAN, 1.0, false).contains("1.000"));
        assert!(build_bgm_filter(true, BgmMode::Mix, 0.5, f64::INFINITY, false).contains("volume=1.000[a0]"));
    }

    #[test]
    fn bgm_mode_parsing_defaults_to_mix() {
        assert_eq!(parse_bgm_mode("replace"), BgmMode::Replace);
        assert_eq!(parse_bgm_mode(" mix "), BgmMode::Mix);
        assert_eq!(parse_bgm_mode("nonsense"), BgmMode::Mix);
        assert_eq!(parse_bgm_mode(""), BgmMode::Mix);
    }

    #[test]
    fn lut_filter_is_plain_at_full_strength() {
        let f = build_lut_filter("C:/luts/a.cube", 1.0);
        assert_eq!(f, "lut3d=file='C\\:/luts/a.cube'");
    }

    #[test]
    fn lut_filter_blends_when_partial_strength() {
        let f = build_lut_filter("C:/luts/a.cube", 0.4);
        assert!(f.contains("split[lut_base][lut_src]"), "{f}");
        assert!(f.contains("lut3d=file='C\\:/luts/a.cube'[lut_done]"), "{f}");
        assert!(f.contains("blend=all_mode=normal:all_opacity=0.400"), "{f}");
        // 最后必须留一个未命名的输出给 -vf
        assert!(!f.ends_with(']'), "{f}");
    }

    #[test]
    fn lut_intensity_is_clamped() {
        assert_eq!(build_lut_filter("a.cube", 2.0), build_lut_filter("a.cube", 1.0));
        assert!(build_lut_filter("a.cube", 0.0).contains("all_opacity=0.000"));
        // NaN 当作全强度，而不是产出一个坏 filter
        assert_eq!(build_lut_filter("a.cube", f64::NAN), build_lut_filter("a.cube", 1.0));
    }

    // ---------------- 路径 ----------------

    #[test]
    fn output_path_keeps_the_stem_and_folder() {
        assert_eq!(
            output_path(Path::new("E:/d/剧01.mp4"), "bgm"),
            PathBuf::from("E:/d/剧01_bgm.mp4")
        );
        assert_eq!(
            output_path(Path::new("剧01.mp4"), "lut"),
            PathBuf::from("剧01_lut.mp4")
        );
    }

    #[test]
    fn apply_validates_inputs_before_spawning() {
        let err = apply_lut(
            Path::new("Z:/nope/ffmpeg.exe"),
            Path::new("Z:/nope/v.mp4"),
            Path::new("Z:/nope/l.cube"),
            Path::new("Z:/nope/out.mp4"),
            1.0,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("素材文件不存在"), "{err}");

        let err = apply_bgm(
            Path::new("Z:/nope/ffmpeg.exe"),
            Path::new("Z:/nope/ffprobe.exe"),
            Path::new("Z:/nope/v.mp4"),
            Path::new("Z:/nope/a.mp3"),
            Path::new("Z:/nope/out.mp4"),
            BgmMode::Mix,
            0.3,
            1.0,
            false,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("素材文件不存在"), "{err}");
    }

    #[test]
    fn scan_rejects_a_missing_directory() {
        let err = scan_dir(Path::new("Z:/nope/nowhere"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("目录不存在"), "{err}");
    }

    #[test]
    fn scan_finds_assets_and_flags_broken_luts() {
        let dir = std::env::temp_dir().join("dramadist_asset_scan");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("boom.mp3"), b"not really audio").unwrap();
        std::fs::write(dir.join("sub").join("good.cube"), valid_cube()).unwrap();
        std::fs::write(dir.join("sub").join("broken.cube"), "LUT_3D_SIZE 2\n0 0 0\n").unwrap();
        std::fs::write(dir.join("ignore.txt"), b"nope").unwrap();

        let found = scan_dir(&dir).unwrap();
        assert_eq!(found.len(), 3, "应当只认出音频与 cube：{found:?}");

        let good = found.iter().find(|a| a.file_name == "good.cube").unwrap();
        assert_eq!(good.kind, "lut");
        assert!(good.problem.is_empty(), "{}", good.problem);

        // 截断的 LUT 在扫描阶段就要标出来，别等到导入才报错
        let broken = found.iter().find(|a| a.file_name == "broken.cube").unwrap();
        assert!(!broken.problem.is_empty(), "坏 LUT 应当被标记");
        assert!(broken.problem.contains("数据不完整"), "{}", broken.problem);

        let audio = found.iter().find(|a| a.file_name == "boom.mp3").unwrap();
        assert_eq!(audio.kind, "audio");
        assert!(audio.size_bytes > 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------- 端到端（真实 ffmpeg，默认跳过） ----------------

    fn test_tools() -> Option<(PathBuf, PathBuf)> {
        let ffmpeg = std::env::var("DRAMADIST_TEST_FFMPEG")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_file())?;
        let ffprobe = std::env::var("DRAMADIST_TEST_FFPROBE")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_file())?;
        Some((ffmpeg, ffprobe))
    }

    fn make_video(ffmpeg: &Path, out: &Path, seconds: u32) {
        let status = no_window(
            Command::new(ffmpeg)
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
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
                    &seconds.to_string(),
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:a",
                    "aac",
                    "-shortest",
                ])
                .arg(out),
        )
        .status()
        .expect("无法启动 ffmpeg");
        assert!(status.success(), "生成测试视频失败");
    }

    /// 一个把颜色整个反过来的 2×2×2 LUT —— 效果足够明显，能验证"确实变了"
    fn write_invert_lut(path: &Path) {
        let mut text = String::from("TITLE \"Invert\"\nLUT_3D_SIZE 2\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    text.push_str(&format!(
                        "{}.0 {}.0 {}.0\n",
                        1 - r,
                        1 - g,
                        1 - b
                    ));
                }
            }
        }
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn end_to_end_applies_bgm_and_lut() {
        let Some((ffmpeg, ffprobe)) = test_tools() else {
            eprintln!("跳过：未设置 DRAMADIST_TEST_FFMPEG / DRAMADIST_TEST_FFPROBE");
            return;
        };

        let dir = std::env::temp_dir().join("dramadist_asset_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let video = dir.join("clip.mp4");
        make_video(&ffmpeg, &video, 3);

        let music = dir.join("bgm.mp3");
        let status = no_window(
            Command::new(&ffmpeg).args([
                "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i",
                "sine=frequency=220", "-t", "1",
            ]),
        )
        .arg("-c:a")
        .arg("libmp3lame")
        .arg(&music)
        .status()
        .expect("无法启动 ffmpeg");
        assert!(status.success(), "生成测试音频失败");

        let probe = crate::media::probe(&ffprobe, &video).unwrap();
        assert!(probe.has_audio, "测试视频应当带音轨");

        // --- BGM ---
        let bgm_out = output_path(&video, "bgm");
        let report = apply_bgm(
            &ffmpeg,
            &ffprobe,
            &video,
            &music,
            &bgm_out,
            BgmMode::Mix,
            0.3,
            1.0,
            true,
            Some(&probe),
        )
        .expect("配 BGM 失败");

        assert!(bgm_out.is_file(), "没有产出成片");
        assert!(report.size_bytes > 1_000, "成片太小：{}", report.size_bytes);
        assert_eq!(report.duration_ms, probe.duration_ms);
        assert!(report.filter.contains("amix"), "{}", report.filter);

        let bgm_probe = crate::media::probe(&ffprobe, &bgm_out).unwrap();
        assert!(bgm_probe.has_audio, "成片应当有音轨");
        // 成片时长要贴着素材，不能被 BGM 拖长
        assert!(
            (bgm_probe.duration_ms - probe.duration_ms).abs() < 700,
            "成片时长 {} 偏离素材 {} 太多",
            bgm_probe.duration_ms,
            probe.duration_ms
        );

        // 循环 + 音频只有 1 秒，不该再报"后半段没声音"
        assert!(
            !report.warnings.iter().any(|w| w.contains("后半段")),
            "勾了循环就不该再提醒：{:?}",
            report.warnings
        );

        // --- LUT ---
        let lut = dir.join("invert.cube");
        write_invert_lut(&lut);

        let lut_out = output_path(&video, "lut");
        let lut_report = apply_lut(&ffmpeg, &video, &lut, &lut_out, 1.0).expect("套用 LUT 失败");
        assert!(lut_out.is_file());
        assert!(lut_report.size_bytes > 1_000);
        assert!(lut_report.filter.starts_with("lut3d=file="), "{}", lut_report.filter);
        assert!(
            lut_report.warnings.iter().any(|w| w.contains("Invert")),
            "报告里应当带上 LUT 的名字：{:?}",
            lut_report.warnings
        );

        // 闭环：用感知哈希确认画面**真的变了**，而不是原样复制了一份
        let after = crate::media::probe(&ffprobe, &lut_out).unwrap();
        let similarity = crate::verify::compare_files(
            &ffmpeg,
            &video,
            &probe,
            &lut_out,
            &after,
            4,
        )
        .expect("比对失败");
        assert!(
            similarity.mean_similarity < 0.9,
            "反色 LUT 之后画面应当明显不同，实际相似度 {:.3}",
            similarity.mean_similarity
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn end_to_end_warns_when_audio_is_shorter_than_the_video() {
        let Some((ffmpeg, ffprobe)) = test_tools() else {
            eprintln!("跳过：未设置 DRAMADIST_TEST_FFMPEG");
            return;
        };

        let dir = std::env::temp_dir().join("dramadist_asset_short");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let video = dir.join("clip.mp4");
        make_video(&ffmpeg, &video, 4);

        let music = dir.join("bgm.mp3");
        let status = no_window(
            Command::new(&ffmpeg).args([
                "-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i",
                "sine=frequency=220", "-t", "1",
            ]),
        )
        .arg("-c:a")
        .arg("libmp3lame")
        .arg(&music)
        .status()
        .expect("无法启动 ffmpeg");
        assert!(status.success());

        let probe = crate::media::probe(&ffprobe, &video).unwrap();
        let out = output_path(&video, "bgm");
        let report = apply_bgm(
            &ffmpeg,
            &ffprobe,
            &video,
            &music,
            &out,
            BgmMode::Mix,
            0.3,
            1.0,
            false, // 不循环
            Some(&probe),
        )
        .expect("配 BGM 失败");

        // 没勾循环时，必须如实提醒后半段没声音，而不是让用户自己发现
        assert!(
            report.warnings.iter().any(|w| w.contains("后半段")),
            "应当提醒音频比素材短：{:?}",
            report.warnings
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
