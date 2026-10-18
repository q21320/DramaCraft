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

/// 一个像素框：左上 + 右下。
type Rect = (f64, f64, f64, f64);

/// 框的纵向中心。
fn center_y(r: &Rect) -> f64 {
    (r.1 + r.3) / 2.0
}

/// 画面里所有要擦的文字区域：字幕 + 水印/台标。
///
/// ## 为什么不是「全部并集」
///
/// 第一版用全部并集，**实测炸了**：一部剧里 OCR 会扫到各种位置的文字
/// （字幕、招牌、画面里的字）。并集出来 269px 高 —— 而字幕只有一行 60px。
/// `delogo` 在那 269px 上做插值，抹出一道横贯画面的拖影，比原来的字幕还难看。
///
/// ## 为什么也不止「最密的一簇」
///
/// 用户的原话是「字幕 水印都要擦的」。真实素材（1280×716）OCR 出来是两簇：
///
///     字幕「行动僵硬迟缓」   x 520~761，y 593~632  ← 居中
///     水印「唐余安漫居」     x 1072~1279，y 648~696 ← 右下角，每帧都有
///
/// 两簇的纵向中心只差 60px，碰巧大于聚簇容差（716×0.05 = 35.8px），能分开。
/// 旧版只留一簇、按中位数挑，水印框数更多就把字幕挤掉了，
/// 结果 `delogo` 只擦水印、字幕原样留着 —— 用户看 quzimu 和原片一模一样。
///
/// 所以：**按纵向中心聚簇，每簇各出一个框**。但簇不能随便多 ——
/// 招牌、画面里的字也会成簇，擦掉就是毁画面。放行规则：
///
/// - **居中**（不贴左右边）的簇：只取一个最像字幕的（命中帧最多、最靠中间）
/// - **边角且窄**的簇：水印/台标，全取
///
/// 返回顺序：字幕在前，水印按命中帧数从多到少。
pub fn text_bands(frames: &[OcrFrame], frame_w: i64, frame_h: i64) -> Vec<SubtitleBand> {
    // 画面小到留不出 1px 边距时 `delogo` 根本用不了，直接当「没有文字」
    if frames.is_empty() || frame_w < 3 || frame_h < 3 {
        return Vec::new();
    }
    let bottom_limit = frame_h as f64 * (1.0 - SEARCH_BOTTOM_RATIO);
    let fw = frame_w as f64;

    // 先把所有合格的框收起来，记着它在第几帧
    let mut items: Vec<(usize, Rect)> = Vec::new();
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
            items.push((fi, (b.x1, b.y1, b.x2, b.y2)));
        }
    }
    if items.is_empty() {
        return Vec::new();
    }

    // **按纵向中心聚簇。**
    //
    // 容差 5% = 96px（1920 上）：够容纳换行的第二行，又分得开字幕和水印。
    // 和旧版的中位数容差一致，只是从「只留一簇」变成「每簇一个框」。
    //
    // 基准用簇内**均值**而不是首元素：单链式聚簇会把
    // 「字幕 1590 → 招牌 1680」一路连成一串。
    items.sort_by(|a, b| {
        center_y(&a.1)
            .partial_cmp(&center_y(&b.1))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let tol = frame_h as f64 * 0.05;
    let mut clusters: Vec<(Vec<(usize, Rect)>, f64)> = Vec::new();
    for item in items {
        let c = center_y(&item.1);
        match clusters.last_mut() {
            Some((list, sum)) if (c - *sum / list.len() as f64).abs() <= tol => {
                *sum += c;
                list.push(item);
            }
            _ => clusters.push((vec![item], c)),
        }
    }

    // 每簇取并集 → 一个框
    let pad = (frame_h as f64 * PAD_RATIO).round().max(2.0);
    let mut candidates: Vec<SubtitleBand> = Vec::new();
    for (list, _) in clusters {
        let mut hit = vec![false; frames.len()];
        let (mut x1, mut y1, mut x2, mut y2) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (fi, r) in &list {
            hit[*fi] = true;
            x1 = x1.min(r.0);
            y1 = y1.min(r.1);
            x2 = x2.max(r.2);
            y2 = y2.max(r.3);
        }
        let hits = hit.iter().filter(|h| **h).count();
        if let Some(band) = make_band(x1, y1, x2, y2, pad, frame_w, frame_h, hits, frames.len()) {
            candidates.push(band);
        }
    }
    if candidates.is_empty() {
        return Vec::new();
    }

    // **边角且窄 = 水印/台标；居中 = 字幕。**
    //
    // 两簇的纵向位置挨得太近，分不开；横向位置才可靠：
    // 字幕是居中的，水印/台标贴左右边角且窄。
    let narrow_w = fw * 0.3;
    let is_corner = |b: &SubtitleBand| {
        let cx = b.x as f64 + b.w as f64 / 2.0;
        (b.w as f64) < narrow_w && (cx < fw * 0.25 || cx > fw * 0.75)
    };
    let centrality = |b: &SubtitleBand| (b.x as f64 + b.w as f64 / 2.0 - fw / 2.0).abs();

    let mut subtitle: Option<SubtitleBand> = None;
    let mut overlays: Vec<SubtitleBand> = Vec::new();
    for b in candidates {
        if is_corner(&b) {
            overlays.push(b);
            continue;
        }
        // 居中的簇只留一个：命中帧多的优先，一样多就选更靠中间的
        let better = match &subtitle {
            None => true,
            Some(cur) => {
                b.hit_frames > cur.hit_frames
                    || (b.hit_frames == cur.hit_frames && centrality(&b) < centrality(cur))
            }
        };
        if better {
            subtitle = Some(b);
        }
    }

    overlays.sort_by(|a, b| b.hit_frames.cmp(&a.hit_frames));
    let mut bands: Vec<SubtitleBand> = Vec::with_capacity(overlays.len() + 1);
    if let Some(b) = subtitle {
        bands.push(b);
    }
    bands.extend(overlays);
    // 两个框挨得太近（padding 后重叠）就并成一个 —— 同一块擦两遍会更糊
    merge_overlapping(bands)
}

/// 只要一个框的旧接口：字幕优先，没有居中字幕时退回边角文字。
#[cfg(test)]
fn subtitle_band(frames: &[OcrFrame], frame_w: i64, frame_h: i64) -> Option<SubtitleBand> {
    text_bands(frames, frame_w, frame_h).into_iter().next()
}

/// 给一簇框扩边、夹进画面、取整，得到一个能直接喂 `delogo` 的框。
///
/// **框的四周都要留 1px。**
///
/// `delogo` 的硬约束（本机 ffmpeg 实测：x=0、y=0、x+w=宽、y+h=高 四种
/// 都直接报 "Logo area is outside of the frame."，整集失败）：
///
///     x ≥ 1、y ≥ 1、x+w ≤ 宽-1、y+h ≤ 高-1
///
/// 以前只把框裁到 [0, 宽]：竖屏剧字幕在画面中间，从没贴过边；换一部
/// 横屏剧、OCR 的框碰到右边界（RapidOCR 映射回原图坐标时能到边界），
/// 整部剧的去字幕就全挂 —— 报错还只有一句 "Logo area is outside"。
fn make_band(
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    pad: f64,
    frame_w: i64,
    frame_h: i64,
    hit_frames: usize,
    total_frames: usize,
) -> Option<SubtitleBand> {
    let px1 = (x1 - pad).max(1.0);
    let py1 = (y1 - pad).max(1.0);
    let px2 = (x2 + pad).min(frame_w as f64 - 1.0);
    let py2 = (y2 + pad).min(frame_h as f64 - 1.0);
    if px2 <= px1 || py2 <= py1 {
        return None;
    }

    // **先取整，再用取整后的左右/上下算宽高。**
    //
    // 之前是分别对左边界、宽度四舍五入：px1=0.5、px2=1080 时
    // x=1、w=round(1079.5)=1080 —— x+w=1081，`delogo` 直接报
    // "Logo area is outside of the frame."，整集失败。
    // 取整后再相减，宽高天然不会多出那一像素。
    let x = px1.round() as i64;
    let y = py1.round() as i64;
    let right = px2.round() as i64;
    let bottom = py2.round() as i64;
    let w = (right - x).min(frame_w - 1 - x);
    let h = (bottom - y).min(frame_h - 1 - y);
    if x < 1 || y < 1 || w <= 0 || h <= 0 {
        return None;
    }
    let band = SubtitleBand {
        x,
        y,
        w,
        h,
        hit_frames,
        total_frames,
    };
    // 偶尔一帧扫到不算数 —— 低于门槛的簇不擦
    if band.coverage() < MIN_COVERAGE {
        return None;
    }
    Some(band)
}

/// 两个框有重叠就并成一个。
fn merge_overlapping(mut bands: Vec<SubtitleBand>) -> Vec<SubtitleBand> {
    let overlaps = |a: &SubtitleBand, b: &SubtitleBand| {
        a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h
    };
    let mut merged = true;
    while merged {
        merged = false;
        'scan: for i in 0..bands.len() {
            for j in (i + 1)..bands.len() {
                if !overlaps(&bands[i], &bands[j]) {
                    continue;
                }
                let x = bands[i].x.min(bands[j].x);
                let y = bands[i].y.min(bands[j].y);
                let right = (bands[i].x + bands[i].w).max(bands[j].x + bands[j].w);
                let bottom = (bands[i].y + bands[i].h).max(bands[j].y + bands[j].h);
                // 命中帧取小的那个：宁可少报覆盖率，不要吹
                let hit_frames = bands[i].hit_frames.min(bands[j].hit_frames);
                let total_frames = bands[i].total_frames;
                bands[i] = SubtitleBand {
                    x,
                    y,
                    w: right - x,
                    h: bottom - y,
                    hit_frames,
                    total_frames,
                };
                bands.remove(j);
                merged = true;
                break 'scan;
            }
        }
    }
    bands
}

