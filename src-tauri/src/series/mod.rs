//! 剧目与集：把散乱的素材按「剧目 → 集」组织起来，并按集重新切分。
//!
//! 短剧素材到手时通常两种乱法：
//!
//! 1. **文件名乱**：同一部剧的集数命名不统一，甚至混着好几部剧；
//! 2. **切分乱**：一整条长视频里连播好几集，切点跟实际集数对不上。
//!
//! 对应两个能力：
//!
//! - **剧目重组**：先按文件名解析「剧目 + 集号」分组；文件名实在没规律的，
//!   再用**片头指纹**聚类 —— 同一部剧的片头是一样的，指纹能把它们找回来；
//! - **集数重切**：用第一段片头当模板，在全片找它的每一次出现，那就是每集的起点。
//!
//! 指纹复用 [`crate::verify`] 的 dHash，不引入任何新的判重/识别依赖。

use crate::error::{AppError, AppResult};
use crate::models::{EpisodeBoundary, SeriesFile, SeriesGroup};
use crate::verify;
use regex::Regex;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 文件名里「集号」的识别模式，按优先级从上到下。
///
/// 顺序很重要：`第01集` 比裸数字可靠得多，先匹配可靠的形式。
const EPISODE_PATTERNS: [&str; 6] = [
    r"第\s*(\d{1,4})\s*[集话回期]",
    r"(?i)S\s*\d{1,3}\s*E\s*(\d{1,4})",
    r"(?i)(?:^|[_\-\s])EP?\s*[-_]?\s*(\d{1,4})(?:[_\-\s.]|$)",
    r"[\[\(（【]\s*(\d{1,3})\s*[\]\)）】]",
    r"(?:^|[_\-\s])(\d{1,4})(?:[_\-\s]|$)",
    r"(\d{1,4})\s*[集话回期]",
];

fn episode_regexes() -> &'static [Regex] {
    static COMPILED: OnceLock<Vec<Regex>> = OnceLock::new();
    COMPILED.get_or_init(|| {
        EPISODE_PATTERNS
            .iter()
            .filter_map(|p| Regex::new(p).ok())
            .collect()
    })
}

fn stem_of(file_name: &str) -> &str {
    match file_name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 5 => stem,
        _ => file_name,
    }
}

/// 在文件名里找集号，同时返回**整个匹配**占据的字符区间（用于反推剧目名）。
///
/// 注意返回的是整段匹配而不是捕获组：`DramaA_E05_1080p` 里捕获组只有 `05`，
/// 只删它会把 `E` 留下，剧目名就变成了 `DramaA E 1080p`。
fn match_episode(stem: &str) -> Option<(i64, std::ops::Range<usize>)> {
    for re in episode_regexes() {
        if let Some(caps) = re.captures(stem) {
            let (Some(whole), Some(number_match)) = (caps.get(0), caps.get(1)) else {
                continue;
            };
            if let Ok(number) = number_match.as_str().parse::<i64>() {
                // 集号从 1 开始；0 视为无效
                if number <= 0 {
                    continue;
                }
                return Some((number, whole.range()));
            }
        }
    }
    // 纯数字文件名：01.mp4
    if let Ok(number) = stem.trim().parse::<i64>() {
        if number > 0 {
            return Some((number, 0..stem.len()));
        }
    }
    None
}

/// 从文件名解析集号。认不出来返回 `None`。
pub fn parse_episode(file_name: &str) -> Option<i64> {
    match_episode(stem_of(file_name)).map(|(n, _)| n)
}

/// 从文件名解析剧目名。认不出集号时，整个文件名（去扩展名）就是剧目名。
pub fn parse_series(file_name: &str) -> String {
    let stem = stem_of(file_name);
    let without_episode = match match_episode(stem) {
        // 把整段集号标记换成空格，避免两侧词被粘在一起
        Some((_, range)) => format!("{} {}", &stem[..range.start], &stem[range.end..]),
        None => stem.to_string(),
    };

    let normalized: String = without_episode
        .chars()
        .map(|c| match c {
            '_' | '-' | '.' | '【' | '】' | '[' | ']' | '（' | '）' | '(' | ')' | '+' => ' ',
            other => other,
        })
        .collect();

    let cleaned = normalized.split_whitespace().collect::<Vec<_>>().join(" ");
    let cleaned = cleaned.trim().to_string();

    // 剥完什么都不剩，就如实返回空 —— 说明这个文件名里压根没有剧名。
    //
    // 以前这里会退回原始文件名，那是个**假答案**：`第01集.mp4` 会被当成
    // 剧名「第01集」。调用方（files_to_entries）本来就会把空串映射成「未命名」，
    // 而流水线要靠"空"来判断"该用目录名当剧名"，所以这里必须诚实。
    cleaned
}

