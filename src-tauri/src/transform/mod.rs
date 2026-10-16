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
use serde::{Deserialize, Serialize};
use crate::models::{CornerLabel, HeaderLabel, LabelPosition, TransformOptions};
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

        // ---- 照 13 项补的那些：**必须原样带过来并夹进安全范围** ----
        //
        // 这里踩过一次坑：批量加字段时脚本把这一段写成了固定默认值
        // （`hue: 0.0` 之类），于是新加的 11 项参数**一个都没生效** ——
        // 用户勾了片头标识、选了 720p，跑完什么都没变。
        // 教训：normalize 是「过滤网」，新字段必须显式过网，漏一个就静默失效。
        hue: clamp_finite(raw.hue, HUE_MIN, HUE_MAX, 0.0),
        sharpen: clamp_finite(raw.sharpen, 0.0, SHARPEN_MAX, 0.0),
        tmix: raw.tmix.min(TMIX_MAX),
        max_height: clamp_i64(raw.max_height, 0, MAX_HEIGHT_MAX, 0),
        fps: clamp_finite(raw.fps, 0.0, FPS_MAX, 0.0),
        shift_x: clamp_i64(raw.shift_x, -SHIFT_MAX, SHIFT_MAX, 0),
        shift_y: clamp_i64(raw.shift_y, -SHIFT_MAX, SHIFT_MAX, 0),
        corner_label: raw.corner_label.clone(),
        gop: clamp_i64(raw.gop, 0, GOP_MAX, 0),
        bframes: clamp_i64(raw.bframes, -1, BF_MAX, -1),
        bitrate_bps: clamp_i64(raw.bitrate_bps, 0, BITRATE_MAX, 0),
    }
}

/// 夹一个整数到范围内；越界就取 default。
fn clamp_i64(v: i64, lo: i64, hi: i64, default: i64) -> i64 {
    if (lo..=hi).contains(&v) {
        v
    } else {
        default
    }
}

/// 新增参数的取值范围。
///
/// 这些是照真实软件那 13 项补贴上去的，不在「轻/中/强」三个档里，
/// 但**必须有上限** —— 用户乱填一个 999 会直接把 ffmpeg 跑挂。
const HUE_MIN: f64 = -30.0;
const HUE_MAX: f64 = 30.0;
const SHARPEN_MAX: f64 = 1.5;
/// 帧混合最多混 4 帧。再多就不是去重而是糊成一片了
const TMIX_MAX: u32 = 4;
/// 输出高度上限（含）。0 = 不缩放
const MAX_HEIGHT_MAX: i64 = 2160;
const FPS_MAX: f64 = 120.0;
/// 几何位移上限（像素）
const SHIFT_MAX: i64 = 40;
const GOP_MAX: i64 = 600;
const BF_MAX: i64 = 8;
/// 码率上限 50Mbps
const BITRATE_MAX: i64 = 50_000_000;
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
///
/// **顺序就是优先级。**
const CJK_FONTS: [&str; 6] = [
    // **雅黑 Bold 排第一。**
    //
    // 常规体在 720p 的角标尺寸下笔画偏细、看着「发虚」
    // （用户原话「字体也感觉有点点难看」）。六款字体画在同一帧上对比过，
    // Bold 在同样字号下最干净。
    r"C:\Windows\Fonts\msyhbd.ttc", // 微软雅黑 Bold
    r"C:\Windows\Fonts\msyh.ttc",   // 微软雅黑（常规，笔画偏细）
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
pub(crate) fn escape_filter_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/").replace(':', r"\\:")
}

