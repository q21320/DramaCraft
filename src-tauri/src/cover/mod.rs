//! 封面生成：用原封面 + 新剧名，合成一张能直接上传的竖版封面。
//!
//! ## 为什么需要它
//!
//! 素材包自带的封面**已经烧上了原剧名**（实测那部片子是金色的书法字，
//! 横在画面顶部）。一旦改过剧名、或者要给不同账号配不同标题，
//! 原封面就不能用了。
//!
//! ## 为什么不做 AI 生图
//!
//! 原软件走的是火山 Seedream「图生图」：去掉旧字、写入新剧名。
//! 那需要云服务 Key，而且产出的图属于 AI 生成内容，**要按标识办法打标**。
//!
//! 这里走**本地合成**：不擦旧字，而是**用一个标题条把顶部那块盖住**，
//! 再把新剧名写上去。离线可用、秒出、不产生 AI 内容、不需要任何 Key。
//! 代价是盖住的那部分画面（通常只是天空/背景）会损失一点。
//!
//! 想用 AI 生图的话，那应该走 `cloud` 那条路，并且记得勾片头标识。

use crate::error::{AppError, AppResult};
use crate::models::{CoverRequest, LabelPosition};
use crate::proc::no_window;
use std::path::Path;
use std::process::Command;

/// 标题条占画面高度的比例（默认）。
///
/// 22% 是照着实测的那张封面定的：旧剧名正好占顶部这块，
/// 盖住它就不需要擦字了。
pub const DEFAULT_BAND_RATIO: f64 = 0.22;

/// 标题最多分几行。再多行就得缩得很小，不如让用户自己改短。
pub const MAX_TITLE_LINES: usize = 2;

/// 把一个长剧名拆成一行或两行。
///
/// 优先在标点处断（观众读起来自然），没有标点就从中间断。
/// 拆成两行能让字号大一倍 —— 封面在信息流里只有指甲盖那么大，
/// 字小了等于没有。
pub fn wrap_title(title: &str, per_line: usize) -> Vec<String> {
    let text = title.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= per_line {
        return vec![text.to_string()];
    }

    // 找靠中间、且靠后的一个标点当作断点
    let mid = chars.len() / 2;
    let punct = ['，', '。', '！', '？', '、', ',', '.', '!', '?', ' ', '·'];
    let mut best: Option<usize> = None;
    for (i, c) in chars.iter().enumerate() {
        if punct.contains(c) && i + 1 >= mid / 2 && i + 1 < chars.len() {
            // 断在标点之后
            best = Some(i + 1);
            if i + 1 >= mid {
                break;
            }
        }
    }
    let cut = best.unwrap_or(mid);

    let first: String = chars[..cut].iter().collect();
    let second: String = chars[cut..].iter().collect();
    let mut lines = vec![first.trim().to_string()];
    if !second.trim().is_empty() {
        lines.push(second.trim().to_string());
    }
    lines.truncate(MAX_TITLE_LINES);
    lines.retain(|l| !l.is_empty());
    lines
}

/// 标题字号：既不能超出宽度，也不能太小。
///
/// 中文字宽约等于字号，所以 `宽度 × 0.92 / 每行字数` 就是能塞下的最大字号。
fn title_font_size(width: i64, height: i64, per_line: usize, lines: usize) -> i64 {
    let by_width = (width as f64 * 0.92) / per_line.max(1) as f64;
    let by_height = (height as f64 * DEFAULT_BAND_RATIO * 0.72) / lines.max(1) as f64;
    let size = by_width.min(by_height).min(height as f64 * 0.11);
    (size.round() as i64).clamp(20, 260)
}

/// 构造合成封面的滤镜串。
///
/// 顺序：裁到目标尺寸 → 压标题条 → 写字。
/// 文字**必须最后**，否则会被裁切或压暗。
pub fn build_cover_filter(req: &CoverRequest) -> AppResult<String> {
    let width = req.width.clamp(240, 4096);
    let height = req.height.clamp(240, 4096);
    let title = crate::transform::sanitize_label_text(&req.title);
    if title.is_empty() {
        return Err(AppError::Msg("封面标题是空的".into()));
    }

    let font = crate::transform::find_cjk_font().ok_or_else(|| {
        AppError::Msg(
            "找不到中文字体，画不出封面标题。可以用环境变量 DRAMADIST_LABEL_FONT 指定一个 .ttf/.ttc"
                .into(),
        )
    })?;

    // 每行放几个字：竖版封面一行 6~8 个字最好读
    let per_line = 7usize;
    let lines = wrap_title(&title, per_line);
    let size = title_font_size(width, height, per_line, lines.len());
    let band = ((height as f64 * DEFAULT_BAND_RATIO).round() as i64).max(size * lines.len() as i64 / 2);

    let mut parts: Vec<String> = vec![
        // 铺满目标尺寸：先按比例放大到盖住，再居中裁掉多余
        format!("scale={width}:{height}:force_original_aspect_ratio=increase"),
        format!("crop={width}:{height}"),
    ];

    // 标题条：用半透明黑压一层。
    // 不透明的话像贴了张黑纸；太透又盖不住旧字，0.62 是试出来的折中。
    let band_y = match req.position {
        LabelPosition::BottomLeft | LabelPosition::BottomRight | LabelPosition::BottomCenter => {
            height - band
        }
        _ => 0,
    };
    parts.push(format!(
        "drawbox=x=0:y={band_y}:w={width}:h={band}:color=black@0.62:t=fill"
    ));

    // 标题文字：多行就多段 drawtext，逐行往下排
    let line_gap = (size as f64 * 1.18).round() as i64;
    let block_h = line_gap * (lines.len() as i64 - 1) + size;
    let block_top = band_y + (band - block_h) / 2;
    for (i, line) in lines.iter().enumerate() {
        let y = block_top + line_gap * i as i64;
        parts.push(format!(
            "drawtext=fontfile={}:text={}:fontcolor=white:fontsize={}:\
             borderw={}:bordercolor=black@0.85:x=(w-text_w)/2:y={}",
            crate::transform::escape_filter_path(&font),
            line,
            size,
            (size / 12).max(2),
            y
        ));
    }

    Ok(parts.join(","))
}

