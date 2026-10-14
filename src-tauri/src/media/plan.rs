//! 切片计划：**纯函数**，不碰文件系统与进程，便于测试。
//!
//! 三种模式的共同保证：
//! - 结果按 index 从 1 连续编号；
//! - 相邻片段首尾严格闭合，不重叠、不留缝；
//! - 短于 `MIN_SEGMENT_MS` 的碎片不会单独成片（尾部并入上一片）。

use crate::models::{SliceMode, SlicePlan};

/// 小于这个长度的片段没有发布价值，单独成片只会产生垃圾文件。
pub const MIN_SEGMENT_MS: i64 = 500;

/// 每 N 秒一片。尾部不足 `MIN_SEGMENT_MS` 时并入上一片。
pub fn plan_every_seconds(total_ms: i64, seconds: f64) -> Vec<SlicePlan> {
    if total_ms <= 0 || !seconds.is_finite() || seconds <= 0.0 {
        return Vec::new();
    }
    let step = (seconds * 1000.0).round() as i64;
    if step < MIN_SEGMENT_MS {
        return Vec::new();
    }

    let mut out: Vec<SlicePlan> = Vec::new();
    let mut start = 0i64;
    while start < total_ms {
        let end = (start + step).min(total_ms);
        out.push(SlicePlan {
            index: out.len() as i64 + 1,
            start_ms: start,
            end_ms: end,
        });
        start = end;
    }

    merge_short_tail(&mut out, total_ms);
    out
}

/// 总时长等分 N 份，余数毫秒均摊到前若干片，保证首尾精确闭合。
pub fn plan_equal_parts(total_ms: i64, parts: i64) -> Vec<SlicePlan> {
    if total_ms <= 0 || parts <= 0 {
        return Vec::new();
    }
    let base = total_ms / parts;
    if base < MIN_SEGMENT_MS {
        // 份数远超时长，等分没有意义；交由调用方提示用户
        return Vec::new();
    }
    let remainder = total_ms % parts;

    let mut out = Vec::with_capacity(parts as usize);
    let mut start = 0i64;
    for i in 0..parts {
        let len = base + if i < remainder { 1 } else { 0 };
        let end = start + len;
        out.push(SlicePlan {
            index: i + 1,
            start_ms: start,
            end_ms: end,
        });
        start = end;
    }

    merge_short_tail(&mut out, total_ms);
    out
}

/// 用户给定切点：自动排序、去重、裁剪到 [0, total]，并补齐首尾。
pub fn plan_manual(total_ms: i64, points: &[i64]) -> Vec<SlicePlan> {
    if total_ms <= 0 {
        return Vec::new();
    }
    let mut cuts: Vec<i64> = points
        .iter()
        .copied()
        .filter(|p| *p > 0 && *p < total_ms)
        .collect();
    cuts.sort_unstable();
    cuts.dedup();

    let mut bounds = Vec::with_capacity(cuts.len() + 2);
    bounds.push(0);
    bounds.extend(cuts.iter().copied());
    bounds.push(total_ms);

    let mut out = Vec::new();
    for pair in bounds.windows(2) {
        let (s, e) = (pair[0], pair[1]);
        if e - s < MIN_SEGMENT_MS {
            continue;
        }
        out.push(SlicePlan {
            index: out.len() as i64 + 1,
            start_ms: s,
            end_ms: e,
        });
    }
    out
}

pub fn build_plan(total_ms: i64, mode: &SliceMode) -> Vec<SlicePlan> {
    match mode {
        SliceMode::EverySeconds { seconds } => plan_every_seconds(total_ms, *seconds),
        SliceMode::EqualParts { parts } => plan_equal_parts(total_ms, *parts),
        SliceMode::Manual { points } => plan_manual(total_ms, points),
    }
}

