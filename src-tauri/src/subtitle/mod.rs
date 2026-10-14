//! 字幕：SRT 读写与按区间投影。
//!
//! 这里是**纯逻辑**，不碰数据库与进程，便于把边界情况测干净。
//! SRT 的解析必须容忍真实世界里的脏数据：BOM、CRLF、缺序号、逗号/点混用、
//! 多余空行、时间轴倒置 —— 播放器能忍的，我们也应该忍，但不能静默丢内容。

use crate::models::Cue;

/// 毫秒 → `HH:MM:SS,mmm`
pub fn ms_to_srt(ms: i64) -> String {
    let ms = ms.max(0);
    let h = ms / 3_600_000;
    let m = (ms % 3_600_000) / 60_000;
    let s = (ms % 60_000) / 1000;
    let milli = ms % 1000;
    format!("{h:02}:{m:02}:{s:02},{milli:03}")
}

/// `HH:MM:SS,mmm` / `HH:MM:SS.mmm` / `MM:SS,mmm` → 毫秒
pub fn parse_srt_time(text: &str) -> Option<i64> {
    let t = text.trim().replace('.', ",");
    let (hms, milli) = match t.split_once(',') {
        Some((a, b)) => (a, b),
        None => (t.as_str(), "0"),
    };
    let parts: Vec<&str> = hms.split(':').collect();
    let (h, m, s) = match parts.len() {
        3 => (parts[0], parts[1], parts[2]),
        2 => ("0", parts[0], parts[1]),
        _ => return None,
    };
    let h: i64 = h.trim().parse().ok()?;
    let m: i64 = m.trim().parse().ok()?;
    let s: i64 = s.trim().parse().ok()?;
    let milli_text = milli.trim();
    let milli: i64 = if milli_text.is_empty() {
        0
    } else {
        // 兼容 1 位/2 位毫秒写法
        let padded = format!("{milli_text:0<3}");
        padded[..3].parse().ok()?
    };
    Some(h * 3_600_000 + m * 60_000 + s * 1000 + milli)
}

/// 把一组字幕写成 SRT。写入前按起点排序并重排序号。
pub fn to_srt(cues: &[Cue]) -> String {
    let mut sorted: Vec<&Cue> = cues.iter().collect();
    sorted.sort_by_key(|c| (c.start_ms, c.end_ms));
    let mut out = String::new();
    for (i, cue) in sorted.iter().enumerate() {
        out.push_str(&(i + 1).to_string());
        out.push('\n');
        out.push_str(&format!(
            "{} --> {}\n",
            ms_to_srt(cue.start_ms),
            ms_to_srt(cue.end_ms.max(cue.start_ms))
        ));
        // 统一换行，去掉首尾空白行，避免播放器把空行当分隔
        let body = cue.text.replace("\r\n", "\n").replace('\r', "\n");
        let body = body.trim_matches('\n');
        out.push_str(body);
        out.push_str("\n\n");
    }
    out
}