/// 生成封面。
pub fn render(ffmpeg: &Path, req: &CoverRequest, out: &Path) -> AppResult<u64> {
    let poster = Path::new(req.poster.trim());
    if !poster.is_file() {
        return Err(AppError::Msg(format!(
            "找不到原封面：{}",
            poster.to_string_lossy()
        )));
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::Msg(format!("建不了输出目录：{e}")))?;
    }

    let filter = build_cover_filter(req)?;
    let mut cmd = Command::new(ffmpeg);
    no_window(&mut cmd);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(poster)
        .arg("-vf")
        .arg(&filter)
        // 封面是单帧图片：只取一帧，避免把 jpg 当成视频无限循环
        .args(["-frames:v", "1", "-q:v", "2"])
        .arg(out);

    let output = cmd
        .output()
        .map_err(|e| AppError::Msg(format!("启动 ffmpeg 失败：{e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::Msg(format!(
            "生成封面失败：{}",
            crate::proc::tail(err.trim())
        )));
    }
    if !out.is_file() {
        return Err(AppError::Msg("ffmpeg 说成功了但没产出文件".into()));
    }
    Ok(std::fs::metadata(out).map(|m| m.len()).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(title: &str) -> CoverRequest {
        CoverRequest {
            poster: "E:/x/poster.jpg".into(),
            title: title.into(),
            out_path: "E:/x/cover.jpg".into(),
            width: 1080,
            height: 1440,
            position: LabelPosition::TopCenter,
            out_root: String::new(),
        }
    }

    #[test]
    fn short_title_stays_on_one_line() {
        assert_eq!(wrap_title("新剧名", 7), vec!["新剧名"]);
        assert_eq!(wrap_title("七字剧名刚刚好", 7), vec!["七字剧名刚刚好"]);
    }

    #[test]
    fn long_title_splits_at_punctuation() {
        // 真实剧名就是这个长度
        let got = wrap_title("弟弟结婚，我的新车竟成了贺礼", 7);
        assert_eq!(got.len(), 2, "{got:?}");
        assert!(got[0].ends_with('，'), "{got:?}");
        assert!(got.iter().all(|l| !l.is_empty()));
    }

    #[test]
    fn long_title_without_punctuation_splits_in_the_middle() {
        let got = wrap_title("一二三四五六七八九十", 7);
        assert_eq!(got.len(), 2, "{got:?}");
        assert_eq!(got[0].chars().count() + got[1].chars().count(), 10);
    }

    #[test]
    fn empty_title_yields_nothing() {
        assert!(wrap_title("   ", 7).is_empty());
    }

    #[test]
    fn font_size_shrinks_as_the_title_grows() {
        let short = title_font_size(1080, 1440, 7, 1);
        let long = title_font_size(1080, 1440, 7, 2);
        assert!(short > long, "两行的字号应当比一行小：{short} vs {long}");
        // 不能小到看不清
        assert!(long >= 20);
    }

    #[test]
    fn filter_covers_then_writes_text_last() {
        if crate::transform::find_cjk_font().is_none() {
            eprintln!("跳过：本机没有中文字体");
            return;
        }
        let f = build_cover_filter(&req("弟弟结婚，我的新车竟成了贺礼")).unwrap();
        let scale_at = f.find("scale=").unwrap();
        let box_at = f.find("drawbox=").expect("要有标题条");
        let text_at = f.find("drawtext=").expect("要有标题");
        assert!(scale_at < box_at, "标题条要压在裁切之后：{f}");
        assert!(
            box_at < text_at,
            "文字必须最后写，否则会被标题条压住：{f}"
        );
        // 目标尺寸要精确
        assert!(f.contains("crop=1080:1440"), "{f}");
    }

    #[test]
    fn bottom_position_moves_the_band_down() {
        if crate::transform::find_cjk_font().is_none() {
            return;
        }
        let mut r = req("剧名");
        r.position = LabelPosition::BottomCenter;
        let f = build_cover_filter(&r).unwrap();
        // 底部时 y 不再是 0（注意别用 `box` 当变量名，那是 Rust 保留字）
        let bar = f.split(',').find(|s| s.starts_with("drawbox=")).unwrap();
        assert!(!bar.contains("y=0:"), "底部位置的标题条不该贴在顶边：{bar}");
    }

    #[test]
    fn empty_title_is_rejected() {
        let err = build_cover_filter(&req("   ")).unwrap_err().to_string();
        assert!(err.contains("标题是空的"), "{err}");
    }

    #[test]
    fn title_text_is_sanitized() {
        if crate::transform::find_cjk_font().is_none() {
            return;
        }
        // 引号冒号这些在滤镜里有特殊含义，清洗掉而不是猜转义
        let f = build_cover_filter(&req("剧名:带'引号'")).unwrap();
        assert!(!f.contains("'"), "{f}");
        assert!(f.contains("剧名"), "{f}");
    }
}