/// 尾片过短就并进上一片（上一片的 end 直接拉到总时长）。
fn merge_short_tail(out: &mut Vec<SlicePlan>, total_ms: i64) {
    if out.len() < 2 {
        return;
    }
    let too_short = out
        .last()
        .map(|s| s.duration_ms() < MIN_SEGMENT_MS)
        .unwrap_or(false);
    if too_short {
        out.pop();
        if let Some(prev) = out.last_mut() {
            prev.end_ms = total_ms;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 通用不变量：连续、无缝、无重叠、编号从 1 开始
    fn assert_contiguous(plans: &[SlicePlan], total_ms: i64) {
        assert!(!plans.is_empty(), "计划不应为空");
        assert_eq!(plans[0].start_ms, 0, "必须从 0 开始");
        assert_eq!(
            plans.last().unwrap().end_ms,
            total_ms,
            "必须正好结束在总时长"
        );
        for (i, p) in plans.iter().enumerate() {
            assert_eq!(p.index, i as i64 + 1, "编号必须连续");
            assert!(p.duration_ms() > 0, "片段长度必须为正");
            if i > 0 {
                assert_eq!(p.start_ms, plans[i - 1].end_ms, "相邻片段必须闭合");
            }
        }
    }

    #[test]
    fn every_seconds_covers_whole_video() {
        let plans = plan_every_seconds(600_000, 60.0);
        assert_eq!(plans.len(), 10);
        assert_contiguous(&plans, 600_000);
        assert!(plans.iter().all(|p| p.duration_ms() == 60_000));
    }

    #[test]
    fn every_seconds_merges_short_tail() {
        // 6400ms，每 3000ms 一片 → 尾片只剩 400ms，应并入上一片
        let plans = plan_every_seconds(6_400, 3.0);
        assert_eq!(plans.len(), 2);
        assert_contiguous(&plans, 6_400);
        assert_eq!(plans[1].duration_ms(), 3_400);
    }

    #[test]
    fn every_seconds_keeps_tail_at_threshold() {
        // 尾片正好 500ms（== MIN），保留
        let plans = plan_every_seconds(6_500, 3.0);
        assert_eq!(plans.len(), 3);
        assert_contiguous(&plans, 6_500);
    }

    #[test]
    fn every_seconds_rejects_degenerate_input() {
        assert!(plan_every_seconds(0, 60.0).is_empty());
        assert!(plan_every_seconds(-1, 60.0).is_empty());
        assert!(plan_every_seconds(100_000, 0.0).is_empty());
        assert!(plan_every_seconds(100_000, -5.0).is_empty());
        assert!(plan_every_seconds(100_000, f64::NAN).is_empty());
        // 步长不足 MIN_SEGMENT_MS
        assert!(plan_every_seconds(100_000, 0.1).is_empty());
    }

    #[test]
    fn equal_parts_sums_exactly_and_is_contiguous() {
        let total = 1_000_003; // 除不尽，考验余数分配
        let plans = plan_equal_parts(total, 7);
        assert_eq!(plans.len(), 7);
        assert_contiguous(&plans, total);
        let sum: i64 = plans.iter().map(|p| p.duration_ms()).sum();
        assert_eq!(sum, total);
        // 余数均摊：前 3 片比后面长 1ms
        assert_eq!(plans[0].duration_ms() - plans[6].duration_ms(), 1);
    }

    #[test]
    fn equal_parts_rejects_too_many_parts() {
        assert!(plan_equal_parts(1_000, 100).is_empty());
        assert!(plan_equal_parts(1_000, 0).is_empty());
        assert!(plan_equal_parts(0, 3).is_empty());
    }

    #[test]
    fn manual_sorts_dedups_and_clamps() {
        let total = 10_000;
        let plans = plan_manual(total, &[5_000, 2_000, 5_000, -100, 99_999, 0, total]);
        // 非法切点被丢弃、重复被去掉 → 只剩 2000 / 5000
        assert_eq!(plans.len(), 3);
        assert_contiguous(&plans, total);
        assert_eq!(plans[0].end_ms, 2_000);
        assert_eq!(plans[1].start_ms, 2_000);
        assert_eq!(plans[1].end_ms, 5_000);
    }

    #[test]
    fn manual_drops_fragments_below_minimum() {
        let plans = plan_manual(10_000, &[200, 9_900]);
        // 200ms 与 100ms 的碎片都被丢弃，只剩中间那片
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0].start_ms, 200);
        assert_eq!(plans[0].end_ms, 9_900);
        assert_eq!(plans[0].index, 1);
    }

    #[test]
    fn manual_with_no_points_yields_single_segment() {
        let plans = plan_manual(10_000, &[]);
        assert_eq!(plans.len(), 1);
        assert_contiguous(&plans, 10_000);
    }

    #[test]
    fn build_plan_dispatches_by_mode() {
        let a = build_plan(120_000, &SliceMode::EverySeconds { seconds: 60.0 });
        assert_eq!(a.len(), 2);

        let b = build_plan(120_000, &SliceMode::EqualParts { parts: 4 });
        assert_eq!(b.len(), 4);

        let c = build_plan(120_000, &SliceMode::Manual { points: vec![30_000] });
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn mode_deserializes_from_frontend_shape() {
        let m: SliceMode = serde_json::from_str(r#"{"mode":"everySeconds","seconds":60}"#).unwrap();
        assert!(matches!(m, SliceMode::EverySeconds { seconds } if (seconds - 60.0).abs() < 1e-9));

        let m: SliceMode = serde_json::from_str(r#"{"mode":"equalParts","parts":8}"#).unwrap();
        assert!(matches!(m, SliceMode::EqualParts { parts: 8 }));

        let m: SliceMode = serde_json::from_str(r#"{"mode":"manual","points":[1000,2000]}"#).unwrap();
        assert!(matches!(m, SliceMode::Manual { points } if points == vec![1000, 2000]));
    }
}
