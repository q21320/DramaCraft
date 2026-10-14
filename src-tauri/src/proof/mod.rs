//! 证明材料生成：把模板图上的空位填上实际数据，产出可直接提交的证明图。
//!
//! 场景：短剧推广报备、结算、版权沟通时，对方常要一张「这部剧做了多少集、
//! 多长时间、投了多少」的图。手填既慢又容易错；模板 + 字段坐标配一次就能反复用。
//!
//! ## 实现方式
//!
//! 用 ffmpeg 的 `drawbox`（擦掉模板上的示例文字）+ `drawtext`（按坐标写字）。
//! 好处是**不引入任何图像库依赖**，中文由 ffmpeg 的 freetype 渲染，系统自带字体即可。
//!
//! 文字走 `textfile=` 而不是 `text=`：这样换行、引号、`%`、`:` 全部不用转义，
//! 也不需要跟 filtergraph 的转义规则斗智斗勇。
//!
//! ## 与前端的边界
//!
//! 「怎么排版」是纯逻辑 —— 换行、按区域缩字号、对齐表达式都在这里，
//! 全部可单测；真正的画图只有最后一步跑 ffmpeg。

use crate::error::{AppError, AppResult};
use crate::models::{ProofField, ProofFieldPreview, ProofLayout, ProofPreview, ProofRenderReport};
use crate::proc::{no_window, tail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 一个字段在排版后的结果（纯计算，不碰磁盘）
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedField {
    pub key: String,
    pub label: String,
    pub lines: Vec<String>,
    pub font_size: i64,
    pub shrunk: bool,
    pub overflow: bool,
    /// 文字文件的文件名（渲染时写在 ffmpeg 的工作目录里）
    pub text_file: String,
}

// ---------------- 文本度量（纯函数） ----------------

/// 估算一个字符占多少个「全角单位」。
///
/// 中日韩字符按 1 个算，ASCII 按 0.55 个算 —— 只是排版估算，
/// 不追求像素级精确，够用来决定「这行放不放得下」。
pub fn char_units(ch: char) -> f64 {
    let code = ch as u32;
    let wide = (0x2E80..=0x9FFF).contains(&code)      // 部首、假名、汉字
        || (0xAC00..=0xD7AF).contains(&code)          // 谚文
        || (0xF900..=0xFAFF).contains(&code)          // 兼容汉字
        || (0xFF00..=0xFF60).contains(&code)          // 全角标点
        || (0x3000..=0x303F).contains(&code); // CJK 标点
    if wide {
        1.0
    } else {
        0.55
    }
}

pub fn display_units(text: &str) -> f64 {
    text.chars().map(char_units).sum()
}

/// 不能出现在行首的标点（中文禁则，取常见的那一批）
const NO_LINE_START: &str = "，。、；：？！）】」』〉》”’%…—·,.;:?!)]}>";
/// 不能出现在行尾的标点
const NO_LINE_END: &str = "（【「『〈《“‘([{<";

/// 按「全角单位」宽度折行，并做基本的中文禁则处理。
///
/// - 宽度超过 `max_units` 就换行；
/// - 下一行不会以收尾标点开头（把标点留在上一行）；
/// - 上一行不会以开引号之类结尾（把它推到下一行）；
/// - 单个字符本身就超宽时也照样输出，不会死循环。
pub fn wrap_text(text: &str, max_units: f64) -> Vec<String> {
    let limit = max_units.max(1.0);
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return vec![String::new()];
    }

    let mut lines: Vec<String> = Vec::new();
    let mut current: Vec<char> = Vec::new();
    let mut width = 0.0;

    for ch in chars {
        let w = char_units(ch);
        if !current.is_empty() && width + w > limit {
            // 该断行了：先处理禁则
            if NO_LINE_START.contains(ch) {
                // 收尾标点不另起一行，挤在上一行
                current.push(ch);
                lines.push(current.iter().collect());
                current = Vec::new();
                width = 0.0;
                continue;
            }
            while let Some(&last) = current.last() {
                if NO_LINE_END.contains(last) && current.len() > 1 {
                    current.pop();
                    lines.push(current.iter().collect());
                    current = vec![last];
                    width = char_units(last);
                    break;
                }
                break;
            }
            if current.is_empty() {
                // 上面那个 while 没动过（正常情况下走不到这里）
                lines.push(String::new());
            } else if width <= limit {
                lines.push(current.iter().collect());
                current = Vec::new();
                width = 0.0;
            }
        }
        current.push(ch);
        width += w;
    }

    if !current.is_empty() {
        lines.push(current.iter().collect());
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// 把 `#RRGGBB` 转成 ffmpeg 认的颜色写法。非法输入返回 `None`。
pub fn to_ffmpeg_color(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let hex = trimmed.strip_prefix('#').unwrap_or(trimmed);
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("0x{}", hex.to_uppercase()))
}

/// 字体路径在 filtergraph 里的写法。统一走 [`crate::proc::filter_path`]，
/// 免得这里一套转义、别处又一套。
pub fn fontfile_expr(path: &str) -> String {
    crate::proc::filter_path(path)
}

/// 水平位置的表达式。用 drawtext 自带的 `text_w`，避免自己算字宽。
pub fn x_expr(box_x: i64, box_w: i64, align: &str) -> String {
    match align {
        "center" => format!("{box_x}+({box_w}-text_w)/2"),
        "right" => format!("{box_x}+{box_w}-text_w"),
        _ => box_x.to_string(),
    }
}

/// 垂直位置的表达式。
pub fn y_expr(box_y: i64, box_h: i64, valign: &str, line_count: usize, font_size: i64, spacing: f64) -> String {
    match valign {
        "top" => box_y.to_string(),
        "bottom" => {
            let block = block_height(line_count, font_size, spacing) as i64;
            format!("{}+{}-{}", box_y, box_h, block)
        }
        _ => format!("{box_y}+({box_h}-text_h)/2"),
    }
}

fn block_height(line_count: usize, font_size: i64, spacing: f64) -> i64 {
    let extra = ((font_size as f64) * (spacing - 1.0)).round().max(0.0) as i64;
    (font_size + extra) * line_count.max(1) as i64
}

// ---------------- 排版（纯函数） ----------------

/// 给一个字段排版：按区域宽度折行，放不下就逐像素缩字号，缩到下限仍放不下则标记溢出。
pub fn plan_field(field: &ProofField, value: &str) -> PlannedField {
    let text_file = format!("{}.txt", sanitize_key(&field.key));
    let box_w = field.box_rect.get(2).copied().unwrap_or(0).max(1);
    let box_h = field.box_rect.get(3).copied().unwrap_or(0).max(1);
    let spacing = if field.line_spacing.is_finite() && field.line_spacing > 0.0 {
        field.line_spacing
    } else {
        1.0
    };

    let original = field.font_size.clamp(4, 400);
    let min_size = field.min_font_size.clamp(4, 400).min(original);
    let mut size = original;

    let mut last_lines = vec![value.to_string()];
    // 走到哪一步由下面这个循环决定，所以这里不预先赋一个会被覆盖的值
    let overflow;

    loop {
        let max_units = (box_w as f64) / (size as f64);
        let lines = if field.wrap {
            wrap_text(value, max_units)
        } else {
            vec![value.to_string()]
        };
        let widest = lines
            .iter()
            .map(|l| display_units(l))
            .fold(0.0_f64, f64::max);
        let needed_w = widest * size as f64;
        let needed_h = block_height(lines.len(), size, spacing) as f64;

        let fits = needed_w <= box_w as f64 + 0.5 && needed_h <= box_h as f64 + 0.5;
        last_lines = lines;

        if fits {
            overflow = false;
            break;
        }
        if size <= min_size {
            overflow = true;
            break;
        }
        size -= 1;
    }

    PlannedField {
        key: field.key.clone(),
        label: field.label.clone(),
        lines: last_lines,
        font_size: size,
        shrunk: size < original,
        overflow,
        text_file,
    }
}

pub fn plan_fields(
    layout: &ProofLayout,
    values: &HashMap<String, String>,
) -> Vec<PlannedField> {
    layout
        .fields
        .iter()
        .map(|field| {
            let value = values.get(&field.key).cloned().unwrap_or_default();
            plan_field(field, &value)
        })
        .collect()
}

/// 字段 key 只允许字母数字下划线连字符，避免被当成路径用。
pub fn sanitize_key(key: &str) -> String {
    let cleaned: String = key
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "field".to_string()
    } else {
        cleaned.chars().take(48).collect()
    }
}