/// 把文字放进 `text=` 里。
///
/// 同样要过两层。这里只用**安全字符**：中文、字母、数字、空格和常见标点。
/// 其余（引号、反斜杠、冒号、百分号、方括号、逗号）一律换成空格 ——
/// 与其猜转义规则猜错，不如不让它们进去。标识本来就该是短标签。
pub(crate) fn sanitize_label_text(text: &str) -> String {
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
pub(crate) fn label_font_size(height: i64, ratio: f64) -> i64 {
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

    // **seconds = 0 表示全程显示。**
    //
    // 用户问「要一直在视频左上方吧」—— 片头 3 秒和全程显示是两种做法：
    // - 片头几秒：常见的「显著标识」，对观感影响最小
    // - 全程显示：最保险，但会一直压在画面上
    // 法规要的是「显著」，两种都算；选哪种是用户的事，不该由工具替他定。
    let always = label.seconds <= 0.0;
    let seconds = if always {
        0.0
    } else if label.seconds.is_finite() {
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
    // **用描边，不用黑底板。**
    //
    // 原来加了 `box=1:boxcolor=black@0.55`，是为了让白字在浅色画面上也看得清
    // （这是披露义务，看不清等于没标）。但用户看到的是"一块黑砖"，很丑。
    //
    // 描边能达到同样的可读性，而且不占画面：白字带一圈深色轮廓，
    // 压在亮处暗处都清楚。实测过：浅色背景上照样读得清。
    let mut f = format!(
        "drawtext=fontfile={}:text={}:fontcolor=white:fontsize={}:\
         borderw={}:bordercolor=black@0.85:x={}:y={}",
        escape_filter_path(&font),
        text,
        size_px,
        (size_px / 12).max(2),
        x,
        y
    );
    // 全程显示就**不加** enable 条件；否则限定在片头那几秒
    if !always {
        f.push_str(&format!(":enable=lt(t\\,{seconds:.2})"));
    }
    Ok(f)
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

    // **色相偏移。** 真实软件那 13 项里的一项（强度 122）。
    // 幅度很小（一两度），肉眼几乎看不出，但会让画面指纹变掉。
    if o.hue.abs() > EPS {
        parts.push(format!("hue=h={:.2}", o.hue));
    }

    // **锐化。** 同样是 13 项之一（强度 119）。
    if o.sharpen > EPS {
        parts.push(format!("unsharp=5:5:{:.2}:5:5:0", o.sharpen));
    }

    // **帧混合（tmix）。** 真实软件的主力手段（强度 124）。
    //
    // 把相邻若干帧叠在一起 —— 去重效果最强，**画质代价也最大**：
    // 运动画面会有明显拖影。所以放在调色之后、标识之前。
    if o.tmix >= 2 {
        parts.push(format!("tmix=frames={}", o.tmix));
    }

    if (o.speed - 1.0).abs() > EPS {
        parts.push(format!("setpts=PTS/{:.4}", o.speed));
    }

    // **几何位移。** 真实软件特意注明「原微旋转」—— 旋转会在四角留黑边，
    // 位移不会。做法是**把裁切框整体挪一点**，画面内容跟着偏，但不出黑边。
    //
    // 必须放在裁切之后：裁切是先定好尺寸的，位移是在那个尺寸里挪位置。
    if o.shift_x != 0 || o.shift_y != 0 {
        // 挪多少就把裁切框放大多少，保证挪完之后仍然铺满
        let dx = o.shift_x.unsigned_abs() as i64;
        let dy = o.shift_y.unsigned_abs() as i64;
        let x = if o.shift_x >= 0 { format!("{dx}") } else { format!("iw-{dx}") };
        let y = if o.shift_y >= 0 { format!("{dy}") } else { format!("ih-{dy}") };
        parts.push(format!("crop=iw-{dx}:ih-{dy}:{x}:{y}"));
        parts.push("scale=trunc(iw/2)*2:trunc(ih/2)*2".to_string());
    }

    // **统一帧率。** 30fps 本身也是一项去重（真实软件的「帧率项」强度 122）。
    // 放在最后一步之前 —— 前面的速度变化会改有效帧率，这里统一收口。
    if o.fps > 0.0 {
        parts.push(format!("fps={}", o.fps as i64));
    }

    // **统一出 720p。** 真实软件是 1080 进、720 出。
    // 放在最后：前面所有处理都在原尺寸上做，最后一步才降下来。
    if o.max_height > 0 {
        parts.push(format!("scale=-2:{}", o.max_height));
    }

    // 片头标识放在**最后**：它要压在最终画面上。
    // 放前面的话，后面的裁切/缩放会把标识一起变形。
    if let Some(label) = &o.header_label {
        // **按缩放之后的高度算字号。**
        //
        // 标识画在 `scale=-2:720` 之后，画面已经是 720 高了；
        // 还按源尺寸 1920 算字号的话，字会大三倍（实测 fontsize=58 画在 720 上）。
        let (w, h) = target.unwrap_or((1080, 1920));
        let size = (w as i64, final_height(&o, h as i64));
        let seg = build_label_filter(label, size)?;
        if !seg.is_empty() {
            parts.push(seg);
        }
    }

    // 剧名角标也是压在最终画面上，和片头标识同理
    if let Some(corner) = &o.corner_label {
        let (w, h) = target.unwrap_or((1080, 1920));
        let size = (w as i64, final_height(&o, h as i64));
        let seg = build_corner_filter(corner, size)?;
        if !seg.is_empty() {
            parts.push(seg);
        }
    }

    Ok(parts.join(","))
}

/// 滤镜跑完之后画面有多高。
///
/// 标识、角标这些**画在最后**的东西，字号要按这个高度算，不能按源尺寸 ——
/// 中间有 `scale=-2:720` 会把画面缩小。
fn final_height(o: &TransformOptions, source_height: i64) -> i64 {
    if o.max_height > 0 && source_height > o.max_height {
        o.max_height
    } else {
        source_height
    }
}
/// 剧名角标：一直显示在角落的小字。
///
/// 和「片头标识」的区别：片头标识是**合规声明**（只显示前几秒、要求显著），
/// 角标是**去重手段**（全程都在、要求低调）。两者目的不同，不能合并。
pub fn build_corner_filter(label: &CornerLabel, size: (i64, i64)) -> AppResult<String> {
    let text = sanitize_label_text(&label.text);
    if text.trim().is_empty() {
        return Ok(String::new());
    }
    let font = find_cjk_font().ok_or_else(|| {
        AppError::Msg("找不到中文字体，角标画不出来（装了微软雅黑或黑体就行）".into())
    })?;
    let (w, h) = size;
    let fs = if label.size_ratio > 0.0 {
        (h as f64 * label.size_ratio).round().max(10.0) as i64
    } else {
        22
    };
    let a = label.opacity.clamp(0.05, 1.0);
    // 边距随字号走，画面大小不同都好看
    let m = (fs / 2).max(8);
    let (x, y) = match label.position {
        LabelPosition::TopLeft => (format!("{m}"), format!("{m}")),
        LabelPosition::TopCenter => ("(w-text_w)/2".to_string(), format!("{m}")),
        LabelPosition::TopRight => (format!("w-text_w-{m}"), format!("{m}")),
        LabelPosition::BottomLeft => (format!("{m}"), format!("h-text_h-{m}")),
        LabelPosition::BottomCenter => (
            "(w-text_w)/2".to_string(),
            format!("h-text_h-{m}"),
        ),
        LabelPosition::BottomRight => (
            format!("w-text_w-{m}"),
            format!("h-text_h-{m}"),
        ),
    };
    let _ = w;
    // 和片头标识一样用**描边**，不用黑底板 —— 角标是全程显示的，
    // 一块黑砖压在画面上更难看。
    Ok(format!(
        "drawtext=fontfile='{}':text='{}':fontsize={fs}:fontcolor=white@{a:.2}:\
         borderw={}:bordercolor=black@0.7:x={x}:y={y}",
        escape_filter_path(&font),
        text,
        (fs / 12).max(2)
    ))
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
            noise: (0, 1),
            speed: (0.99, 1.01),
            volume: (0.99, 1.01),
        },
        Intensity::Medium => Ranges {
            crop: (0.010, 0.030),
            scale: (0.990, 1.010),
            brightness: 0.020,
            contrast: 0.010,
            saturation: 0.010,
            noise: (1, 2),
            speed: (0.97, 1.03),
            volume: (0.98, 1.02),
        },
        Intensity::Strong => Ranges {
            crop: (0.020, 0.060),
            scale: (0.970, 1.030),
            brightness: 0.040,
            contrast: 0.030,
            saturation: 0.030,
            noise: (2, 4),
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

}

/// 按档位与序号生成一组处理参数。同一个序号结果稳定。
pub fn preset_for(level: Intensity, index: usize, st: &Strengths) -> TransformOptions {
    let r = ranges(level);
    let mut rng = Rng::new(index as u64 + 1);

    // **强度决定落在档位区间的哪个位置**：115 → 区间偏下，125 → 顶到上沿。
    //
    // 不直接用 `strength/100` 乘一个固定基准，是因为那样会**跑出档位区间** ——
    // 「轻/中/强」三个档就失去意义了（实测撞到：强档的 crop 算出来超出了
    // 强档自己的上沿）。落在区间里既保住档位语义，也让强度真正起作用。
    let pick = |rng: &mut Rng, range: (f64, f64), strength: i32| -> f64 {
        let t = ((strength - STRENGTH_MIN) as f64 / (STRENGTH_MAX - STRENGTH_MIN) as f64)
            .clamp(0.0, 1.0);
        // 落在区间的 55%~100%：强度低的也不至于弱到没效果
        let base = range.0 + (range.1 - range.0) * (0.55 + 0.45 * t);
        // **每个文件再抖一点** —— 否则同一次运行里所有文件参数一模一样。
        // 去重的意义正是「每一片都不一样」。
        let jitter = (range.1 - range.0) * 0.18;
        (base + rng.range(-jitter, jitter)).clamp(range.0, range.1)
    };

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
        // 每一项 = 基准 × （这次运行摇到的强度 / 100）。
        // **不写死** —— 固定值意味着每次跑出来的处理完全一样，去重就没意义了。
        crop_ratio: pick(&mut rng, r.crop, st.crop),
        scale_ratio: pick(&mut rng, r.scale, st.color),
        brightness: signed(&mut rng, r.brightness * Strengths::factor(st.color) / 1.2),
        contrast: 1.0 + signed(&mut rng, r.contrast * Strengths::factor(st.color) / 1.2),
        saturation: 1.0 + signed(&mut rng, r.saturation * Strengths::factor(st.color) / 1.2),
        noise: pick(&mut rng, (r.noise.0 as f64, r.noise.1 as f64), st.noise).round() as u32,
        speed: pick(&mut rng, r.speed, st.speed),
        volume: pick(&mut rng, r.volume, st.audio),
        // 片头标识是**合规开关**，由用户显式要求；随机预设不擅自加，
        // 也不能擅自加 —— 内容是 AI 生成的才需要标，标错了是另一种不实。
        header_label: None,

        // ---- 下面照真实软件那 13 项补齐 ----
        hue: signed(&mut rng, BASE_HUE * Strengths::factor(st.hue)),
        // 锐化减半：0.6 × 1.2 ≈ 0.72 在 720p 上已经能看出「数码味」，
        // 降到 0.35 左右既有去重效果又不刺眼
        sharpen: BASE_SHARPEN * Strengths::factor(st.sharpen) * 0.5,
        // **帧混合默认关。**
        //
        // 我一开始把它当主力开了（强度 124 × 2.4 = 3 帧），结果成片
        // **发灰、发闷，动起来三重拖影** —— 用户原话「看着有点晃眼啊我去」。
        //
        // 错在映射：真实软件日志里那个 `tmix=124` 是**强度序号**（115~125 那一套），
        // 不是帧数。我把它乘成帧数，等于永远顶格，画质必然崩。
        //
        // 相邻帧叠加一定损画质，所以它必须由用户显式开，不能默认上。
        tmix: 0,
        // **统一出 720p**：真实软件是 1080 进、720 出。省一半时间与空间，
        // 而平台本来也会重压一遍。
        max_height: 720,
        fps: 30.0,
        // 几何位移而不是微旋转 —— 真实软件特意注明「原微旋转」，
        // 旋转会在四角留黑边，位移不会。
        shift_x: (BASE_SHIFT * Strengths::factor(st.shift)) as i64
            * if rng.next_f64() < 0.5 { -1 } else { 1 },
        shift_y: (BASE_SHIFT * Strengths::factor(st.shift)) as i64
            * if rng.next_f64() < 0.5 { -1 } else { 1 },
        // 剧名角标由调用方塞（只有它知道最终剧名），这里只留位置
        corner_label: None,
        // 编码：GOP 固定 60，B 帧随机 0/1/2（真实软件就是这么随机化的）
        gop: 60,
        bframes: (rng.next_f64() * 3.0) as i64,
        // ≥6Mbps 是真实软件对成片的要求
        bitrate_bps: 6_000_000,
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

/// 这一片到底改了什么 —— 一句话说清楚。
///
/// 用户抱怨「看不到整集去重后的效果差异日志」：
/// 原来只报「去重完成，13.2 MB」，**看不出跟原片差在哪**。
/// 每片用的参数其实都不一样（按序号生成，故意各不相同），
/// 把差异列出来，用户才知道这一步真干了活、干的是什么。
///
/// 只报**动了的部分** —— 中性值的项不列，否则一行全是"没改"。
pub fn describe(opts: &TransformOptions) -> String {
    let mut parts: Vec<String> = Vec::new();

    if opts.mirror {
        parts.push("水平镜像".to_string());
    }
    if opts.crop_ratio > 0.0 {
        parts.push(format!("四边裁 {:.1}%", opts.crop_ratio * 100.0));
    }
    if (opts.scale_ratio - 1.0).abs() > 0.0005 {
        parts.push(format!("缩放 {:.1}%", (opts.scale_ratio - 1.0) * 100.0));
    }
    if opts.brightness.abs() > 0.0005 {
        parts.push(format!("亮度 {:+.0}%", opts.brightness * 100.0));
    }
    if (opts.contrast - 1.0).abs() > 0.0005 {
        parts.push(format!("对比度 {:+.0}%", (opts.contrast - 1.0) * 100.0));
    }
    if (opts.saturation - 1.0).abs() > 0.0005 {
        parts.push(format!("饱和 {:+.0}%", (opts.saturation - 1.0) * 100.0));
    }
    if opts.noise > 0 {
        parts.push(format!("噪点 {}", opts.noise));
    }
    if (opts.speed - 1.0).abs() > 0.0005 {
        parts.push(format!("变速 {:.0}%", opts.speed * 100.0));
    }
    if (opts.volume - 1.0).abs() > 0.0005 {
        parts.push(format!("音量 {:.0}%", opts.volume * 100.0));
    }
    if let Some(l) = &opts.header_label {
        parts.push(format!("片头「{}」", l.text));
    }

    if parts.is_empty() {
        "没有启用任何处理项".to_string()
    } else {
        parts.join("、")
    }
}

// ---------------- 平台输出规范 ----------------

/// 发布前的硬门槛。照着真实软件日志里那句「输出已通过校验：
/// ≤500MB、720p、≥4Mbps、≥30s」定的 —— 这几条是平台上传的常见拒收原因。
pub const MAX_BYTES: i64 = 500 * 1024 * 1024;
pub const MIN_HEIGHT: i64 = 720;
pub const MIN_BITRATE_BPS: i64 = 4_000_000;
pub const MIN_DURATION_MS: i64 = 30_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecProblem {
    pub file: String,
    pub problems: Vec<String>,
}

/// 检查一个成片是否满足平台上传规范。
///
/// **只报不改**：这些是提示，不是失败。短片（比如 10 秒分镜）本来就不满足
/// 时长要求，那是正常的 —— 硬失败会把正常流程卡死。
pub fn check_spec(ffprobe: &Path, file: &Path) -> Vec<String> {
    let mut problems = Vec::new();

    let size = std::fs::metadata(file).map(|m| m.len() as i64).unwrap_or(0);
    if size == 0 {
        problems.push("文件不存在或是空的".to_string());
        return problems;
    }
    if size > MAX_BYTES {
        problems.push(format!("体积 {:.0} MB 超过 500 MB", size as f64 / 1048576.0));
    }

    let output = match no_window(
        Command::new(ffprobe).args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,bit_rate",
            "-show_entries",
            "format=duration,bit_rate",
            "-of",
            "json",
        ]),
    )
    .arg(file)
    .output()
    {
        Ok(o) => o,
        Err(_) => {
            problems.push("探测不了这个文件".to_string());
            return problems;
        }
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        problems.push("探测结果读不出来".to_string());
        return problems;
    };

    let height = v
        .pointer("/streams/0/height")
        .and_then(|h| h.as_i64())
        .unwrap_or(0);
    if height > 0 && height < MIN_HEIGHT {
        problems.push(format!("分辨率 {height}p 低于 720p"));
    }

    // 码率优先取流上的，没有就用格式上的；都没有就用 体积/时长 算
    let duration_s = v
        .pointer("/format/duration")
        .and_then(|d| d.as_str())
        .and_then(|d| d.parse::<f64>().ok())
        .unwrap_or(0.0);
    let stream_rate = v
        .pointer("/streams/0/bit_rate")
        .and_then(|b| b.as_str())
        .and_then(|b| b.parse::<i64>().ok())
        .unwrap_or(0);
    let format_rate = v
        .pointer("/format/bit_rate")
        .and_then(|b| b.as_str())
        .and_then(|b| b.parse::<i64>().ok())
        .unwrap_or(0);
    let bitrate = if stream_rate > 0 {
        stream_rate
    } else if format_rate > 0 {
        format_rate
    } else if duration_s > 0.0 {
        (size as f64 * 8.0 / duration_s) as i64
    } else {
        0
    };
    if bitrate > 0 && bitrate < MIN_BITRATE_BPS {
        problems.push(format!("码率 {:.1} Mbps 低于 4 Mbps", bitrate as f64 / 1_000_000.0));
    }

    if duration_s > 0.0 && (duration_s * 1000.0) < MIN_DURATION_MS as f64 {
        problems.push(format!("时长 {duration_s:.1}s 短于 30s"));
    }

    problems
}

// ---- 新增那几项的基准 ----
//
// 这几项没有「轻/中/强」区间（是照真实软件的 13 项补贴上去的），
// 所以只用强度当倍数：115 → ×1.15，125 → ×1.25。

/// 色相偏移的基准（度）。×1.22 ≈ 1.6 度 —— 肉眼几乎看不出，但指纹会变
const BASE_HUE: f64 = 1.3;
/// 锐化的基准。×1.19 ≈ 0.71
const BASE_SHARPEN: f64 = 0.60;
/// 几何位移的基准（像素）。×1.24 ≈ 6
const BASE_SHIFT: f64 = 5.0;
// ---------------- 强度（每次运行随机） ----------------

/// 13 项去重手段的强度。
///
/// **照真实软件的做法：每项每次运行随机 115~125**（100 = 基准）。
/// 它的日志原话：
///
/// ```text
/// 超强去重强度：各项随机 115～125（帧混合 tmix=124，裁切=116，变速=117，
/// 帧率项(已统一30fps)=122，剧名角标=120，几何位移(原微旋转)=124，
/// 亮度/对比/饱和=118，色相=122，噪点=121，锐化=119，微水印=122，
/// 去头尾=121，音频指纹=115）
/// ```
///
/// **不能写死。** 固定值意味着每次跑出来的处理完全一样 ——
/// 那去重的意义就没了。这里用一个种子生成，同一轮内保持一致、
/// 换一轮就换一套。
#[derive(Debug, Clone, PartialEq)]
pub struct Strengths {
    pub tmix: i32,
    pub crop: i32,
    pub speed: i32,
    pub fps: i32,
    pub corner_label: i32,
    pub shift: i32,
    pub color: i32,
    pub hue: i32,
    pub noise: i32,
    pub sharpen: i32,
    pub watermark: i32,
    pub trim: i32,
    pub audio: i32,
}

/// 强度取值范围（含两端）
pub const STRENGTH_MIN: i32 = 115;
pub const STRENGTH_MAX: i32 = 125;

impl Strengths {
    /// 用种子生成一套强度。同一个种子结果相同 ——
    /// 这样「同一次运行里的每个文件」共享一套，而「每次运行」都不一样。
    pub fn from_seed(seed: u64) -> Self {
        // 自己写个小 PRNG：只为取随机数，不想为它引一个依赖
        let mut s = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut next = || {
            // xorshift64*
            s ^= s >> 12;
            s ^= s << 25;
            s ^= s >> 27;
            let v = s.wrapping_mul(2685821657736338717);
            STRENGTH_MIN + (v >> 33) as i32 % (STRENGTH_MAX - STRENGTH_MIN + 1)
        };
        Self {
            tmix: next(),
            crop: next(),
            speed: next(),
            fps: next(),
            corner_label: next(),
            shift: next(),
            color: next(),
            hue: next(),
            noise: next(),
            sharpen: next(),
            watermark: next(),
            trim: next(),
            audio: next(),
        }
    }

    /// 当前时间当种子 —— 每次跑都不一样。
    pub fn fresh() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(12345);
        Self::from_seed(nanos)
    }

    /// 这一项相对基准的倍数：116 → 1.16
    fn factor(v: i32) -> f64 {
        v as f64 / 100.0
    }

    /// 给人看的一行 —— 真实软件也是把这一行写进日志的，
    /// 用户回头才知道「这次跑用的是什么强度」。
    pub fn describe(&self) -> String {
        format!(
            "各项随机 {STRENGTH_MIN}～{STRENGTH_MAX}（帧混合 tmix={}，裁切={}，变速={}，\
             帧率项={}，剧名角标={}，几何位移={}，亮度/对比/饱和={}，色相={}，噪点={}，\
             锐化={}，微水印={}，去头尾={}，音频指纹={}）",
            self.tmix,
            self.crop,
            self.speed,
            self.fps,
            self.corner_label,
            self.shift,
            self.color,
            self.hue,
            self.noise,
            self.sharpen,
            self.watermark,
            self.trim,
            self.audio
        )
    }
}