/// `delogo` 的滤镜串。
///
/// 多个区域直接逗号串起来 —— ffmpeg 按顺序一块一块擦。
///
/// 为什么用 `delogo` 而不是 `boxblur`/`drawbox`：
/// `delogo` 是**用四周的像素往中间插值**，不是糊一块黑或高斯模糊 ——
/// 在纯色/渐变背景上几乎看不出来。代价是它要求整个框都在画面内。
pub fn delogo_filter(bands: &[SubtitleBand]) -> String {
    bands
        .iter()
        .map(|b| format!("delogo=x={}:y={}:w={}:h={}", b.x, b.y, b.w, b.h))
        .collect::<Vec<_>>()
        .join(",")
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

    /// **贴边就被拒。** 实测 `delogo` 要求 x≥1、y≥1、x+w≤宽-1、y+h≤高-1：
    /// 1280×700 上 x=1027、w=253（x+w=1280）直接报
    /// "Logo area is outside of the frame."，整集失败。
    /// OCR 的框偶尔会顶到画面最右/最下，这条保证算出来的框永远不贴边。
    #[test]
    fn band_keeps_one_pixel_from_every_edge() {
        let frames = vec![OcrFrame {
            time_ms: 0,
            text: "贴边".into(),
            boxes: vec![OcrBox { x1: 1020.0, y1: 640.0, x2: 1280.0, y2: 700.0 }],
        }];
        let b = subtitle_band(&frames, 1280, 700).expect("该找到");
        assert!(b.x >= 1 && b.y >= 1, "上/左要留 1px：{b:?}");
        assert!(b.x + b.w <= 1279, "右边要留 1px：{b:?}");
        assert!(b.y + b.h <= 699, "下边要留 1px：{b:?}");
    }

    /// 框从画面左上角开始也一样：x=0 / y=0 同样被 `delogo` 拒。
    #[test]
    fn band_keeps_one_pixel_at_the_top_left() {
        let frames = vec![OcrFrame {
            time_ms: 0,
            text: "左上".into(),
            boxes: vec![OcrBox { x1: 0.0, y1: 500.0, x2: 300.0, y2: 560.0 }],
        }];
        let b = subtitle_band(&frames, 1280, 700).expect("该找到");
        assert!(b.x >= 1 && b.y >= 1, "{b:?}");
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

    /// 半像素边界也不能越界：分别四舍五入曾把 x+w 撑大 1px。
    #[test]
    fn rounding_never_pushes_the_band_past_the_edge() {
        let frames = vec![OcrFrame {
            time_ms: 0,
            text: "贴右边界".into(),
            boxes: vec![OcrBox { x1: 0.0, y1: 1700.0, x2: 1080.0, y2: 1800.0 }],
        }];
        let b = subtitle_band(&frames, 1080, 1920).expect("该找到");
        assert!(b.x + b.w <= 1080, "右边越界：{b:?}");
        assert!(b.y + b.h <= 1920, "下边越界：{b:?}");
        assert!(b.w > 0 && b.h > 0, "宽高必须为正：{b:?}");
    }

    #[test]
    fn corner_watermark_does_not_steal_the_subtitle_band() {
        // 真实素材（1280×716）的 OCR 结果，每帧两个框：
        //   居中的字幕 + 右下角的水印（x 1072~1279，y 648~696）
        // 旧逻辑按纵向中位数聚簇，水印框多、中位数落在水印上，
        // 结果只擦了水印，字幕原样留着。
        let mut frames = Vec::new();
        for (i, (x1, x2)) in [(520.0, 761.0), (480.0, 803.0), (360.0, 925.0)]
            .iter()
            .cycle()
            .take(12)
            .enumerate()
        {
            let text = if i == 0 { "" } else { "行动僵硬迟缓" };
            let mut boxes = vec![OcrBox {
                x1: 1072.0,
                y1: 648.0,
                x2: 1279.0,
                y2: 696.0,
            }];
            if !text.is_empty() {
                boxes.push(OcrBox {
                    x1: *x1,
                    y1: 593.0,
                    x2: *x2,
                    y2: 632.0,
                });
            }
            frames.push(OcrFrame {
                time_ms: 0,
                text: text.into(),
                boxes,
            });
        }
        let b = subtitle_band(&frames, 1280, 716).expect("该找到字幕带");
        assert!(b.x <= 360 && b.x + b.w >= 925, "要盖住居中的字幕：{b:?}");
        assert!(
            b.x + b.w < 1060,
            "不该被右下角水印带到右边去（那就是旧 bug）：{b:?}"
        );
        assert!(b.y <= 593 && b.y + b.h >= 632, "要盖住字幕的上下范围：{b:?}");
    }

    #[test]
    fn a_lone_corner_watermark_still_gets_covered() {
        // 全片只有边角一处文字（没有任何居中字幕）时退出筛选，
        // 保持旧行为 —— 擦水印总比什么都不做好。
        let frames = vec![frame(648.0, 696.0, 1072.0, 1279.0); 3];
        let b = subtitle_band(&frames, 1280, 716).expect("该找到");
        assert!(b.x >= 1060, "只该盖住右下角那块：{b:?}");
    }

    /// 用户的原话：「字幕 水印都要擦的」。
    ///
    /// 真实素材（1280×716）每帧两个框：居中的字幕 + 右下角水印。
    /// 两个都得是独立的 `delogo` 区域。
    #[test]
    fn subtitle_and_corner_watermark_are_both_removed() {
        let mut frames = Vec::new();
        for (i, (x1, x2)) in [(520.0, 761.0), (480.0, 803.0), (360.0, 925.0)]
            .iter()
            .cycle()
            .take(12)
            .enumerate()
        {
            let text = if i == 0 { "" } else { "行动僵硬迟缓" };
            let mut boxes = vec![OcrBox {
                x1: 1072.0,
                y1: 648.0,
                x2: 1279.0,
                y2: 696.0,
            }];
            if !text.is_empty() {
                boxes.push(OcrBox {
                    x1: *x1,
                    y1: 593.0,
                    x2: *x2,
                    y2: 632.0,
                });
            }
            frames.push(OcrFrame {
                time_ms: 0,
                text: text.into(),
                boxes,
            });
        }
        let bands = text_bands(&frames, 1280, 716);
        assert_eq!(bands.len(), 2, "字幕 + 水印各一个框：{bands:?}");
        assert!(
            bands[0].x <= 360 && bands[0].x + bands[0].w >= 925 && bands[0].x + bands[0].w < 1060,
            "第一个是居中字幕：{bands:?}"
        );
        assert!(
            bands[1].x >= 1060 && bands[1].y <= 648 && bands[1].y + bands[1].h >= 696,
            "第二个是右下角水印：{bands:?}"
        );
        let filter = delogo_filter(&bands);
        assert_eq!(filter.matches("delogo=").count(), 2, "两块都要擦：{filter}");
    }

    /// 画面里还有别的居中文字（招牌、画面里的字）时，只当字幕擦**一条**。
    #[test]
    fn only_one_centered_cluster_is_treated_as_subtitles() {
        // 主字幕 4 帧都在；另一行居中文字只出现 2 帧（50%，过了门槛）
        let mut frames = vec![frame(1600.0, 1700.0, 300.0, 780.0); 4];
        for f in frames.iter_mut().take(2) {
            f.boxes.push(OcrBox {
                x1: 320.0,
                y1: 1450.0,
                x2: 760.0,
                y2: 1500.0,
            });
        }
        let bands = text_bands(&frames, 1080, 1920);
        assert_eq!(bands.len(), 1, "只留命中最多那条：{bands:?}");
        assert!(bands[0].y >= 1580, "留下的应该是主字幕：{bands:?}");
    }

    #[test]
    fn delogo_filter_is_well_formed() {
        let bands = [
            SubtitleBand {
                x: 10,
                y: 1600,
                w: 800,
                h: 100,
                hit_frames: 3,
                total_frames: 3,
            },
            SubtitleBand {
                x: 1068,
                y: 644,
                w: 211,
                h: 56,
                hit_frames: 3,
                total_frames: 3,
            },
        ];
        assert_eq!(
            delogo_filter(&bands),
            "delogo=x=10:y=1600:w=800:h=100,delogo=x=1068:y=644:w=211:h=56"
        );
        assert!(delogo_filter(&[]).is_empty());
    }
}
