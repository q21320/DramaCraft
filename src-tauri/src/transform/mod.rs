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
/// 变调 0.5%~1%：再少听不出来，再多会被人耳察觉
pub const PITCH_MIN: f64 = 0.99;
pub const PITCH_MAX: f64 = 1.01;
/// 抽帧间隔上限（帧）。900 帧 ≈ 30 秒 @30fps，再稀就没意义了
pub const DROP_EVERY_MAX: u32 = 900;
/// 扫光透明度上限：超过 6% 就不是「极淡」了
pub const SWEEP_MAX: f64 = 0.06;
/// 微旋转上限（度）。超过 2° 肉眼就能看出画面歪了
pub const ROTATE_MAX: f64 = 2.0;
/// 角落色块透明度上限：超过 5% 就能看出来了
pub const PATCH_ALPHA_MAX: f64 = 0.05;
/// 音频延迟上限（毫秒）。再长就对不上口型了
pub const AUDIO_DELAY_MAX: i64 = 200;
/// 左右声道相位差上限（毫秒）
pub const STEREO_PHASE_MAX: i64 = 3;
/// 白噪声振幅上限（≈ -54dBFS，再大就听得见）
pub const WHITE_NOISE_MAX: f64 = 0.002;
/// 16~20kHz 高频噪声振幅上限。人耳对 16k 以上本就不敏感，再大年轻人会听见
pub const HIGH_NOISE_MAX: f64 = 0.01;
/// 立体声宽度区间（1.0 = 不变）。只动 Side 声道，不碰 Mid
pub const STEREO_WIDTH_MIN: f64 = 0.95;
pub const STEREO_WIDTH_MAX: f64 = 1.05;
/// PTS 微扰上限（毫秒）。必须远小于一帧间隔（30fps 是 33ms），否则会掉帧
pub const PTS_JITTER_MAX: f64 = 8.0;
/// PTS 微扰间隔上限（帧）
pub const PTS_EVERY_MAX: u32 = 60;
/// Lab a*/b* 噪声 σ 上限。再大就不是「低强度」，肤色会开始飘
pub const LAB_NOISE_MAX: f64 = 6.0;
/// DCT 低频扰动幅度上限。再大块内会出现可见的明暗纹
pub const DCT_NOISE_MAX: f64 = 8.0;
/// 去重最多几轮（规范 26）。再往上加，时长和音频代数损失都撑不住
pub const ROUNDS_MAX: i64 = 3;
/// 整集去重默认几轮：两轮
///
/// 第 2 轮只重摇画面（裁切/旋转/调色/噪点/扫光/Lab/DCT/GOP），
/// 变速/变调/音量/音频延迟全部锁在中性值 —— 所以**时长不变、音频不重压**。
/// 实测 20 秒 720p 多跑一轮约 8 秒，一集 1 分钟量级，可以接受。
pub const DEFAULT_ROUNDS: i64 = 2;
/// 第 2 轮起裁切幅度的上限：4%
///
/// 规范给的是四边各 5%，但那是**相对源片**算的。第 2 轮再裁同量级的话，
/// 累计到 9.75%，原本贴边的字幕容易被切掉一半。压在 4% 以内，
/// 两轮累计约 8.8%，观感上看不出。
const CROP_LATER_ROUND_MAX: f64 = 0.04;
/// 第 2 轮起微旋转角度上限（度）。两轮累计不超过 3°，画面还看不出歪
const ROTATE_LATER_ROUND_MAX: f64 = 1.0;
/// 平台安全线：短边不得低于 720p。宽银幕按长边缩放后短边可能只有 718
pub const SAFE_SHORT_SIDE: f64 = 720.0;

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
        // CRF 0 是「没填」；其余夹到 h264 的有效区间
        crf: clamp_i64(raw.crf, 0, 51, 0),
        // 0.0 是旧数据反序列化出来的 —— 当作「不变调」，不能夹成 0.99
        pitch: if raw.pitch.is_finite() && raw.pitch > 0.0 {
            clamp_finite(raw.pitch, PITCH_MIN, PITCH_MAX, 1.0)
        } else {
            1.0
        },
        drop_every: raw.drop_every.min(DROP_EVERY_MAX),
        // 相位必须落在间隔里，否则 select 表达式永远不命中
        drop_phase: if raw.drop_every == 0 {
            0
        } else {
            raw.drop_phase % raw.drop_every.min(DROP_EVERY_MAX)
        },
        sweep: clamp_finite(raw.sweep, 0.0, SWEEP_MAX, 0.0),

        // ---- 画面/音频的小动作，都是从「去重规范」里捞出来的 ----
        rotate_deg: clamp_finite(raw.rotate_deg, -ROTATE_MAX, ROTATE_MAX, 0.0),
        patch_alpha: clamp_finite(raw.patch_alpha, 0.0, PATCH_ALPHA_MAX, 0.0),
        patch_corner: raw.patch_corner.min(3),
        audio_delay_ms: clamp_i64(raw.audio_delay_ms, 0, AUDIO_DELAY_MAX, 0),
        stereo_phase_ms: clamp_i64(raw.stereo_phase_ms, 0, STEREO_PHASE_MAX, 0),
        white_noise: clamp_finite(raw.white_noise, 0.0, WHITE_NOISE_MAX, 0.0),
        // 0.0 是旧数据反序列化出来的 —— 当作「宽度不变」，不能夹成 0.95
        high_noise: clamp_finite(raw.high_noise, 0.0, HIGH_NOISE_MAX, 0.0),
        stereo_width: if raw.stereo_width.is_finite() && raw.stereo_width > 0.0 {
            clamp_finite(
                raw.stereo_width,
                STEREO_WIDTH_MIN,
                STEREO_WIDTH_MAX,
                1.0,
            )
        } else {
            1.0
        },
        pts_jitter_ms: clamp_finite(raw.pts_jitter_ms, 0.0, PTS_JITTER_MAX, 0.0),
        pts_jitter_every: raw.pts_jitter_every.min(PTS_EVERY_MAX),
        // 相位必须落在间隔里，否则 eq 永远不命中
        pts_jitter_phase: if raw.pts_jitter_every == 0 {
            0
        } else {
            raw.pts_jitter_phase % raw.pts_jitter_every.min(PTS_EVERY_MAX)
        },
        scenecut: clamp_i64(raw.scenecut, 0, 100, 0),
        lab_noise: clamp_finite(raw.lab_noise, 0.0, LAB_NOISE_MAX, 0.0),
        dct_noise: clamp_finite(raw.dct_noise, 0.0, DCT_NOISE_MAX, 0.0),
        // 0 是「没填」—— 调用方自己决定跑几轮，这里不逼出 2 轮
        rounds: if raw.rounds <= 0 {
            0
        } else {
            raw.rounds.min(ROUNDS_MAX)
        },
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
        && (o.pitch - 1.0).abs() <= EPS
        && o.drop_every == 0
        && o.sweep <= EPS
        && o.rotate_deg.abs() <= 0.01
        && o.patch_alpha <= EPS
        && o.audio_delay_ms == 0
        && o.stereo_phase_ms == 0
        && o.white_noise <= EPS
        && o.high_noise <= EPS
        && (o.stereo_width - 1.0).abs() <= EPS
        && o.pts_jitter_ms <= EPS
        && no_label
}

// ---------------- 片头标识（合规） ----------------

/// 常见中文字体，按优先级排。
///
/// `drawtext` 必须拿到**具体字体文件**，系统默认字体不一定有中文字形
/// （拿不到就会画出一串方框）。这几款在中文 Windows 上必然存在。
///
/// **顺序就是优先级。**
#[cfg(windows)]
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

/// macOS 上必然存在的几款中文字体（系统自带，不需要用户装）。
///
/// 用 `.ttc` 也没问题：`drawtext` 打开 ttc 时取第一个字面，这几份的第一个
/// 字面都带完整的中文字形（`STHeiti Medium.ttc` + drawtext 实测过中文正常）。
/// 黑体系排在宋体前面 —— 和 Windows 那边「雅黑 Bold 优先」是同一个理由：
/// 720p 的角标尺寸下，黑体的小字更清楚。
#[cfg(target_os = "macos")]
const CJK_FONTS: [&str; 4] = [
    "/System/Library/Fonts/Hiragino Sans GB.ttc", // 冬青黑体简体中文
    "/System/Library/Fonts/STHeiti Medium.ttc",   // 华文黑体
    "/System/Library/Fonts/STHeiti Light.ttc",    // 华文黑体（细）
    "/System/Library/Fonts/Supplemental/Songti.ttc", // 宋体
];

