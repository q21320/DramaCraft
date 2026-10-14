//! 画面处理（差异化）：镜像、裁切、缩放、调色、噪点、变速。
//!
//! 这些是**通用视频处理能力**，用于二次创作时让每个片段有区别。
//! 前提是你对素材拥有相应权利 —— 工具本身不改变这一点。
//!
//! 三个设计要点：
//! 1. 滤镜串构造是**纯函数**，可以把每个开关和边界测干净；
//! 2. 所有数值先夹到安全范围（含 NaN），用户输入绝不原样进命令行；
//! 3. 每个片段只重编码一次，临时文件 + 原子重命名。

use crate::error::{AppError, AppResult};
use crate::models::TransformOptions;
use crate::proc::{no_window, tail};
use std::path::{Path, PathBuf};
use std::process::Command;

// ---------------- 安全范围 ----------------

pub const CROP_MIN: f64 = 0.0;
pub const CROP_MAX: f64 = 0.10;
pub const SCALE_MIN: f64 = 0.96;
pub const SCALE_MAX: f64 = 1.04;
pub const BRIGHTNESS_MIN: f64 = -0.05;
pub const BRIGHTNESS_MAX: f64 = 0.05;
pub const CONTRAST_MIN: f64 = 0.95;
pub const CONTRAST_MAX: f64 = 1.05;
pub const NOISE_MAX: u32 = 10;
pub const SPEED_MIN: f64 = 0.90;
pub const SPEED_MAX: f64 = 1.10;
pub const VOLUME_MIN: f64 = 0.80;
pub const VOLUME_MAX: f64 = 1.20;

/// 浮点比较容差：小于它就当作"没改"，避免生成 `scale=1.0000` 这种无用滤镜。
const EPS: f64 = 1e-4;

fn clamp_finite(value: f64, lo: f64, hi: f64, fallback: f64) -> f64 {
    if !value.is_finite() {
        return fallback;
    }
    value.clamp(lo, hi)
}

/// 把所有字段夹到安全范围。NaN / 无穷会退回默认值。
pub fn normalize(raw: &TransformOptions) -> TransformOptions {
    TransformOptions {
        mirror: raw.mirror,
        crop_ratio: clamp_finite(raw.crop_ratio, CROP_MIN, CROP_MAX, 0.0),
        scale_ratio: clamp_finite(raw.scale_ratio, SCALE_MIN, SCALE_MAX, 1.0),
        brightness: clamp_finite(raw.brightness, BRIGHTNESS_MIN, BRIGHTNESS_MAX, 0.0),
        contrast: clamp_finite(raw.contrast, CONTRAST_MIN, CONTRAST_MAX, 1.0),
        saturation: clamp_finite(raw.saturation, CONTRAST_MIN, CONTRAST_MAX, 1.0),
        noise: raw.noise.min(NOISE_MAX),
        speed: clamp_finite(raw.speed, SPEED_MIN, SPEED_MAX, 1.0),
        volume: clamp_finite(raw.volume, VOLUME_MIN, VOLUME_MAX, 1.0),
    }
}

/// 是否为"什么都不做"。
pub fn is_noop(raw: &TransformOptions) -> bool {
    let o = normalize(raw);
    !o.mirror
        && o.crop_ratio <= EPS
        && (o.scale_ratio - 1.0).abs() <= EPS
        && o.brightness.abs() <= EPS
        && (o.contrast - 1.0).abs() <= EPS
        && (o.saturation - 1.0).abs() <= EPS
        && o.noise == 0
        && (o.speed - 1.0).abs() <= EPS
        && (o.volume - 1.0).abs() <= EPS
}

// ---------------- 滤镜构造（纯函数） ----------------

/// 偶数对齐：libx264 不接受奇数宽高。
fn even(value: f64) -> u32 {
    let v = value.max(2.0);
    let v = v.round() as u32;
    if v % 2 == 0 {
        v
    } else {
        v - 1
    }
}

