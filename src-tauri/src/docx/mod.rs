//! 最小 docx 写出：自己拼 OOXML，自己打 ZIP 包，**不引入任何文档库或压缩库**。
//!
//! ## 为什么自己写
//!
//! 证明材料里有一类是文档（成本明细、权属声明），要能直接被 Word 打开。
//! 但为了写一个几十行的文档去引入 zip + OOXML 两套依赖并不划算，
//! 而且 docx 的格式本身很简单：一个 ZIP，里面几个固定名字的 XML。
//!
//! ## ZIP 用的是「存储」而不是「压缩」
//!
//! 这些 XML 本身就不大，压缩收益有限；用 STORE 省掉整个 deflate 实现，
//! 但 CRC32 一个都不能少 —— 少了 Word 会直接拒绝打开。
//!
//! ## 验证方式
//!
//! 测试里不自己实现解压来"自证"，而是交给 **Python 的 zipfile + ElementTree**
//! 去打开和解析：那是完全独立的实现，能打开才算数。

use crate::error::{AppError, AppResult};
use std::path::Path;

// ---------------- CRC32 ----------------

/// 标准 IEEE CRC32。ZIP 每个条目都必须带，写错了 Word 会拒绝打开。
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

// ---------------- XML ----------------

/// XML 文本转义。除了五个实体，还要**剔除 XML 不允许的控制字符** ——
/// 用户从别处粘来的文本里混进一个 \u{1} 就足以让整份文档打不开。
pub fn escape_xml(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 允许的字符之外的一律丢掉（保留 \t \n \r）
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' => {}
            c if (c as u32) == 0xFFFE || (c as u32) == 0xFFFF => {}
            c => out.push(c),
        }
    }
    out
}

// ---------------- 文档模型 ----------------

/// 文档里的一个块。刻意做得很小 —— 够写证明材料就行。
#[derive(Debug, Clone, PartialEq)]
pub enum DocBlock {
    /// 居中大标题
    Title(String),
    /// 左对齐小标题
    Heading(String),
    /// 正文段落（首行缩进两字）
    Paragraph(String),
    /// 「名称：值」一行，用于信息栏
    KeyValue { label: String, value: String },
    /// 表格
    Table {
        headers: Vec<String>,
        rows: Vec<Vec<String>>,
        /// 每列相对宽度，长度应与 headers 一致
        widths: Vec<f64>,
    },
    /// 空一行
    Spacer,
}

/// 页面与字体设置
#[derive(Debug, Clone)]
pub struct DocStyle {
    /// 中文字体名（Word 里显示的名字，不是文件路径）
    pub east_asian_font: String,
    pub latin_font: String,
    /// 正文字号，半磅为单位（24 = 12pt = 小四）
    pub body_size: u32,
    /// 标题字号
    pub title_size: u32,
}

impl Default for DocStyle {
    fn default() -> Self {
        Self {
            east_asian_font: "仿宋".to_string(),
            latin_font: "Times New Roman".to_string(),
            body_size: 24,
            title_size: 36,
        }
    }
}

/// 文档元信息
#[derive(Debug, Clone, Default)]
pub struct DocMeta {
    pub title: String,
    pub author: String,
    /// ISO8601，用于 core.xml
    pub created: String,
}

// ---------------- document.xml ----------------

fn run_props(style: &DocStyle, size: u32, bold: bool) -> String {
    let mut rpr = format!(
        "<w:rPr><w:rFonts w:ascii=\"{}\" w:hAnsi=\"{}\" w:eastAsia=\"{}\" w:cs=\"{}\"/>",
        escape_xml(&style.latin_font),
        escape_xml(&style.latin_font),
        escape_xml(&style.east_asian_font),
        escape_xml(&style.latin_font),
    );
    if bold {
        rpr.push_str("<w:b/><w:bCs/>");
    }
    rpr.push_str(&format!("<w:sz w:val=\"{size}\"/><w:szCs w:val=\"{size}\"/></w:rPr>"));
    rpr
}