// ---------------- filtergraph ----------------

/// 构造 `-vf` 串。纯函数，只依赖排版结果。
pub fn build_filter(layout: &ProofLayout, planned: &[PlannedField]) -> String {
    let mut parts: Vec<String> = Vec::new();

    for (field, plan) in layout.fields.iter().zip(planned.iter()) {
        // 1) 先擦掉模板上的示例文字
        if let Some(rect) = field.erase_rect.as_ref().filter(|r| r.len() == 4) {
            let (x1, y1, x2, y2) = (rect[0], rect[1], rect[2], rect[3]);
            let w = (x2 - x1).max(1);
            let h = (y2 - y1).max(1);
            let color = to_ffmpeg_color(&field.erase_color)
                .unwrap_or_else(|| "0xFFFFFF".to_string());
            parts.push(format!(
                "drawbox=x={x1}:y={y1}:w={w}:h={h}:color={color}@1:t=fill"
            ));
        }

        // 2) 再写字
        let color = to_ffmpeg_color(&field.color).unwrap_or_else(|| "0x000000".to_string());
        let spacing = if field.line_spacing.is_finite() && field.line_spacing > 0.0 {
            field.line_spacing
        } else {
            1.0
        };
        let line_spacing_px =
            ((plan.font_size as f64) * (spacing - 1.0)).round().max(0.0) as i64;

        let mut expr = format!(
            "drawtext=fontfile={}:textfile='{}':expansion=none:fontsize={}:fontcolor={}:x={}:y={}",
            fontfile_expr(&layout.font_path),
            plan.text_file,
            plan.font_size,
            color,
            x_expr(
                field.box_rect.first().copied().unwrap_or(0),
                field.box_rect.get(2).copied().unwrap_or(1),
                &field.align
            ),
            y_expr(
                field.box_rect.get(1).copied().unwrap_or(0),
                field.box_rect.get(3).copied().unwrap_or(1),
                &field.valign,
                plan.lines.len(),
                plan.font_size,
                spacing
            ),
        );
        if line_spacing_px > 0 {
            expr.push_str(&format!(":line_spacing={line_spacing_px}"));
        }
        parts.push(expr);
    }

    parts.join(",")
}