/// 视频滤镜串。顺序固定：裁切 → 缩放 → 镜像 → 调色 → 噪点 → 变速。
///
/// `target` 是源画面的宽高。**已知尺寸时一律用精确像素值缩放**：
/// 如果走 `scale=trunc(iw/keep/2)*2` 这种相对表达式，裁切与放大两次取整
/// 会让画面每次处理缩水 1~2 像素，反复处理会越缩越小。
pub fn build_video_filter(raw: &TransformOptions, target: Option<(u32, u32)>) -> String {
    let o = normalize(raw);
    let mut parts: Vec<String> = Vec::new();

    if o.crop_ratio > EPS {
        let keep = 1.0 - 2.0 * o.crop_ratio;
        match target {
            Some((w, h)) => {
                let cw = even(w as f64 * keep);
                let ch = even(h as f64 * keep);
                // 横向居中、纵向**贴底**。
                //
                // 不能居中裁：字幕通常压在画面底部，居中裁会把底部那一条切掉，
                // 字幕就缺一半。真实素材上撞到过（那部片子带硬字幕）。
                // 贴底裁等于只从上方和两侧收，底部原样保留。
                parts.push(format!("crop={cw}:{ch}:(iw-{cw})/2:ih-{ch}"));
                parts.push(format!("scale={}:{}", even(w as f64), even(h as f64)));
            }
            None => {
                // 兜底：不知道源尺寸时只能用相对表达式。
                // 同样贴底：y 取 ih - 裁后高度。
                parts.push(format!(
                    "crop=trunc(iw*{keep:.4}/2)*2:trunc(ih*{keep:.4}/2)*2:(iw-trunc(iw*{keep:.4}/2)*2)/2:ih-trunc(ih*{keep:.4}/2)*2"
                ));
                parts.push(format!(
                    "scale=trunc(iw/{keep:.4}/2)*2:trunc(ih/{keep:.4}/2)*2"
                ));
            }
        }
    }

    if (o.scale_ratio - 1.0).abs() > EPS {
        let s = o.scale_ratio;
        match target {
            Some((w, h)) => parts.push(format!(
                "scale={}:{}",
                even(w as f64 * s),
                even(h as f64 * s)
            )),
            None => parts.push(format!("scale=trunc(iw*{s:.4}/2)*2:trunc(ih*{s:.4}/2)*2")),
        }
    }

    if o.mirror {
        parts.push("hflip".into());
    }

    if o.brightness.abs() > EPS
        || (o.contrast - 1.0).abs() > EPS
        || (o.saturation - 1.0).abs() > EPS
    {
        parts.push(format!(
            "eq=brightness={:.3}:contrast={:.3}:saturation={:.3}",
            o.brightness, o.contrast, o.saturation
        ));
    }

    if o.noise > 0 {
        parts.push(format!("noise=alls={}:allf=t+u", o.noise));
    }

    if (o.speed - 1.0).abs() > EPS {
        parts.push(format!("setpts=PTS/{:.4}", o.speed));
    }

    parts.join(",")
}

/// 音频滤镜串。变速必须与视频一致，否则音画不同步。
pub fn build_audio_filter(raw: &TransformOptions) -> String {
    let o = normalize(raw);
    let mut parts: Vec<String> = Vec::new();

    if (o.volume - 1.0).abs() > EPS {
        parts.push(format!("volume={:.3}", o.volume));
    }
    if (o.speed - 1.0).abs() > EPS {
        parts.push(format!("atempo={:.4}", o.speed));
    }

    parts.join(",")
}

// ---------------- 档位预设 ----------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intensity {
    Light,
    Medium,
    Strong,
}

impl Intensity {
    pub fn parse(raw: &str) -> Intensity {
        match raw.trim().to_lowercase().as_str() {
            "light" => Intensity::Light,
            "strong" => Intensity::Strong,
            _ => Intensity::Medium,
        }
    }
}

struct Ranges {
    crop: (f64, f64),
    scale: (f64, f64),
    brightness: f64,
    contrast: f64,
    saturation: f64,
    noise: (u32, u32),
    speed: (f64, f64),
    volume: (f64, f64),
}

fn ranges(level: Intensity) -> Ranges {
    match level {
        Intensity::Light => Ranges {
            crop: (0.0, 0.010),
            scale: (0.995, 1.005),
            brightness: 0.010,
            contrast: 0.005,
            saturation: 0.005,
            noise: (0, 2),
            speed: (0.99, 1.01),
            volume: (0.99, 1.01),
        },
        Intensity::Medium => Ranges {
            crop: (0.010, 0.030),
            scale: (0.990, 1.010),
            brightness: 0.020,
            contrast: 0.010,
            saturation: 0.010,
            noise: (1, 4),
            speed: (0.97, 1.03),
            volume: (0.98, 1.02),
        },
        Intensity::Strong => Ranges {
            crop: (0.020, 0.060),
            scale: (0.970, 1.030),
            brightness: 0.040,
            contrast: 0.030,
            saturation: 0.030,
            noise: (3, 8),
            speed: (0.95, 1.05),
            volume: (0.95, 1.05),
        },
    }
}