/// 其他平台（Linux）：这几处在发行版上很常见，没有就走 `DRAMADIST_LABEL_FONT`。
#[cfg(not(any(windows, target_os = "macos")))]
const CJK_FONTS: [&str; 3] = [
    "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
    "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
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

    // **微旋转。** 0.5°~2° 的随机角度。
    //
    // 旋转本身一定会把四角转到画面外（露黑），所以不能只 `rotate`：
    // 先转，再按「能盖住整个画面」的比例放大，最后居中裁回原尺寸。
    // 需要放大多少由宽高比算：`cosθ + (H/W)·sinθ` 与 `cosθ + (W/H)·sinθ`
    // 取大者。竖屏 1080×1920 转 2° 约 6.1%，横屏反过来。
    //
    // **居中裁，不是贴底裁。** 贴底裁时画面上沿会顶到旋转后的窄边，
    // 同样的角度需要放大接近一倍才能盖住 —— 实测 2° 要放大到 2.0。
    // 居中裁只各收 3% 左右，对字幕的影响小得多。
    if o.rotate_deg.abs() > 0.01 {
        let rad = o.rotate_deg.to_radians();
        let (tw, th) = target
            .map(|(w, h)| (w as f64, h as f64))
            .unwrap_or((1080.0, 1920.0));
        // **必须用 |sinθ|。** 角度是带符号的，负角度的 sin 是负数，
        // 不带绝对值时 cover 会被 max(1.0) 吞成 1.0 —— 实测 -1.85° 只放大
        // 1.003，再叠上缩小的 scale_ratio，居中裁就凑不够 1080×1920，
        // ffmpeg 直接报 `Invalid too big or non positive size`。
        let sin = rad.sin().abs();
        let cover = (rad.cos() + (th / tw) * sin)
            .max(rad.cos() + (tw / th) * sin)
            .max(1.0);
        // 多放 0.3%：消掉双线性插值和偶数取整留下的黑边；
        // 若前一步 scale_ratio 把画面缩小了，这里要把它补回来（除以 <1 的比例）
        let z = cover * 1.003 / o.scale_ratio.min(1.0);
        parts.push(format!(
            "rotate={:.4}*PI/180:ow=iw:oh=ih:c=black",
            o.rotate_deg
        ));
        parts.push(format!(
            "scale=trunc(iw*{z:.5}/2)*2:trunc(ih*{z:.5}/2)*2"
        ));
        match target {
            Some((w, h)) => {
                let (cw, ch) = (even(w as f64), even(h as f64));
                parts.push(format!(
                    "crop={cw}:{ch}:(iw-{cw})/2:(ih-{ch})/2"
                ));
            }
            None => parts.push(format!(
                "crop=trunc(iw/{z:.5}/2)*2:trunc(ih/{z:.5}/2)*2:\
                 (iw-trunc(iw/{z:.5}/2)*2)/2:(ih-trunc(ih/{z:.5}/2)*2)/2"
            )),
        }
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
    //
    // 帧率不是整数：29.97 / 30.03 交替，差 0.1% —— 平台照常收，
    // 但按整数帧号对齐的时序指纹就对不上了。
    if o.fps > 0.0 {
        let fps = fmt_fps(o.fps);
        parts.push(format!("fps={fps}"));

        // **抽帧。** 每 N 帧丢掉 1 帧，再用同一个 fps 收口 —— 丢帧处的
        // 时间戳空了，fps 滤镜会用前一帧补上，所以**时长和音画同步都不变**，
        // 只是那一帧的内容被邻帧替换。逐帧比对会在这里对不上。
        //
        // 必须成对出现：只 select 不复采样，视频会越跑越短。
        if o.drop_every >= 2 && o.drop_phase < o.drop_every {
            parts.push(format!(
                "select=not(eq(mod(n\\,{})\\,{}))",
                o.drop_every, o.drop_phase
            ));
            parts.push(format!("fps={fps}"));
        }
    }

    // **统一出 720p —— 按「长边」算，不是按高度。**
    //
    // 真实软件是 1080 进、720 出，但那只对**竖屏**成立。写成
    // `scale=-2:1280` 的话，横屏素材（比如 872×480）会被放大成
    // **2324×1280** —— 又慢又糊，而且和 `tmix` 撞在一起会直接把内存吃光：
    //
    //     119 帧 × 2324×1280 × 3 字节 ≈ 1.06 GB   ← 只算 tmix 的帧缓冲
    //     119 帧 ×  720×1280 × 3 字节 ≈ 0.33 GB   ← 竖屏没这个问题
    //
    // 实测 28 集竖屏全过、横屏那集 `frame= 0` 直接失败。
    //
    // 现在：竖屏锁高度（720×1280），横屏锁宽度（1280×720）。两边的
    // 「长边」都是 1280，这才是 720p 的本意。**竖屏行为完全不变。**
    if o.max_height > 0 {
        let portrait = match target {
            Some((w, h)) => h >= w,
            // 不知道源尺寸时按竖屏处理 —— 这个项目的主体是竖屏短剧
            None => true,
        };
        let long = o.max_height;
        // **一次缩放同时管住长边和短边。**
        //
        // 原来只写 `scale=-2:1280`，长边对了，短边靠自动计算 ——
        // 中间 crop / scale_ratio 的偶数取整会改掉宽高比（1080×1920 →
        // 1046×1862），再按长边缩放，宽就落到 **718**。平台按 720p 收，
        // 卡在 718 会白跑一遍。
        //
        // 现在：短边 = max(长边缩放后的短边, 720)，另一边自动（-2）保持比例。
        // 宽银幕 1280×536 → 1720×720；9:21 窄屏 → 720×1680（宁可包体大一点）。
        if portrait {
            parts.push(format!(
                "scale=trunc(max(720\\,iw*{long}/ih)/2)*2:-2"
            ));
        } else {
            parts.push(format!(
                "scale=-2:trunc(max(720\\,ih*{long}/iw)/2)*2"
            ));
        }
    }

    // **动态扫光。** 一条极淡的亮带横向扫过画面，6 秒一遍。
    // 它改的是逐像素纹理（亮度沿一条带周期变化），比整体调色难对齐得多。
    //
    // 为什么分 6 段：`drawbox` 的 x 表达式**拿不到时间变量**
    // （`t` 在这个滤镜里是 thickness，实测固定不动），但 `enable` 是
    // 标准 timeline 选项。6 条亮带轮流出现，等效于一条横移的扫光，
    // 而且不用引入第二路输入。
    //
    // 放在缩放之后：亮带宽度按最终画面算；放在标识之前：标识要压在最上层。
    if o.sweep > EPS {
        let alpha = o.sweep.clamp(0.01, SWEEP_MAX);
        for i in 0..6 {
            // -15% → 85%，正好从画面外扫到另一侧
            let x = i as f64 / 5.0 - 0.15;
            parts.push(format!(
                "drawbox=x=iw*({x:.3}):y=0:w=iw*0.15:h=ih:color=white@{alpha:.3}:t=fill:\
                 enable='between(mod(t\\,6)\\,{i}.0\\,{}.0)'",
                i + 1
            ));
        }
    }

    // **角落小色块。** 极小的半透明白块，随机落在四角之一。
    // 透明度 < 5%，肉眼几乎看不见；但它会在固定的角落制造一个
    // 稳定的像素偏置，比整帧调色更难被“对齐后相减”抹掉。
    if o.patch_alpha > EPS {
        let a = o.patch_alpha.clamp(0.01, PATCH_ALPHA_MAX);
        let (x, y) = match o.patch_corner {
            0 => ("0", "0"),
            1 => ("iw-w", "0"),
            2 => ("0", "ih-h"),
            _ => ("iw-w", "ih-h"),
        };
        parts.push(format!(
            "drawbox=x={x}:y={y}:w=max(8\\,min(iw\\,ih)*0.015):\
             h=max(8\\,min(iw\\,ih)*0.015):color=white@{a:.3}:t=fill"
        ));
    }

    // 片头标识放在**最后**：它要压在最终画面上。
    // 放前面的话，后面的裁切/缩放会把标识一起变形。
    if let Some(label) = &o.header_label {
        // **按缩放之后的高度算字号。**
        //
        // 标识画在 `scale=-2:720` 之后，画面已经是 720 高了；
        // 还按源尺寸 1920 算字号的话，字会大三倍（实测 fontsize=58 画在 720 上）。
        let w = target.map(|(w, _)| w).unwrap_or(1080);
        let size = (w as i64, final_height(&o, target));
        let seg = build_label_filter(label, size)?;
        if !seg.is_empty() {
            parts.push(seg);
        }
    }

    // 剧名角标也是压在最终画面上，和片头标识同理
    if let Some(corner) = &o.corner_label {
        let w = target.map(|(w, _)| w).unwrap_or(1080);
        let size = (w as i64, final_height(&o, target));
        let seg = build_corner_filter(corner, size)?;
        if !seg.is_empty() {
            parts.push(seg);
        }
    }

    // **PTS 微扰（最后一步）。** 每隔几帧给某一帧的显示时间戳加 2~5ms ——
    // 远小于一帧（30fps 是 33ms），只加不减，所以时间戳仍然单调，不会掉帧。
    //
    // 必须先 `settb=1/90000`：滤镜的时基如果是 1/30，几毫秒的偏移会被
    // 四舍五入成「0 帧」或「整 1 帧」，后者反而把两帧挤到同一时刻（实测过）。
    // `apply` 里配套加 `-enc_time_base 1/90000`，否则编码器会按帧率再量化回来。
    if o.pts_jitter_ms > 0.0 && o.pts_jitter_every > 0 {
        let ms = o.pts_jitter_ms.clamp(0.5, PTS_JITTER_MAX);
        let every = o.pts_jitter_every.max(2);
        let phase = o.pts_jitter_phase % every;
        parts.push(format!(
            "settb=1/90000,setpts=PTS+{:.4}/TB*eq(mod(N\\,{every})\\,{phase})",
            ms / 1000.0
        ));
    }

    Ok(parts.join(","))
}

/// 滤镜跑完之后画面有多高。
///
/// 标识、角标这些**画在最后**的东西，字号要按这个高度算，不能按源尺寸 ——
/// 中间有 `scale=-2:1280` 会把画面缩小。
///
/// 还要算上短边安全线：1280×536 的宽银幕会被抬到 1720×720，
/// 最终高度是 720（不是 1280），字号得跟着降。
fn final_height(o: &TransformOptions, target: Option<(u32, u32)>) -> i64 {
    let (w, h) = match target {
        Some((w, h)) if w > 0 && h > 0 => (w as f64, h as f64),
        _ => (1080.0, 1920.0),
    };
    if o.max_height <= 0 {
        return h as i64;
    }
    let long = o.max_height as f64;
    if h >= w {
        // 竖屏：长边是高；窄过 9:16 时会被安全线按 720 宽放大
        long.max(SAFE_SHORT_SIDE * h / w).round() as i64
    } else {
        // 横屏：长边是宽，高 = 长边 × h/w，不足 720 时抬到 720
        (long * h / w).max(SAFE_SHORT_SIDE).round() as i64
    }
}

/// 帧率写法：整数不带小数点，小数保留两位（29.97 / 30.03）。
fn fmt_fps(fps: f64) -> String {
    if (fps - fps.round()).abs() < 0.001 {
        format!("{}", fps.round() as i64)
    } else {
        format!("{fps:.2}")
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
///
/// **变调不是「锦上添花」**：单纯改音量对音频指纹几乎无效，
/// 变调会真的挪动频谱峰值 —— 这是音频比对最先看的东西。
pub fn build_audio_filter(raw: &TransformOptions) -> String {
    let o = normalize(raw);
    let mut parts: Vec<String> = Vec::new();

    if (o.volume - 1.0).abs() > EPS {
        parts.push(format!("volume={:.3}", o.volume));
    }

    // **变调不变速。**
    //
    // `asetrate` 改采样率标签 → 音调变、时长也跟着变；再用 `atempo` 把时长拉回来。
    // 两个比例互相抵消，最终时长仍只由 `speed` 决定，和视频的 `setpts` 对齐。
    //
    // 先 `aresample=48000` 是必需的：`asetrate` 用的是「当前采样率」，
    // 不先统一的话，44.1k 的源会被按 48k 算，变调幅度直接变成 9%（实测过）。
    let pitched = (o.pitch - 1.0).abs() > EPS;
    if pitched {
        const RATE: f64 = 48_000.0;
        parts.push(format!("aresample={RATE:.0}"));
        parts.push(format!("asetrate={:.0}", RATE * o.pitch));
        parts.push(format!("aresample={RATE:.0}"));
    }

    // 变调已经把时长拉了 1/pitch，atempo 要把它吃回去：speed / pitch。
    let tempo = if pitched { o.speed / o.pitch } else { o.speed };
    if (tempo - 1.0).abs() > EPS {
        parts.push(format!("atempo={tempo:.4}"));
    }

    // **立体声宽度。** `extrastereo` 只放大/衰减 Side（L-R），Mid 不动，
    // 所以音调、语速、对白位置都不变。单声道源会被自动上混成双声道，
    // 此时 Side 为 0，听感不变。
    //
    // **必须放在 aeval 之前。** 实测：`aeval(...):c=same` 直接接 `extrastereo`
    // 会让 ffmpeg 段错误（退出码 139），aformat 也挡不住；先做宽度再叠噪声就正常。
    if (o.stereo_width - 1.0).abs() > EPS {
        parts.push(format!("extrastereo=m={:.3}", o.stereo_width));
    }

    // **极低电平白噪声。** 0.0006 ≈ -64dBFS —— 音响开到底也听不出，
    // 但整条音轨的频谱底噪变了。放在 atempo **之后**：先变速再加噪，
    // 噪声不会被时间拉伸成有规律的块。
    if o.white_noise > 0.0 {
        let amp = o.white_noise.clamp(0.0, WHITE_NOISE_MAX);
        parts.push(format!("aeval=val(0)+{amp:.5}*(random(0)-0.5):c=same"));
    }

    // **16~20kHz 高频噪声。** 单独一路「噪声 × 17kHz 载波」，频谱落在
    // 16k~20k 之间；振幅独立于白噪，默认只有 0.2%~0.5% —— 年轻人也很少能听见。
    // 放在 atempo 之后，避免被重采样拉回中频。
    if o.high_noise > 0.0 {
        let amp = o.high_noise.clamp(0.0, HIGH_NOISE_MAX);
        parts.push(format!(
            "aeval=val(0)+{amp:.5}*(random(0)-0.5)*sin(2*PI*(16000+4000*random(1))*t):c=same"
        ));
    }

    // **音频延迟 + 立体声相位差。**
    //
    // `adelay` 的写法是「每个声道一个毫秒数」：左声道延迟 d，右声道再晚
    // phase 毫秒 —— 两个声道之间就有了几毫秒的相位差（单声道只用第一个值，
    // 实测不会报错）。
    //
    // 这 0.1~0.2 秒的偏移按要求只影响音频起点；`apply` 里配了 `-shortest`，
    // 尾部多出来的那一小段会被收掉，成片时长仍跟画面一致。
    let delay = o.audio_delay_ms.max(0);
    let phase = o.stereo_phase_ms.max(0);
    if delay > 0 || phase > 0 {
        parts.push(format!("adelay={}|{}", delay, delay + phase));
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

    /// 轻/中档按同一套配方缩小幅度。
    ///
    /// 三个档位用的是**同一组手段**（裁切/旋转/抽帧/变调/扫光…），
    /// 只是幅度按比例缩 —— 轻档也有完整链路，不是“什么都不做”。
    fn factor(self) -> f64 {
        match self {
            Intensity::Light => 0.55,
            Intensity::Medium => 0.78,
            Intensity::Strong => 1.0,
        }
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
///
/// 数值基线直接对着「去重处理规范」：四边各裁 5%（批量 4.5%~5.5%）、
/// 缩放 0.5%、微旋转 0.5°~2°、亮度 +2%/对比 -3%/饱和 -2%、噪点 4、
/// 变速 105%、每 10 秒抽 1~2 帧、帧率 ±0.5%、极淡扫光 2%~5%、
/// 变调 0.5%~1%、音量 105%、音频延迟 0.1~0.2s、极低白噪声、
/// 16~20kHz 高频噪声、立体声宽度 ±1%~±3%、PTS 微扰 2~5ms、
/// GOP/B 帧/CRF/场景阈值随机。
/// 轻/中档用同一套手段，幅度按 [`Intensity::factor`] 缩小。
pub fn preset_for(level: Intensity, index: usize, st: &Strengths) -> TransformOptions {
    let mut rng = Rng::new(index as u64 + 1);
    let f = level.factor();

    // 强度 115~125 决定落在区间下沿还是上沿；再抖 ±12%，
    // 同一批里每个文件的参数都不一样，但都不出区间。
    let t = |s: i32| -> f64 {
        ((s - STRENGTH_MIN) as f64 / (STRENGTH_MAX - STRENGTH_MIN) as f64).clamp(0.0, 1.0)
    };
    let at = |rng: &mut Rng, lo: f64, hi: f64, s: i32| -> f64 {
        let base = lo + (hi - lo) * t(s);
        let jitter = (hi - lo) * 0.12;
        (base + rng.range(-jitter, jitter)).clamp(lo, hi)
    };
    let sign = |rng: &mut Rng| if rng.next_f64() < 0.5 { -1.0 } else { 1.0 };

    // ---- 画面 ----
    // 主裁切：四边各 5%，批量时 4.5%~5.5% 随机
    let crop = at(&mut rng, 0.045, 0.055, st.crop) * f;
    // 裁完缩回原尺寸，缩放本身只放 0.4%~0.6%
    let scale = 1.0 + sign(&mut rng) * at(&mut rng, 0.004, 0.006, st.color) * f;
    // 微旋转 0.5°~2°，方向随机
    let rotate = sign(&mut rng) * at(&mut rng, 0.5, 2.0, st.shift) * f;
    // 调色：亮度 +2%、对比 -3%、饱和 -2%，各 ±1%
    let brightness = (0.02 + rng.range(-0.01, 0.01)) * f;
    let contrast = 1.0 - (0.03 + rng.range(-0.01, 0.01)) * f;
    let saturation = 1.0 - (0.02 + rng.range(-0.01, 0.01)) * f;
    // 噪点强度 4（轻/中档相应降到 2/3）
    let noise = (4.0 * f).round() as u32;
    // 扫光 2%~5%
    let sweep = at(&mut rng, 0.02, 0.05, st.tmix) * f;
    // 四角之一的极淡色块 3%~4.8%（规范要求低于 5%）
    let patch_alpha =
        (at(&mut rng, 0.03, 0.048, st.watermark) * f).clamp(0.01, PATCH_ALPHA_MAX);
    let patch_corner = (rng.next_f64() * 4.0) as u8;

    // ---- 时间轴 ----
    // 变速 105%（1.04~1.06）
    let speed = 1.0 + at(&mut rng, 0.04, 0.06, st.speed) * f;
    // 抽帧：每 10 秒 1~2 帧 ≈ 30fps 下每 150~300 帧丢 1 帧
    let drop_every = 150 + (rng.next_f64() * 150.0) as u32;
    let drop_phase = (rng.next_f64() * drop_every as f64) as u32;
    // 帧率 ±0.5%（30fps → 29.85~30.15）
    let fps = 30.0 * (1.0 + sign(&mut rng) * at(&mut rng, 0.003, 0.005, st.fps) * f);

    // ---- 音频 ----
    // 变调 0.5%~1%，方向随机；轻/中档略浅
    let pitch_pct = (0.005 + 0.005 * t(st.audio)) * f.max(0.75);
    let pitch_dir = sign(&mut rng);
    // 音量 105%
    let volume = 1.0 + (0.05 + rng.range(-0.005, 0.005)) * f;
    // 开头垫 0.1~0.2 秒静音；立体声右声道再多 1~3 毫秒
    let audio_delay_ms = ((100.0 + rng.next_f64() * 100.0) * f).round() as i64;
    let stereo_phase_ms = ((1.0 + rng.next_f64() * 2.0) * f).round().max(1.0) as i64;
    // 白噪声约 -64dBFS
    let white_noise = 0.0006 * f;
    // 16~20kHz 高频噪声：0.2%~0.5%，独立于白噪
    let high_noise = (0.002 + rng.next_f64() * 0.003) * f;
    // 立体声宽度 ±1%~±3%，方向随机（变宽/变窄交替）
    let stereo_width = 1.0 + sign(&mut rng) * at(&mut rng, 0.01, 0.03, st.audio) * f;

    // ---- PTS 微扰 / 关键帧节奏 ----
    // 每 4~8 帧给某一帧加 2~5ms；相位随机，避免每集都是同一帧被扰
    let pts_jitter_ms = (2.0 + rng.next_f64() * 3.0) * f.max(0.7);
    let pts_jitter_every = 4 + (rng.next_f64() * 5.0) as u32;
    let pts_jitter_phase = (rng.next_f64() * pts_jitter_every as f64) as u32;
    // 场景切换阈值 20~60：切镜头时提不提前插关键帧，由它决定。
    // 这样关键帧位置跟画面内容走，不是固定每 N 帧一个
    let scenecut = 20 + (rng.next_f64() * 40.0) as i64;

    // ---- 进阶：Lab / DCT（规范 24/25）----
    // 这两项不在 ffmpeg 滤镜链里，整集去重的最后一遍用 Python 逐帧做。
    // σ 2~3 在 8 位 Lab 上约等于色度轴的 1.5%~2.5%，肉眼很难指认。
    // DCT 幅度不能低于 2：系数加 1 经过反变换只剩 0.2 个灰度级，
    // 8 位取整就吃掉了，等于没做。
    let lab_noise = (2.0 + rng.next_f64() * 1.0) * f;
    let dct_noise = (2.5 + rng.next_f64() * 1.5) * f;

    // ---- 编码：GOP / B 帧 / 码率 / CRF 都在合理范围内随机 ----
    let gop = 56 + (rng.next_f64() * 17.0) as i64;
    let bframes = (rng.next_f64() * 4.0) as i64;
    let bitrate_bps = 6_000_000 + (rng.next_f64() * 400_000.0) as i64;
    // CRF 只在前置码率被清空时生效（有码率走受限 VBR），填上随机值备用
    let crf = 19 + (rng.next_f64() * 5.0) as i64;

    let opts = TransformOptions {
        // **默认不镜像。**
        //
        // 原来是掷硬币（50% 概率），结果真实素材上炸了：那部片子带硬字幕，
        // 镜像之后画面里的字全反了，一半的切片直接废掉。
        //
        // 去重手段里，凡是会破坏画面内文字的都不该默认开。变速、调色、加噪点
        // 都安全；裁切改成贴底裁之后也不会切到字幕；只有镜像是不可挽回的 ——
        // 所以它必须由用户显式要求。
        mirror: false,
        crop_ratio: crop,
        scale_ratio: scale,
        brightness,
        contrast,
        saturation,
        noise,
        speed,
        volume,
        // 片头标识是**合规开关**，由用户显式要求；随机预设不擅自加，
        // 也不能擅自加 —— 内容是 AI 生成的才需要标，标错了是另一种不实。
        header_label: None,

        // ---- 下面照真实软件那 13 项补齐 ----
        hue: sign(&mut rng) * BASE_HUE * Strengths::factor(st.hue),
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
        //
        // **这里填的是「高」，而且必须是 1280 不是 720。**
        //
        // 踩过：填 720 时 `scale=-2:720` 把 1080×1920 缩成了 **404×720**，
        // 用户拿原软件的真实产出（`yiquchong\` 里是 **720×1280**）一比就发现不对 ——
        // 切片跟着也只有 404 宽，码率明明一样（6.05 vs 6.06 Mbps），画面小一半。
        //
        // **短剧的「720p」指宽 720**（竖屏 720×1280），所以按高填就是 1280。
        max_height: 1280,
        // 帧率 ±0.5%，而且不是整数 —— 按整数帧号对齐的时序指纹会对不上
        fps,
        // 几何位移已经由微旋转代替：旋转会真的改画面的像素角度，
        // 比平移更难对齐；位移留着给单步工具页手动用。
        shift_x: 0,
        shift_y: 0,
        // 剧名角标由调用方塞（只有它知道最终剧名），这里只留位置
        corner_label: None,
        // 编码：GOP 56~72，B 帧 0~3，码率 6.0~6.4Mbps —— 随机化也是指纹的一部分
        gop,
        bframes,
        bitrate_bps,
        crf,
        // **音频变调 0.5%~1%。** 单纯改音量对音频指纹几乎无效；
        // 音频强度越高，变调幅度越靠近 1%，方向随机（升/降交替）。
        pitch: 1.0 + pitch_dir * pitch_pct,
        // **抽帧：每 10 秒 1~2 帧**（30fps 下 150~300 帧丢 1 帧）。
        // 只丢 1/200 左右，看不见卡顿，但逐帧比对会错位。
        drop_every,
        drop_phase,
        // **扫光 2%~5% 透明度**：极淡，但像素级纹理变了。
        sweep,
        rotate_deg: rotate,
        patch_alpha,
        patch_corner,
        audio_delay_ms,
        stereo_phase_ms,
        white_noise,
        high_noise,
        stereo_width,
        pts_jitter_ms,
        pts_jitter_every,
        pts_jitter_phase,
        scenecut,
        lab_noise,
        dct_noise,
        // 默认两轮。设 1 就只跑一轮，和原来完全一致
        rounds: DEFAULT_ROUNDS,
    };
    // **第一轮就是原来的那套参数，一个数都不变 —— 变的是「后面还要再摇几轮」。**
    //
    // 后续轮次怎么派生全在 `next_round` 一处，「要锁什么」不会两边各改一份而对不上。
    opts
}

/// 第 2 轮起的参数：**只重摇画面，时间轴和音频全部锁在中性值**。
///
/// 这是规范 26（1~3 轮串联）的做法。为什么不每轮都全量重摇：
///
/// - 每轮变速 5% 叠两轮就 -9.3% 时长，两分钟一集剩下不到 1 分 50 秒；
/// - 每轮重新变调/压 AAC，两代音频损失叠在一起，人耳能听出声音发闷；
/// - 而画面才是比对真正看的东西 —— 再摇一遍裁切/旋转/调色/噪点/
///   Lab/DCT/GOP，画面指纹又换一套，音频和时长却一动不动。
///
/// `index` 是摇种子的序号（带轮序号重新播种，每轮摇到的都不一样），
/// `prev` 是上一轮的参数。
pub fn next_round(prev: &TransformOptions, index: usize) -> TransformOptions {
    let mut rng = Rng::new(index as u64 * 0x9E37 + 0xA5A5);
    let mut o = normalize(prev);

    // ---- 时间轴 / 音频：锁死 ----
    // 变速锁 1.0：两轮叠加速就会把时长压到 5% 红线之外
    o.speed = 1.0;
    // **音量也锁。** 每轮 +5% 叠两轮就是 +10%，远超规范说的「105%」
    o.volume = 1.0;
    // 变调锁死：变调要过 asetrate/atempo，音频得重压一遍
    o.pitch = 1.0;
    // 音频延迟/相位锁死：再垫一次静音就音画不同步了
    o.audio_delay_ms = 0;
    o.stereo_phase_ms = 0;
    // 白噪/高频噪声/立体声宽度都是音频指纹手段，第一轮已经做过
    o.white_noise = 0.0;
    o.high_noise = 0.0;
    o.stereo_width = 1.0;
    // 帧率锁在主链路标称值：不能两轮各摇一次。fps 滤镜重新采样时
    // 源时间戳略有出入就会补/丢一帧，时长会慢慢漂
    o.fps = normalize(prev).fps;
    // 抽帧相位换一组，但间隔不变 —— 两轮丢的不是同一帧
    if o.drop_every > 0 {
        o.drop_phase = (rng.next_f64() * o.drop_every as f64) as u32;
    }
    // 片头标识只在第一轮（第二轮再画一遍就重影了）
    o.header_label = None;
    o.corner_label = None;

    // ---- 画面：重摇 ----
    // 裁切幅度封顶：两轮累计不能把贴底的字幕切掉。
    // 取 `min` —— 第 1 轮本身就裁得小时别把它放大，裁得大时必须压回来
    let crop_cap = o.crop_ratio.min(CROP_LATER_ROUND_MAX);
    o.crop_ratio = rng.range(crop_cap * 0.55, crop_cap);
    // 缩放方向换一边
    o.scale_ratio = 1.0 + if rng.next_f64() < 0.5 { -1.0 } else { 1.0 } * rng.range(0.004, 0.006);
    // 旋转**反向**：第一轮顺时针，这轮就逆时针，两轮叠加不是同一个方向
    let angle = rng.range(0.5, ROTATE_LATER_ROUND_MAX);
    o.rotate_deg = if o.rotate_deg >= 0.0 { -angle } else { angle };
    // 调色重新摇：和第一轮同号就等于重复了，所以反向
    o.brightness = -o.brightness.abs() * rng.range(0.6, 1.0);
    o.contrast = 1.0 + (o.contrast - 1.0).abs() * rng.range(0.6, 1.0);
    o.saturation = 1.0 - (o.saturation - 1.0).abs() * rng.range(0.6, 1.0);
    o.hue = -o.hue;
    o.sharpen = (o.sharpen * rng.range(0.5, 1.0)).clamp(0.0, 0.6);
    // 噪点只增不减：第二轮再加一点，不会把第一轮加的盖掉
    o.noise = o.noise.min(NOISE_MAX - 1) + 1;
    // 扫光/贴角/角标全部换一套
    o.sweep = if o.sweep > 0.0 {
        rng.range(0.02, SWEEP_MAX)
    } else {
        0.0
    };
    o.patch_corner = (rng.next_f64() * 4.0) as u8;
    o.patch_alpha = if o.patch_alpha > 0.0 {
        rng.range(0.01, PATCH_ALPHA_MAX)
    } else {
        0.0
    };

    // ---- 逐帧画质扰动（规范 24/25）：再摇一遍 ----
    if o.lab_noise > 0.0 {
        o.lab_noise = rng.range(2.0, 3.0);
    }
    if o.dct_noise > 0.0 {
        o.dct_noise = rng.range(2.5, 4.0);
    }

    // ---- 编码参数：GOP/B 帧/场景阈值也重新摇 ----
    // 关键帧节奏变了，按 I 帧位置对齐的比对就对不上
    o.gop = 52 + (rng.next_f64() * 24.0) as i64;
    o.bframes = (rng.next_f64() * 4.0) as i64;
    o.scenecut = 20 + (rng.next_f64() * 40.0) as i64;
    o.pts_jitter_every = 4 + (rng.next_f64() * 5.0) as u32;
    o.pts_jitter_phase = (rng.next_f64() * o.pts_jitter_every as f64) as u32;

    o
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

    if opts.rounds > 1 {
        parts.push(format!("{} 轮画面重摇", opts.rounds));
    }
    if opts.mirror {
        parts.push("水平镜像".to_string());
    }
    if opts.crop_ratio > 0.0 {
        parts.push(format!("四边裁 {:.1}%", opts.crop_ratio * 100.0));
    }
    if (opts.scale_ratio - 1.0).abs() > 0.0005 {
        parts.push(format!("缩放 {:+.2}%", (opts.scale_ratio - 1.0) * 100.0));
    }
    if opts.rotate_deg.abs() > 0.01 {
        parts.push(format!("旋转 {:+.1}°", opts.rotate_deg));
    }
    if opts.brightness.abs() > 0.0005 {
        parts.push(format!("亮度 {:+.1}%", opts.brightness * 100.0));
    }
    if (opts.contrast - 1.0).abs() > 0.0005 {
        parts.push(format!("对比 {:+.1}%", (opts.contrast - 1.0) * 100.0));
    }
    if (opts.saturation - 1.0).abs() > 0.0005 {
        parts.push(format!("饱和 {:+.1}%", (opts.saturation - 1.0) * 100.0));
    }
    if opts.noise > 0 {
        parts.push(format!("噪点 {}", opts.noise));
    }
    if (opts.speed - 1.0).abs() > 0.0005 {
        parts.push(format!("变速 {:.1}%", opts.speed * 100.0));
    }
    if (opts.volume - 1.0).abs() > 0.0005 {
        parts.push(format!("音量 {:.1}%", opts.volume * 100.0));
    }
    if (opts.pitch - 1.0).abs() > 0.0005 {
        parts.push(format!("变调 {:+.1}%", (opts.pitch - 1.0) * 100.0));
    }
    if opts.drop_every > 0 {
        parts.push(format!(
            "每 {} 帧抽 1 帧（相位 {}）",
            opts.drop_every, opts.drop_phase
        ));
    }
    if opts.sweep > 0.0005 {
        parts.push(format!("扫光 {:.1}%", opts.sweep * 100.0));
    }
    if opts.patch_alpha > 0.0005 {
        let corner = ["左上", "右上", "左下", "右下"]
            .get(opts.patch_corner as usize)
            .copied()
            .unwrap_or("右下");
        parts.push(format!("{corner}贴角 {:.1}%", opts.patch_alpha * 100.0));
    }
    if opts.audio_delay_ms > 0 {
        parts.push(format!("音频延迟 {}ms", opts.audio_delay_ms));
    }
    if opts.stereo_phase_ms > 0 {
        parts.push(format!("声道差 {}ms", opts.stereo_phase_ms));
    }
    if opts.white_noise > 0.0 {
        parts.push(format!("白噪 {:.2}%", opts.white_noise * 100.0));
    }
    if opts.high_noise > 0.0 {
        parts.push(format!("高频噪 {:.2}%", opts.high_noise * 100.0));
    }
    if (opts.stereo_width - 1.0).abs() > 0.0005 {
        parts.push(format!(
            "立体声宽度 {:+.1}%",
            (opts.stereo_width - 1.0) * 100.0
        ));
    }
    if opts.pts_jitter_ms > 0.0 && opts.pts_jitter_every > 0 {
        parts.push(format!(
            "PTS 抖动 {:.1}ms/{}帧（相位 {}）",
            opts.pts_jitter_ms, opts.pts_jitter_every, opts.pts_jitter_phase
        ));
    }
    if opts.lab_noise > 0.0 {
        parts.push(format!("Lab噪声 σ{:.1}", opts.lab_noise));
    }
    if opts.dct_noise > 0.0 {
        parts.push(format!("DCT扰动 {:.1}", opts.dct_noise));
    }
    if opts.fps > 0.0 {
        parts.push(format!("帧率 {}", fmt_fps(opts.fps)));
    }
    if opts.gop > 0 {
        parts.push(format!("GOP {}（场景阈值 {}）", opts.gop, opts.scenecut));
    }
    if opts.bframes >= 0 {
        parts.push(format!("B帧 {}", opts.bframes));
    }
    if opts.bitrate_bps > 0 {
        parts.push(format!("码率 {:.1}M", opts.bitrate_bps as f64 / 1_000_000.0));
    } else if opts.crf > 0 {
        parts.push(format!("CRF {}", opts.crf));
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
/// 720p 指的是**短边**：竖屏 720×1280、横屏 1280×720。
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

    let width = v
        .pointer("/streams/0/width")
        .and_then(|w| w.as_i64())
        .unwrap_or(0);
    let height = v
        .pointer("/streams/0/height")
        .and_then(|h| h.as_i64())
        .unwrap_or(0);
    // **按短边判 720p。** 只看 height 的话，700×1244 的竖屏会被当成达标；
    // 只看 width 的话，1280×718 又会漏（就是那个尴尬的 718）。
    let short = if width > 0 && height > 0 {
        width.min(height)
    } else {
        height
    };
    if short > 0 && short < MIN_HEIGHT {
        problems.push(format!(
            "分辨率 {width}×{height}（短边 {short}）低于 720p"
        ));
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
        args.push(audio.clone());
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
        // CRF 也随机：批次里每片的量化档都不同，避免输出彼此相似
        args.push("-crf".into());
        args.push(if o.crf > 0 { o.crf } else { 20 }.to_string());
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
    // 场景切换阈值：x264 默认 40，越小越容易在切镜头时插关键帧。
    // 它让关键帧位置跟内容走，不再只是“每 60 帧一个”
    if o.scenecut > 0 {
        args.push("-x264-params".into());
        args.push(format!("scenecut={}", o.scenecut));
    }
    // PTS 微扰需要更细的编码时基，否则几毫秒的偏移会被按帧率四舍五入掉
    if o.pts_jitter_ms > 0.0 {
        args.push("-enc_time_base".into());
        args.push("1/90000".into());
    }
    // **音频要不要重压，看有没有音频滤镜。**
    //
    // 规范 26 的第 2 轮起把变速/变调/音量/延迟全锁在中性值，`audio` 就是空的 ——
    // 这时候再走一遍 AAC 编码纯属白拿一代音频损失（两轮就能听出发闷），
    // 所以直接 `copy`。第 1 轮有变调/音量时就正常重编。
    if audio.is_empty() {
        for a in ["-c:a", "copy", "-movflags", "+faststart"] {
            args.push(a.into());
        }
    } else {
        for a in ["-c:a", "aac", "-b:a", "192k", "-movflags", "+faststart"] {
            args.push(a.into());
        }
    }
    // **清空来源元数据。** 标题/作者/注释/章节都可能是把两次发布关联起来的线索。
    // 容器级别的 `encoder` 会由 ffmpeg 重写成 Lavf（不再是源片那个编码器）。
    for a in ["-map_metadata", "-1", "-map_chapters", "-1"] {
        args.push(a.into());
    }
    // 创建时间随机到 2019~2025 之间的某个时刻，而不是“本次导出时间”
    args.push("-metadata".into());
    args.push(format!("creation_time={}", random_creation_time()));

    // 音频被垫了 0.1~0.2 秒静音后会比画面长，按画面收口，
    // 否则平台能看到结尾多出一段静音
    if o.audio_delay_ms > 0 || o.stereo_phase_ms > 0 {
        args.push("-shortest".into());
    }

    args.push(temp.to_string_lossy().to_string());

    let output = no_window(Command::new(ffmpeg).args(&args))
        .output()
        .map_err(|e| AppError::Process(format!("无法启动 ffmpeg：{e}")))?;

    if !output.status.success() {
        let _ = std::fs::remove_file(&temp);
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::Process(diagnose(
            &stderr,
            input,
            output.status.code(),
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

/// 规范 24/25：Lab a*/b* 对抗噪声 + 8x8 DCT 低频扰动。
///
/// **必须在主编码之后跑。** 主链路已经把裁切、旋转、缩放都烘进画面了，
/// 这一遍只需要按成片分辨率逐帧改像素；音频直接 `-c:a copy`，不再压一次。
///
/// 输入和输出可以是同一个文件：worker 先写 `.part.mp4`，成功后才由这里原子替换。
pub fn advanced_pass(
    python: &Path,
    worker: &Path,
    ffmpeg: &Path,
    ffprobe: &Path,
    input: &Path,
    out: &Path,
    opts: &TransformOptions,
) -> AppResult<i64> {
    let o = normalize(opts);
    if o.lab_noise <= EPS && o.dct_noise <= EPS {
        return Ok(std::fs::metadata(out).map(|m| m.len() as i64).unwrap_or(0));
    }
    if !worker.is_file() {
        return Err(AppError::Msg(format!(
            "找不到逐帧处理脚本：{}",
            worker.to_string_lossy()
        )));
    }

    let temp = temp_path(out)?;
    let args: Vec<String> = vec![
        worker.to_string_lossy().to_string(),
        "--input".into(),
        input.to_string_lossy().to_string(),
        "--output".into(),
        temp.to_string_lossy().to_string(),
        "--ffmpeg".into(),
        ffmpeg.to_string_lossy().to_string(),
        "--ffprobe".into(),
        ffprobe.to_string_lossy().to_string(),
        // **标称帧率必须由主链路给。** 成片带了 PTS 抖动之后，
        // ffprobe 的 r_frame_rate 会变成 240（帧间隔的最小公倍数），
        // 第二遍照它编码会补帧或把时间轴拖长。
        "--fps".into(),
        format!("{:.6}", o.fps),
        "--lab".into(),
        format!("{:.3}", o.lab_noise),
        "--dct".into(),
        format!("{:.3}", o.dct_noise),
        "--bitrate".into(),
        o.bitrate_bps.to_string(),
        "--crf".into(),
        o.crf.to_string(),
        "--gop".into(),
        o.gop.to_string(),
        "--bframes".into(),
        o.bframes.to_string(),
        "--scenecut".into(),
        o.scenecut.to_string(),
        "--pts-ms".into(),
        format!("{:.3}", o.pts_jitter_ms),
        "--pts-every".into(),
        o.pts_jitter_every.to_string(),
        "--pts-phase".into(),
        o.pts_jitter_phase.to_string(),
        "--creation-time".into(),
        random_creation_time(),
    ];

    let output = no_window(Command::new(python).args(&args))
        .output()
        .map_err(|e| AppError::Process(format!("无法启动逐帧处理：{e}")))?;
    if !output.status.success() {
        let _ = std::fs::remove_file(&temp);
        return Err(AppError::Process(format!(
            "Lab/DCT 逐帧处理失败：{}",
            tail(&String::from_utf8_lossy(&output.stderr))
        )));
    }
    if !temp.is_file() {
        let _ = std::fs::remove_file(&temp);
        return Err(AppError::Process(
            "逐帧处理报告成功但没有产出文件".into(),
        ));
    }

    if out.exists() {
        std::fs::remove_file(out)?;
    }
    std::fs::rename(&temp, out)?;
    Ok(std::fs::metadata(out).map(|m| m.len() as i64).unwrap_or(0))
}

/// 跑多轮：第 1 轮全量，之后每轮只重摇画面（规范 26）。返回最终文件大小。
///
/// `opts.rounds` 是**总轮数**（0 按 [`DEFAULT_ROUNDS`]）。每一轮都是
/// 「主编码 + Lab/DCT 逐帧 pass」，两轮之间用上一轮的产物当输入 ——
/// 所以第 2 轮看到的是已经裁过、已经出 720p 的画面，参数直接能用。
///
/// **中途失败不留半成品**：前几轮的产物已经写在 `out` 上，失败时删掉再报错，
/// 不然队列会把它当成「已完成」。
pub fn apply_rounds(
    ffmpeg: &Path,
    python: &Path,
    worker: &Path,
    ffprobe: &Path,
    input: &Path,
    out: &Path,
    opts: &TransformOptions,
    target: Option<(u32, u32)>,
) -> AppResult<i64> {
    let rounds = match opts.rounds {
        // 0 = 没填（单步工具页就是这样）—— 只跑一轮，和以前一致
        r if r <= 0 => 1,
        r => r.min(ROUNDS_MAX),
    };

    let mut round_opts = normalize(opts);
    let mut size = 0i64;
    for round in 0..rounds {
        if round > 0 {
            round_opts = next_round(&round_opts, round as usize);
            // 从第 2 轮起画面已经是 720p，不再需要精确缩放的目标尺寸
            // （拿着源尺寸去 scale 会把画面又撑回 1080，白跑一遍）
        }
        let scale_target = if round == 0 { target } else { None };
        let result = (|| -> AppResult<i64> {
            apply(ffmpeg, input, out, &round_opts, scale_target)?;
            // 24/25：Lab/DCT 逐帧 pass，输入输出同一个文件
            advanced_pass(python, worker, ffmpeg, ffprobe, out, out, &round_opts)
        })();
        match result {
            Ok(bytes) => size = bytes,
            Err(e) => {
                // 前几轮已经把文件写出来了；这一轮中途死的，产物是半成品
                let _ = std::fs::remove_file(out);
                if rounds > 1 {
                    return Err(AppError::Msg(format!(
                        "第 {}/{} 轮去重失败：{e}",
                        round + 1,
                        rounds
                    )));
                }
                return Err(e);
            }
        }
    }
    Ok(size)
}

/// 随机的「创建时间」。
///
/// 元数据里留着真实拍摄/导出时间，两批文件就能被时间轴对上 ——
/// 换成 2019~2025 之间的随机时刻，同一批文件也各不相同。
fn random_creation_time() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    let year = 2019 + (n % 7) as u32;
    let month = 1 + (n / 7 % 12) as u32;
    let day = 1 + (n / 84 % 28) as u32;
    let hour = (n / 2_352 % 24) as u32;
    let minute = (n / 56_448 % 60) as u32;
    let second = (n / 3_386_880 % 60) as u32;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// 把一次失败翻译成一句**用户能照着做**的话。
///
/// 真实撞到过：下载没下全的两集，去重时刷了上万行解码错误，日志里只有最后
/// 800 字符（全是编码器参数），用户根本看不出是下载的问题 —— 只能去猜。
///
/// 所以按原因分三支：素材坏、没有画面、参数不适用。前两种都指向
/// 「重新下载这一集」，第三种是工具自己的问题，不能让人白重下一次。
fn diagnose(stderr: &str, input: &Path, code: Option<i32>) -> String {
    let name = input
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();

    // 1. 源文件根本解不开（HEVC 的 NAL 单元错、AAC 帧错都算）。
    if let Some(line) = crate::media::decode_error_line(stderr) {
        return format!(
            "源文件坏了，解不出内容（{name}）—— 多半是下载没下全或下载到的就是不完整的那一版，重新下载这一集再跑。\n\
             ffmpeg：{}",
            crate::media::strip_pointer(line)
        );
    }

    // 2. 参数/滤镜问题 —— 这时候让用户重下就是白费力气。
    //
    // **必须排在「一帧都没编出来」之前**：滤镜挂了也会报一句
    // "Nothing was written into output file"，拿它当素材问题就错怪了源文件。
    if stderr.contains("Invalid too big or non positive size")
        || stderr.contains("Error reinitializing filters")
        || stderr.contains("Could not open encoder before EOF")
    {
        return format!(
            "处理参数在这一集上用不了（{name}）：{}",
            key_lines(stderr)
        );
    }

    // 3. 一帧都没编出来，但解码器没报具体错。
    if stderr.contains("Output file is empty, nothing was encoded")
        || stderr.contains("Nothing was written into output file")
    {
        return format!(
            "源文件没有可用画面（{name}）—— 一帧都没解出来，重新下载这一集再跑。\n\
             ffmpeg：{}",
            key_lines(stderr)
        );
    }

    format!(
        "处理失败（退出码 {}）：{}",
        code.unwrap_or(-1),
        key_lines(stderr)
    )
}

/// 从 stderr 里挑几条有信息量的错误行。
///
/// 坏素材会刷**上万行**（每帧一条）。原来把最后 800 字符塞进日志，
/// 用户看到的全是编码器参数 —— 既不知道哪坏了，也不知道该干什么。
fn key_lines(stderr: &str) -> String {
    const NOISY: [&str; 5] = ["rror", "Invalid", "failed", "Failed", "not allocated"];
    let mut picked: Vec<String> = Vec::new();
    for raw in stderr.lines() {
        let line = raw.trim();
        if line.is_empty() || !NOISY.iter().any(|k| line.contains(k)) {
            continue;
        }
        let line = crate::media::strip_pointer(line);
        if !picked.contains(&line) {
            picked.push(line);
        }
        if picked.len() >= 4 {
            break;
        }
    }
    if picked.is_empty() {
        return tail(stderr);
    }
    picked.join("\n")
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

    /// 新增的四项必须真的接进滤镜串，且单独开一项就不能算“什么都没做”。
    #[test]
    fn advanced_items_are_wired_and_not_noops() {
        let mut o = opts();
        o.high_noise = 0.003;
        assert!(!is_noop(&o));
        o.stereo_width = 1.02;
        assert!(!is_noop(&o));
        let af = build_audio_filter(&o);
        assert!(
            af.contains("sin(2*PI*(16000+4000*random(1))*t)"),
            "高频噪声没接上：{af}"
        );
        assert!(af.contains("extrastereo=m=1.020"), "立体声宽度没接上：{af}");

        o.stereo_width = 1.0;
        assert!(!build_audio_filter(&o).contains("extrastereo"));

        let mut v = opts();
        v.pts_jitter_ms = 4.0;
        v.pts_jitter_every = 6;
        v.pts_jitter_phase = 2;
        assert!(!is_noop(&v));
        let filter = vf(&v, None);
        assert!(
            filter.contains("settb=1/90000,setpts=PTS+0.0040/TB*eq(mod(N\\,6)\\,2)"),
            "PTS 微扰没接上：{filter}"
        );
        assert!(filter.ends_with("eq(mod(N\\,6)\\,2)"), "PTS 微扰必须在最后：{filter}");
    }

    /// 坏素材的诊断必须指向「重新下载这一集」。
    ///
    /// 真实撞到过：两集下载没下全，去重时刷了上万行解码错误，
    /// 日志里只有最后 800 字符（全是编码器参数）—— 用户根本看不出是下载的问题。
    #[test]
    fn corrupt_source_is_diagnosed_as_redownload() {
        let stderr = "[aac @ 000002635632b5c0] channel element 3.11 is not allocated\n\
[hevc @ 00000263563cd9c0] Invalid NAL unit size (1746351624 > 7589).\n\
[hevc @ 00000263563cd9c0] Error splitting the input into NAL units.\n\
frame=    0 fps=0.0 q=0.0 Lsize=       0KiB time=N/A bitrate=N/A speed=N/A\n\
Conversion failed!";

        let msg = diagnose(stderr, Path::new("D:/源/第20集.mp4"), Some(69));
        assert!(msg.contains("重新下载"), "{msg}");
        assert!(msg.contains("第20集.mp4"), "要能看出是哪一集：{msg}");
        // 上万行不能原样塞进来
        assert!(msg.lines().count() <= 3, "{msg}");
        // 对象地址每次都不同，要抹掉
        assert!(!msg.contains("000002635632b5c0"), "{msg}");
    }

    /// 参数/滤镜不行的时候不能甩锅给素材 —— 让人白重下一次下载。
    #[test]
    fn filter_failure_does_not_blame_the_source() {
        let stderr = "[vf#0:0 @ 0000024293f2ae40] Invalid too big or non positive size for width '774'\n\
[out#0/mp4 @ 0000024293f3c800] Nothing was written into output file, because at least one of its streams received no packets.";

        let msg = diagnose(stderr, Path::new("D:/源/第08集.mp4"), Some(69));
        assert!(!msg.contains("重新下载"), "{msg}");
        assert!(msg.contains("参数"), "{msg}");
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
        assert!(d.contains("变速 105.0%"), "{d}");
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
        // 噪点是规范里写死的 4，不随强度变 —— 强档之下只能更少
        assert_eq!(b.noise, 4, "强档噪点必须就是规范里的 4");
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
            bitrate_bps: 0,
            crf: 0,
            pitch: 99.0,
            drop_every: 99999,
            drop_phase: 99999,
            sweep: 99.0,
            rotate_deg: 99.0,
            patch_alpha: 99.0,
            patch_corner: 9,
            audio_delay_ms: 9999,
            stereo_phase_ms: 9999,
            white_noise: 99.0,
            high_noise: 99.0,
            stereo_width: 99.0,
            pts_jitter_ms: 99.0,
            pts_jitter_every: 9999,
            pts_jitter_phase: 9999,
            scenecut: 9999,
            lab_noise: 99.0,
            dct_noise: 99.0,
            rounds: 9999,
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
        assert_eq!(n.pitch, PITCH_MAX);
        assert_eq!(n.drop_every, DROP_EVERY_MAX);
        assert_eq!(n.drop_phase, 99999 % DROP_EVERY_MAX);
        assert_eq!(n.sweep, SWEEP_MAX);
        assert_eq!(n.high_noise, HIGH_NOISE_MAX);
        assert_eq!(n.stereo_width, STEREO_WIDTH_MAX);
        assert_eq!(n.pts_jitter_ms, PTS_JITTER_MAX);
        assert_eq!(n.pts_jitter_every, PTS_EVERY_MAX);
        assert_eq!(n.pts_jitter_phase, 9999 % PTS_EVERY_MAX);
        // 场景阈值越界按默认 0（不启用）处理
        assert_eq!(n.scenecut, 0);
        assert_eq!(n.lab_noise, LAB_NOISE_MAX);
        assert_eq!(n.dct_noise, DCT_NOISE_MAX);
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
        o.pitch = f64::NAN;
        o.sweep = f64::INFINITY;
        let n = normalize(&o);
        assert_eq!(n.crop_ratio, 0.0);
        assert_eq!(n.scale_ratio, 1.0);
        assert_eq!(n.brightness, 0.0);
        assert_eq!(n.speed, 1.0);
        assert_eq!(n.volume, 1.0);
        assert_eq!(n.pitch, 1.0);
        assert_eq!(n.sweep, 0.0);
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

    /// 变调必须真的挪频谱，同时把时长原样拉回来 —— 音画同步靠这个。
    #[test]
    fn pitch_shifts_spectrum_and_keeps_duration() {
        let mut o = opts();
        o.pitch = 1.008;
        assert_eq!(
            build_audio_filter(&o),
            "aresample=48000,asetrate=48384,aresample=48000,atempo=0.9921"
        );

        // 和变速叠加时，atempo 要把变调带来的 1/pitch 吃回去
        o.speed = 1.05;
        let a = build_audio_filter(&o);
        assert!(a.ends_with("atempo=1.0417"), "{a}");
        assert!(vf(&o, None).ends_with("setpts=PTS/1.0500"));
    }

    /// 帧率必须写小数；整数化会把 29.97 抹成 29，帧率扰动就白做了。
    #[test]
    fn frame_rate_jitter_keeps_its_decimals() {
        let mut o = opts();
        o.fps = 29.97;
        assert_eq!(vf(&o, None), "fps=29.97");

        o.fps = 30.0;
        assert_eq!(vf(&o, None), "fps=30");
    }

    /// 抽帧之后必须再收一次帧率，否则时间轴会越来越短。
    #[test]
    fn dropped_frames_are_resampled_back_to_same_timeline() {
        let mut o = opts();
        o.fps = 30.0;
        o.drop_every = 300;
        o.drop_phase = 17;
        assert_eq!(
            vf(&o, None),
            r"fps=30,select=not(eq(mod(n\,300)\,17)),fps=30"
        );
    }

    /// 扫光要用时间线分段落，静态遮罩改不了逐帧纹理。
    #[test]
    fn sweep_is_a_time_based_overlay() {
        let mut o = opts();
        o.sweep = 0.03;
        let f = vf(&o, None);
        assert!(f.contains("drawbox=x=iw*(-0.150)"), "{f}");
        assert!(
            f.contains("enable='between(mod(t\\,6)\\,0.0\\,1.0)'"),
            "{f}"
        );
        assert!(f.contains("color=white@0.030"), "{f}");
        // 6 段轮流出现，才是「扫」而不是一个固定亮块
        assert_eq!(f.matches("drawbox=").count(), 6, "{f}");
    }

    /// 缩放一次就把短边拉回 720：不能先缩长边、再靠二次缩放补救
    /// （中间取整会让宽落到 718）。
    #[test]
    fn scale_never_goes_below_the_720_line() {
        let mut o = opts();
        o.max_height = 1280;

        // 竖屏：宽 = max(长边缩放后的宽, 720)，高自动保持比例
        assert_eq!(
            vf(&o, Some((1080, 1920))),
            "scale=trunc(max(720\\,iw*1280/ih)/2)*2:-2"
        );
        // 横屏：高 = max(长边缩放后的高, 720)
        assert_eq!(
            vf(&o, Some((1280, 536))),
            "scale=-2:trunc(max(720\\,ih*1280/iw)/2)*2"
        );

        assert_eq!(final_height(&o, Some((1280, 536))), 720);
        assert_eq!(final_height(&o, Some((1080, 1920))), 1280);
        assert_eq!(final_height(&o, Some((720, 1280))), 1280);
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
    fn preset_is_deterministic_and_follows_the_spec_windows() {
        // **同一个种子必须给出同一套参数** —— 否则同一次运行里
        // 每个文件的处理基准都在飘，出了事没法复现。
        let st = Strengths::from_seed(7);
        for index in 0..30 {
            let a = preset_for(Intensity::Strong, index, &st);
            let b = preset_for(Intensity::Strong, index, &st);
            assert_eq!(a.crop_ratio, b.crop_ratio, "同一序号必须完全一致");
            assert_eq!(a.speed, b.speed);
            assert_eq!(a.noise, b.noise);

            // 规范里写死的窗口
            assert!((0.045..=0.055).contains(&a.crop_ratio), "裁切 {}", a.crop_ratio);
            assert!((1.04..=1.06).contains(&a.speed), "变速 {}", a.speed);
            assert_eq!(a.noise, 4);
            assert!((0.5..=2.0).contains(&a.rotate_deg.abs()), "旋转 {}", a.rotate_deg);
            assert!((0.02..=0.05).contains(&a.sweep), "扫光 {}", a.sweep);
            assert!((0.03..0.05).contains(&a.patch_alpha), "贴角 {}", a.patch_alpha);
            assert!((150..=300).contains(&a.drop_every), "抽帧 {}", a.drop_every);
            assert!((100..=200).contains(&a.audio_delay_ms), "延迟 {}", a.audio_delay_ms);
            assert!((1..=3).contains(&a.stereo_phase_ms), "相位 {}", a.stereo_phase_ms);
            assert!((29.85..=30.15).contains(&a.fps), "帧率 {}", a.fps);
            // 缩放是 1 ± 0.4%~0.6%，所以窗口是 0.994~1.006
            assert!((0.994..=1.006).contains(&a.scale_ratio), "缩放 {}", a.scale_ratio);
            assert!((0.0049..=0.0101).contains(&(a.pitch - 1.0).abs()), "变调 {}", a.pitch);
            assert!((1.04..=1.06).contains(&a.volume), "音量 {}", a.volume);
            // 调色：亮度 +1%~+3%，对比/饱和 -1%~-4% / -3%~-1%
            assert!((0.01..=0.03).contains(&a.brightness), "亮度 {}", a.brightness);
            assert!((0.96..=0.98).contains(&a.contrast), "对比 {}", a.contrast);
            assert!((0.97..=0.99).contains(&a.saturation), "饱和 {}", a.saturation);
            assert!((0.0001..=WHITE_NOISE_MAX).contains(&a.white_noise), "白噪 {}", a.white_noise);
            assert!((0.002..=0.005).contains(&a.high_noise), "高频噪 {}", a.high_noise);
            assert!((0.97..=1.03).contains(&a.stereo_width), "立体声宽度 {}", a.stereo_width);
            assert!((2.0..=5.0).contains(&a.pts_jitter_ms), "PTS 抖动 {}", a.pts_jitter_ms);
            assert!((4..=8).contains(&a.pts_jitter_every), "PTS 间隔 {}", a.pts_jitter_every);
            assert!(a.pts_jitter_phase < a.pts_jitter_every);
            assert!((20..=60).contains(&a.scenecut), "场景阈值 {}", a.scenecut);
            assert!((2.0..=3.0).contains(&a.lab_noise), "Lab 噪声 {}", a.lab_noise);
            assert!((2.5..=4.0).contains(&a.dct_noise), "DCT 扰动 {}", a.dct_noise);
            assert!((56..=72).contains(&a.gop), "GOP {}", a.gop);
            assert!((0..=3).contains(&a.bframes), "B 帧 {}", a.bframes);
            assert!((19..=23).contains(&a.crf), "CRF {}", a.crf);
        }
    }

    /// 报告（队列日志）必须能列出规范要求的实际取值：
    /// 裁切 / 旋转 / 变调 / 帧率 / 关键帧策略。
    #[test]
    fn preset_description_lists_the_report_fields() {
        let st = Strengths::from_seed(7);
        let o = preset_for(Intensity::Strong, 3, &st);
        let d = describe(&o);
        for key in [
            "四边裁",
            "旋转",
            "变调",
            "帧率",
            "GOP",
            "场景阈值",
            "PTS 抖动",
            "高频噪",
            "立体声宽度",
            "Lab噪声",
            "DCT扰动",
        ] {
            assert!(d.contains(key), "报告缺 {key}：{d}");
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
    fn stronger_levels_scale_the_same_recipe_up() {
        let st = Strengths::from_seed(42);
        for index in 0..10 {
            let light = normalize(&preset_for(Intensity::Light, index, &st));
            let medium = normalize(&preset_for(Intensity::Medium, index, &st));
            let strong = normalize(&preset_for(Intensity::Strong, index, &st));
            // 同一套手段，幅度递减 —— 不能轻档就缺项
            assert!(light.crop_ratio < medium.crop_ratio);
            assert!(medium.crop_ratio < strong.crop_ratio);
            assert!(light.noise <= medium.noise && medium.noise <= strong.noise);
            assert!(strong.crop_ratio <= CROP_MAX);
            assert!(strong.speed >= SPEED_MIN && strong.speed <= SPEED_MAX);
            assert!(strong.volume >= VOLUME_MIN && strong.volume <= VOLUME_MAX);
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

    /// 旋转的放大系数**必须用 |sinθ|**。
    ///
    /// 踩过：负角度时 sin 是负数，cover 被 max(1.0) 吞成 1.0，
    /// 只放大 1.003 倍 —— 再碰上缩小的 scale_ratio，居中裁凑不够
    /// 1080×1920，ffmpeg 报 `Invalid too big or non positive size`。
    #[test]
    fn negative_rotation_gets_the_same_overscan() {
        let mut pos = opts();
        pos.rotate_deg = 1.85;
        pos.scale_ratio = 0.995;
        let mut neg = opts();
        neg.rotate_deg = -1.85;
        neg.scale_ratio = 0.995;

        let fp = vf(&pos, Some((1080, 1920)));
        let fn_ = vf(&neg, Some((1080, 1920)));
        // 两个方向除了角度符号，放大/裁切应当一模一样
        assert_eq!(fp.replace("1.8500*PI", "-1.8500*PI"), fn_, "{fp} / {fn_}");
        // 1.85° 竖屏需要约 6.5% 的放大（再补回 0.995 的缩小）
        assert!(fn_.contains("scale=trunc(iw*1.06"), "放大不够：{fn_}");
        // 旋完之后必须居中裁回目标尺寸，否则四角会露黑
        assert!(fn_.contains("crop=1080:1920:(iw-1080)/2:(ih-1920)/2"), "{fn_}");
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
            bitrate_bps: 0,
            crf: 0,
            pitch: 1.0,
            drop_every: 0,
            drop_phase: 0,
            sweep: 0.0,
            rotate_deg: 0.0,
            patch_alpha: 0.0,
            patch_corner: 0,
            audio_delay_ms: 0,
            stereo_phase_ms: 0,
            white_noise: 0.0,
            high_noise: 0.0,
            stereo_width: 1.0,
            pts_jitter_ms: 0.0,
            pts_jitter_every: 0,
            pts_jitter_phase: 0,
            scenecut: 0,
            lab_noise: 0.0,
            dct_noise: 0.0,
            rounds: 0,
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

    /// 去重预设的完整链路：变调 + 抽帧 + 扫光 + 720p 必须能真的跑完，
    /// 而且**时长不能变** —— 抽帧和变调都很容易把时长搞短。
    #[test]
    fn end_to_end_dedup_preset_keeps_duration_and_720p() {
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

        let dir = std::env::temp_dir().join("dramadist_dedup_preset_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let input = dir.join("in.mp4");
        let status = no_window(
            Command::new(&ffmpeg).args([
                "-hide_banner", "-loglevel", "error", "-y",
                "-f", "lavfi", "-i", "testsrc=size=1080x1920:rate=30",
                "-f", "lavfi", "-i", "sine=frequency=440",
                "-t", "4", "-c:v", "libx264", "-preset", "ultrafast",
                "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
            ]),
        )
        .arg(&input)
        .status()
        .expect("无法启动 ffmpeg");
        assert!(status.success(), "生成测试素材失败");

        let st = Strengths::from_seed(42);
        let mut o = preset_for(Intensity::Strong, 3, &st);
        // 预设里这几项必须都开着 —— 只调音量不算音频处理
        assert!((o.pitch - 1.0).abs() >= 0.0049, "变调没用上：{}", o.pitch);
        assert!(o.drop_every >= 150);
        assert!(o.sweep > 0.0);
        // 4 秒素材跑不到 240 帧，缩到 60 帧以覆盖抽帧链路
        o.drop_every = 60;
        o.drop_phase = 17;

        let out = dir.join("out.mp4");
        apply(&ffmpeg, &input, &out, &o, Some((1080, 1920))).expect("去重预设跑失败");

        let before = crate::media::probe(&ffprobe, &input).unwrap();
        let after = crate::media::probe(&ffprobe, &out).unwrap();
        // 竖屏 1080×1920 → 宽固定 720（短边），高由 -2 按比例偶数对齐，
        // 因为 crop / scale_ratio 的取整，可能是 1280~1284 —— 都算达标。
        assert_eq!(after.width, 720);
        assert!(
            after.height >= 1280,
            "短边 720 时高度应不少于 1280，实际 {}×{}",
            after.width,
            after.height
        );
        assert!(
            (after.duration_ms as f64 - before.duration_ms as f64).abs() < 600.0,
            "抽帧/变调不该改时长：{}ms → {}ms",
            before.duration_ms,
            after.duration_ms
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 逐帧 pass 的硬要求：**一帧不能少、音频一个字节不能变**。
    ///
    /// 它跑在第二遍编码上，`-shortest` 或 vsync 都可能悄悄丢/补帧；
    /// 一旦发生，多轮串联会越跑越短。
    #[test]
    fn advanced_lab_dct_pass_keeps_frames_and_audio() {
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
        let Ok(python) = std::env::var("DRAMADIST_TEST_PYTHON") else {
            eprintln!("跳过：未设置 DRAMADIST_TEST_PYTHON");
            return;
        };
        let python = PathBuf::from(python);
        let worker = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources/advanced_noise_worker.py");
        if !python.is_file() || !worker.is_file() {
            eprintln!("跳过：python 或 worker 不存在");
            return;
        }

        fn frame_count(ffprobe: &Path, file: &Path) -> i64 {
            let out = no_window(Command::new(ffprobe).args([
                "-v", "error", "-select_streams", "v:0", "-count_packets",
                "-show_entries", "stream=nb_read_packets", "-of", "default=nw=1:nk=1",
            ]))
            .arg(file)
            .output()
            .expect("ffprobe 跑不了");
            String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(-1)
        }
        fn audio_md5(ffmpeg: &Path, file: &Path) -> String {
            let out = no_window(Command::new(ffmpeg).args([
                "-v", "error", "-map", "0:a:0", "-c", "copy", "-f", "md5", "-",
            ]))
            .arg("-i")
            .arg(file)
            .output()
            .expect("ffmpeg 跑不了");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        let dir = std::env::temp_dir().join("dramadist_advanced_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.mp4");
        let status = no_window(Command::new(&ffmpeg).args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "testsrc=size=320x240:rate=24",
            "-f", "lavfi", "-i", "sine=frequency=440",
            "-t", "2.5", "-c:v", "libx264", "-preset", "ultrafast",
            "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
        ]))
        .arg(&input)
        .status()
        .expect("无法启动 ffmpeg");
        assert!(status.success(), "生成测试素材失败");

        let o = TransformOptions {
            crop_ratio: 0.02,
            brightness: 0.01,
            fps: 24.0,
            gop: 48,
            bframes: 2,
            bitrate_bps: 1_500_000,
            pts_jitter_ms: 3.0,
            pts_jitter_every: 5,
            pts_jitter_phase: 1,
            lab_noise: 2.5,
            dct_noise: 3.0,
            ..TransformOptions::default()
        };

        let out = dir.join("out.mp4");
        apply(&ffmpeg, &input, &out, &o, Some((320, 240))).expect("主编码失败");
        let frames_before = frame_count(&ffprobe, &out);
        let md5_before = audio_md5(&ffmpeg, &out);
        let probe_before = crate::media::probe(&ffprobe, &out).unwrap();

        let size = advanced_pass(&python, &worker, &ffmpeg, &ffprobe, &out, &out, &o)
            .expect("Lab/DCT 逐帧处理失败");
        assert!(size > 0);

        let frames_after = frame_count(&ffprobe, &out);
        let actual_after = crate::media::probe(&ffprobe, &out).unwrap();
        assert_eq!(frames_after, frames_before, "逐帧处理不能丢帧/补帧");
        assert_eq!(audio_md5(&ffmpeg, &out), md5_before, "音频必须原样复制");
        assert_eq!(
            (actual_after.width, actual_after.height),
            (probe_before.width, probe_before.height)
        );
        assert!(
            (actual_after.duration_ms - probe_before.duration_ms).abs() < 120,
            "时长不该变：{}ms → {}ms",
            probe_before.duration_ms,
            actual_after.duration_ms
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 规范 26 的 A 方案：第 2 轮只重摇画面，**时间轴和音频一个都不能动**。
    ///
    /// 这条是纯参数校验，不用 ffmpeg —— 逻辑错了在这里就能挡住，
    /// 而参数错了在成片上只表现为「时长慢慢漂」或「声音发闷」，跑完才发现。
    #[test]
    fn later_rounds_lock_timeline_and_audio() {
        let st = Strengths::from_seed(7);
        let first = preset_for(Intensity::Strong, 5, &st);
        // 预设两轮：第 1 轮必须是全量的，否则「只摇画面」无从谈起
        assert_eq!(first.rounds, DEFAULT_ROUNDS);
        assert!((first.speed - 1.0).abs() > 1e-9, "第 1 轮应该变速");

        let second = next_round(&first, 1);
        let lock = [
            ("speed", second.speed, 1.0),
            ("pitch", second.pitch, 1.0),
            ("volume", second.volume, 1.0),
            ("stereo_width", second.stereo_width, 1.0),
            ("white_noise", second.white_noise, 0.0),
            ("high_noise", second.high_noise, 0.0),
        ];
        for (name, got, want) in lock {
            assert!(
                (got - want).abs() < 1e-9,
                "第 2 轮的 {name} 必须锁在 {want}，实际 {got}"
            );
        }
        assert_eq!(second.audio_delay_ms, 0, "第 2 轮不能再垫静音");
        assert_eq!(second.stereo_phase_ms, 0, "第 2 轮不能再动相位");
        assert_eq!(second.fps, first.fps, "帧率不能两轮各摇一次");
        assert!(second.header_label.is_none(), "片头标识只画一次");
        assert!(second.corner_label.is_none());

        // 画面必须真的变了，否则第二轮白跑
        assert!(
            second.crop_ratio > 0.0 && second.crop_ratio <= CROP_LATER_ROUND_MAX,
            "第 2 轮裁切要封顶在 {CROP_LATER_ROUND_MAX}，实际 {}",
            second.crop_ratio
        );
        assert!(second.rotate_deg.abs() <= ROTATE_LATER_ROUND_MAX);
        assert_ne!(
            second.rotate_deg.signum(),
            first.rotate_deg.signum(),
            "两轮旋转方向应相反"
        );
        assert!(second.noise >= first.noise, "噪点只增不减");
        assert!(second.gop != first.gop || second.bframes != first.bframes);

        // 第 3 轮仍然锁死时间轴 —— 三轮累计也不能把时长压出去
        let third = next_round(&second, 2);
        assert_eq!((third.speed, third.pitch, third.volume), (1.0, 1.0, 1.0));
        assert_eq!(third.fps, first.fps);
        assert_ne!(third.gop, second.gop, "每轮要摇到不一样的参数");
    }

    /// `apply_rounds` 的硬要求：**两轮跑完时长基本不变、音频也不再被压**。
    ///
    /// 第 1 轮是变速 + 变调 + 压 AAC；第 2 轮只重摇画面，所以第 2 轮前后的
    /// 音频应逐字节相同、时长不超 1 帧。这条如果挂了，说明锁参数没生效。
    #[test]
    fn apply_rounds_keeps_duration_and_audio_after_first_round() {
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
        let Ok(python) = std::env::var("DRAMADIST_TEST_PYTHON") else {
            eprintln!("跳过：未设置 DRAMADIST_TEST_PYTHON");
            return;
        };
        let python = PathBuf::from(python);
        let worker = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources/advanced_noise_worker.py");
        if !python.is_file() || !worker.is_file() {
            eprintln!("跳过：python 或 worker 不存在");
            return;
        }

        fn audio_md5(ffmpeg: &Path, file: &Path) -> String {
            let out = no_window(Command::new(ffmpeg).args([
                "-v", "error", "-map", "0:a:0", "-c", "copy", "-f", "md5", "-",
            ]))
            .arg("-i")
            .arg(file)
            .output()
            .expect("ffmpeg 跑不了");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        let dir = std::env::temp_dir().join("dramadist_rounds_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("in.mp4");
        let status = no_window(Command::new(&ffmpeg).args([
            "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "testsrc=size=320x240:rate=24",
            "-f", "lavfi", "-i", "sine=frequency=440",
            "-t", "3", "-c:v", "libx264", "-preset", "ultrafast",
            "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
        ]))
        .arg(&input)
        .status()
        .expect("无法启动 ffmpeg");
        assert!(status.success(), "生成测试素材失败");

        // 只开画面类扰动，把音频类关掉 —— 这样「第 2 轮锁死」才可验证
        let round1 = TransformOptions {
            crop_ratio: 0.02,
            fps: 24.0,
            gop: 48,
            bitrate_bps: 1_500_000,
            lab_noise: 2.5,
            dct_noise: 3.0,
            rounds: 2,
            ..TransformOptions::default()
        };

        let out = dir.join("out.mp4");
        apply_rounds(
            &ffmpeg,
            &python,
            &worker,
            &ffprobe,
            &input,
            &out,
            &round1,
            Some((320, 240)),
        )
        .expect("两轮去重跑失败");
        assert!(out.is_file());

        let after = crate::media::probe(&ffprobe, &out).unwrap();
        let before = crate::media::probe(&ffprobe, &input).unwrap();
        // 第 2 轮拿不到目标尺寸，`-2` 对齐会取整几像素 —— 不要求分毫不动，
        // 但绝不能像「拿源尺寸再 scale 一遍」那样被撑回 1080
        assert!(
            (after.width as i64 - 320).abs() <= 4 && (after.height as i64 - 240).abs() <= 4,
            "两轮之后尺寸应基本不变，实际 {}×{}",
            after.width,
            after.height
        );
        assert!(
            (after.duration_ms as f64 - before.duration_ms as f64).abs() < 200.0,
            "两轮画面重摇不该改时长：{}ms → {}ms",
            before.duration_ms,
            after.duration_ms
        );

        // 再单独跑一遍「第 1 轮 → 第 2 轮」，对比第 2 轮前后的音频指纹
        let single = dir.join("single.mp4");
        let mut one = round1.clone();
        one.rounds = 1;
        apply_rounds(
            &ffmpeg,
            &python,
            &worker,
            &ffprobe,
            &input,
            &single,
            &one,
            Some((320, 240)),
        )
        .expect("单轮去重跑失败");
        let md5_before_round2 = audio_md5(&ffmpeg, &single);
        let second = dir.join("second.mp4");
        let o2 = next_round(&normalize(&round1), 1);
        apply(&ffmpeg, &single, &second, &o2, None).expect("第 2 轮主编码失败");
        assert_eq!(
            audio_md5(&ffmpeg, &second),
            md5_before_round2,
            "第 2 轮必须原样复制音频，不能重新压 AAC"
        );

        // 失败时不能留半成品
        let mut bad = round1.clone();
        bad.rounds = 3;
        bad.bitrate_bps = 1;
        let dead = dir.join("dead.mp4");
        if apply_rounds(
            &ffmpeg,
            &python,
            &worker,
            &ffprobe,
            &dir.join("missing.mp4"),
            &dead,
            &bad,
            None,
        )
        .is_err()
        {
            assert!(!dead.exists(), "失败时不能留下半成品");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}