/// 只算不跑：给界面看每个字段会怎么排、有没有溢出。
pub fn preview(layout: &ProofLayout, values: &HashMap<String, String>) -> ProofPreview {
    let planned = plan_fields(layout, values);
    let mut warnings: Vec<String> = Vec::new();

    for plan in &planned {
        if plan.overflow {
            warnings.push(format!(
                "字段「{}」缩到最小字号仍然放不下，内容可能被裁掉",
                plan.label
            ));
        }
    }
    if !Path::new(&layout.template).is_file() {
        warnings.push(format!("模板文件不存在：{}", layout.template));
    }
    if !Path::new(&layout.font_path).is_file() {
        warnings.push(format!("字体文件不存在：{}", layout.font_path));
    }
    if layout.fields.is_empty() {
        warnings.push("还没有配置任何字段".to_string());
    }

    ProofPreview {
        filter: build_filter(layout, &planned),
        fields: planned
            .into_iter()
            .map(|p| ProofFieldPreview {
                key: p.key,
                label: p.label,
                lines: p.lines,
                font_size: p.font_size,
                shrunk: p.shrunk,
                overflow: p.overflow,
            })
            .collect(),
        warnings,
    }
}

// ---------------- 渲染 ----------------

/// 按布局渲染出一张证明图。
pub fn render(
    ffmpeg: &Path,
    layout: &ProofLayout,
    values: &HashMap<String, String>,
    out: &Path,
) -> AppResult<ProofRenderReport> {
    let template = PathBuf::from(layout.template.trim());
    if !template.is_file() {
        return Err(AppError::Msg(format!(
            "模板图片不存在：{}",
            template.to_string_lossy()
        )));
    }
    let font = PathBuf::from(layout.font_path.trim());
    if !font.is_file() {
        return Err(AppError::Msg(format!(
            "字体文件不存在：{}。可以在「设置」里换一个字体。",
            font.to_string_lossy()
        )));
    }
    if layout.fields.is_empty() {
        return Err(AppError::Msg("还没有配置任何字段，没有可填的内容".into()));
    }
    if out.as_os_str().is_empty() {
        return Err(AppError::Msg("请先选择输出路径".into()));
    }

    let planned = plan_fields(layout, values);
    let filter = build_filter(layout, &planned);

    // ffmpeg 的工作目录放文字文件，这样 textfile 只写文件名，不必转义 Windows 路径
    let work_dir = out
        .parent()
        .map(|d| d.to_path_buf())
        .unwrap_or_else(std::env::temp_dir)
        .join(format!(
            "{}.proofwork",
            out.file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "proof".into())
        ));
    let _ = std::fs::remove_dir_all(&work_dir);
    std::fs::create_dir_all(&work_dir)?;

    for plan in &planned {
        let body = plan.lines.join("\n");
        std::fs::write(work_dir.join(&plan.text_file), body)?;
    }

    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if out.exists() {
        let _ = std::fs::remove_file(out);
    }

    let output = no_window(
        Command::new(ffmpeg)
            .current_dir(&work_dir)
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-y")
            .arg("-i")
            .arg(template.as_os_str())
            .arg("-vf")
            .arg(&filter)
            .arg("-frames:v")
            .arg("1")
            .arg(out.as_os_str()),
    )
    .output()
    .map_err(|e| AppError::Process(format!("无法启动 ffmpeg：{e}")))?;

    let _ = std::fs::remove_dir_all(&work_dir);

    if !output.status.success() {
        return Err(AppError::Process(format!(
            "生成证明图失败（退出码 {:?}）：{}",
            output.status.code(),
            tail(&String::from_utf8_lossy(&output.stderr))
        )));
    }
    if !out.is_file() {
        return Err(AppError::Process(
            "ffmpeg 报告成功但没有产出图片".into(),
        ));
    }

    let mut warnings: Vec<String> = Vec::new();
    for plan in &planned {
        if plan.overflow {
            warnings.push(format!(
                "字段「{}」缩到最小字号仍放不下，内容可能被裁掉",
                plan.label
            ));
        }
    }

    Ok(ProofRenderReport {
        out_path: out.to_string_lossy().to_string(),
        size_bytes: std::fs::metadata(out).map(|m| m.len() as i64).unwrap_or(0),
        field_count: planned.len() as i64,
        filter,
        warnings,
    })
}