/// 把路径列表解析成条目（不分组）。
pub fn files_to_entries(paths: &[String]) -> Vec<SeriesFile> {
    paths
        .iter()
        .map(|path| {
            let file_name = Path::new(path)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| path.clone());
            let series = parse_series(&file_name);
            SeriesFile {
                path: path.clone(),
                file_name,
                series: if series.is_empty() {
                    "未命名".to_string()
                } else {
                    series
                },
                episode: parse_episode(path),
            }
        })
        .collect()
}

/// 组内排序：认得出集号的按集号，认不出的排在最后按文件名。
pub fn sort_episode_entries(episodes: &mut [SeriesFile]) {
    episodes.sort_by(|a, b| match (a.episode, b.episode) {
        (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.file_name.cmp(&b.file_name)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.file_name.cmp(&b.file_name),
    });
}

/// 把一批文件按剧目分组。组内按集号排序，认不出集号的排在最后。
pub fn group_files(paths: &[String]) -> Vec<SeriesGroup> {
    let mut order: Vec<String> = Vec::new();
    let mut buckets: std::collections::HashMap<String, Vec<SeriesFile>> =
        std::collections::HashMap::new();

    for entry in files_to_entries(paths) {
        let key = entry.series.clone();
        if !buckets.contains_key(&key) {
            order.push(key.clone());
        }
        buckets.entry(key).or_default().push(entry);
    }

    let mut groups: Vec<SeriesGroup> = order
        .into_iter()
        .map(|series| {
            let mut episodes = buckets.remove(&series).unwrap_or_default();
            sort_episode_entries(&mut episodes);
            SeriesGroup { series, episodes }
        })
        .collect();

    // 集数多的剧目排前面，便于一眼看到主库
    groups.sort_by(|a, b| {
        b.episodes
            .len()
            .cmp(&a.episodes.len())
            .then_with(|| a.series.cmp(&b.series))
    });
    groups
}

// ---------------- 片头指纹 ----------------

/// 按固定间隔采样整片，返回 (时间毫秒, 哈希)。
///
/// 时间戳由序号推算：抽 N 帧铺满整片，第 i 帧对应 `i * 时长 / N`。
pub fn sample_timeline(
    ffmpeg: &Path,
    input: &Path,
    duration_ms: i64,
    fps: f64,
    max_frames: usize,
) -> AppResult<Vec<(i64, u64)>> {
    if duration_ms <= 0 {
        return Err(AppError::Msg("素材时长为 0，无法采样".into()));
    }
    let wanted = ((duration_ms as f64 / 1000.0) * fps.max(0.05)).round() as usize;
    let count = wanted.clamp(2, max_frames.max(2));

    let hashes = verify::sample_hashes(ffmpeg, input, duration_ms, count)?;
    let n = hashes.len() as i64;
    Ok(hashes
        .into_iter()
        .enumerate()
        .map(|(i, h)| ((i as i64) * duration_ms / n, h))
        .collect())
}

/// 取前 `intro_ms` 毫秒的采样作为片头模板。
pub fn intro_template(samples: &[(i64, u64)], intro_ms: i64) -> Vec<u64> {
    samples
        .iter()
        .take_while(|(t, _)| *t < intro_ms)
        .map(|(_, h)| *h)
        .collect()
}

/// 在采样序列里找片头模板出现的位置（毫秒）。
///
/// - `tolerance`：单帧汉明距离容忍度，越小越严格；
/// - `min_gap_ms`：两个命中点至少隔这么久，避免同一段片头被反复计数。
pub fn find_intro_positions(
    samples: &[(i64, u64)],
    template: &[u64],
    tolerance: u32,
    min_gap_ms: i64,
) -> Vec<i64> {
    if template.is_empty() || samples.len() < template.len() {
        return Vec::new();
    }

    let mut hits: Vec<i64> = Vec::new();
    let mut i = 0usize;
    while i + template.len() <= samples.len() {
        let matched = template
            .iter()
            .enumerate()
            .all(|(k, t)| verify::hamming(samples[i + k].1, *t) <= tolerance);

        if matched {
            let at = samples[i].0;
            let far_enough = hits.last().map(|last| at - *last >= min_gap_ms).unwrap_or(true);
            if far_enough {
                hits.push(at);
            }
            // 跳过整段片头，避免在它内部重复命中
            i += template.len();
        } else {
            i += 1;
        }
    }
    hits
}

/// 把命中点转成集边界：第一集起点固定为 0，逐集给出起止与置信度。
pub fn boundaries_from_hits(hits: &[i64], total_ms: i64, template_len: usize) -> Vec<EpisodeBoundary> {
    let mut starts: Vec<i64> = hits.to_vec();
    starts.sort_unstable();
    starts.dedup();
    if starts.first().map(|s| *s > 0).unwrap_or(false) {
        // 片头没在 0 附近命中：可能是从中间截取的素材，仍把 0 当作第一集起点
        starts.insert(0, 0);
    }
    if starts.is_empty() {
        return Vec::new();
    }

    let mut out = Vec::with_capacity(starts.len());
    for (i, start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(total_ms);
        if end <= *start {
            continue;
        }
        out.push(EpisodeBoundary {
            index_no: i as i64 + 1,
            start_ms: *start,
            end_ms: end,
            duration_ms: end - start,
            // 起点是识别出来的（0 也来自识别序列），模板越长越可信
            confidence: if template_len >= 3 { "high" } else { "low" }.to_string(),
        });
    }
    out
}

// ---------------- 指纹聚类 ----------------

/// 两段指纹是否属于同一部剧：多数帧足够接近，且平均距离也够小。
pub fn fingerprints_match(a: &[u64], b: &[u64], tolerance: u32) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    let n = a.len().min(b.len());
    let mut close = 0usize;
    let mut total: u64 = 0;
    for k in 0..n {
        let d = verify::hamming(a[k], b[k]);
        total += d as u64;
        if d <= tolerance {
            close += 1;
        }
    }
    close * 2 >= n && (total as f64 / n as f64) <= tolerance as f64
}