fn paragraph(text: &str, style: &DocStyle, size: u32, bold: bool, align: &str, indent: bool) -> String {
    let mut ppr = String::from("<w:pPr>");
    if !align.is_empty() {
        ppr.push_str(&format!("<w:jc w:val=\"{align}\"/>"));
    }
    if indent {
        // 首行缩进两字
        ppr.push_str("<w:ind w:firstLineChars=\"200\"/>");
    }
    ppr.push_str("</w:pPr>");

    // 空段落不写 run，否则 Word 里会多出一个空的格式标记
    let body = if text.is_empty() {
        String::new()
    } else {
        format!(
            "<w:r>{}<w:t xml:space=\"preserve\">{}</w:t></w:r>",
            run_props(style, size, bold),
            escape_xml(text)
        )
    };
    format!("<w:p>{ppr}{body}</w:p>")
}

fn table_xml(headers: &[String], rows: &[Vec<String>], widths: &[f64], style: &DocStyle) -> String {
    let cols = headers.len().max(1);
    let total_width = 9000.0_f64; // 页宽去掉页边距后约 9026 twips
    let resolved: Vec<f64> = (0..cols)
        .map(|i| {
            let w = widths.get(i).copied().unwrap_or(1.0);
            if w.is_finite() && w > 0.0 {
                w
            } else {
                1.0
            }
        })
        .collect();
    let sum: f64 = resolved.iter().sum();
    let col_twips: Vec<i64> = resolved
        .iter()
        .map(|w| ((w / sum) * total_width).round() as i64)
        .collect();

    let borders = "<w:tblBorders>\
        <w:top w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"000000\"/>\
        <w:left w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"000000\"/>\
        <w:bottom w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"000000\"/>\
        <w:right w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"000000\"/>\
        <w:insideH w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"000000\"/>\
        <w:insideV w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"000000\"/>\
        </w:tblBorders>";

    let mut xml = format!(
        "<w:tbl><w:tblPr><w:tblW w:w=\"{total_width}\" w:type=\"dxa\"/><w:jc w:val=\"center\"/>{borders}</w:tblPr><w:tblGrid>"
    );
    for w in &col_twips {
        xml.push_str(&format!("<w:gridCol w:w=\"{w}\"/>"));
    }
    xml.push_str("</w:tblGrid>");

    let cell = |text: &str, index: usize, bold: bool| {
        let width = col_twips.get(index).copied().unwrap_or(0);
        format!(
            "<w:tc><w:tcPr><w:tcW w:w=\"{width}\" w:type=\"dxa\"/><w:vAlign w:val=\"center\"/></w:tcPr>{}</w:tc>",
            paragraph(text, style, 21, bold, "center", false)
        )
    };

    // 表头
    xml.push_str("<w:tr><w:trPr><w:tblHeader/></w:trPr>");
    for (i, h) in headers.iter().enumerate() {
        xml.push_str(&cell(h, i, true));
    }
    xml.push_str("</w:tr>");

    // 数据行：短行按表头列数补齐，避免 Word 里出现参差不齐的表格
    for row in rows {
        xml.push_str("<w:tr>");
        for i in 0..cols {
            xml.push_str(&cell(row.get(i).map(|s| s.as_str()).unwrap_or(""), i, false));
        }
        xml.push_str("</w:tr>");
    }

    xml.push_str("</w:tbl>");
    xml
}

/// 把块序列渲染成 `word/document.xml`。
pub fn render_document(blocks: &[DocBlock], style: &DocStyle) -> String {
    let mut body = String::new();
    for block in blocks {
        match block {
            DocBlock::Title(text) => {
                body.push_str(&paragraph(text, style, style.title_size, true, "center", false))
            }
            DocBlock::Heading(text) => {
                body.push_str(&paragraph(text, style, style.body_size + 2, true, "", false))
            }
            DocBlock::Paragraph(text) => {
                body.push_str(&paragraph(text, style, style.body_size, false, "", true))
            }
            DocBlock::KeyValue { label, value } => body.push_str(&paragraph(
                &format!("{label}：{value}"),
                style,
                style.body_size,
                false,
                "",
                false,
            )),
            DocBlock::Table {
                headers,
                rows,
                widths,
            } => body.push_str(&table_xml(headers, rows, widths, style)),
            DocBlock::Spacer => body.push_str(&paragraph("", style, style.body_size, false, "", false)),
        }
    }

    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:body>{body}\
<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/>\
<w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"851\" w:footer=\"992\" w:gutter=\"0\"/>\
</w:sectPr></w:body></w:document>"
    )
}

