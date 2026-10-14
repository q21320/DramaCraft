//! 去重效果验证：量化「处理前后到底差了多少」。
//!
//! 只做处理、不做验证，等于闭着眼睛调参 —— 改了半天可能平台判重仍然命中，
//! 也可能改过头把画面搞坏了。这个模块给处理**闭环**。
//!
//! ## 做法（全部是公开的标准算法，不依赖任何平台的判重逻辑）
//!
//! 1. 用 ffmpeg 从两个视频里**按相对位置**各抽 N 帧（`fps = N / 时长`），
//!    缩到 9×8 灰度、rawvideo 输出 —— 处理前后时长不同也能一一对上；
//! 2. 每帧算 **dHash**（相邻像素比较，64 位）；
//! 3. 同位置帧做**汉明距离**，换算成相似度。
//!
//! dHash 对整体亮度、轻微噪点不敏感，对**镜像、裁切、构图变化**非常敏感 ——
//! 正好是这类处理关心的地方。
//!
//! ## 阈值是经验值，不是平台的判重标准
//!
//! 各平台判重算法不公开且会变。这里的结论只回答「改动幅度够不够明显」，
//! **不承诺能过任何平台的判重**。界面上也这么说。

use crate::error::{AppError, AppResult};
use crate::models::{MediaProbe, SimilarityReport, SimilaritySample};
use crate::proc::{no_window, tail};
use std::path::Path;
use std::process::Command;

/// dHash 的采样尺寸：宽 = 高 + 1，每行产生「宽-1」个比特。
pub const HASH_W: u32 = 9;
pub const HASH_H: u32 = 8;
const FRAME_BYTES: usize = (HASH_W * HASH_H) as usize;

/// 默认抽帧数。太少会被单个镜头变化误导，太多则慢。
pub const DEFAULT_SAMPLES: usize = 8;

pub const VERDICT_NEARLY_SAME: &str = "几乎没变";
pub const VERDICT_CHANGED: &str = "有差异";
pub const VERDICT_STRONG: &str = "差异明显";

/// 一行 9 个灰度像素 → 8 个比特（左 > 右 记 1）。
pub fn dhash(bytes: &[u8]) -> u64 {
    let mut bits = 0u64;
    let mut bit_index = 0;
    let mut row_start = 0usize;
    for _row in 0..HASH_H {
        for col in 0..(HASH_W as usize - 1) {
            let left = bytes[row_start + col];
            let right = bytes[row_start + col + 1];
            if left > right {
                bits |= 1u64 << bit_index;
            }
            bit_index += 1;
        }
        row_start += HASH_W as usize;
    }
    bits
}

pub fn hamming(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

/// 汉明距离 → 相似度（0..=1）。距离 0 得 1.0，全部 64 位都不同得 0.0。
pub fn distance_to_similarity(distance: u32) -> f64 {
    let d = distance.min(64) as f64;
    1.0 - d / 64.0
}

/// 结论。阈值是经验值 —— 见模块头部的说明。
pub fn verdict(mean_similarity: f64) -> &'static str {
    if mean_similarity >= 0.95 {
        VERDICT_NEARLY_SAME
    } else if mean_similarity >= 0.80 {
        VERDICT_CHANGED
    } else {
        VERDICT_STRONG
    }
}

/// 把一段 rawvideo 字节切成若干帧并算 dHash。不足一帧的尾巴丢掉。
pub fn hashes_from_raw(raw: &[u8]) -> Vec<u64> {
    raw.chunks_exact(FRAME_BYTES).map(dhash).collect()
}