/// 确定性伪随机：同一个 (档位, 序号) 永远得到同一组参数。
///
/// 用序号当种子而不是用随机数，是为了**可复现** —— 同一批素材重复处理结果一致，
/// 出问题能照着日志复算。
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // 先打散一次，避免相邻序号产生高度相关的序列
        let mixed = seed
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(0xBF58_476D_1CE4_E5B9);
        Rng(mixed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next_f64()
    }

    fn range_u32(&mut self, lo: u32, hi: u32) -> u32 {
        if hi <= lo {
            return lo;
        }
        lo + (self.next_u64() % (hi - lo + 1) as u64) as u32
    }
}

/// 按档位与序号生成一组处理参数。同一个序号结果稳定。
pub fn preset_for(level: Intensity, index: usize) -> TransformOptions {
    let r = ranges(level);
    let mut rng = Rng::new(index as u64 + 1);

    let signed = |rng: &mut Rng, amplitude: f64| -> f64 {
        if amplitude <= 0.0 {
            0.0
        } else {
            rng.range(-amplitude, amplitude)
        }
    };

    TransformOptions {
        // **默认不镜像。**
        //
        // 原来是掷硬币（50% 概率），结果真实素材上炸了：那部片子带硬字幕，
        // 镜像之后画面里的字全反了，一半的切片直接废掉。
        //
        // 去重手段里，凡是会破坏画面内文字的都不该默认开。变速、调色、加噪点
        // 都安全；裁切改成贴底裁之后也不会切到字幕；只有镜像是不可挽回的 ——
        // 所以它必须由用户显式要求。
        mirror: false,
        crop_ratio: rng.range(r.crop.0, r.crop.1),
        scale_ratio: rng.range(r.scale.0, r.scale.1),
        brightness: signed(&mut rng, r.brightness),
        contrast: 1.0 + signed(&mut rng, r.contrast),
        saturation: 1.0 + signed(&mut rng, r.saturation),
        noise: rng.range_u32(r.noise.0, r.noise.1),
        speed: rng.range(r.speed.0, r.speed.1),
        volume: rng.range(r.volume.0, r.volume.1),
    }
}

// ---------------- 执行 ----------------

fn temp_path(out: &Path) -> AppResult<PathBuf> {
    let stem = out
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "clip".into());
    let ext = out
        .extension()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "mp4".into());
    let dir = out.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    Ok(dir.join(format!("{stem}.part.{ext}")))
}

/// 处理前的文件名。重复处理时用同一个名字，不会越叠越长。
pub fn processed_path(original: &Path) -> PathBuf {
    let stem = original
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "clip".into());
    let stem = stem.strip_suffix("_t").unwrap_or(&stem).to_string();
    let ext = original
        .extension()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "mp4".into());
    let dir = original.parent().unwrap_or_else(|| Path::new("."));
    dir.join(format!("{stem}_t.{ext}"))
}