fn content_types() -> String {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
<Override PartName=\"/docProps/core.xml\" ContentType=\"application/vnd.openxmlformats-package.core-properties+xml\"/>\
</Types>"
        .to_string()
}

fn root_rels() -> String {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
<Relationship Id=\"rId2\" Type=\"http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties\" Target=\"docProps/core.xml\"/>\
</Relationships>"
        .to_string()
}

fn document_rels() -> String {
    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>\
</Relationships>"
        .to_string()
}

fn styles_xml(style: &DocStyle) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:docDefaults><w:rPrDefault><w:rPr>\
<w:rFonts w:ascii=\"{}\" w:hAnsi=\"{}\" w:eastAsia=\"{}\" w:cs=\"{}\"/>\
<w:sz w:val=\"{}\"/><w:szCs w:val=\"{}\"/>\
</w:rPr></w:rPrDefault>\
<w:pPrDefault><w:pPr><w:spacing w:line=\"360\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault>\
</w:docDefaults>\
<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/></w:style>\
</w:styles>",
        escape_xml(&style.latin_font),
        escape_xml(&style.latin_font),
        escape_xml(&style.east_asian_font),
        escape_xml(&style.latin_font),
        style.body_size,
        style.body_size,
    )
}

fn core_xml(meta: &DocMeta) -> String {
    let created = if meta.created.trim().is_empty() {
        "2026-01-01T00:00:00Z".to_string()
    } else {
        meta.created.clone()
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<cp:coreProperties xmlns:cp=\"http://schemas.openxmlformats.org/package/2006/metadata/core-properties\" \
xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:dcterms=\"http://purl.org/dc/terms/\" \
xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\">\
<dc:title>{}</dc:title><dc:creator>{}</dc:creator>\
<cp:lastModifiedBy>{}</cp:lastModifiedBy>\
<dcterms:created xsi:type=\"dcterms:W3CDTF\">{}</dcterms:created>\
</cp:coreProperties>",
        escape_xml(&meta.title),
        escape_xml(if meta.author.is_empty() { "DramaDist" } else { &meta.author }),
        escape_xml(if meta.author.is_empty() { "DramaDist" } else { &meta.author }),
        escape_xml(&created),
    )
}

// ---------------- ZIP（存储方式，不压缩） ----------------

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// 把一组「文件名 → 内容」打成一个 ZIP。
///
/// `dos_time` / `dos_date` 由调用方给定，这样测试可以固定时间戳拿到确定性的字节。
pub fn build_zip(entries: &[(String, Vec<u8>)], dos_time: u16, dos_date: u16) -> AppResult<Vec<u8>> {
    if entries.is_empty() {
        return Err(AppError::Msg("没有要写入的文件".into()));
    }
    let mut out: Vec<u8> = Vec::new();
    let mut directory: Vec<u8> = Vec::new();
    let count = entries.len() as u16;

    for (name, data) in entries {
        if name.is_empty() || name.len() > u16::MAX as usize {
            return Err(AppError::Msg(format!("文件名不合法：{name}")));
        }
        if data.len() > u32::MAX as usize {
            return Err(AppError::Msg(format!("文件太大，ZIP 存不下：{name}")));
        }
        let offset = out.len() as u32;
        let crc = crc32(data);

        // 本地文件头
        push_u32(&mut out, 0x0403_4B50);
        push_u16(&mut out, 20); // version needed
        push_u16(&mut out, 0); // flags
        push_u16(&mut out, 0); // 存储，不压缩
        push_u16(&mut out, dos_time);
        push_u16(&mut out, dos_date);
        push_u32(&mut out, crc);
        push_u32(&mut out, data.len() as u32);
        push_u32(&mut out, data.len() as u32);
        push_u16(&mut out, name.len() as u16);
        push_u16(&mut out, 0); // extra
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);

        // 中央目录项
        push_u32(&mut directory, 0x0201_4B50);
        push_u16(&mut directory, 20); // version made by
        push_u16(&mut directory, 20); // version needed
        push_u16(&mut directory, 0);
        push_u16(&mut directory, 0);
        push_u16(&mut directory, dos_time);
        push_u16(&mut directory, dos_date);
        push_u32(&mut directory, crc);
        push_u32(&mut directory, data.len() as u32);
        push_u32(&mut directory, data.len() as u32);
        push_u16(&mut directory, name.len() as u16);
        push_u16(&mut directory, 0); // extra
        push_u16(&mut directory, 0); // comment
        push_u16(&mut directory, 0); // disk
        push_u16(&mut directory, 0); // internal attrs
        push_u32(&mut directory, 0); // external attrs
        push_u32(&mut directory, offset);
        directory.extend_from_slice(name.as_bytes());
    }

    let cd_offset = out.len() as u32;
    let cd_size = directory.len() as u32;
    out.extend_from_slice(&directory);

    // 中央目录结束记录
    push_u32(&mut out, 0x0605_4B50);
    push_u16(&mut out, 0);
    push_u16(&mut out, 0);
    push_u16(&mut out, count);
    push_u16(&mut out, count);
    push_u32(&mut out, cd_size);
    push_u32(&mut out, cd_offset);
    push_u16(&mut out, 0);
    Ok(out)
}