/// 从视频抽 N 帧并算 dHash。
///
/// `duration_ms` 用来把抽帧均匀铺满整条片子：处理前后时长不同（比如改了速度），
/// 也能按相对位置一一对应。
pub fn sample_hashes(
    ffmpeg: &Path,
    input: &Path,
    duration_ms: i64,
    samples: usize,
) -> AppResult<Vec<u64>> {
    if !input.is_file() {
        return Err(AppError::Msg(format!(
            "文件不存在：{}",
            input.to_string_lossy()
        )));
    }
    let samples = samples.clamp(1, 60);
    let duration_secs = (duration_ms.max(1) as f64) / 1000.0;
    let fps = (samples as f64 / duration_secs).max(0.001);

    let filter = format!(
        "fps={fps:.6},scale={HASH_W}:{HASH_H}:flags=bilinear,format=gray"
    );

    let output = no_window(
        Command::new(ffmpeg)
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-i")
            .arg(input.as_os_str())
            .arg("-an")
            .arg("-sn")
            .arg("-vf")
            .arg(&filter)
            .arg("-frames:v")
            .arg(samples.to_string())
            .arg("-pix_fmt")
            .arg("gray")
            .arg("-f")
            .arg("rawvideo")
            .arg("-"),
    )
    .output()
    .map_err(|e| AppError::Process(format!("无法启动 ffmpeg：{e}")))?;

    if !output.status.success() {
        return Err(AppError::Process(format!(
            "抽帧失败（退出码 {:?}）：{}",
            output.status.code(),
            tail(&String::from_utf8_lossy(&output.stderr))
        )));
    }

    let hashes = hashes_from_raw(&output.stdout);
    if hashes.is_empty() {
        return Err(AppError::Process(
            "抽帧没有拿到任何完整帧，视频可能损坏或过短".into(),
        ));
    }
    Ok(hashes)
}

/// 比对两组哈希。数量不等时按较短的一方对齐（多余部分忽略并记一次告警）。
pub fn compare_hashes(before: &[u64], after: &[u64]) -> (Vec<SimilaritySample>, f64) {
    let count = before.len().min(after.len());
    let mut samples = Vec::with_capacity(count);
    let mut total = 0.0;

    for i in 0..count {
        let distance = hamming(before[i], after[i]);
        let similarity = distance_to_similarity(distance);
        total += similarity;
        samples.push(SimilaritySample {
            index: i as i64 + 1,
            distance,
            similarity,
        });
    }

    let mean = if count == 0 { 0.0 } else { total / count as f64 };
    (samples, mean)
}

