//! 本地硬字幕去除。
//!
//! 真实软件走的是**开拍 `videoscreenclear` 在线服务**（日志里能看到
//! `已提交异步任务 task_id=t_...`）。但那条路要求接口文档 + 额度 + 网络。
//!
//! 这里走**本地**：
//!
//! 1. 抽若干帧 → OCR 找出画面里的文字框
//! 2. 取底部那些框的**并集**（硬字幕就在底部一条带上）
//! 3. ffmpeg `delogo` 用周围像素把那一条盖掉
//!
//! 不需要任何在线服务，也不用手填坐标 —— 位置是**扫出来的**。
//!
//! ## 为什么不用固定坐标
//!
//! 每部剧、每一集字幕的位置和字号都可能不一样。写死一个「底部 15%」
//! 要么盖不住字（留一道残影），要么盖掉一大块画面。
//! OCR 扫一遍就知道真实位置，代价是多抽几帧。

use serde::{Deserialize, Serialize};

use crate::models::OcrFrame;

/// 字幕带的位置（源画面像素坐标）。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubtitleBand {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
    /// 有多少帧在这个位置看到了字 —— 用来判断可信度
    pub hit_frames: usize,
    /// 一共抽了多少帧
    pub total_frames: usize,
}

impl SubtitleBand {
    /// 覆盖了抽样的多大比例。太低说明可能只是偶然扫到一行字。
    pub fn coverage(&self) -> f64 {
        if self.total_frames == 0 {
            0.0
        } else {
            self.hit_frames as f64 / self.total_frames as f64
        }
    }
}

/// 只在画面**下面这个比例**里找字幕。
///
/// 硬字幕在竖屏短剧里几乎都在底部；取 0.45 是留足余量 ——
/// 有的片子字幕偏上，卡太死会漏。
const SEARCH_BOTTOM_RATIO: f64 = 0.45;

/// 向外扩一点，别让字的抗锯齿边缘漏出来。
/// 按画面高度的比例给，不同分辨率下观感一致。
const PAD_RATIO: f64 = 0.006;

/// 至少有这么多帧看到字，才认为「确实有硬字幕」。
///
/// 不能要求 100%：有的帧刚好是空镜 / 转场，本来就没字幕。
const MIN_COVERAGE: f64 = 0.25;