/// 解析 SRT。返回的 `warnings` 记录被跳过的块与原因 —— 不静默丢内容。
pub fn parse_srt(text: &str) -> (Vec<Cue>, Vec<String>) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");

    let mut cues: Vec<Cue> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    for (block_index, block) in normalized.split("\n\n").enumerate() {
        let lines: Vec<&str> = block.lines().filter(|l| !l.trim().is_empty()).collect();
        if lines.is_empty() {
            continue;
        }

        // 定位含 "-->" 的那一行：它前面可能有、也可能没有序号
        let arrow_at = lines.iter().position(|l| l.contains("-->"));
        let Some(arrow_at) = arrow_at else {
            warnings.push(format!("第 {} 块没有时间轴，已跳过", block_index + 1));
            continue;
        };

        let arrow_line = lines[arrow_at];
        let (left, right) = match arrow_line.split_once("-->") {
            Some(pair) => pair,
            None => continue,
        };
        // 右侧可能跟着播放器私有参数（如 X1:... ），只取第一段
        let right = right.split_whitespace().next().unwrap_or("");
        let (Some(start_ms), Some(end_ms)) = (parse_srt_time(left), parse_srt_time(right)) else {
            warnings.push(format!(
                "第 {} 块时间轴无法解析（{arrow_line}），已跳过",
                block_index + 1
            ));
            continue;
        };

        let body = lines[arrow_at + 1..].join("\n");
        if body.trim().is_empty() {
            warnings.push(format!("第 {} 块没有文本，已跳过", block_index + 1));
            continue;
        }

        if end_ms < start_ms {
            warnings.push(format!(
                "第 {} 块时间轴倒置（{} → {}），已交换",
                block_index + 1,
                ms_to_srt(start_ms),
                ms_to_srt(end_ms)
            ));
        }

        cues.push(Cue {
            id: 0,
            media_id: 0,
            index_no: 0,
            start_ms: start_ms.min(end_ms),
            end_ms: start_ms.max(end_ms),
            text: body.trim_end().to_string(),
        });
    }

    cues.sort_by_key(|c| (c.start_ms, c.end_ms));
    for (i, cue) in cues.iter_mut().enumerate() {
        cue.index_no = i as i64 + 1;
    }
    (cues, warnings)
}