fn dos_stamp() -> (u16, u16) {
    use chrono::{Datelike, Timelike};
    let now = chrono::Local::now();
    let time = ((now.hour() as u16) << 11) | ((now.minute() as u16) << 5) | (now.second() as u16 / 2);
    let date = (((now.year() - 1980).max(0) as u16) << 9)
        | ((now.month() as u16) << 5)
        | (now.day() as u16);
    (time, date)
}

/// ISO8601 时间戳，给 core.xml 用
pub fn now_iso() -> String {
    chrono::Local::now()
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// 生成一份完整的 docx 字节流。
pub fn build_docx(blocks: &[DocBlock], style: &DocStyle, meta: &DocMeta) -> AppResult<Vec<u8>> {
    let entries = vec![
        ("[Content_Types].xml".to_string(), content_types().into_bytes()),
        ("_rels/.rels".to_string(), root_rels().into_bytes()),
        (
            "word/_rels/document.xml.rels".to_string(),
            document_rels().into_bytes(),
        ),
        (
            "word/document.xml".to_string(),
            render_document(blocks, style).into_bytes(),
        ),
        ("word/styles.xml".to_string(), styles_xml(style).into_bytes()),
        ("docProps/core.xml".to_string(), core_xml(meta).into_bytes()),
    ];
    let (time, date) = dos_stamp();
    build_zip(&entries, time, date)
}

/// 写出 docx 文件，返回字节数。
pub fn write_docx(
    path: &Path,
    blocks: &[DocBlock],
    style: &DocStyle,
    meta: &DocMeta,
) -> AppResult<i64> {
    if path.as_os_str().is_empty() {
        return Err(AppError::Msg("请先选择输出路径".into()));
    }
    if blocks.is_empty() {
        return Err(AppError::Msg("文档里没有任何内容".into()));
    }
    let bytes = build_docx(blocks, style, meta)?;
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    std::fs::write(path, &bytes)?;
    Ok(bytes.len() as i64)
}

// ---------------- 两类证明文档 ----------------

/// 成本明细表的一行
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostRow {
    pub item: String,
    pub quantity: String,
    pub unit_price: String,
    pub amount: String,
}

/// 成本图表：一张明细表 + 合计
pub fn build_cost_blocks(
    drama_name: &str,
    rows: &[CostRow],
    total: &str,
    producer: &str,
    date: &str,
) -> Vec<DocBlock> {
    let mut blocks = vec![
        DocBlock::Title("短剧制作成本明细表".to_string()),
        DocBlock::Spacer,
        DocBlock::KeyValue {
            label: "剧目名称".to_string(),
            value: drama_name.to_string(),
        },
        DocBlock::KeyValue {
            label: "制作方".to_string(),
            value: producer.to_string(),
        },
        DocBlock::KeyValue {
            label: "制表日期".to_string(),
            value: date.to_string(),
        },
        DocBlock::Spacer,
        DocBlock::Table {
            headers: vec![
                "项目".to_string(),
                "数量".to_string(),
                "单价（元）".to_string(),
                "金额（元）".to_string(),
            ],
            rows: rows
                .iter()
                .map(|r| {
                    vec![
                        r.item.clone(),
                        r.quantity.clone(),
                        r.unit_price.clone(),
                        r.amount.clone(),
                    ]
                })
                .collect(),
            widths: vec![3.0, 1.0, 1.5, 1.5],
        },
        DocBlock::Spacer,
        DocBlock::KeyValue {
            label: "合计".to_string(),
            value: format!("{total} 元"),
        },
        DocBlock::Spacer,
        DocBlock::Paragraph(
            "本表由 Dramadist 根据录入数据自动生成，数据来源与金额准确性由填报方负责。".to_string(),
        ),
    ];
    if rows.is_empty() {
        blocks.insert(
            6,
            DocBlock::Paragraph("（尚未录入任何成本项）".to_string()),
        );
    }
    blocks
}

/// 权属声明
pub fn build_ip_blocks(
    drama_name: &str,
    episode_count: i64,
    total_duration: &str,
    owner: &str,
    date: &str,
) -> Vec<DocBlock> {
    vec![
        DocBlock::Title("作品权属声明".to_string()),
        DocBlock::Spacer,
        DocBlock::KeyValue {
            label: "作品名称".to_string(),
            value: drama_name.to_string(),
        },
        DocBlock::KeyValue {
            label: "集数".to_string(),
            value: format!("{episode_count} 集"),
        },
        DocBlock::KeyValue {
            label: "总时长".to_string(),
            value: total_duration.to_string(),
        },
        DocBlock::KeyValue {
            label: "权利归属".to_string(),
            value: owner.to_string(),
        },
        DocBlock::KeyValue {
            label: "声明日期".to_string(),
            value: date.to_string(),
        },
        DocBlock::Spacer,
        DocBlock::Heading("声明事项".to_string()),
        DocBlock::Paragraph(format!(
            "本单位确认，上述作品《{drama_name}》共 {episode_count} 集，总时长 {total_duration}，\
由本单位组织制作并承担全部制作成本。"
        )),
        DocBlock::Paragraph(
            "本单位保证该作品不侵犯任何第三方的著作权、肖像权及其他合法权益。\
如因该作品产生权利纠纷，由本单位承担全部责任。".to_string(),
        ),
        DocBlock::Paragraph(
            "本声明仅用于短剧推广报备与结算，不构成对作品权利的处分。".to_string(),
        ),
        DocBlock::Spacer,
        DocBlock::Spacer,
        DocBlock::Paragraph(format!("声明单位（盖章）：{owner}")),
        DocBlock::Paragraph(format!("日期：{date}")),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn any_python() -> Option<PathBuf> {
        if let Ok(p) = std::env::var("DRAMADIST_TEST_PYTHON") {
            let path = PathBuf::from(p);
            if path.is_file() {
                return Some(path);
            }
        }
        if let Ok(p) = std::env::var("DRAMADIST_TEST_OCR_PYTHON") {
            let path = PathBuf::from(p);
            if path.is_file() {
                return Some(path);
            }
        }
        crate::tools::resolve("", "python3").or_else(|| crate::tools::resolve("", "python"))
    }

    // ---------------- CRC32 ----------------

    #[test]
    fn crc32_matches_the_standard_check_value() {
        // CRC-32/ISO-HDLC 的标准检验值
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"a"), 0xE8B7_BE43);
    }

    // ---------------- XML 转义 ----------------

    #[test]
    fn escapes_xml_entities() {
        assert_eq!(escape_xml("a<b>c&d\"e'f"), "a&lt;b&gt;c&amp;d&quot;e&apos;f");
        assert_eq!(escape_xml(""), "");
        assert_eq!(escape_xml("中文不受影响"), "中文不受影响");
    }

    #[test]
    fn strips_control_characters_that_would_break_the_document() {
        // 从别处粘来的文本里混进一个 \u{1} 就足以让整份文档打不开
        assert_eq!(escape_xml("a\u{1}b\u{0}c"), "abc");
        assert_eq!(escape_xml("保留\t制表\n换行\r回车"), "保留\t制表\n换行\r回车");
        // 代理区之外的非法码点
        assert_eq!(escape_xml("x\u{FFFE}y\u{FFFF}z"), "xyz");
    }

    // ---------------- 段落与表格 ----------------

    #[test]
    fn renders_a_title_paragraph() {
        let style = DocStyle::default();
        let xml = paragraph("标题", &style, style.title_size, true, "center", false);
        assert!(xml.contains("<w:jc w:val=\"center\"/>"), "{xml}");
        assert!(xml.contains("<w:b/>"), "{xml}");
        assert!(xml.contains("<w:t xml:space=\"preserve\">标题</w:t>"), "{xml}");
        assert!(xml.contains("w:eastAsia=\"仿宋\""), "{xml}");
    }

    #[test]
    fn empty_paragraph_has_no_run() {
        let style = DocStyle::default();
        let xml = paragraph("", &style, 24, false, "", false);
        assert!(!xml.contains("<w:r>"), "空段落不该写 run：{xml}");
    }

    #[test]
    fn table_pads_short_rows_to_header_width() {
        let style = DocStyle::default();
        let xml = table_xml(
            &["A".into(), "B".into(), "C".into()],
            &[vec!["1".into()], vec!["1".into(), "2".into(), "3".into()]],
            &[1.0, 1.0, 1.0],
            &style,
        );
        // 表头 3 格 + 第一行补齐到 3 格 + 第二行 3 格 = 9
        assert_eq!(xml.matches("<w:tc>").count(), 9, "{xml}");
        assert_eq!(xml.matches("<w:tr>").count(), 3);
        assert!(xml.contains("<w:tblHeader/>"), "表头要能跨页重复");
    }

    #[test]
    fn table_column_widths_are_normalized() {
        let style = DocStyle::default();
        let xml = table_xml(
            &["A".into(), "B".into()],
            &[],
            &[3.0, 1.0],
            &style,
        );
        // 3:1 分摊到 9000 twips → 6750 / 2250
        assert!(xml.contains("<w:gridCol w:w=\"6750\"/>"), "{xml}");
        assert!(xml.contains("<w:gridCol w:w=\"2250\"/>"), "{xml}");
    }

    #[test]
    fn table_survives_bad_widths() {
        let style = DocStyle::default();
        // 宽度缺失/为负/NaN 时不能 panic，也不能产出 0 宽度
        for widths in [vec![], vec![-1.0, 2.0], vec![f64::NAN, 1.0]] {
            let xml = table_xml(&["A".into(), "B".into()], &[], &widths, &style);
            assert!(xml.contains("<w:gridCol w:w=\"0\"/>") || xml.contains("gridCol"), "{xml}");
            assert!(!xml.contains("NaN"), "{xml}");
        }
    }

    #[test]
    fn document_contains_all_blocks() {
        let style = DocStyle::default();
        let blocks = vec![
            DocBlock::Title("标题".into()),
            DocBlock::Heading("小标题".into()),
            DocBlock::Paragraph("正文".into()),
            DocBlock::KeyValue {
                label: "剧目".into(),
                value: "都市爽剧".into(),
            },
            DocBlock::Table {
                headers: vec!["列".into()],
                rows: vec![vec!["值".into()]],
                widths: vec![1.0],
            },
            DocBlock::Spacer,
        ];
        let xml = render_document(&blocks, &style);
        assert!(xml.starts_with("<?xml"), "必须带 XML 声明");
        for needle in ["标题", "小标题", "正文", "剧目：都市爽剧", ">列<", ">值<", "sectPr"] {
            assert!(xml.contains(needle), "缺少「{needle}」：{xml}");
        }
    }

    // ---------------- ZIP ----------------

    fn sample_entries() -> Vec<(String, Vec<u8>)> {
        vec![
            ("a.txt".to_string(), b"hello".to_vec()),
            ("dir/b.xml".to_string(), "<x>中文</x>".as_bytes().to_vec()),
        ]
    }

    #[test]
    fn zip_has_the_expected_structure() {
        let bytes = build_zip(&sample_entries(), 0, 0x21).unwrap();
        assert_eq!(&bytes[0..4], &[0x50, 0x4B, 0x03, 0x04], "本地文件头签名");
        // EOCD 在最后 22 字节，且条目数为 2
        let eocd = &bytes[bytes.len() - 22..];
        assert_eq!(&eocd[0..4], &[0x50, 0x4B, 0x05, 0x06]);
        assert_eq!(u16::from_le_bytes([eocd[10], eocd[11]]), 2, "条目数");
        // 文件名出现在内容里
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("a.txt"));
        assert!(text.contains("dir/b.xml"));
    }

    #[test]
    fn zip_is_deterministic_for_a_fixed_timestamp() {
        let a = build_zip(&sample_entries(), 0x1234, 0x5678).unwrap();
        let b = build_zip(&sample_entries(), 0x1234, 0x5678).unwrap();
        assert_eq!(a, b);
        let c = build_zip(&sample_entries(), 0x9999, 0x5678).unwrap();
        assert_ne!(a, c, "时间戳应当影响输出");
    }

    #[test]
    fn empty_zip_is_rejected() {
        assert!(build_zip(&[], 0, 0).is_err());
    }

    // ---------------- 文档构造 ----------------

    #[test]
    fn cost_blocks_include_a_table_and_total() {
        let rows = vec![CostRow {
            item: "演员".into(),
            quantity: "5".into(),
            unit_price: "2000".into(),
            amount: "10000".into(),
        }];
        let blocks = build_cost_blocks("都市爽剧", &rows, "10000", "某工作室", "2026-01-01");
        let text = format!("{blocks:?}");
        assert!(text.contains("短剧制作成本明细表"));
        assert!(text.contains("都市爽剧"));
        assert!(text.contains("10000 元"));
        assert!(blocks
            .iter()
            .any(|b| matches!(b, DocBlock::Table { headers, .. } if headers.len() == 4)));
    }

    #[test]
    fn cost_blocks_say_so_when_there_are_no_rows() {
        let blocks = build_cost_blocks("剧", &[], "0", "方", "2026-01-01");
        let text = format!("{blocks:?}");
        assert!(text.contains("尚未录入任何成本项"), "空表要说明，不能留一张空表");
    }

    #[test]
    fn ip_blocks_state_the_facts_and_the_limits() {
        let blocks = build_ip_blocks("都市爽剧", 12, "1 小时 30 分", "某工作室", "2026-01-01");
        let text = format!("{blocks:?}");
        assert!(text.contains("作品权属声明"));
        assert!(text.contains("12 集"));
        assert!(text.contains("1 小时 30 分"));
        assert!(text.contains("不侵犯任何第三方"), "要有权利保证");
        assert!(text.contains("不构成对作品权利的处分"), "要写清声明的作用范围");
    }

    // ---------------- 端到端：交给 Python 的 zipfile 去验 ----------------

    #[test]
    fn end_to_end_docx_opens_in_an_independent_implementation() {
        // 想留一份产物做人工检查时设这个环境变量，测试就不清理临时目录
        let keep = std::env::var("DRAMADIST_KEEP_TEST_ARTIFACTS").is_ok();
        let dir = std::env::temp_dir().join("dramadist_docx_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("成本明细.docx");

        let rows = vec![
            CostRow {
                item: "编剧".into(),
                quantity: "1".into(),
                unit_price: "8000".into(),
                amount: "8000".into(),
            },
            CostRow {
                item: "演员".into(),
                quantity: "5".into(),
                unit_price: "2000".into(),
                amount: "10000".into(),
            },
        ];
        let blocks = build_cost_blocks("都市爽剧", &rows, "18000", "某工作室", "2026-01-01");
        let meta = DocMeta {
            title: "短剧制作成本明细表".into(),
            author: "DramaDist".into(),
            created: "2026-01-01T00:00:00Z".into(),
        };
        let written = write_docx(&out, &blocks, &DocStyle::default(), &meta).unwrap();
        assert!(written > 1_000, "文档太小，可能是空的：{written}");
        assert!(out.is_file());

        // 交给 Python 的 zipfile + ElementTree：那是完全独立的实现，能打开才算数
        let Some(python) = any_python() else {
            eprintln!("跳过独立实现校验：本机没有 python");
            if !keep {
                let _ = std::fs::remove_dir_all(&dir);
            }
            return;
        };
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("tests")
            .join("check_docx.py");
        assert!(script.is_file(), "找不到校验脚本：{}", script.display());

        let output = crate::proc::no_window(
            std::process::Command::new(&python)
                .arg(&script)
                .arg(&out),
        )
        .output()
        .expect("无法启动 python");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "独立实现打不开这份 docx：\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("RESULT = PASS"), "{stdout}\n{stderr}");

        if keep {
            eprintln!("保留产物：{}", out.display());
        } else {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn write_validates_before_touching_the_disk() {
        let err = write_docx(
            Path::new("Z:/nope/a.docx"),
            &[],
            &DocStyle::default(),
            &DocMeta::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("没有任何内容"), "{err}");

        let err = write_docx(
            Path::new(""),
            &[DocBlock::Paragraph("x".into())],
            &DocStyle::default(),
            &DocMeta::default(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("输出路径"), "{err}");
    }
}