/// 贪心聚类：把指纹相近的文件归到一组。返回每组的原始下标。
pub fn cluster_by_fingerprint(fingerprints: &[Vec<u64>], tolerance: u32) -> Vec<Vec<usize>> {
    let mut clusters: Vec<Vec<usize>> = Vec::new();
    let mut assigned = vec![false; fingerprints.len()];

    for i in 0..fingerprints.len() {
        if assigned[i] {
            continue;
        }
        assigned[i] = true;
        let mut group = vec![i];
        for j in (i + 1)..fingerprints.len() {
            if assigned[j] {
                continue;
            }
            if fingerprints_match(&fingerprints[i], &fingerprints[j], tolerance) {
                group.push(j);
                assigned[j] = true;
            }
        }
        group.sort_unstable();
        clusters.push(group);
    }
    clusters
}

/// 提取一个文件的片头指纹。
pub fn intro_fingerprint(
    ffmpeg: &Path,
    input: &Path,
    duration_ms: i64,
    intro_ms: i64,
    fps: f64,
) -> AppResult<Vec<u64>> {
    let samples = sample_timeline(ffmpeg, input, duration_ms, fps, 200)?;
    Ok(intro_template(&samples, intro_ms))
}

// ---------------- 按集切分 ----------------

/// 按集边界切分原片，每集一个文件（流拷贝优先）。返回产出路径。
pub fn resplit_by_episodes(
    ffmpeg: &Path,
    input: &Path,
    boundaries: &[EpisodeBoundary],
    out_dir: &Path,
) -> AppResult<Vec<PathBuf>> {
    if boundaries.is_empty() {
        return Err(AppError::Msg("没有可用的集边界".into()));
    }
    std::fs::create_dir_all(out_dir)?;

    let mut produced = Vec::with_capacity(boundaries.len());
    for b in boundaries {
        if b.end_ms <= b.start_ms {
            continue;
        }
        let plan = crate::models::SlicePlan {
            index: b.index_no,
            start_ms: b.start_ms,
            end_ms: b.end_ms,
        };
        let out = out_dir.join(format!("第{:02}集.mp4", b.index_no));
        match crate::media::slice(ffmpeg, input, &out, &plan, true) {
            Ok(_) => produced.push(out),
            Err(e) => {
                return Err(AppError::Process(format!(
                    "第 {} 集切分失败：{e}",
                    b.index_no
                )))
            }
        }
    }
    Ok(produced)
}