/// 把整片字幕投影到某个时间区间上（用于给切片配字幕）。
///
/// 规则：
/// - 与区间**无交集**的字幕丢弃；
/// - 跨越边界的字幕被裁剪到边界内，时间整体左移 `start_ms`；
/// - 裁剪后短于 `min_ms` 的碎片丢弃（默认 200ms，避免出现闪一下就走的字幕）。
pub fn project_range(cues: &[Cue], start_ms: i64, end_ms: i64, min_ms: i64) -> Vec<Cue> {
    if end_ms <= start_ms {
        return Vec::new();
    }
    let mut out: Vec<Cue> = Vec::new();
    for cue in cues {
        let s = cue.start_ms.max(start_ms);
        let e = cue.end_ms.min(end_ms);
        if e <= s {
            continue;
        }
        if e - s < min_ms {
            continue;
        }
        out.push(Cue {
            id: 0,
            media_id: cue.media_id,
            index_no: 0,
            start_ms: s - start_ms,
            end_ms: e - start_ms,
            text: cue.text.clone(),
        });
    }
    out.sort_by_key(|c| (c.start_ms, c.end_ms));
    for (i, cue) in out.iter_mut().enumerate() {
        cue.index_no = i as i64 + 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(start: i64, end: i64, text: &str) -> Cue {
        Cue {
            id: 0,
            media_id: 1,
            index_no: 0,
            start_ms: start,
            end_ms: end,
            text: text.to_string(),
        }
    }

    #[test]
    fn formats_timecodes() {
        assert_eq!(ms_to_srt(0), "00:00:00,000");
        assert_eq!(ms_to_srt(1), "00:00:00,001");
        assert_eq!(ms_to_srt(61_500), "00:01:01,500");
        assert_eq!(ms_to_srt(3_600_000), "01:00:00,000");
        // 负数按 0 处理，不产生 "-00:00:01"
        assert_eq!(ms_to_srt(-5), "00:00:00,000");
    }

    #[test]
    fn parses_timecodes_including_loose_forms() {
        assert_eq!(parse_srt_time("00:00:01,500"), Some(1_500));
        assert_eq!(parse_srt_time("00:00:01.500"), Some(1_500));
        assert_eq!(parse_srt_time("01:02:03,004"), Some(3_723_004));
        // 缺小时
        assert_eq!(parse_srt_time("02:03,500"), Some(123_500));
        // 毫秒位数不足
        assert_eq!(parse_srt_time("00:00:01,5"), Some(1_500));
        assert_eq!(parse_srt_time("garbage"), None);
    }

    #[test]
    fn round_trips_through_srt() {
        let cues = vec![cue(0, 1_500, "第一句"), cue(1_500, 4_000, "第二句\n第二行")];
        let text = to_srt(&cues);
        assert!(text.contains("1\n00:00:00,000 --> 00:00:01,500\n第一句"));
        let (parsed, warnings) = parse_srt(&text);
        assert!(warnings.is_empty(), "不该有告警：{warnings:?}");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].text, "第一句");
        assert_eq!(parsed[1].text, "第二句\n第二行");
        assert_eq!(parsed[1].start_ms, 1_500);
        assert_eq!(parsed[1].index_no, 2);
    }

    #[test]
    fn to_srt_sorts_and_renumbers() {
        let cues = vec![cue(5_000, 6_000, "后"), cue(1_000, 2_000, "前")];
        let text = to_srt(&cues);
        let (parsed, _) = parse_srt(&text);
        assert_eq!(parsed[0].text, "前");
        assert_eq!(parsed[0].index_no, 1);
        assert_eq!(parsed[1].text, "后");
    }

    #[test]
    fn tolerates_bom_crlf_and_missing_indices() {
        let raw = "\u{feff}1\r\n00:00:01,000 --> 00:00:02,000\r\n你好\r\n\r\n00:00:03,000 --> 00:00:04,000\r\n世界\r\n";
        let (cues, warnings) = parse_srt(raw);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[1].text, "世界");
        assert_eq!(cues[1].index_no, 2);
    }

    #[test]
    fn skips_bad_blocks_but_reports_them() {
        let raw = "1\n00:00:01,000 --> 00:00:02,000\n好的\n\n2\n这不是时间轴\n坏块\n\n3\n00:00:05,000 --> 00:00:06,000\n";
        let (cues, warnings) = parse_srt(raw);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "好的");
        // 无时间轴的块 + 无文本的块，都要有告警
        assert_eq!(warnings.len(), 2, "{warnings:?}");
    }

    #[test]
    fn swaps_inverted_timecodes_with_warning() {
        let raw = "1\n00:00:05,000 --> 00:00:02,000\n倒置\n";
        let (cues, warnings) = parse_srt(raw);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start_ms, 2_000);
        assert_eq!(cues[0].end_ms, 5_000);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn ignores_player_private_parameters_after_timeline() {
        let raw = "1\n00:00:01,000 --> 00:00:02,000 X1:0 X2:100\n带参数\n";
        let (cues, _) = parse_srt(raw);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start_ms, 1_000);
        assert_eq!(cues[0].end_ms, 2_000);
    }

    #[test]
    fn project_range_clips_and_shifts() {
        let cues = vec![
            cue(0, 1_000, "被切掉前半"),
            cue(1_000, 3_000, "完整在内"),
            cue(3_500, 5_000, "跨越尾部"),
            cue(9_000, 10_000, "完全在外"),
        ];
        let out = project_range(&cues, 500, 4_000, 200);
        assert_eq!(out.len(), 3);
        // 第一条左移后起点为 0
        assert_eq!((out[0].start_ms, out[0].end_ms), (0, 500));
        assert_eq!(out[0].text, "被切掉前半");
        assert_eq!((out[1].start_ms, out[1].end_ms), (500, 2_500));
        // 跨越尾部的被裁到区间末尾
        assert_eq!((out[2].start_ms, out[2].end_ms), (3_000, 3_500));
        assert_eq!(out[2].index_no, 3);
    }

    #[test]
    fn project_range_drops_slivers() {
        let cues = vec![cue(1_000, 1_050, "只有 50ms")];
        assert!(project_range(&cues, 0, 5_000, 200).is_empty());
        // 区间非法时返回空，而不是 panic
        assert!(project_range(&cues, 5_000, 5_000, 200).is_empty());
        assert!(project_range(&cues, 9_000, 1_000, 200).is_empty());
    }

    #[test]
    fn project_range_keeps_cue_exactly_at_boundary() {
        let cues = vec![cue(5_000, 6_000, "正好在起点")];
        let out = project_range(&cues, 5_000, 8_000, 200);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].start_ms, out[0].end_ms), (0, 1_000));
    }
}