// ---------------- 并行度 ----------------

/// 画面处理同时跑几路。
///
/// 依据来自真实软件的运行日志：它用 **4 路并行**，28 个文件 7 分钟跑完；
/// 同样的活串行要接近半小时。差距主要不在编码器，在并行度。
///
/// 也不能开太大：每路 ffmpeg 的滤镜链会吃掉 2~3 个核
/// （scale / eq / noise 都是逐像素运算），开超过核数只会互相抢。
pub fn transform_concurrency() -> usize {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    // 每路按 3 个核算，夹在 1~4 路之间
    (cores / 3).clamp(1, 4)
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

    // 编码参数要从**归一化之后**的选项读 —— 归一化会把越界值夹回安全范围，
    // 直接用原始 opts 可能拿到 0 或负数（GOP/B 帧）。
    let o = normalize(opts);
    let video = build_video_filter(&o, target)?;
    let audio = build_audio_filter(&o);
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
        "-c:v", "libx264", "-preset", "veryfast", "-pix_fmt", "yuv420p",
    ] {
        args.push(a.into());
    }
    // **码率下限 vs CRF。** 真实软件要求成片 ≥6Mbps，
    // 而 CRF 20 在 720p 上通常只有 2~4Mbps —— 达不到那条线。
    // 给了目标码率就走受限 VBR（minrate 保证不低于），否则走 CRF。
    if o.bitrate_bps > 0 {
        let kbps = o.bitrate_bps / 1000;
        for a in [
            "-b:v".to_string(),
            format!("{kbps}k"),
            "-minrate".to_string(),
            format!("{kbps}k"),
            "-maxrate".to_string(),
            format!("{}k", kbps * 4 / 3),
            "-bufsize".to_string(),
            format!("{}k", kbps * 2),
        ] {
            args.push(a);
        }
    } else {
        args.push("-crf".into());
        args.push("20".into());
    }
    // **GOP 与 B 帧。** 真实软件固定 GOP=60、B 帧在 0/1/2 之间随机 ——
    // 这两项也是去重指纹的一部分。
    if o.gop > 0 {
        args.push("-g".into());
        args.push(o.gop.to_string());
    }
    if o.bframes >= 0 {
        args.push("-bf".into());
        args.push(o.bframes.to_string());
    }
    for a in ["-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart"] {
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
    /// 「改了什么」要说人话，而且**只列动过的项**。
    ///
    /// 用户抱怨过「看不到整集去重后的效果差异日志」—— 原来只报一个文件大小，
    /// 看不出跟原片差在哪。这里保证：动了什么就写什么，没动的不占地方。
    #[test]
    fn describe_only_lists_what_actually_changed() {
        let neutral = TransformOptions::default();
        assert_eq!(describe(&neutral), "没有启用任何处理项");

        let mut o = TransformOptions::default();
        o.crop_ratio = 0.04;
        o.speed = 1.05;
        let d = describe(&o);
        assert!(d.contains("四边裁 4.0%"), "{d}");
        assert!(d.contains("变速 105%"), "{d}");
        // 没动的项不该出现
        assert!(!d.contains("噪点"), "没开噪点就不该提：{d}");
        assert!(!d.contains("镜像"), "没镜像就不该提：{d}");

        let mut o2 = TransformOptions::default();
        o2.mirror = true;
        o2.noise = 6;
        o2.header_label = Some(HeaderLabel::default());
        let d2 = describe(&o2);
        assert!(d2.contains("水平镜像"), "{d2}");
        assert!(d2.contains("噪点 6"), "{d2}");
        assert!(d2.contains("片头"), "加了片头标识要说出来：{d2}");
    }

    #[test]
    fn spec_check_flags_low_resolution_and_short_duration() {
        let (Some(ffmpeg), Some(ffprobe)) = (
            std::env::var("DRAMADIST_TEST_FFMPEG")
                .ok()
                .map(PathBuf::from)
                .filter(|p| p.is_file()),
            std::env::var("DRAMADIST_TEST_FFPROBE")
                .ok()
                .map(PathBuf::from)
                .filter(|p| p.is_file()),
        ) else {
            eprintln!("跳过：需要 ffmpeg / ffprobe");
            return;
        };

        let dir = std::env::temp_dir().join("dramadist_spec_check");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 360p、4 秒、码率极低 —— 三条都不达标
        let small = dir.join("small.mp4");
        let st = crate::proc::no_window(
            Command::new(&ffmpeg).args([
                "-hide_banner", "-loglevel", "error", "-y",
                "-f", "lavfi", "-i", "testsrc=size=640x360:rate=15",
                "-t", "4", "-c:v", "libx264", "-preset", "ultrafast", "-crf", "40",
                "-pix_fmt", "yuv420p",
            ]),
        )
        .arg(&small)
        .status()
        .expect("生成测试视频失败");
        assert!(st.success());

        let problems = check_spec(&ffprobe, &small);
        assert!(
            problems.iter().any(|p| p.contains("720p")),
            "应当报分辨率低：{problems:?}"
        );
        assert!(
            problems.iter().any(|p| p.contains("短于 30s")),
            "应当报时长不足：{problems:?}"
        );

        // 不存在文件要如实报，而不是崩
        let missing = check_spec(&ffprobe, &dir.join("nope.mp4"));
        assert!(missing.iter().any(|p| p.contains("不存在")), "{missing:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn concurrency_is_bounded_and_reasonable() {
        let n = transform_concurrency();
        assert!((1..=4).contains(&n), "并行度应当在 1~4 之间，实际 {n}");
    }

    /// **13 项强度每次运行都要重新摇。**
    ///
    /// 用户的原话：「不能写死呀 他是随机的值好像 各项强度每次随机 115~125」。
    /// 对。固定值意味着每次跑出来的处理完全一样 —— 那去重的意义就没了。
    #[test]
    fn strengths_are_random_per_run_and_inside_range() {
        // 同一个种子必须给同一套（同一次运行里要一致）
        assert_eq!(Strengths::from_seed(9), Strengths::from_seed(9));
        // 不同种子必须给不同套（每次运行要不一样）
        assert_ne!(Strengths::from_seed(1), Strengths::from_seed(2));

        // 摇 200 次，每一项都必须落在 115~125
        for seed in 0..200u64 {
            let s = Strengths::from_seed(seed);
            for (name, v) in [
                ("tmix", s.tmix),
                ("crop", s.crop),
                ("speed", s.speed),
                ("fps", s.fps),
                ("corner_label", s.corner_label),
                ("shift", s.shift),
                ("color", s.color),
                ("hue", s.hue),
                ("noise", s.noise),
                ("sharpen", s.sharpen),
                ("watermark", s.watermark),
                ("trim", s.trim),
                ("audio", s.audio),
            ] {
                assert!(
                    (STRENGTH_MIN..=STRENGTH_MAX).contains(&v),
                    "{name} 越界：{v}（种子 {seed}）"
                );
            }
        }

        // 真要"每项都独立随机"：不能 13 项永远相等
        let s = Strengths::from_seed(12345);
        let all = [
            s.tmix, s.crop, s.speed, s.fps, s.corner_label, s.shift, s.color, s.hue,
            s.noise, s.sharpen, s.watermark, s.trim, s.audio,
        ];
        let distinct: std::collections::HashSet<i32> = all.iter().copied().collect();
        assert!(distinct.len() >= 5, "13 项应当各不相同，实际只有 {} 种", distinct.len());
    }

    /// 强度确实会影响参数 —— 否则摇它干什么。
    #[test]
    fn strength_actually_changes_the_output() {
        let low = Strengths {
            tmix: 115, crop: 115, speed: 115, fps: 115, corner_label: 115, shift: 115,
            color: 115, hue: 115, noise: 115, sharpen: 115, watermark: 115, trim: 115,
            audio: 115,
        };
        let high = Strengths {
            tmix: 125, crop: 125, speed: 125, fps: 125, corner_label: 125, shift: 125,
            color: 125, hue: 125, noise: 125, sharpen: 125, watermark: 125, trim: 125,
            audio: 125,
        };
        let a = preset_for(Intensity::Strong, 3, &low);
        let b = preset_for(Intensity::Strong, 3, &high);
        assert!(b.crop_ratio > a.crop_ratio, "裁切该随强度变：{} vs {}", a.crop_ratio, b.crop_ratio);
        assert!(b.speed > a.speed, "变速该随强度变");
        assert!(b.noise > a.noise, "噪点该随强度变");
    }
    /// 临时：打印实际生成的滤镜串，用来核对片头标识有没有进去。
    #[test]
    #[ignore]
    fn dump_label_filter() {
        let st = Strengths::from_seed(42);
        let mut o = preset_for(Intensity::Strong, 0, &st);
        o.header_label = Some(HeaderLabel {
            text: "AI创作".into(),
            seconds: 3.0,
            position: LabelPosition::TopLeft,
            size_ratio: 0.03,
        });
        let n = normalize(&o);
        eprintln!("=== 无标识 ===");
        eprintln!("{}", build_video_filter(&o, Some((1080, 1920))).unwrap());
        eprintln!("=== 有标识（normalize 之后）===");
        eprintln!("{}", build_video_filter(&n, Some((1080, 1920))).unwrap());
        eprintln!("=== normalize 之后 header_label 还在吗: {} ===", n.header_label.is_some());
        eprintln!("=== is_noop: {} ===", is_noop(&n));
    }
    #[test]
    fn generated_presets_never_mirror() {
        // 固定种子 —— 测试要可复现，不能用 fresh()
        let st = Strengths::from_seed(42);
        for level in [Intensity::Light, Intensity::Medium, Intensity::Strong] {
            for index in 0..200 {
                let o = preset_for(level, index, &st);
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
        // 白字要**有描边**，否则压在浅色画面上看不清。
        //
        // 原来这里断言的是 `box=1`（半透明黑底板）。用户反馈
        // 「怎么有个黑色的底板呢 透明的不行嘛？」—— 换成描边后
        // 可读性一样，但不占画面。判据跟着改成描边。
        assert!(f.contains("borderw="), "白字得有描边：{f}");
        assert!(f.contains("bordercolor="), "描边颜色没设：{f}");
        assert!(!f.contains("box=1"), "不该再有黑底板了：{f}");
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

        // **seconds <= 0 现在表示「全程显示」**（不加 enable 条件）。
        //
        // 用户问过「要一直在视频左上方吧」—— 片头几秒和全程显示是两种
        // 做法，法规上都算「显著标识」，选哪种该由用户定。
        // 所以这里不再是"夹到下限 0.5"，而是"0 及以下 = 一直显示"。
        let f = build_label_filter(
            &HeaderLabel {
                seconds: -5.0,
                ..label()
            },
            (1080, 1920),
        )
        .unwrap();
        assert!(
            !f.contains("enable="),
            "seconds <= 0 应当全程显示、不加 enable 条件：{f}"
        );

        let f = build_label_filter(
            &HeaderLabel {
                seconds: 0.0,
                ..label()
            },
            (1080, 1920),
        )
        .unwrap();
        assert!(!f.contains("enable="), "0 也是全程显示：{f}");
        assert!(f.starts_with("drawtext="), "还是要画出来：{f}");
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
        let st = Strengths::from_seed(42);
        for level in [Intensity::Light, Intensity::Medium, Intensity::Strong] {
            for index in 0..50 {
                assert!(
                    preset_for(level, index, &st).header_label.is_none(),
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
            hue: 0.0,
            sharpen: 0.0,
            tmix: 0,
            max_height: 0,
            fps: 0.0,
            shift_x: 0,
            shift_y: 0,
            corner_label: None,
            gop: 0,
            bframes: -1,
            bitrate_bps: 0,        };
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
        // **同一个种子必须给出同一套参数** —— 否则同一次运行里
        // 每个文件的处理基准都在飘，出了事没法复现。
        let st = Strengths::from_seed(7);
        for index in 0..30 {
            let a = preset_for(Intensity::Medium, index, &st);
            let b = preset_for(Intensity::Medium, index, &st);
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
        let st = Strengths::from_seed(7);
        let mut distinct = std::collections::HashSet::new();
        for index in 0..20 {
            let o = preset_for(Intensity::Medium, index, &st);
            distinct.insert(format!("{:.6}|{:.6}|{}", o.crop_ratio, o.speed, o.noise));
        }
        assert!(distinct.len() >= 15, "相邻序号应产生不同参数，实际 {} 种", distinct.len());
    }

    #[test]
    fn stronger_levels_stay_inside_their_own_ranges() {
        let st = Strengths::from_seed(42);
        let light = ranges(Intensity::Light);
        let strong = ranges(Intensity::Strong);
        assert!(light.crop.1 < strong.crop.0, "档位区间应递增");
        assert!(light.noise.1 < strong.noise.0);
        // 任何档位生成的参数都必须在全局安全范围内
        for level in [Intensity::Light, Intensity::Medium, Intensity::Strong] {
            for index in 0..10 {
                let n = normalize(&preset_for(level, index, &st));
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
            hue: 0.0,
            sharpen: 0.0,
            tmix: 0,
            max_height: 0,
            fps: 0.0,
            shift_x: 0,
            shift_y: 0,
            corner_label: None,
            gop: 0,
            bframes: -1,
            bitrate_bps: 0,        };
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