// ---------------- 布局读写 ----------------

pub fn load_layout(path: &Path) -> AppResult<ProofLayout> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| AppError::Msg(format!("读取布局文件失败：{e}")))?;
    serde_json::from_str(&text).map_err(|e| AppError::Msg(format!("布局文件格式不对：{e}")))
}

pub fn save_layout(path: &Path, layout: &ProofLayout) -> AppResult<()> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    let text = serde_json::to_string_pretty(layout)?;
    std::fs::write(path, text)?;
    Ok(())
}

/// 挑一个系统里存在的中文字体，省得用户第一件事就是找字体。
pub fn guess_font() -> String {
    const CANDIDATES: [&str; 5] = [
        "C:/Windows/Fonts/simfang.ttf",
        "C:/Windows/Fonts/simhei.ttf",
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/simsun.ttc",
        "C:/Windows/Fonts/arial.ttf",
    ];
    CANDIDATES
        .iter()
        .find(|p| Path::new(p).is_file())
        .map(|p| (*p).to_string())
        .unwrap_or_default()
}

/// 一份起步布局：三个最常用的字段，坐标需要用户按自己的模板调。
pub fn default_layout() -> ProofLayout {
    let field = |key: &str, label: &str, x: i64, y: i64, w: i64, h: i64| ProofField {
        key: key.to_string(),
        label: label.to_string(),
        erase_rect: Some(vec![x - 4, y - 4, x + w + 4, y + h + 4]),
        erase_color: "#FFFFFF".to_string(),
        box_rect: vec![x, y, w, h],
        font_size: 16,
        min_font_size: 12,
        color: "#757575".to_string(),
        align: "center".to_string(),
        valign: "center".to_string(),
        wrap: true,
        line_spacing: 1.12,
    };

    ProofLayout {
        template: String::new(),
        image_size: vec![1280, 1656],
        font_path: guess_font(),
        fields: vec![
            field("drama_name", "剧目名称", 332, 512, 138, 84),
            field("episodes_duration", "集数与时长", 480, 512, 190, 84),
            field("investment", "投资金额", 690, 512, 158, 84),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(key: &str, font_size: i64, rect: [i64; 4]) -> ProofField {
        ProofField {
            key: key.to_string(),
            label: key.to_string(),
            erase_rect: None,
            erase_color: "#FFFFFF".to_string(),
            box_rect: rect.to_vec(),
            font_size,
            min_font_size: 8,
            color: "#000000".to_string(),
            align: "center".to_string(),
            valign: "center".to_string(),
            wrap: true,
            line_spacing: 1.0,
        }
    }

    fn values(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    // ---------------- 文本度量与折行 ----------------

    #[test]
    fn cjk_counts_wide_ascii_counts_narrow() {
        assert_eq!(display_units("你好"), 2.0);
        assert!((display_units("ab") - 1.1).abs() < 1e-6);
        assert!((display_units("你a") - 1.55).abs() < 1e-6);
        assert_eq!(display_units(""), 0.0);
    }

    #[test]
    fn wrap_breaks_by_width() {
        let lines = wrap_text("一二三四五六七八九十", 3.0);
        assert_eq!(lines, vec!["一二三", "四五六", "七八九", "十"]);
    }

    #[test]
    fn wrap_keeps_closing_punctuation_on_the_previous_line() {
        // 宽度只够 2 个字，「，」不该被挤到下一行开头
        let lines = wrap_text("你好，世界", 2.0);
        assert_eq!(lines[0], "你好，");
        assert_eq!(lines[1], "世界");
    }

    #[test]
    fn wrap_never_starts_a_line_with_closing_punctuation() {
        for text in ["甲甲甲。乙", "甲乙丙丁？戊", "一二三！四"] {
            let lines = wrap_text(text, 3.0);
            for line in lines.iter().skip(1) {
                let first = line.chars().next().unwrap();
                assert!(
                    !NO_LINE_START.contains(first),
                    "第二行不应以「{first}」开头：{lines:?}"
                );
            }
        }
    }

    #[test]
    fn wrap_handles_degenerate_inputs() {
        assert_eq!(wrap_text("", 5.0), vec![""]);
        // 单字就超宽也不能死循环
        let lines = wrap_text("你", 0.1);
        assert_eq!(lines, vec!["你"]);
        // 极窄宽度
        let lines = wrap_text("你好世界", 1.0);
        assert_eq!(lines.len(), 4);
    }

    #[test]
    fn wrap_respects_explicit_newlines_via_units() {
        // 已经带了换行的文本，按单位折行后内容不丢
        let lines = wrap_text("第一行内容很长需要折", 4.0);
        let joined: String = lines.concat();
        assert_eq!(joined, "第一行内容很长需要折");
    }

    // ---------------- 颜色与表达式 ----------------

    #[test]
    fn colors_are_converted_and_invalid_rejected() {
        assert_eq!(to_ffmpeg_color("#757575").unwrap(), "0x757575");
        assert_eq!(to_ffmpeg_color("757575").unwrap(), "0x757575");
        assert_eq!(to_ffmpeg_color("#aabbcc").unwrap(), "0xAABBCC");
        assert!(to_ffmpeg_color("#xyzxyz").is_none());
        assert!(to_ffmpeg_color("#12345").is_none());
        assert!(to_ffmpeg_color("").is_none());
    }

    #[test]
    fn font_path_is_escaped_for_the_filtergraph() {
        assert_eq!(
            fontfile_expr("C:/Windows/Fonts/simfang.ttf"),
            "'C\\:/Windows/Fonts/simfang.ttf'"
        );
        // 反斜杠统一成正斜杠
        assert_eq!(
            fontfile_expr(r"C:\Windows\Fonts\simhei.ttf"),
            "'C\\:/Windows/Fonts/simhei.ttf'"
        );
    }

    #[test]
    fn alignment_expressions() {
        assert_eq!(x_expr(100, 200, "left"), "100");
        assert_eq!(x_expr(100, 200, "center"), "100+(200-text_w)/2");
        assert_eq!(x_expr(100, 200, "right"), "100+200-text_w");
        assert_eq!(y_expr(50, 80, "top", 1, 16, 1.0), "50");
        assert_eq!(y_expr(50, 80, "center", 1, 16, 1.0), "50+(80-text_h)/2");
        assert_eq!(y_expr(50, 80, "bottom", 2, 16, 1.0), "50+80-32");
    }

    // ---------------- 排版 ----------------

    #[test]
    fn plan_field_keeps_font_size_when_it_fits() {
        let f = field("k", 16, [0, 0, 320, 200]);
        let plan = plan_field(&f, "短剧");
        assert_eq!(plan.font_size, 16);
        assert!(!plan.shrunk);
        assert!(!plan.overflow);
        assert_eq!(plan.text_file, "k.txt");
    }

    #[test]
    fn plan_field_shrinks_to_fit_a_small_box() {
        let f = field("k", 40, [0, 0, 100, 40]);
        let plan = plan_field(&f, "很长的一段剧目名称内容");
        assert!(plan.font_size < 40, "应当缩小字号，实际 {}", plan.font_size);
        assert!(plan.shrunk);
    }

    #[test]
    fn plan_field_flags_overflow_instead_of_lying() {
        let f = field("k", 20, [0, 0, 10, 10]);
        let plan = plan_field(&f, "内容实在太多太多了放不下");
        assert!(plan.overflow, "放不下时必须如实标记");
        assert_eq!(plan.font_size, 8, "应当缩到下限");
    }

    #[test]
    fn plan_field_wraps_into_multiple_lines() {
        let f = field("k", 10, [0, 0, 100, 100]);
        let plan = plan_field(&f, "一二三四五六七八九十一二三四五六七八九十");
        assert!(plan.lines.len() > 1, "{:?}", plan.lines);
        assert!(plan.lines.iter().all(|l| display_units(l) <= 10.5));
    }

    #[test]
    fn plan_field_without_wrap_keeps_one_line() {
        let mut f = field("k", 10, [0, 0, 100, 100]);
        f.wrap = false;
        let plan = plan_field(&f, "一二三四五六七八九十");
        assert_eq!(plan.lines.len(), 1);
    }

    #[test]
    fn field_keys_are_sanitized_for_use_as_filenames() {
        assert_eq!(sanitize_key("drama_name"), "drama_name");
        assert_eq!(sanitize_key("../../etc/passwd"), "______etc_passwd");
        assert_eq!(sanitize_key(""), "field");
        assert_eq!(sanitize_key(&"a".repeat(100)).len(), 48);
    }

    // ---------------- filtergraph ----------------

    #[test]
    fn filter_contains_erase_then_draw_for_each_field() {
        let mut layout = default_layout();
        layout.font_path = "C:/Windows/Fonts/simfang.ttf".into();
        layout.fields = vec![ProofField {
            erase_rect: Some(vec![10, 20, 110, 60]),
            ..field("drama_name", 16, [12, 22, 96, 36])
        }];
        let planned = plan_fields(&layout, &values(&[("drama_name", "都市爽剧")]));
        let filter = build_filter(&layout, &planned);

        assert!(filter.contains("drawbox=x=10:y=20:w=100:h=40:color=0xFFFFFF@1:t=fill"), "{filter}");
        assert!(filter.contains("drawtext=fontfile='C\\:/Windows/Fonts/simfang.ttf'"), "{filter}");
        assert!(filter.contains("textfile='drama_name.txt'"), "{filter}");
        assert!(filter.contains("expansion=none"), "必须关掉 % 展开：{filter}");
        // drawbox 必须在 drawtext 之前，否则会把字擦掉
        assert!(filter.find("drawbox").unwrap() < filter.find("drawtext").unwrap());
    }

    #[test]
    fn filter_omits_drawbox_when_no_erase_rect() {
        let mut layout = default_layout();
        layout.fields = vec![field("k", 16, [0, 0, 100, 50])];
        let planned = plan_fields(&layout, &values(&[("k", "x")]));
        assert!(!build_filter(&layout, &planned).contains("drawbox"));
    }

    #[test]
    fn preview_reports_missing_files_and_overflow() {
        let mut layout = default_layout();
        layout.template = "Z:/nope/muban.jpg".into();
        layout.font_path = "Z:/nope/font.ttf".into();
        layout.fields = vec![field("k", 20, [0, 0, 8, 8])];

        let p = preview(&layout, &values(&[("k", "内容很多很多")]));
        assert!(p.warnings.iter().any(|w| w.contains("模板文件不存在")));
        assert!(p.warnings.iter().any(|w| w.contains("字体文件不存在")));
        assert!(p.warnings.iter().any(|w| w.contains("放不下")), "{:?}", p.warnings);
        assert!(p.fields[0].overflow);
    }

    #[test]
    fn preview_warns_when_no_fields_configured() {
        let mut layout = default_layout();
        layout.fields.clear();
        assert!(preview(&layout, &values(&[]))
            .warnings
            .iter()
            .any(|w| w.contains("还没有配置")));
    }

    #[test]
    fn missing_values_render_as_empty_not_as_an_error() {
        let mut layout = default_layout();
        layout.fields = vec![field("k", 16, [0, 0, 100, 50])];
        let plan = plan_fields(&layout, &values(&[]));
        assert_eq!(plan[0].lines, vec![String::new()]);
        assert!(!plan[0].overflow, "空值不该被判成溢出");
    }

    #[test]
    fn render_validates_inputs_before_spawning_ffmpeg() {
        let layout = default_layout();
        let err = render(
            Path::new("Z:/nope/ffmpeg.exe"),
            &layout,
            &values(&[]),
            Path::new("Z:/nope/out.png"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("模板图片不存在"), "实际：{err}");

        let mut with_template = layout.clone();
        with_template.template = std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        // 模板换成真实文件后，再把字体指向一个不存在的位置
        with_template.font_path = "Z:/nope/font.ttf".into();
        let err = render(
            Path::new("Z:/nope/ffmpeg.exe"),
            &with_template,
            &values(&[]),
            Path::new("Z:/nope/out.png"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("字体文件不存在"), "实际：{err}");

        // 字段为空时也要在跑 ffmpeg 之前拦下
        let mut no_fields = with_template.clone();
        no_fields.font_path = guess_font();
        no_fields.fields.clear();
        let err = render(
            Path::new("Z:/nope/ffmpeg.exe"),
            &no_fields,
            &values(&[]),
            Path::new("Z:/nope/out.png"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("还没有配置任何字段"), "实际：{err}");
    }

    #[test]
    fn default_layout_has_common_fields_and_a_real_font() {
        let layout = default_layout();
        assert_eq!(layout.fields.len(), 3);
        let keys: Vec<&str> = layout.fields.iter().map(|f| f.key.as_str()).collect();
        assert!(keys.contains(&"drama_name"));
        assert!(keys.contains(&"investment"));
        // 本机应当能猜到一个字体；猜不到也不该 panic
        if !layout.font_path.is_empty() {
            assert!(Path::new(&layout.font_path).is_file(), "{}", layout.font_path);
        }
    }

    #[test]
    fn layout_round_trips_through_json() {
        let dir = std::env::temp_dir().join("dramadist_proof_layout");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("layout.json");
        let layout = default_layout();
        save_layout(&path, &layout).unwrap();
        let loaded = load_layout(&path).unwrap();
        assert_eq!(loaded.fields.len(), layout.fields.len());
        assert_eq!(loaded.image_size, layout.image_size);
        assert_eq!(loaded.fields[0].key, layout.fields[0].key);
        // eraseColor 有默认值，缺字段的旧文件也能读
        let raw = std::fs::read_to_string(&path).unwrap();
        let without = raw.replace("\"eraseColor\": \"#FFFFFF\",", "");
        let fallback: ProofLayout = serde_json::from_str(&without).unwrap();
        assert_eq!(fallback.fields[0].erase_color, "#FFFFFF");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn broken_layout_file_is_reported_clearly() {
        let dir = std::env::temp_dir().join("dramadist_proof_bad");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("bad.json");
        std::fs::write(&path, "{ not json }").unwrap();
        let err = load_layout(&path).unwrap_err().to_string();
        assert!(err.contains("格式不对"), "{err}");
        let err = load_layout(&dir.join("missing.json")).unwrap_err().to_string();
        assert!(err.contains("读取布局文件失败"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------------- 端到端（真实 ffmpeg，默认跳过） ----------------

    fn test_ffmpeg() -> Option<PathBuf> {
        std::env::var("DRAMADIST_TEST_FFMPEG")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
    }

    /// 造一张浅灰底的模板图，并在字段位置写一段"示例文字"
    fn make_template(ffmpeg: &Path, out: &Path, font: &str) {
        let filter = format!(
            "drawbox=x=10:y=20:w=460:h=120:color=0xEEEEEE@1:t=fill,\
             drawtext=fontfile={}:text='SAMPLE':expansion=none:fontsize=16:fontcolor=0x999999:x=20:y=60",
            fontfile_expr(font)
        );
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
                    "color=c=white:s=640x360",
                    "-vf",
                    &filter,
                    "-frames:v",
                    "1",
                ])
                .arg(out),
        )
        .status()
        .expect("无法启动 ffmpeg");
        assert!(status.success(), "生成模板图失败");
    }

    #[test]
    fn end_to_end_renders_a_filled_proof_image() {
        let Some(ffmpeg) = test_ffmpeg() else {
            eprintln!("跳过：未设置 DRAMADIST_TEST_FFMPEG");
            return;
        };
        let font = guess_font();
        if font.is_empty() {
            eprintln!("跳过：本机没有可用字体");
            return;
        }

        let dir = std::env::temp_dir().join("dramadist_proof_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let template = dir.join("muban.png");
        make_template(&ffmpeg, &template, &font);

        let layout = ProofLayout {
            template: template.to_string_lossy().to_string(),
            image_size: vec![640, 360],
            font_path: font.clone(),
            fields: vec![
                ProofField {
                    key: "drama_name".into(),
                    label: "剧目名称".into(),
                    erase_rect: Some(vec![10, 20, 240, 80]),
                    erase_color: "#FFFFFF".into(),
                    box_rect: vec![14, 24, 220, 50],
                    font_size: 20,
                    min_font_size: 10,
                    color: "#333333".into(),
                    align: "center".into(),
                    valign: "center".into(),
                    wrap: true,
                    line_spacing: 1.1,
                },
                ProofField {
                    key: "investment".into(),
                    label: "投资金额".into(),
                    erase_rect: Some(vec![250, 20, 470, 80]),
                    erase_color: "#FFFFFF".into(),
                    box_rect: vec![254, 24, 210, 50],
                    font_size: 20,
                    min_font_size: 10,
                    color: "#333333".into(),
                    align: "center".into(),
                    valign: "center".into(),
                    wrap: true,
                    line_spacing: 1.1,
                },
            ],
        };

        let out = dir.join("proof.png");
        let report = render(
            &ffmpeg,
            &layout,
            &values(&[("drama_name", "都市爽剧"), ("investment", "120000")]),
            &out,
        )
        .expect("生成证明图失败");

        assert_eq!(report.field_count, 2);
        assert!(out.is_file());
        assert!(report.size_bytes > 1_000, "图片太小：{}", report.size_bytes);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        // 中间产物必须清掉
        assert!(!dir.join("proof.proofwork").exists(), "临时目录没清掉");

        // 尺寸应与模板一致（我们没有缩放）
        let probe = crate::media::probe(
            &std::env::var("DRAMADIST_TEST_FFPROBE")
                .ok()
                .map(PathBuf::from)
                .unwrap_or_else(|| ffmpeg.clone()),
            &out,
        );
        if let Ok(info) = probe {
            if info.width > 0 {
                assert_eq!((info.width, info.height), (640, 360), "输出尺寸应保持模板尺寸");
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 闭环验证：渲染出来的字，能不能被 OCR 读回来
    #[test]
    fn end_to_end_rendered_text_is_readable_by_ocr() {
        let (Some(ffmpeg), Some(python)) = (
            test_ffmpeg(),
            std::env::var("DRAMADIST_TEST_OCR_PYTHON")
                .ok()
                .map(PathBuf::from)
                .filter(|p| p.is_file()),
        ) else {
            eprintln!("跳过：需要 DRAMADIST_TEST_FFMPEG 与 DRAMADIST_TEST_OCR_PYTHON");
            return;
        };
        let font = guess_font();
        let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("ocr_worker.py");
        if font.is_empty() || !worker.is_file() {
            eprintln!("跳过：没有字体或 ocr_worker.py");
            return;
        }

        let dir = std::env::temp_dir().join("dramadist_proof_ocr_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let template = dir.join("muban.png");
        // 白底模板，字段区留空
        let status = no_window(
            Command::new(&ffmpeg)
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=c=white:s=640x200",
                    "-frames:v",
                    "1",
                ])
                .arg(&template),
        )
        .status()
        .expect("无法启动 ffmpeg");
        assert!(status.success());

        let layout = ProofLayout {
            template: template.to_string_lossy().to_string(),
            image_size: vec![640, 200],
            font_path: font,
            fields: vec![ProofField {
                key: "drama_name".into(),
                label: "剧目名称".into(),
                erase_rect: None,
                erase_color: "#FFFFFF".into(),
                box_rect: vec![20, 60, 600, 80],
                font_size: 48,
                min_font_size: 24,
                color: "#000000".into(),
                align: "center".into(),
                valign: "center".into(),
                wrap: false,
                line_spacing: 1.0,
            }],
        };

        let out = dir.join("proof.png");
        render(
            &ffmpeg,
            &layout,
            &values(&[("drama_name", "HELLO 7788")]),
            &out,
        )
        .expect("生成证明图失败");

        // 交给 OCR 读回来
        let opts = crate::ocr::default_options();
        let (recognized, _) = crate::ocr::run_worker(
            &python,
            &worker,
            std::slice::from_ref(&out),
            &opts,
            std::time::Duration::from_secs(300),
        )
        .expect("OCR 失败");

        let joined: String = recognized
            .iter()
            .map(|f| crate::ocr::normalize_text(&f.text))
            .collect();
        assert!(
            joined.contains("hello"),
            "渲染出来的文字没被读回来：{joined}"
        );
        assert!(joined.contains("7788"), "数字没被读回来：{joined}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
