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
use crate::models::{HeaderLabel, LabelPosition, TransformOptions};
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
        // 标识在这里不夹数值：文字要按绘图规则清洗、字号要按画面高度算，
        // 那两件事都在 build_label_filter 里做（这里拿不到画面尺寸）
        header_label: raw.header_label.clone(),
    }
}

/// 是否为"什么都不做"。
pub fn is_noop(raw: &TransformOptions) -> bool {
    let o = normalize(raw);
    // 有标识文字就算"要做点什么" —— 哪怕其它参数全是中性值
    let no_label = o
        .header_label
        .as_ref()
        .map(|l| sanitize_label_text(&l.text).is_empty())
        .unwrap_or(true);
    !o.mirror
        && o.crop_ratio <= EPS
        && (o.scale_ratio - 1.0).abs() <= EPS
        && o.brightness.abs() <= EPS
        && (o.contrast - 1.0).abs() <= EPS
        && (o.saturation - 1.0).abs() <= EPS
        && o.noise == 0
        && (o.speed - 1.0).abs() <= EPS
        && (o.volume - 1.0).abs() <= EPS
        && no_label
}

// ---------------- 片头标识（合规） ----------------

/// 常见中文字体，按优先级排。
///
/// `drawtext` 必须拿到**具体字体文件**，系统默认字体不一定有中文字形
/// （拿不到就会画出一串方框）。这几款在中文 Windows 上必然存在。
const CJK_FONTS: [&str; 6] = [
    r"C:\Windows\Fonts\msyh.ttc",   // 微软雅黑（系统默认界面字体）
    r"C:\Windows\Fonts\msyhbd.ttc", // 微软雅黑 Bold
    r"C:\Windows\Fonts\simhei.ttf", // 黑体
    r"C:\Windows\Fonts\Deng.ttf",   // 等线
    r"C:\Windows\Fonts\simsun.ttc", // 宋体
    r"C:\Windows\Fonts\simkai.ttf", // 楷体
];

/// 找一个能画中文的字体。找不到就返回 `None`。
pub fn find_cjk_font() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("DRAMADIST_LABEL_FONT") {
        let p = PathBuf::from(custom);
        if p.is_file() {
            return Some(p);
        }
    }
    CJK_FONTS
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
}

/// 把路径放进滤镜选项里。
///
/// ffmpeg 要过**两层**解析：滤镜描述层先吃掉一次转义，选项层再吃一次。
/// 所以 Windows 路径里的 `C:` 必须写成 `C\\:`（两个反斜杠）——
/// 少一个就会变成「选项名附近没有名字」那种报错。这是实测出来的，不是猜的。
fn escape_filter_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").replace(':', r"\\:")
}

/// 把文字放进 `text=` 里。
///
/// 同样要过两层。这里只用**安全字符**：中文、字母、数字、空格和常见标点。
/// 其余（引号、反斜杠、冒号、百分号、方括号、逗号）一律换成空格 ——
/// 与其猜转义规则猜错，不如不让它们进去。标识本来就该是短标签。
fn sanitize_label_text(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c.is_whitespace() || "·-—".contains(c) {
                c
            } else {
                ' '
            }
        })
        .collect();
    let squeezed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    squeezed.chars().take(16).collect()
}

/// 标识字号 = 画面高度 × 比例，夹到一个合理区间。
fn label_font_size(height: i64, ratio: f64) -> i64 {
    let r = if ratio.is_finite() {
        ratio.clamp(0.015, 0.09)
    } else {
        0.034
    };
    ((height as f64 * r).round() as i64).clamp(18, 200)
}