/// 比对两个视频文件的处理前后差异。
pub fn compare_files(
    ffmpeg: &Path,
    before: &Path,
    before_probe: &MediaProbe,
    after: &Path,
    after_probe: &MediaProbe,
    samples: usize,
) -> AppResult<SimilarityReport> {
    let hashes_before = sample_hashes(ffmpeg, before, before_probe.duration_ms, samples)?;
    let hashes_after = sample_hashes(ffmpeg, after, after_probe.duration_ms, samples)?;

    let (items, mean) = compare_hashes(&hashes_before, &hashes_after);

    let mut warnings: Vec<String> = Vec::new();
    if hashes_before.len() != hashes_after.len() {
        warnings.push(format!(
            "两边抽到的帧数不同（{} vs {}），只比对前 {} 帧",
            hashes_before.len(),
            hashes_after.len(),
            items.len()
        ));
    }
    if items.len() < 3 {
        warnings.push("抽到的帧太少，结论仅供参考".into());
    }
    if mean >= 0.999 {
        warnings.push("两边几乎完全一致 —— 确认处理真的生效了吗？".into());
    }

    Ok(SimilarityReport {
        before: before.to_string_lossy().to_string(),
        after: after.to_string_lossy().to_string(),
        samples_compared: items.len() as i64,
        mean_distance: if items.is_empty() {
            0.0
        } else {
            items.iter().map(|s| s.distance as f64).sum::<f64>() / items.len() as f64
        },
        mean_similarity: mean,
        verdict: verdict(mean).to_string(),
        samples: items,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn frame_row(values: &[u8]) -> Vec<u8> {
        // 造一张 9x8 的灰度图：每行都用给定的 9 个值
        let mut bytes = Vec::with_capacity(FRAME_BYTES);
        for _ in 0..HASH_H {
            bytes.extend_from_slice(values);
        }
        bytes
    }

    #[test]
    fn dhash_of_flat_image_is_zero() {
        // 全灰：左右相等 → 全部记 0
        let bytes = vec![128u8; FRAME_BYTES];
        assert_eq!(dhash(&bytes), 0);
    }

    #[test]
    fn dhash_marks_decreasing_steps() {
        // 每行严格递减 → 每次比较都「左 > 右」→ 64 位全 1
        let descending = frame_row(&[255, 224, 192, 160, 128, 96, 64, 32, 0]);
        assert_eq!(dhash(&descending), u64::MAX);
        // 严格递增则相反：全部「左 < 右」→ 全 0
        let ascending = frame_row(&[0, 32, 64, 96, 128, 160, 192, 224, 255]);
        assert_eq!(dhash(&ascending), 0);
    }

    #[test]
    fn dhash_of_mirrored_row_flips_all_bits() {
        let ascending = frame_row(&[0, 32, 64, 96, 128, 160, 192, 224, 255]);
        let descending = frame_row(&[255, 224, 192, 160, 128, 96, 64, 32, 0]);
        assert_eq!(hamming(dhash(&ascending), dhash(&descending)), 64);
        assert_eq!(distance_to_similarity(64), 0.0);
    }

    #[test]
    fn hamming_and_similarity_are_consistent() {
        assert_eq!(hamming(0, 0), 0);
        assert_eq!(hamming(0, u64::MAX), 64);
        assert_eq!(distance_to_similarity(0), 1.0);
        assert_eq!(distance_to_similarity(32), 0.5);
        // 超过 64 也不会算出负数
        assert_eq!(distance_to_similarity(999), 0.0);
    }

    #[test]
    fn verdict_thresholds() {
        assert_eq!(verdict(1.0), VERDICT_NEARLY_SAME);
        assert_eq!(verdict(0.95), VERDICT_NEARLY_SAME);
        assert_eq!(verdict(0.94), VERDICT_CHANGED);
        assert_eq!(verdict(0.80), VERDICT_CHANGED);
        assert_eq!(verdict(0.79), VERDICT_STRONG);
    }

    #[test]
    fn hashes_from_raw_chunks_and_drops_partial_tail() {
        let mut raw = frame_row(&[255, 224, 192, 160, 128, 96, 64, 32, 0]); // 全 1
        raw.extend_from_slice(&[1, 2, 3]); // 半帧，应被丢掉
        raw.extend(std::iter::repeat_n(128u8, FRAME_BYTES)); // 第二帧：全灰 → 全 0
        let hashes = hashes_from_raw(&raw);
        assert_eq!(hashes.len(), 2);
        assert_eq!(hashes[0], u64::MAX);
        assert_eq!(hashes[1], 0);
    }

    #[test]
    fn compare_identical_hashes_gives_full_similarity() {
        let a = vec![0b1010u64, u64::MAX, 12345];
        let (samples, mean) = compare_hashes(&a, &a);
        assert_eq!(samples.len(), 3);
        assert_eq!(mean, 1.0);
        assert!(samples.iter().all(|s| s.distance == 0));
        assert_eq!(verdict(mean), VERDICT_NEARLY_SAME);
    }

    #[test]
    fn compare_all_flipped_gives_zero_similarity() {
        let a = vec![0u64, 0u64];
        let b = vec![u64::MAX, u64::MAX];
        let (samples, mean) = compare_hashes(&a, &b);
        assert_eq!(mean, 0.0);
        assert_eq!(samples[0].distance, 64);
        assert_eq!(verdict(mean), VERDICT_STRONG);
    }

    #[test]
    fn compare_aligns_to_the_shorter_side() {
        let a = vec![0u64, 0u64, 0u64];
        let b = vec![0u64, 0u64];
        let (samples, mean) = compare_hashes(&a, &b);
        assert_eq!(samples.len(), 2, "多余的一帧应被忽略而不是 panic");
        assert_eq!(mean, 1.0);
        assert_eq!(samples[0].index, 1);
        assert_eq!(samples[1].index, 2);
    }

    #[test]
    fn compare_empty_inputs_do_not_panic() {
        let (samples, mean) = compare_hashes(&[], &[]);
        assert!(samples.is_empty());
        assert_eq!(mean, 0.0);
    }

    #[test]
    fn missing_file_is_reported_before_spawning() {
        let err = sample_hashes(
            Path::new("Z:/nope/ffmpeg.exe"),
            Path::new("Z:/nope/missing.mp4"),
            1_000,
            4,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("文件不存在"), "实际：{err}");
    }

    // ---------------- 端到端（需要真实 ffmpeg，默认跳过） ----------------

    fn test_ffmpeg() -> Option<PathBuf> {
        std::env::var("DRAMADIST_TEST_FFMPEG")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
    }

    fn make_clip(ffmpeg: &Path, out: &Path) {
        let status = no_window(
            Command::new(ffmpeg).args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=320x240:rate=15",
                "-t",
                "8",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
            ]),
        )
        .arg(out)
        .status()
        .expect("无法启动 ffmpeg");
        assert!(status.success(), "生成测试素材失败");
    }

    #[test]
    fn end_to_end_identical_file_is_fully_similar() {
        let Some(ffmpeg) = test_ffmpeg() else {
            eprintln!("跳过：未设置 DRAMADIST_TEST_FFMPEG");
            return;
        };
        let dir = std::env::temp_dir().join("dramadist_verify_same");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("a.mp4");
        make_clip(&ffmpeg, &clip);

        let probe = MediaProbe {
            duration_ms: 8_000,
            ..Default::default()
        };
        let report = compare_files(&ffmpeg, &clip, &probe, &clip, &probe, 6).unwrap();
        assert_eq!(report.samples_compared, 6);
        assert!(
            report.mean_similarity > 0.99,
            "同一个文件应当几乎完全一致，实际 {}",
            report.mean_similarity
        );
        assert_eq!(report.verdict, VERDICT_NEARLY_SAME);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn end_to_end_mirrored_file_is_clearly_different() {
        let Some(ffmpeg) = test_ffmpeg() else {
            eprintln!("跳过：未设置 DRAMADIST_TEST_FFMPEG");
            return;
        };
        let dir = std::env::temp_dir().join("dramadist_verify_mirror");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let original = dir.join("orig.mp4");
        make_clip(&ffmpeg, &original);

        // 镜像 + 调色 + 噪点，模拟"处理过"的片段
        let mirrored = dir.join("mirrored.mp4");
        let status = no_window(
            Command::new(&ffmpeg)
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-y",
                    "-i",
                ])
                .arg(&original)
                .args([
                    "-vf",
                    "hflip,eq=brightness=0.03:contrast=1.02,noise=alls=6:allf=t+u",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-pix_fmt",
                    "yuv420p",
                ])
                .arg(&mirrored),
        )
        .status()
        .expect("无法启动 ffmpeg");
        assert!(status.success());

        let probe = MediaProbe {
            duration_ms: 8_000,
            ..Default::default()
        };
        let report = compare_files(&ffmpeg, &original, &probe, &mirrored, &probe, 6).unwrap();

        assert_eq!(report.samples_compared, 6);
        assert!(
            report.mean_similarity < 0.80,
            "镜像后差异应当很明显，实际相似度 {}",
            report.mean_similarity
        );
        assert_eq!(report.verdict, VERDICT_STRONG);
        // 逐帧明细要有，且帧号从 1 开始
        assert_eq!(report.samples[0].index, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