/// 对单个文件应用处理。返回产出文件大小。
///
/// `target` 为源画面宽高（通常来自素材探测结果），传了就做精确缩放。
pub fn apply(
    ffmpeg: &Path,
    input: &Path,
    out: &Path,
    opts: &TransformOptions,
    target: Option<(u32, u32)>,
) -> AppResult<i64> {
    // 先判空操作：用户什么都没选时，直接告诉他，而不是去报文件找不到
    if is_noop(opts) {
        return Err(AppError::Msg(
            "没有选择任何处理项，至少勾选或调整一项".into(),
        ));
    }
    if !input.is_file() {
        return Err(AppError::Msg(format!(
            "片段文件不存在：{}",
            input.to_string_lossy()
        )));
    }

    let video = build_video_filter(opts, target);
    let audio = build_audio_filter(opts);
    let temp = temp_path(out)?;

    let mut args: Vec<String> = vec![
        "-hide_banner".into(),
        "-nostdin".into(),
        "-y".into(),
        "-i".into(),
        input.to_string_lossy().to_string(),
    ];
    if !video.is_empty() {
        args.push("-vf".into());
        args.push(video);
    }
    if !audio.is_empty() {
        args.push("-af".into());
        args.push(audio);
    }
    for a in [
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "20",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-b:a",
        "192k",
        "-movflags",
        "+faststart",
    ] {
        args.push(a.into());
    }
    args.push(temp.to_string_lossy().to_string());

    let output = no_window(Command::new(ffmpeg).args(&args))
        .output()
        .map_err(|e| AppError::Process(format!("无法启动 ffmpeg：{e}")))?;

    if !output.status.success() {
        let _ = std::fs::remove_file(&temp);
        return Err(AppError::Process(format!(
            "处理失败（退出码 {:?}）：{}",
            output.status.code(),
            tail(&String::from_utf8_lossy(&output.stderr))
        )));
    }
    if !temp.is_file() {
        let _ = std::fs::remove_file(&temp);
        return Err(AppError::Process("ffmpeg 报告成功但没有产出文件".into()));
    }

    if out.exists() {
        std::fs::remove_file(out)?;
    }
    std::fs::rename(&temp, out)?;
    Ok(std::fs::metadata(out).map(|m| m.len() as i64).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> TransformOptions {
        TransformOptions::default()
    }

    #[test]
    fn defaults_are_a_noop() {
        assert!(is_noop(&opts()));
        assert_eq!(build_video_filter(&opts(), None), "");
        assert_eq!(build_audio_filter(&opts()), "");
    }

    /// 自动生成的参数**永远不能镜像**。
    ///
    /// 真实素材上炸过：那部片子带硬字幕，镜像之后画面里的字全反了，
    /// 一半的切片直接废掉。镜像是不可挽回的，只能由用户显式要求。
    #[test]
    fn generated_presets_never_mirror() {
        for level in [Intensity::Light, Intensity::Medium, Intensity::Strong] {
            for index in 0..200 {
                let o = preset_for(level, index);
                assert!(
                    !o.mirror,
                    "{level:?} 档第 {index} 组参数镜像了 —— 会自动毁掉画面里的字幕"
                );
            }
        }
    }

    /// 裁切必须贴底，不能切掉底部字幕。
    #[test]
    fn crop_keeps_the_bottom_edge() {
        let mut o = opts();
        o.crop_ratio = 0.05;
        let f = build_video_filter(&o, Some((1080, 1920)));
        // 贴底裁：y 用 ih - 裁后高度
        assert!(
            f.contains("ih-"),
            "裁切应当贴底（保留底部字幕），实际滤镜：{f}"
        );
        // 横向仍居中
        assert!(f.contains("(iw-"), "横向应当居中，实际滤镜：{f}");
        // 1080×1920 的源裁 5%：宽 972、高 1728
        assert!(f.contains("crop=972:1728"), "实际滤镜：{f}");
    }

    #[test]
    fn crop_without_target_size_is_also_bottom_anchored() {
        let mut o = opts();
        o.crop_ratio = 0.04;
        let f = build_video_filter(&o, None);
        assert!(
            f.contains("ih-trunc"),
            "不知道源尺寸时也要贴底，实际滤镜：{f}"
        );
    }

    #[test]
    fn normalize_clamps_every_field() {
        let wild = TransformOptions {
            mirror: true,
            crop_ratio: 99.0,
            scale_ratio: 0.01,
            brightness: -50.0,
            contrast: 1e9,
            saturation: f64::NAN,
            noise: 999,
            speed: 0.0,
            volume: 100.0,
        };
        let n = normalize(&wild);
        assert_eq!(n.crop_ratio, CROP_MAX);
        assert_eq!(n.scale_ratio, SCALE_MIN);
        assert_eq!(n.brightness, BRIGHTNESS_MIN);
        assert_eq!(n.contrast, CONTRAST_MAX);
        assert_eq!(n.saturation, 1.0, "NaN 应退回默认值");
        assert_eq!(n.noise, NOISE_MAX);
        assert_eq!(n.speed, SPEED_MIN);
        assert_eq!(n.volume, VOLUME_MAX);
        assert!(n.mirror);
    }

    #[test]
    fn nan_and_infinity_fall_back_to_defaults() {
        let mut o = opts();
        o.crop_ratio = f64::INFINITY;
        o.scale_ratio = f64::NEG_INFINITY;
        o.brightness = f64::NAN;
        o.speed = f64::NAN;
        o.volume = f64::NAN;
        let n = normalize(&o);
        assert_eq!(n.crop_ratio, 0.0);
        assert_eq!(n.scale_ratio, 1.0);
        assert_eq!(n.brightness, 0.0);
        assert_eq!(n.speed, 1.0);
        assert_eq!(n.volume, 1.0);
        assert!(is_noop(&n));
    }

    #[test]
    fn individual_switches_produce_expected_filters() {
        let mut o = opts();
        o.mirror = true;
        assert_eq!(build_video_filter(&o, None), "hflip");

        let mut o = opts();
        o.noise = 5;
        assert_eq!(build_video_filter(&o, None), "noise=alls=5:allf=t+u");

        let mut o = opts();
        o.speed = 1.05;
        assert_eq!(build_video_filter(&o, None), "setpts=PTS/1.0500");
        assert_eq!(build_audio_filter(&o), "atempo=1.0500");

        let mut o = opts();
        o.volume = 0.9;
        assert_eq!(build_audio_filter(&o), "volume=0.900");
        assert_eq!(build_video_filter(&o, None), "", "只改音量不该动画面");
    }

    #[test]
    fn crop_is_followed_by_scale_back_to_original_size() {
        let mut o = opts();
        o.crop_ratio = 0.05;
        let f = build_video_filter(&o, None);
        assert!(f.starts_with("crop=trunc(iw*0.9000/2)*2:trunc(ih*0.9000/2)*2"), "{f}");
        assert!(f.contains("scale=trunc(iw/0.9000/2)*2:trunc(ih/0.9000/2)*2"), "{f}");
        // 偶数对齐，避免 libx264 因奇数尺寸报错
        assert!(f.contains("/2)*2"), "所有 scale/crop 都应做偶数对齐：{f}");
    }

    #[test]
    fn color_adjustment_becomes_one_eq_filter() {
        let mut o = opts();
        o.brightness = 0.02;
        o.contrast = 1.01;
        o.saturation = 0.99;
        let f = build_video_filter(&o, None);
        assert_eq!(f, "eq=brightness=0.020:contrast=1.010:saturation=0.990");
    }

    #[test]
    fn filters_are_emitted_in_a_fixed_order() {
        let mut o = opts();
        o.crop_ratio = 0.02;
        o.scale_ratio = 1.02;
        o.mirror = true;
        o.brightness = 0.01;
        o.noise = 3;
        o.speed = 1.02;
        let f = build_video_filter(&o, None);
        let order = ["crop=", "scale=", "hflip", "eq=", "noise=", "setpts="];
        let mut last = 0usize;
        for token in order {
            let at = f.find(token).unwrap_or_else(|| panic!("缺少 {token}：{f}"));
            assert!(at >= last, "{token} 顺序不对：{f}");
            last = at;
        }
    }

    #[test]
    fn tiny_deviations_are_treated_as_no_change() {
        let mut o = opts();
        o.scale_ratio = 1.0 + 1e-9;
        o.brightness = 1e-9;
        assert_eq!(build_video_filter(&o, None), "");
        assert!(is_noop(&o));
    }

    #[test]
    fn preset_is_deterministic_and_within_range() {
        for index in 0..30 {
            let a = preset_for(Intensity::Medium, index);
            let b = preset_for(Intensity::Medium, index);
            assert_eq!(a.crop_ratio, b.crop_ratio, "同一序号必须完全一致");
            assert_eq!(a.speed, b.speed);
            assert_eq!(a.noise, b.noise);

            let n = normalize(&a);
            let r = ranges(Intensity::Medium);
            assert!(n.crop_ratio >= r.crop.0 - 1e-9 && n.crop_ratio <= r.crop.1 + 1e-9);
            assert!(n.speed >= r.speed.0 - 1e-9 && n.speed <= r.speed.1 + 1e-9);
            assert!(n.noise >= r.noise.0 && n.noise <= r.noise.1);
        }
    }

    #[test]
    fn consecutive_presets_differ_from_each_other() {
        let mut distinct = std::collections::HashSet::new();
        for index in 0..20 {
            let o = preset_for(Intensity::Medium, index);
            distinct.insert(format!("{:.6}|{:.6}|{}", o.crop_ratio, o.speed, o.noise));
        }
        assert!(distinct.len() >= 15, "相邻序号应产生不同参数，实际 {} 种", distinct.len());
    }

    #[test]
    fn stronger_levels_stay_inside_their_own_ranges() {
        let light = ranges(Intensity::Light);
        let strong = ranges(Intensity::Strong);
        assert!(light.crop.1 < strong.crop.0, "档位区间应递增");
        assert!(light.noise.1 < strong.noise.0);
        // 任何档位生成的参数都必须在全局安全范围内
        for level in [Intensity::Light, Intensity::Medium, Intensity::Strong] {
            for index in 0..10 {
                let n = normalize(&preset_for(level, index));
                assert!(n.crop_ratio <= CROP_MAX);
                assert!(n.speed >= SPEED_MIN && n.speed <= SPEED_MAX);
                assert!(n.volume >= VOLUME_MIN && n.volume <= VOLUME_MAX);
            }
        }
    }

    #[test]
    fn intensity_parses_with_medium_as_fallback() {
        assert_eq!(Intensity::parse("light"), Intensity::Light);
        assert_eq!(Intensity::parse(" STRONG "), Intensity::Strong);
        assert_eq!(Intensity::parse("nonsense"), Intensity::Medium);
    }

    #[test]
    fn processed_path_does_not_stack_suffixes() {
        let p = PathBuf::from("E:/out/001.mp4");
        // 用 Path 比较：Windows 上 / 与 \ 视为等价
        assert_eq!(processed_path(&p), PathBuf::from("E:/out/001_t.mp4"));
        // 二次处理仍指向同一个文件，不会变成 001_t_t.mp4
        assert_eq!(
            processed_path(&processed_path(&p)),
            PathBuf::from("E:/out/001_t.mp4")
        );
    }

    #[test]
    fn crop_uses_exact_dimensions_when_source_size_is_known() {
        let mut o = opts();
        o.crop_ratio = 0.05; // keep = 0.9
        // 横向居中、纵向贴底：字幕在底部，居中裁会切到它
        assert_eq!(
            build_video_filter(&o, Some((1080, 1920))),
            "crop=972:1728:(iw-972)/2:ih-1728,scale=1080:1920"
        );
    }

    #[test]
    fn explicit_scaling_rounds_to_even_numbers() {
        let mut o = opts();
        o.scale_ratio = 1.03;
        // 1081×1.03 = 1113.43 → 1113 → 偶数 1112；1921×1.03 = 1978.63 → 1979 → 偶数 1978
        assert_eq!(
            build_video_filter(&o, Some((1081, 1921))),
            "scale=1112:1978"
        );
    }

    #[test]
    fn noop_is_rejected_before_spawning_ffmpeg() {
        let err = apply(
            Path::new("Z:/nope/ffmpeg.exe"),
            Path::new("Z:/nope/in.mp4"),
            Path::new("Z:/nope/out.mp4"),
            &opts(),
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("没有选择任何处理项"), "实际：{err}");
    }

    #[test]
    fn missing_input_is_reported() {
        let mut o = opts();
        o.mirror = true;
        let err = apply(
            Path::new("Z:/nope/ffmpeg.exe"),
            Path::new("Z:/nope/missing.mp4"),
            Path::new("Z:/nope/out.mp4"),
            &o,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("片段文件不存在"), "实际：{err}");
    }

    // ---------------- 端到端（需要真实 ffmpeg，默认跳过） ----------------

    #[test]
    fn end_to_end_transform_with_real_ffmpeg() {
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

        let dir = std::env::temp_dir().join("dramadist_transform_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 造一段 4 秒素材
        let input = dir.join("in.mp4");
        let status = no_window(
            Command::new(&ffmpeg).args([
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
                "4",
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
        .status()
        .expect("无法启动 ffmpeg");
        assert!(status.success(), "生成测试素材失败");

        // 处理：镜像 + 裁切 + 调色 + 噪点 + 加速 1.1 倍（不缩放，便于断言尺寸不变）
        let o = TransformOptions {
            mirror: true,
            crop_ratio: 0.03,
            scale_ratio: 1.0,
            brightness: 0.02,
            contrast: 1.01,
            saturation: 0.99,
            noise: 3,
            speed: 1.1,
            volume: 0.95,
        };
        let out = processed_path(&input);
        // 已知源尺寸 → 精确缩放，裁切后必须严格回到 320x240
        let size = apply(&ffmpeg, &input, &out, &o, Some((320, 240))).expect("画面处理失败");
        assert!(out.is_file());
        assert!(size > 0);

        // 不应残留临时文件
        let leftovers = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".part."))
            .count();
        assert_eq!(leftovers, 0);

        // 有 ffprobe 就顺便验证尺寸不变、时长按速度缩短
        if let Some(ffprobe) = ffprobe {
            let before = crate::media::probe(&ffprobe, &input).unwrap();
            let after = crate::media::probe(&ffprobe, &out).unwrap();
            assert_eq!(
                (after.width, after.height),
                (before.width, before.height),
                "裁切后应放大回原尺寸"
            );
            let expected = before.duration_ms as f64 / 1.1;
            let delta = (after.duration_ms as f64 - expected).abs();
            assert!(
                delta < 500.0,
                "时长应约为 {expected:.0}ms，实际 {}ms",
                after.duration_ms
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