/// 从 OCR 结果里算出字幕带。找不到就返回 `None`。
///
/// ## 为什么是「最密的一簇」而不是「全部并集」
///
/// 第一版用全部并集，**实测炸了**：一部剧里 OCR 会扫到各种位置的文字
/// （字幕、招牌、画面里的字）。并集出来 269px 高 —— 而字幕只有一行 60px。
/// `delogo` 在那 269px 上做插值，抹出一道横贯画面的拖影，比原来的字幕还难看。
///
/// 正确做法：**字幕是出现最频繁的那个位置**。按纵向中心聚簇，
/// 取最大的一簇，只盖它。
pub fn subtitle_band(frames: &[OcrFrame], frame_w: i64, frame_h: i64) -> Option<SubtitleBand> {
    if frames.is_empty() || frame_w <= 0 || frame_h <= 0 {
        return None;
    }
    let bottom_limit = frame_h as f64 * (1.0 - SEARCH_BOTTOM_RATIO);

    // 先把所有合格的框收起来
    let mut boxes: Vec<(f64, f64, f64, f64)> = Vec::new();
    let mut frames_with_hit = vec![false; frames.len()];
    for (fi, frame) in frames.iter().enumerate() {
        for b in &frame.boxes {
            // 只要**整体在下方**的框。跨过中线的多半是画面内容被误认成字。
            if b.y1 < bottom_limit {
                continue;
            }
            if !(b.x1.is_finite() && b.y1.is_finite() && b.x2.is_finite() && b.y2.is_finite()) {
                continue;
            }
            if b.x2 <= b.x1 || b.y2 <= b.y1 {
                continue;
            }
            boxes.push((b.x1, b.y1, b.x2, b.y2));
            frames_with_hit[fi] = true;
        }
    }
    if boxes.is_empty() {
        return None;
    }

    // **以中位数为基准剔除离群点**，再取剩下的并集。
    //
    // 真实素材上的实测数据（1080×1920，一部剧抽 42 帧）：
    //
    //   24 个框落在 y 1288~1379  ← 就是那一行字幕，高 91px
    //    1 个框落在 y 1524~1557  ← 某帧的误检（画面里的字被当成字幕）
    //
    // 那一个误检把并集从 91px 拖到 269px，`delogo` 在 269px 上插值 = 一道
    // 横贯画面的拖影，比原字幕还难看。
    //
    // 聚簇试过、不够：字间距和误检间距会连成一片。用中位数最稳 ——
    // 字幕是**出现最频繁的那个位置**，中位数天然落在它上面。
    let mut centers: Vec<f64> = boxes.iter().map(|b| (b.1 + b.3) / 2.0).collect();
    centers.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = centers[centers.len() / 2];

    // 允许偏离中位数多少：两行字幕的行距大约就这么高。
    // 取 5% = 96px（1920 上）—— 够容纳换行的第二行，又排除得了误检。
    let tolerance = frame_h as f64 * 0.05;
    let kept: Vec<&(f64, f64, f64, f64)> = boxes
        .iter()
        .filter(|b| ((b.1 + b.3) / 2.0 - median).abs() <= tolerance)
        .collect();
    if kept.is_empty() {
        return None;
    }
    let hit_frames = frames_with_hit.iter().filter(|h| **h).count();

    let x1 = kept.iter().map(|b| b.0).fold(f64::MAX, f64::min);
    let y1 = kept.iter().map(|b| b.1).fold(f64::MAX, f64::min);
    let x2 = kept.iter().map(|b| b.2).fold(f64::MIN, f64::max);
    let y2 = kept.iter().map(|b| b.3).fold(f64::MIN, f64::max);

    let pad = (frame_h as f64 * PAD_RATIO).round().max(2.0);
    let px1 = (x1 - pad).max(0.0);
    let py1 = (y1 - pad).max(0.0);
    let px2 = (x2 + pad).min(frame_w as f64);
    let py2 = (y2 + pad).min(frame_h as f64);
    if px2 <= px1 || py2 <= py1 {
        return None;
    }

    let band = SubtitleBand {
        x: px1.round() as i64,
        y: py1.round() as i64,
        w: (px2 - px1).round() as i64,
        h: (py2 - py1).round() as i64,
        hit_frames: frames_with_hit.iter().filter(|h| **h).count(),
        total_frames: frames.len(),
    };

    if band.coverage() < MIN_COVERAGE {
        return None;
    }
    Some(band)
}