/// 构造片头标识的 drawtext 段。返回 `None` 表示不加（没字、或找不到字体）。
///
/// `size` 是画面尺寸 —— 字号和边距都要按它算，否则 480p 上会糊成一片、
/// 4K 上会小得看不见。
pub fn build_label_filter(label: &HeaderLabel, size: (i64, i64)) -> AppResult<String> {
    let text = sanitize_label_text(&label.text);
    if text.is_empty() {
        return Ok(String::new());
    }
    let font = find_cjk_font().ok_or_else(|| {
        AppError::Msg(format!(
            "要加片头标识但找不到中文字体。找过这些：{}。\
             可以用环境变量 DRAMADIST_LABEL_FONT 指定一个 .ttf/.ttc。",
            CJK_FONTS.join("、")
        ))
    })?;

    let seconds = if label.seconds.is_finite() {
        label.seconds.clamp(0.5, 15.0)
    } else {
        3.0
    };
    let size_px = label_font_size(size.1, label.size_ratio);
    // 边距跟着字号走，不同分辨率下观感一致
    let margin = (size_px as f64 * 0.9).round() as i64;
    let (x, y) = match label.position {
        LabelPosition::TopLeft => (format!("{margin}"), format!("{margin}")),
        LabelPosition::TopRight => (format!("w-tw-{margin}"), format!("{margin}")),
        LabelPosition::BottomLeft => (format!("{margin}"), format!("h-th-{margin}")),
        LabelPosition::BottomRight => (format!("w-tw-{margin}"), format!("h-th-{margin}")),
        LabelPosition::TopCenter => ("(w-tw)/2".to_string(), format!("{margin}")),
        LabelPosition::BottomCenter => ("(w-tw)/2".to_string(), format!("h-th-{margin}")),
    };

    // 加个半透明底衬：白字压在浅色画面上会看不清，
    // 而这是个披露义务，看不清等于没标。
    Ok(format!(
        "drawtext=fontfile={}:text={}:fontcolor=white:fontsize={}:box=1:boxcolor=black@0.55:boxborderw={}:x={}:y={}:enable=lt(t\\,{:.2})",
        escape_filter_path(&font),
        text,
        size_px,
        (size_px / 5).max(6),
        x,
        y,
        seconds
    ))
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
pub fn build_video_filter(raw: &TransformOptions, target: Option<(u32, u32)>) -> AppResult<String> {
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

    // 片头标识放在**最后**：它要压在最终画面上。
    // 放前面的话，后面的裁切/缩放会把标识一起变形。
    if let Some(label) = &o.header_label {
        let size = target.map(|(w, h)| (w as i64, h as i64)).unwrap_or((1080, 1920));
        let seg = build_label_filter(label, size)?;
        if !seg.is_empty() {
            parts.push(seg);
        }
    }

    Ok(parts.join(","))
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
        // 片头标识是**合规开关**，由用户显式要求；随机预设不擅自加，
        // 也不能擅自加 —— 内容是 AI 生成的才需要标，标错了是另一种不实。
        header_label: None,
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

    let video = build_video_filter(opts, target)?;
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
    /// 测试里绝大多数断言只关心滤镜串本身，这里把 Result 拆掉。
    fn vf(o: &TransformOptions, target: Option<(u32, u32)>) -> String {
        build_video_filter(o, target).expect("滤镜构造失败")
    }

    use super::*;

    fn opts() -> TransformOptions {
        TransformOptions::default()
    }

    #[test]
    fn defaults_are_a_noop() {
        assert!(is_noop(&opts()));
        assert_eq!(vf(&opts(), None), "");
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
        let f = vf(&o, Some((1080, 1920)));
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
        let f = vf(&o, None);
        assert!(
            f.contains("ih-trunc"),
            "不知道源尺寸时也要贴底，实际滤镜：{f}"
        );
    }

    // ---------------- 片头标识（合规） ----------------

    fn label() -> HeaderLabel {
        HeaderLabel::default()
    }

    #[test]
    fn label_filter_draws_text_only_for_the_first_seconds() {
        if find_cjk_font().is_none() {
            eprintln!("跳过：本机没有中文字体");
            return;
        }
        let mut o = opts();
        o.header_label = Some(label());
        let f = vf(&o, Some((1080, 1920)));

        assert!(f.contains("drawtext="), "实际滤镜：{f}");
        assert!(f.contains("AI创作"), "实际滤镜：{f}");
        // 只在前 3 秒显示 —— 逗号必须转义，否则会被当成滤镜分隔符
        assert!(
            f.contains(r"enable=lt(t\,3.00)"),
            "时间范围没写对，实际滤镜：{f}"
        );
        // 白字要有底衬，否则压在浅色画面上看不清
        assert!(f.contains("box=1"), "实际滤镜：{f}");
        // 字号按画面高度算：1920 × 0.034 ≈ 65
        assert!(f.contains("fontsize=65"), "实际滤镜：{f}");
    }

    #[test]
    fn label_font_size_follows_the_frame_height() {
        assert_eq!(label_font_size(1920, 0.034), 65);
        assert_eq!(label_font_size(480, 0.034), 18); // 夹到最小值，太小看不见
        // 比例被夹住，防止有人传 5.0 让字号盖满全屏
        assert!(label_font_size(1920, 5.0) <= 200);
        // NaN 退回默认而不是崩
        assert_eq!(label_font_size(1920, f64::NAN), 65);
    }

    #[test]
    fn label_text_is_sanitized_not_escaped_by_guesswork() {
        // 这些字符在滤镜里都有特殊含义。与其猜转义规则猜错，
        // 不如不让它们进去 —— 标识本来就该是短标签。
        assert_eq!(sanitize_label_text("AI创作"), "AI创作");
        assert_eq!(sanitize_label_text("AI:创作"), "AI 创作");
        assert_eq!(sanitize_label_text("a'b\\c%d[e]f,g;h"), "a b c d e f g h");
        // 换行/多余空格压成一个
        assert_eq!(sanitize_label_text("  AI   创作  "), "AI 创作");
        // 太长截断
        assert_eq!(sanitize_label_text(&"字".repeat(40)).chars().count(), 16);
    }

    #[test]
    fn empty_label_text_means_no_label_at_all() {
        let mut o = opts();
        o.header_label = Some(HeaderLabel {
            text: "   ".to_string(),
            ..label()
        });
        assert!(is_noop(&o), "标识文字是空的就该当成不加标识");
        assert_eq!(vf(&o, Some((1080, 1920))), "");
    }

    #[test]
    fn label_alone_still_makes_the_filter_non_empty() {
        let mut o = opts();
        o.header_label = Some(label());
        assert!(!is_noop(&o), "只加标识也是要处理的");
        assert!(!vf(&o, Some((1080, 1920))).is_empty());
    }

    #[test]
    fn label_goes_last_so_it_is_not_cropped_away() {
        if find_cjk_font().is_none() {
            eprintln!("跳过：本机没有中文字体");
            return;
        }
        let mut o = opts();
        o.crop_ratio = 0.05;
        o.header_label = Some(label());
        let f = vf(&o, Some((1080, 1920)));
        let crop_at = f.find("crop=").expect("应当有裁切");
        let label_at = f.find("drawtext=").expect("应当有标识");
        assert!(
            label_at > crop_at,
            "标识必须排在裁切之后，否则会被一起裁掉：{f}"
        );
    }

    #[test]
    fn label_positions_map_to_different_corners() {
        if find_cjk_font().is_none() {
            eprintln!("跳过：本机没有中文字体");
            return;
        }
        let mut seen = std::collections::HashSet::new();
        for pos in [
            LabelPosition::TopLeft,
            LabelPosition::TopRight,
            LabelPosition::BottomLeft,
            LabelPosition::BottomRight,
            LabelPosition::TopCenter,
            LabelPosition::BottomCenter,
        ] {
            let f = build_label_filter(
                &HeaderLabel {
                    position: pos,
                    ..label()
                },
                (1080, 1920),
            )
            .unwrap();
            // 取出 x= / y= 部分作为这个位置的指纹
            let key = f
                .split(':')
                .filter(|s| s.starts_with("x=") || s.starts_with("y="))
                .collect::<Vec<_>>()
                .join("|");
            assert!(seen.insert(key.clone()), "位置 {pos:?} 和别的重合了：{key}");
        }
    }

    #[test]
    fn label_seconds_are_clamped() {
        if find_cjk_font().is_none() {
            eprintln!("跳过：本机没有中文字体");
            return;
        }
        let f = build_label_filter(
            &HeaderLabel {
                seconds: 999.0,
                ..label()
            },
            (1080, 1920),
        )
        .unwrap();
        assert!(f.contains(r"lt(t\,15.00)"), "上限应当是 15 秒：{f}");

        let f = build_label_filter(
            &HeaderLabel {
                seconds: -5.0,
                ..label()
            },
            (1080, 1920),
        )
        .unwrap();
        assert!(f.contains(r"lt(t\,0.50)"), "下限应当是 0.5 秒：{f}");
    }

    /// 在**真实素材**上加标识，产出留给人工/OCR 复核。
    ///
    /// 需要环境变量：
    /// - `DRAMADIST_TEST_FFMPEG`
    /// - `DRAMADIST_TEST_REAL_SLICE`：一个真实的切片文件
    /// - `DRAMADIST_KEEP_TEST_ARTIFACTS`：设了就把产物留在临时目录里
    ///
    /// 为什么单独做这一条：单测只能证明"滤镜串长得对"，
    /// 证明不了"字真的画上去了、中文字形没变成方框"。
    /// 这一路踩的坑几乎都属于后者。
    #[test]
    fn label_renders_on_real_material() {
        let (Some(ffmpeg), Ok(slice)) = (
            std::env::var("DRAMADIST_TEST_FFMPEG")
                .ok()
                .map(PathBuf::from)
                .filter(|p| p.is_file()),
            std::env::var("DRAMADIST_TEST_REAL_SLICE"),
        ) else {
            eprintln!("跳过：未设置 DRAMADIST_TEST_FFMPEG / DRAMADIST_TEST_REAL_SLICE");
            return;
        };
        let input = PathBuf::from(&slice);
        if !input.is_file() {
            eprintln!("跳过：找不到 {slice}");
            return;
        }

        let dir = std::env::temp_dir().join("dramadist_label_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("labeled.mp4");

        let o = TransformOptions {
            header_label: Some(HeaderLabel {
                seconds: 3.0,
                ..Default::default()
            }),
            ..Default::default()
        };
        apply(&ffmpeg, &input, &out, &o, Some((1080, 1920))).expect("加标识失败");
        assert!(out.is_file(), "没有产出文件");

        let size = std::fs::metadata(&out).unwrap().len();
        assert!(size > 1000, "产出文件太小：{size} 字节");

        // 抽第 1 秒那一帧，留给 OCR 复核（这里只保证抽出来了）
        let frame = dir.join("t1.png");
        let st = crate::proc::no_window(
            Command::new(&ffmpeg).args(["-hide_banner", "-loglevel", "error", "-y", "-ss", "1"]),
        )
        .arg("-i")
        .arg(&out)
        .args(["-frames:v", "1"])
        .arg(&frame)
        .status()
        .expect("抽帧失败");
        assert!(st.success() && frame.is_file(), "抽帧没出来");

        // 第 5 秒应当已经没有标识了
        let frame5 = dir.join("t5.png");
        let _ = crate::proc::no_window(
            Command::new(&ffmpeg).args(["-hide_banner", "-loglevel", "error", "-y", "-ss", "5"]),
        )
        .arg("-i")
        .arg(&out)
        .args(["-frames:v", "1"])
        .arg(&frame5)
        .status();

        if std::env::var("DRAMADIST_KEEP_TEST_ARTIFACTS").is_ok() {
            eprintln!("产物留在这里：{}", dir.display());
        } else {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn generated_presets_never_add_a_label() {
        for level in [Intensity::Light, Intensity::Medium, Intensity::Strong] {
            for index in 0..50 {
                assert!(
                    preset_for(level, index).header_label.is_none(),
                    "{level:?} 档第 {index} 组偷偷加了标识"
                );
            }
        }
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
            header_label: None,
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
        assert_eq!(vf(&o, None), "hflip");

        let mut o = opts();
        o.noise = 5;
        assert_eq!(vf(&o, None), "noise=alls=5:allf=t+u");

        let mut o = opts();
        o.speed = 1.05;
        assert_eq!(vf(&o, None), "setpts=PTS/1.0500");
        assert_eq!(build_audio_filter(&o), "atempo=1.0500");

        let mut o = opts();
        o.volume = 0.9;
        assert_eq!(build_audio_filter(&o), "volume=0.900");
        assert_eq!(vf(&o, None), "", "只改音量不该动画面");
    }

    #[test]
    fn crop_is_followed_by_scale_back_to_original_size() {
        let mut o = opts();
        o.crop_ratio = 0.05;
        let f = vf(&o, None);
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
        let f = vf(&o, None);
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
        let f = vf(&o, None);
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
        assert_eq!(vf(&o, None), "");
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
            vf(&o, Some((1080, 1920))),
            "crop=972:1728:(iw-972)/2:ih-1728,scale=1080:1920"
        );
    }

    #[test]
    fn explicit_scaling_rounds_to_even_numbers() {
        let mut o = opts();
        o.scale_ratio = 1.03;
        // 1081×1.03 = 1113.43 → 1113 → 偶数 1112；1921×1.03 = 1978.63 → 1979 → 偶数 1978
        assert_eq!(
            vf(&o, Some((1081, 1921))),
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
            header_label: None,
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