/// 列目录下的视频文件。
pub fn list_video_files(dir: &Path) -> AppResult<Vec<String>> {
    if !dir.is_dir() {
        return Err(AppError::Msg(format!(
            "目录不存在：{}",
            dir.to_string_lossy()
        )));
    }
    const EXT: [&str; 11] = [
        "mp4", "mkv", "mov", "avi", "ts", "flv", "wmv", "m4v", "webm", "mpg", "mpeg",
    ];
    let mut files: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .filter(|p| {
            p.extension()
                .map(|x| EXT.contains(&x.to_string_lossy().to_lowercase().as_str()))
                .unwrap_or(false)
        })
        .map(|p| p.to_string_lossy().to_string())
        .collect();
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    // ---------------- 文件名解析 ----------------

    #[test]
    fn parses_common_chinese_episode_forms() {
        assert_eq!(parse_episode("都市爽剧_第01集.mp4"), Some(1));
        assert_eq!(parse_episode("都市爽剧 第 12 集.mp4"), Some(12));
        assert_eq!(parse_episode("第003话.mkv"), Some(3));
        assert_eq!(parse_episode("某剧第7回.mp4"), Some(7));
        assert_eq!(parse_episode("某剧01集.mp4"), Some(1));
    }

    #[test]
    fn parses_latin_episode_forms() {
        assert_eq!(parse_episode("DramaA_E05_1080p.mkv"), Some(5));
        assert_eq!(parse_episode("DramaA-EP12.mp4"), Some(12));
        assert_eq!(parse_episode("DramaA ep-3.mkv"), Some(3));
        assert_eq!(parse_episode("Show.S01E07.mkv"), Some(7));
        assert_eq!(parse_episode("剧名 (12).mp4"), Some(12));
        assert_eq!(parse_episode("剧名-07.mp4"), Some(7));
        assert_eq!(parse_episode("01.mp4"), Some(1));
    }

    #[test]
    fn does_not_mistake_resolution_or_year_for_episode() {
        // 1080p 里的 1080 不该被当成集号
        assert_eq!(parse_episode("DramaA_1080p.mkv"), None);
        assert_eq!(parse_episode("某剧 2160p HDR.mp4"), None);
        // 生成式命名乱但没集号时也返回 None，而不是瞎猜
        assert_eq!(parse_episode("最终版(不要删).mp4"), None);
    }

    #[test]
    fn episode_zero_is_invalid() {
        assert_eq!(parse_episode("第0集.mp4"), None);
        assert_eq!(parse_episode("0.mp4"), None);
    }

    #[test]
    fn series_name_strips_episode_marker_and_separators() {
        assert_eq!(parse_series("都市爽剧_第01集.mp4"), "都市爽剧");
        assert_eq!(parse_series("DramaA_E05_1080p.mkv"), "DramaA 1080p");
        assert_eq!(parse_series("剧名 (12).mp4"), "剧名");
        assert_eq!(parse_series("剧名-07.mp4"), "剧名");
    }

    #[test]
    fn a_name_without_an_episode_marker_is_kept_as_is() {
        assert_eq!(parse_series("没有集号的名字.mp4"), "没有集号的名字");
        // 但「01.mp4」整个就是集号 —— 剥掉之后什么都不剩，
        // 这时返回空串才诚实（以前会退回 "01"，那是把集号当成了剧名）
        assert_eq!(parse_series("01.mp4"), "");
    }

    // ---------------- 分组 ----------------

    #[test]
    fn groups_files_by_series_and_sorts_by_episode() {
        let files = vec![
            "E:/d/甲剧_第03集.mp4".to_string(),
            "E:/d/乙剧_第01集.mp4".to_string(),
            "E:/d/甲剧_第01集.mp4".to_string(),
            "E:/d/甲剧_第02集.mp4".to_string(),
            "E:/d/乙剧_第02集.mp4".to_string(),
            "E:/d/甲剧_第04集.mp4".to_string(),
        ];
        let groups = group_files(&files);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].series, "甲剧", "集数多的排前面");
        assert_eq!(groups[0].episodes.len(), 4);
        let eps: Vec<Option<i64>> = groups[0].episodes.iter().map(|e| e.episode).collect();
        assert_eq!(eps, vec![Some(1), Some(2), Some(3), Some(4)]);
        assert_eq!(groups[1].series, "乙剧");
    }

    #[test]
    fn unnumbered_files_sort_last_within_group() {
        // 「剧.mp4」去掉扩展名就是剧目名，会跟带集号的落在同一组，但没有集号
        let files = vec![
            "E:/d/剧.mp4".to_string(),
            "E:/d/剧_第02集.mp4".to_string(),
            "E:/d/剧_第01集.mp4".to_string(),
        ];
        let groups = group_files(&files);
        assert_eq!(groups.len(), 1, "三个文件应当同属「剧」这一组");
        assert_eq!(groups[0].episodes.len(), 3);
        let eps: Vec<Option<i64>> = groups[0].episodes.iter().map(|e| e.episode).collect();
        assert_eq!(eps, vec![Some(1), Some(2), None], "没集号的排最后");
        assert_eq!(groups[0].episodes.last().unwrap().file_name, "剧.mp4");
    }

    #[test]
    fn filename_with_only_an_episode_marker_has_no_series_name() {
        // 剥掉集号标记后什么都不剩 —— 这时必须如实返回空，
        // 不能退回原始文件名，否则「第01集」会被当成剧名。
        for name in ["第01集.mp4", "EP05.mkv", "07.mp4", "S01E07.mp4"] {
            let got = parse_series(name);
            assert!(
                got.is_empty(),
                "{name} 里没有剧名，应当返回空串，实际得到「{got}」"
            );
        }
        // 而带剧名的仍然正常
        assert_eq!(parse_series("都市爽剧_第01集.mp4"), "都市爽剧");
    }

    #[test]
    fn differently_named_files_are_not_forced_together() {
        // 「剧_花絮」解析出的剧目名就是「剧 花絮」，与「剧」不同组 —— 这是对的，不该硬凑
        let files = vec![
            "E:/d/剧_第01集.mp4".to_string(),
            "E:/d/剧_花絮.mp4".to_string(),
        ];
        let groups = group_files(&files);
        assert_eq!(groups.len(), 2);
    }

    #[test]
    fn empty_input_gives_no_groups() {
        assert!(group_files(&[]).is_empty());
    }

    // ---------------- 片头定位 ----------------

    fn samples(pairs: &[(i64, u64)]) -> Vec<(i64, u64)> {
        pairs.to_vec()
    }

    #[test]
    fn finds_every_intro_occurrence() {
        // 片头模板：3 帧
        let template = vec![0b1010u64, 0b0101u64, 0b1111u64];
        let data = samples(&[
            (0, 0b1010),
            (500, 0b0101),
            (1000, 0b1111),
            (1500, 0b0000), // 内容
            (2000, 0b0011),
            (2500, 0b1100),
            (3000, 0b1010), // 第二集片头
            (3500, 0b0101),
            (4000, 0b1111),
            (4500, 0b0110),
            (5000, 0b1001),
            (5500, 0b1010), // 第三集片头
            (6000, 0b0101),
            (6500, 0b1111),
        ]);
        let hits = find_intro_positions(&data, &template, 0, 2000);
        assert_eq!(hits, vec![0, 3000, 5500]);
    }

    #[test]
    fn tolerance_allows_small_differences() {
        // 模板两帧彼此差 8 位，方便构造"差一点"和"差很多"
        let template = vec![0b1111_0000u64, 0b0000_1111u64];
        let data = samples(&[
            (0, 0b1111_0000),    // 完全一致
            (500, 0b0000_1111),  // 完全一致
            (1000, 0b0000_0000), // 各差 4 位
            (1500, 0b0000_0000),
            (2000, 0b1111_0001), // 各差 1 位
            (2500, 0b0000_1110),
        ]);
        // 严格模式只认完全一致
        assert_eq!(find_intro_positions(&data, &template, 0, 1000), vec![0]);
        // 放宽到 1 位后能认出 2000 那处
        assert_eq!(
            find_intro_positions(&data, &template, 1, 1000),
            vec![0, 2000]
        );
        // 容忍度 4 时连 0b0000_0000 也会命中
        assert_eq!(
            find_intro_positions(&data, &template, 4, 1000),
            vec![0, 1000, 2000]
        );
    }

    #[test]
    fn min_gap_suppresses_repeats_inside_one_intro() {
        let template = vec![7u64, 7u64];
        let data = samples(&[(0, 7), (200, 7), (400, 7), (600, 7), (800, 7)]);
        // 没有间隔限制时，跳过一个模板长度后会再命中一次
        assert_eq!(find_intro_positions(&data, &template, 0, 0), vec![0, 400]);
        // 设了 1000ms 间隔后只记第一次
        assert_eq!(find_intro_positions(&data, &template, 0, 1000), vec![0]);
    }

    #[test]
    fn degenerate_intro_inputs_are_safe() {
        let data = samples(&[(0, 1), (500, 2)]);
        assert!(find_intro_positions(&data, &[], 0, 1000).is_empty());
        assert!(find_intro_positions(&[], &[1, 2, 3], 0, 1000).is_empty());
        // 模板比数据长
        assert!(find_intro_positions(&data, &[1, 2, 3], 0, 1000).is_empty());
    }

    #[test]
    fn boundaries_cover_the_whole_timeline() {
        let boundaries = boundaries_from_hits(&[0, 5000, 10000], 15000, 4);
        assert_eq!(boundaries.len(), 3);
        assert_eq!((boundaries[0].start_ms, boundaries[0].end_ms), (0, 5000));
        assert_eq!((boundaries[1].start_ms, boundaries[1].end_ms), (5000, 10000));
        assert_eq!(
            (boundaries[2].start_ms, boundaries[2].end_ms),
            (10000, 15000),
            "最后一集要一直到最后"
        );
        assert_eq!(boundaries[2].duration_ms, 5000);
        assert_eq!(boundaries[0].index_no, 1);
        assert_eq!(boundaries[2].confidence, "high");
    }

    #[test]
    fn boundaries_insert_zero_when_first_hit_is_late() {
        // 素材从中间截的：第一个片头在 3 秒处，仍要把 0..3000 当作第一集
        let boundaries = boundaries_from_hits(&[3000, 8000], 13000, 4);
        assert_eq!(boundaries.len(), 3);
        assert_eq!(boundaries[0].start_ms, 0);
        assert_eq!(boundaries[0].end_ms, 3000);
        assert_eq!(boundaries[1].start_ms, 3000);
    }

    #[test]
    fn short_template_is_marked_low_confidence() {
        let b = boundaries_from_hits(&[0, 5000], 10000, 2);
        assert_eq!(b[0].confidence, "low");
    }

    #[test]
    fn no_hits_gives_no_boundaries() {
        assert!(boundaries_from_hits(&[], 10000, 4).is_empty());
    }

    // ---------------- 指纹聚类 ----------------

    #[test]
    fn clusters_identical_fingerprints_together() {
        let intro_a = vec![0b1111u64, 0b0000, 0b1010];
        let intro_b = vec![0b0101u64, 0b1010, 0b0101];
        let fps = vec![
            intro_a.clone(),
            intro_b.clone(),
            intro_a.clone(),
            intro_b.clone(),
            intro_a.clone(),
        ];
        let clusters = cluster_by_fingerprint(&fps, 2);
        assert_eq!(clusters.len(), 2);
        assert_eq!(clusters[0], vec![0, 2, 4]);
        assert_eq!(clusters[1], vec![1, 3]);
    }

    #[test]
    fn clustering_tolerates_noise() {
        let base = vec![0b1111u64, 0b0000, 0b1010, 0b0101];
        let noisy = vec![0b1110u64, 0b0000, 0b1011, 0b0101]; // 两帧各差 1 位
        let different = vec![0b0000u64, 0b1111, 0b0101, 0b1010];
        let clusters = cluster_by_fingerprint(&[base, noisy, different], 2);
        assert_eq!(clusters.len(), 2, "{clusters:?}");
        assert_eq!(clusters[0], vec![0, 1]);
        assert_eq!(clusters[1], vec![2]);
    }

    #[test]
    fn empty_fingerprints_never_match() {
        assert!(!fingerprints_match(&[], &[1, 2, 3], 64));
        assert!(!fingerprints_match(&[1, 2, 3], &[], 64));
        // 空指纹各自成组，不会因为"都很空"被并到一起
        let clusters = cluster_by_fingerprint(&[vec![], vec![], vec![7, 7]], 2);
        assert_eq!(clusters.len(), 3);
    }

    #[test]
    fn clustering_of_empty_input_is_empty() {
        assert!(cluster_by_fingerprint(&[], 2).is_empty());
    }

    #[test]
    fn fingerprints_match_requires_majority_and_mean() {
        let a = vec![0u64; 4];
        // 每帧差 8 位：一帧都不接近
        let far = vec![0xFFu64, 0xFF, 0xFF, 0xFF];
        assert!(!fingerprints_match(&a, &far, 3));
        // 每帧差 1 位：全部在容忍度内
        let close = vec![0b1u64, 0b1, 0b1, 0b1];
        assert!(fingerprints_match(&a, &close, 3));
        // 半数的帧接近（过半），但平均距离 4.5 超出容忍度 3 → 不算同一部剧
        let mixed = vec![0b1u64, 0b1, 0xFF, 0xFF];
        assert!(!fingerprints_match(&a, &mixed, 3));
    }

    #[test]
    fn list_video_files_rejects_missing_directory() {
        let err = list_video_files(Path::new("Z:/nope/nowhere"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("目录不存在"), "实际：{err}");
    }

    // ---------------- 端到端（真实 ffmpeg，默认跳过） ----------------

    #[test]
    fn end_to_end_detects_and_splits_three_episodes() {
        let Some(ffmpeg) = std::env::var("DRAMADIST_TEST_FFMPEG")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
        else {
            eprintln!("跳过：未设置 DRAMADIST_TEST_FFMPEG");
            return;
        };

        let dir = std::env::temp_dir().join("dramadist_series_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 3 集连播：每集 = 2 秒片头（彩条）+ 3 秒内容（testsrc），共 15 秒
        let intro = "smptebars=size=320x240:rate=15";
        let content = "testsrc=size=320x240:rate=15";
        let mut filter = String::new();
        for ep in 0..3 {
            let base = ep * 5;
            // 每集 5 秒：[base, base+2) 是片头，[base+2, base+5) 是内容
            filter.push_str(&format!(
                "[0:v]trim=start={base}:end={},setpts=PTS-STARTPTS[a{ep}];",
                base + 2,
                ep = ep
            ));
            filter.push_str(&format!(
                "[1:v]trim=start={}:end={},setpts=PTS-STARTPTS[b{ep}];",
                base + 2,
                base + 5,
                ep = ep
            ));
        }
        filter.push_str("[a0][b0][a1][b1][a2][b2]concat=n=6:v=1:a=0[out]");

        let video = dir.join("three_episodes.mp4");
        let args: Vec<String> = vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-y".into(),
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            intro.into(),
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            content.into(),
            "-filter_complex".into(),
            filter,
            "-map".into(),
            "[out]".into(),
            "-t".into(),
            "15".into(),
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "ultrafast".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
            video.to_string_lossy().to_string(),
        ];
        let status = crate::proc::no_window(Command::new(&ffmpeg).args(&args))
            .status()
            .expect("无法启动 ffmpeg");
        assert!(status.success(), "生成三集连播测试素材失败");

        let total_ms = 15_000i64;
        let samples = sample_timeline(&ffmpeg, &video, total_ms, 2.0, 200).expect("采样失败");
        assert!(samples.len() >= 25, "15 秒 @2fps 应抽到约 30 帧，实际 {}", samples.len());

        let template = intro_template(&samples, 2_000);
        assert!(template.len() >= 3, "片头模板太短：{}", template.len());

        let hits = find_intro_positions(&samples, &template, 6, 3_000);
        assert_eq!(hits.len(), 3, "应当找到 3 次片头，实际命中 {hits:?}");

        let boundaries = boundaries_from_hits(&hits, total_ms, template.len());
        assert_eq!(boundaries.len(), 3);
        for (i, expect_start) in [0i64, 5_000, 10_000].iter().enumerate() {
            let delta = (boundaries[i].start_ms - expect_start).abs();
            assert!(
                delta <= 600,
                "第 {} 集起点应在 {}ms 附近（±600ms），实际 {}ms",
                i + 1,
                expect_start,
                boundaries[i].start_ms
            );
        }

        // 按边界切出来
        let out_dir = dir.join("episodes");
        let produced = resplit_by_episodes(&ffmpeg, &video, &boundaries, &out_dir).expect("切分失败");
        assert_eq!(produced.len(), 3);
        for (i, path) in produced.iter().enumerate() {
            assert!(path.is_file(), "第 {} 集没有产出文件", i + 1);
            let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            assert!(size > 0, "第 {} 集是空文件", i + 1);
        }
        assert!(out_dir.join("第01集.mp4").is_file());
        assert!(out_dir.join("第03集.mp4").is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