/// `delogo` 的滤镜串。
///
/// 为什么用 `delogo` 而不是 `boxblur`/`drawbox`：
/// `delogo` 是**用四周的像素往中间插值**，不是糊一块黑或高斯模糊 ——
/// 在纯色/渐变背景上几乎看不出来。代价是它要求整个框都在画面内。
pub fn delogo_filter(band: &SubtitleBand) -> String {
    format!("delogo=x={}:y={}:w={}:h={}", band.x, band.y, band.w, band.h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::OcrBox;

    fn frame(y1: f64, y2: f64, x1: f64, x2: f64) -> OcrFrame {
        OcrFrame {
            time_ms: 0,
            text: "你好".into(),
            boxes: vec![OcrBox { x1, y1, x2, y2 }],
        }
    }

    /// 底部一条字幕：并集要盖住所有帧里出现过的字。
    #[test]
    fn band_covers_every_subtitle_line() {
        let frames = vec![
            frame(1600.0, 1700.0, 300.0, 700.0),
            frame(1600.0, 1700.0, 200.0, 800.0), // 这一句更长
            frame(1600.0, 1700.0, 400.0, 600.0),
        ];
        let b = subtitle_band(&frames, 1080, 1920).expect("该找到字幕带");
        // 左右要取并集（200~800），不是平均
        assert!(b.x <= 200, "左边要盖到最宽那句：{}", b.x);
        assert!(b.x + b.w >= 800, "右边也要：{}", b.x + b.w);
        assert_eq!(b.hit_frames, 3);
        assert!((b.coverage() - 1.0).abs() < 1e-9);
    }

    /// **画面中上部的字不算字幕。** 片名、水印都在上面，盖掉就毁画面了。
    #[test]
    fn boxes_above_the_middle_are_ignored() {
        let frames = vec![frame(100.0, 180.0, 200.0, 900.0)];
        assert!(
            subtitle_band(&frames, 1080, 1920).is_none(),
            "画面顶部的字不该被当成硬字幕"
        );
    }

    /// 偶尔一帧扫到不算数 —— 可能是误识别。
    #[test]
    fn a_single_hit_among_many_frames_is_not_enough() {
        let mut frames = vec![frame(1600.0, 1700.0, 300.0, 700.0)];
        for _ in 0..9 {
            frames.push(OcrFrame {
                time_ms: 0,
                text: String::new(),
                boxes: vec![],
            });
        }
        assert!(
            subtitle_band(&frames, 1080, 1920).is_none(),
            "10 帧里只有 1 帧扫到（10%），低于 25% 的门槛，不该认定有硬字幕"
        );
    }

    /// 换行字幕：两行都要盖住。
    #[test]
    fn multi_line_subtitles_are_covered_together() {
        let frames = vec![OcrFrame {
            time_ms: 0,
            text: "第一行 第二行".into(),
            boxes: vec![
                OcrBox { x1: 300.0, y1: 1560.0, x2: 780.0, y2: 1620.0 },
                OcrBox { x1: 260.0, y1: 1630.0, x2: 820.0, y2: 1690.0 },
            ],
        }];
        let b = subtitle_band(&frames, 1080, 1920).expect("该找到");
        assert!(b.y <= 1560 && b.y + b.h >= 1690, "两行都要在里面：{b:?}");
    }

    /// 算出来的框必须在画面内 —— `delogo` 越界会直接报错。
    #[test]
    fn band_never_leaves_the_frame() {
        let frames = vec![OcrFrame {
            time_ms: 0,
            text: "贴边的字".into(),
            boxes: vec![OcrBox { x1: 0.0, y1: 1900.0, x2: 1080.0, y2: 1920.0 }],
        }];
        let b = subtitle_band(&frames, 1080, 1920).expect("该找到");
        assert!(b.x >= 0 && b.y >= 0, "{b:?}");
        assert!(b.x + b.w <= 1080, "右边越界了：{b:?}");
        assert!(b.y + b.h <= 1920, "下边越界了：{b:?}");
    }

    /// 没有字就没有字幕带。
    #[test]
    fn no_boxes_means_no_band() {
        let frames = vec![OcrFrame {
            time_ms: 0,
            text: String::new(),
            boxes: vec![],
        }];
        assert!(subtitle_band(&frames, 1080, 1920).is_none());
        assert!(subtitle_band(&[], 1080, 1920).is_none());
    }

    /// 脏数据不能让整件事崩掉。
    #[test]
    fn garbage_boxes_are_discarded() {
        let frames = vec![OcrFrame {
            time_ms: 0,
            text: "坏框".into(),
            boxes: vec![
                OcrBox { x1: f64::NAN, y1: 1600.0, x2: 700.0, y2: 1700.0 },
                OcrBox { x1: 700.0, y1: 1700.0, x2: 300.0, y2: 1600.0 }, // 反的
                OcrBox { x1: 300.0, y1: 1600.0, x2: 700.0, y2: 1700.0 }, // 好的
            ],
        }];
        let b = subtitle_band(&frames, 1080, 1920).expect("好框还在，该找到");
        assert!(b.x >= 280 && b.x <= 300, "只有好框参与：{b:?}");
    }

    #[test]
    fn delogo_filter_is_well_formed() {
        let b = SubtitleBand {
            x: 10,
            y: 1600,
            w: 800,
            h: 100,
            hit_frames: 3,
            total_frames: 3,
        };
        assert_eq!(delogo_filter(&b), "delogo=x=10:y=1600:w=800:h=100");
    }
}
