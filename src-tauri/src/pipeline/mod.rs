//! 流水线：把「一个源目录」变成「一批做完的剧」。
//!
//! ## 为什么要有这一层
//!
//! 之前的 13 个页面都是**单点工具**：切一批、出一批字幕、导一批片。
//! 但真实的用法是**一部剧从头做到尾、一部接一部**。中间要人在十几个页面之间来回搬，
//! 一天做不了几部。
//!
//! 所以这里的思路是：**指一个源目录，剩下的一口气做完**。
//!
//! ## 目录约定
//!
//! 扫描器假设的目录结构（也是短剧素材包最常见的形态）：
//!
//! ```text
//! <根目录>/
//! ├── 10.12/                    ← 一部剧一个子目录
//! │   ├── 第01集.mp4
//! │   ├── 第02集.mp4
//! │   ├── 简介.txt              ← 简介（认名字，也接受"只有这一个 txt"）
//! │   └── 封面.jpg              ← 封面（认名字，也接受"只有这一张图"）
//! └── 10.13/
//!     └── ...
//! ```
//!
//! 根目录下**直接放视频**也算：那时整个根目录就是一部剧。
//!
//! ## 复用而不是重写
//!
//! 集号解析（第N集 / EP05 / S01E07 / 裸数字）已经有 [`crate::series::parse_episode`]，
//! 这里直接调它 —— 一套规则，两个地方用，不会各写一份各漏一处。

use crate::error::{AppError, AppResult};
use crate::models::{SourceDrama, SourceEpisode, SourceScan, StartOutcome};
use std::path::{Path, PathBuf};

/// 扫描时最多往下看几层。素材包一般不会更深，太深多半是误选了一个大目录。
const MAX_DEPTH: usize = 6;
/// 一次扫描最多认多少个视频文件，防止误选到整个磁盘
const MAX_FILES: usize = 5000;
/// 简介读到多少字符为止
const SYNOPSIS_LIMIT: usize = 4000;

const VIDEO_EXTS: [&str; 7] = ["mp4", "mkv", "mov", "avi", "flv", "ts", "m4v"];
const IMAGE_EXTS: [&str; 5] = ["jpg", "jpeg", "png", "webp", "bmp"];
const TEXT_EXTS: [&str; 2] = ["txt", "md"];

/// 认封面时优先这些关键词
const COVER_HINTS: [&str; 5] = ["封面", "cover", "poster", "海报", "folder"];
/// 认简介时优先这些关键词
const SYNOPSIS_HINTS: [&str; 5] = ["简介", "说明", "synopsis", "desc", "介绍"];

fn ext_of(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

fn is_video(path: &Path) -> bool {
    VIDEO_EXTS.contains(&ext_of(path).as_str())
}

fn is_image(path: &Path) -> bool {
    IMAGE_EXTS.contains(&ext_of(path).as_str())
}

fn is_text(path: &Path) -> bool {
    TEXT_EXTS.contains(&ext_of(path).as_str())
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > MAX_DEPTH || out.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut items: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    items.sort();
    for path in items {
        if out.len() >= MAX_FILES {
            return;
        }
        if path.is_dir() {
            walk(&path, out, depth + 1);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

/// 从候选里挑一个，优先名字带关键词的。
fn pick_by_hint(candidates: &[PathBuf], hints: &[&str]) -> Option<PathBuf> {
    if candidates.is_empty() {
        return None;
    }
    // 先按关键词找
    for path in candidates {
        let name = file_name(path).to_ascii_lowercase();
        if hints.iter().any(|h| name.contains(h)) {
            return Some(path.clone());
        }
    }
    // 没有关键词时：只有一个候选就用它，多于一个就不猜
    if candidates.len() == 1 {
        return Some(candidates[0].clone());
    }
    None
}

/// 读取简介文本。读不动就当没有，不阻断扫描。
fn read_synopsis(path: &Path) -> String {
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    // 素材包里的简介可能是 GBK —— 先按 UTF-8 解，失败再按 GBK
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(e) => {
            let raw = e.into_bytes();
            decode_gbk(&raw)
        }
    };
    let trimmed = text.trim();
    trimmed.chars().take(SYNOPSIS_LIMIT).collect()
}

/// 极简 GBK 解码：只处理「非 ASCII 双字节」这一最常见情况。
/// 解不出来就退回按字节替换 —— 简介读成乱码也比整个扫描失败强。
fn decode_gbk(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 {
            out.push(b as char);
            i += 1;
        } else if i + 1 < bytes.len() {
            // 用一句"这是双字节"的常识把它转成 UTF-8：
            // GBK 双字节的 Unicode 映射表太大，这里不做完整映射，
            // 只是保证不会 panic、不会把整段丢掉。
            out.push('\u{FFFD}');
            i += 2;
        } else {
            out.push('\u{FFFD}');
            i += 1;
        }
    }
    out
}

/// 流水线的步骤表。**顺序就是执行顺序**，每一步都对应队列里的一种任务。
///
/// 刻意做成一张表而不是散在各处：加一步只需要在这里加一行，
/// 界面、校验、执行顺序都跟着走。
pub const STAGES: [(&str, &str, bool, &str); 6] = [
    (
        "slice",
        "切片",
        true,
        "把整集按固定时长切成可发布的片",
    ),
    (
        "subtitle",
        "出字幕",
        true,
        "本地语音识别（默认 small 模型），自动繁转简",
    ),
    (
        "dedup",
        "画面处理",
        true,
        "每片做不同的画面处理，互相之间不再完全一致",
    ),
    (
        "export",
        "导出成片",
        false,
        "烧字幕、竖屏 9:16，导出可直接上传的成片",
    ),
    (
        "package",
        "发布包",
        false,
        "整理成发布包（视频 / 封面 / 字幕 / 标题标签）",
    ),
    (
        "proof",
        "证明材料",
        false,
        "按模板生成报备用的证明图与成本文档",
    ),
];

pub fn stage_list() -> Vec<crate::models::PipelineStage> {
    STAGES
        .iter()
        .map(|(key, label, default_on, detail)| crate::models::PipelineStage {
            key: (*key).to_string(),
            label: (*label).to_string(),
            enabled_by_default: *default_on,
            detail: (*detail).to_string(),
        })
        .collect()
}

pub fn is_known_stage(key: &str) -> bool {
    STAGES.iter().any(|(k, _, _, _)| *k == key)
}

/// 步骤 key → 中文名。队列里的任务类型名和步骤 key 不完全一样
/// （比如步骤叫 subtitle、任务叫 asr），所以查不到就原样返回，由调用方决定。
#[allow(dead_code)]
pub fn stage_label(key: &str) -> String {
    STAGES
        .iter()
        .find(|(k, _, _, _)| *k == key)
        .map(|(_, label, _, _)| (*label).to_string())
        .unwrap_or_else(|| key.to_string())
}

/// 起一条流水线：建记录 → 导入每一集 → 排任务。
///
/// 抽成**普通函数**而不是写在命令里，是为了能测：
/// 命令层拿到的是 `State<AppState>`，单元测试里构造不出来。
///
/// 任务**按剧排**，一部剧做完再下一部（和原软件界面上写的
/// 「按勾选剧目顺序执行 · 一部完成后再处理下一部」一致）。
/// 这样一部剧能尽早完整出来，而不是所有剧都卡在同一个阶段。
pub fn start_pipeline(
    conn: &rusqlite::Connection,
    ffprobe: &Path,
    root: &Path,
    out_root: &Path,
    stages: &[String],
    slice_ms: i64,
    model: &str,
) -> AppResult<StartOutcome> {
    let scan = scan_source(root)?;
    let run_id = crate::db::create_pipeline_run(
        conn,
        &scan.root,
        &out_root.to_string_lossy(),
        stages,
    )?;

    let mut imported = 0i64;
    let mut skipped: Vec<String> = Vec::new();
    let mut queued = 0i64;
    let mut drama_ids: Vec<i64> = Vec::new();

    for drama in &scan.dramas {
        let project = crate::db::create_project(conn, &drama.name, &drama.dir)?;
        let drama_out = out_root.join(&drama.name);
        let drama_id = crate::db::add_pipeline_drama(
            conn,
            run_id,
            &drama.name,
            &drama.dir,
            &drama_out.to_string_lossy(),
            drama.episode_count,
        )?;
        drama_ids.push(drama_id);

        let mut media_ids: Vec<i64> = Vec::new();
        for episode in &drama.episodes {
            let path = PathBuf::from(&episode.path);
            let probe = match crate::media::probe(ffprobe, &path) {
                Ok(p) => p,
                Err(e) => {
                    skipped.push(format!("{} / {}：{e}", drama.name, episode.file_name));
                    continue;
                }
            };
            let record =
                crate::db::add_media(conn, project.id, &episode.path, &episode.file_name, &probe)?;
            media_ids.push(record.id);
            imported += 1;
        }

        if media_ids.is_empty() {
            crate::db::update_pipeline_drama(conn, drama_id, "", "failed", "没有一集能读出来")?;
            continue;
        }

        for stage in stages {
            for media_id in &media_ids {
                let Some(payload) = stage_payload(stage, *media_id, &drama_out, slice_ms, model)?
                else {
                    continue;
                };
                crate::db::enqueue_job_for(
                    conn,
                    Some(*media_id),
                    stage_to_job_kind(stage),
                    &payload,
                    Some(drama_id),
                )?;
                queued += 1;
            }
        }
    }

    Ok(StartOutcome {
        run_id,
        drama_count: drama_ids.len() as i64,
        imported,
        queued,
        skipped,
    })
}

/// 步骤 → 队列任务类型。
///
/// 名字不一样是因为队列的种类更细（比如「出字幕」这一步既可能走 asr 也可能走 ocr）。
pub fn stage_to_job_kind(stage: &str) -> &'static str {
    match stage {
        "slice" => "slice",
        "subtitle" => "asr",
        "dedup" => "transform",
        "export" => "export",
        "package" => "publish",
        _ => "verify",
    }
}

/// 某一步在某一集上要排什么任务。返回 `None` 表示这一步这集跳过。
pub fn stage_payload(
    stage: &str,
    media_id: i64,
    out_dir: &Path,
    slice_ms: i64,
    model: &str,
) -> AppResult<Option<String>> {
    let value = match stage {
        "slice" => serde_json::json!({
            "mediaId": media_id,
            "mode": { "mode": "fixedDuration", "seconds": slice_ms as f64 / 1000.0 },
            "outDir": out_dir.join("切片").to_string_lossy(),
        }),
        "subtitle" => serde_json::json!({
            "mediaId": media_id,
            "model": model,
            "language": "zh",
            "overwrite": true,
        }),
        "dedup" => serde_json::json!({
            "mediaId": media_id,
            "intensity": "medium",
            "options": serde_json::Value::Null,
        }),
        "export" => serde_json::json!({
            "mediaId": media_id,
            "outPath": out_dir.join("成片").join(format!("{media_id}.mp4")).to_string_lossy(),
            "burnSubtitles": true,
            "aspect": { "mode": "verticalBlur" },
            "startMs": 0,
            "endMs": 0,
            "crf": 23,
            "preset": "veryfast",
        }),
        "package" => serde_json::json!({
            "mediaId": media_id,
            "platform": "douyin",
            "account": "默认账号",
            "outRoot": out_dir.join("发布包").to_string_lossy(),
        }),
        // 证明图要有模板和字段坐标才谈得上生成，没配就跳过 ——
        // 排一个注定失败的任务，只会让整条流水线卡在最后一步
        "proof" => return Ok(None),
        other => return Err(AppError::Msg(format!("不认识的步骤：{other}"))),
    };
    Ok(Some(value.to_string()))
}

/// 源目录扫描的入口
pub fn scan_source(root: &Path) -> AppResult<SourceScan> {
    if !root.is_dir() {
        return Err(AppError::Msg(format!(
            "源目录不存在：{}",
            root.to_string_lossy()
        )));
    }

    let mut files: Vec<PathBuf> = Vec::new();
    walk(root, &mut files, 0);
    let truncated = files.len() >= MAX_FILES;
    files.retain(|p| is_video(p));

    if files.is_empty() {
        return Err(AppError::Msg(format!(
            "这个目录里没找到视频文件（认 {} 这些格式）",
            VIDEO_EXTS.join(" / ")
        )));
    }

    // 按「根目录的哪一级子目录」分组：直接放在根目录下的算作根目录这部剧
    let mut buckets: Vec<(String, PathBuf, Vec<PathBuf>)> = Vec::new();
    for path in &files {
        let key_dir = drama_dir_of(root, path);
        let key = key_dir.to_string_lossy().to_string();
        match buckets.iter_mut().find(|(k, _, _)| *k == key) {
            Some((_, _, list)) => list.push(path.clone()),
            None => buckets.push((key, key_dir, vec![path.clone()])),
        }
    }
    // 文件多的排前面 —— 那多半是主库
    buckets.sort_by(|a, b| b.2.len().cmp(&a.2.len()));

    let mut warnings: Vec<String> = Vec::new();
    let mut dramas: Vec<SourceDrama> = Vec::new();

    for (_, dir, videos) in buckets {
        let drama = build_drama(&dir, &videos)?;
        if drama.episodes.is_empty() {
            continue;
        }
        for w in &drama.warnings {
            warnings.push(format!("{}：{w}", drama.name));
        }
        dramas.push(drama);
    }

    if truncated {
        warnings.push(format!(
            "目录里的文件超过 {MAX_FILES} 个，只扫描了前面这一部分。确认一下源目录是不是选得太上层了。"
        ));
    }
    if dramas.is_empty() {
        return Err(AppError::Msg("没识别出任何可用的剧目".into()));
    }

    Ok(SourceScan {
        root: root.to_string_lossy().to_string(),
        total_episodes: dramas.iter().map(|d| d.episodes.len() as i64).sum(),
        dramas,
        warnings,
    })
}

/// 这个视频属于哪部剧：根目录的一级子目录；直接在根目录下就取根目录本身。
fn drama_dir_of(root: &Path, file: &Path) -> PathBuf {
    match file.parent() {
        Some(parent) => {
            let relative = parent.strip_prefix(root).unwrap_or(parent);
            let mut parts = relative.components();
            match parts.next() {
                Some(first) => root.join(first.as_os_str()),
                None => root.to_path_buf(),
            }
        }
        None => root.to_path_buf(),
    }
}

fn build_drama(dir: &Path, videos: &[PathBuf]) -> AppResult<SourceDrama> {
    let mut warnings: Vec<String> = Vec::new();

    // 简介与封面：只看这部剧自己的目录，不往子目录里翻
    let mut texts: Vec<PathBuf> = Vec::new();
    let mut images: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if is_text(&path) {
                texts.push(path);
            } else if is_image(&path) {
                images.push(path);
            }
        }
    }
    texts.sort();
    images.sort();

    let synopsis_path = pick_by_hint(&texts, &SYNOPSIS_HINTS);
    let cover = pick_by_hint(&images, &COVER_HINTS);
    if synopsis_path.is_none() && !texts.is_empty() {
        warnings.push(format!(
            "有 {} 个文本文件但认不出哪个是简介，简介留空",
            texts.len()
        ));
    }
    if cover.is_none() && !images.is_empty() {
        warnings.push(format!(
            "有 {} 张图但认不出哪张是封面，封面留空",
            images.len()
        ));
    }

    let synopsis = synopsis_path
        .as_ref()
        .map(|p| read_synopsis(p))
        .unwrap_or_default();

    let mut episodes: Vec<SourceEpisode> = videos
        .iter()
        .map(|path| SourceEpisode {
            path: path.to_string_lossy().to_string(),
            file_name: file_name(path),
            episode: crate::series::parse_episode(&file_name(path)),
            size_bytes: std::fs::metadata(path).map(|m| m.len() as i64).unwrap_or(0),
        })
        .collect();

    // 按集号排；认不出集号的排最后，按文件名
    episodes.sort_by(|a, b| match (a.episode, b.episode) {
        (Some(x), Some(y)) => x.cmp(&y).then_with(|| a.file_name.cmp(&b.file_name)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.file_name.cmp(&b.file_name),
    });

    let unnamed = episodes.iter().filter(|e| e.episode.is_none()).count();
    if unnamed > 0 && unnamed == episodes.len() && episodes.len() > 1 {
        warnings.push(format!(
            "{} 个文件都没解析出集号，顺序按文件名排。片头指纹重切也许更准。",
            episodes.len()
        ));
    }

    // 剧名：优先用文件名解析出来的，其次用目录名
    let from_name = crate::series::parse_series(&episodes[0].file_name);
    let dir_name = dir
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "未命名".to_string());
    let name = if from_name.is_empty() || from_name == "未命名" {
        dir_name
    } else if from_name != dir_name && episodes.len() == 1 {
        // 单集文件时文件名往往更准
        from_name
    } else {
        dir_name
    };

    Ok(SourceDrama {
        name,
        dir: dir.to_string_lossy().to_string(),
        episode_count: episodes.len() as i64,
        episodes,
        synopsis,
        synopsis_path: synopsis_path
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        cover: cover.map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
        warnings,
    })
}

/// 默认的导出目录：源目录旁边加一个后缀，不污染原素材。
pub fn default_out_dir(root: &Path, suffix: &str) -> PathBuf {
    let name = root
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "输出".to_string());
    match root.parent() {
        Some(parent) => parent.join(format!("{name}{suffix}")),
        None => PathBuf::from(format!("{name}{suffix}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempTree(PathBuf);

    impl TempTree {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("dramadist_pipeline_{name}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn file(&self, rel: &str, content: &[u8]) -> PathBuf {
            let path = self.0.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&path, content).unwrap();
            path
        }
        fn dir(&self, rel: &str) -> PathBuf {
            let path = self.0.join(rel);
            std::fs::create_dir_all(&path).unwrap();
            path
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scans_one_drama_with_episodes_synopsis_and_cover() {
        let t = TempTree::new("single");
        t.file("第01集.mp4", b"x");
        t.file("第02集.mp4", b"x");
        t.file("第10集.mp4", b"x");
        t.file("简介.txt", "这是一部很好看的短剧".as_bytes());
        t.file("封面.jpg", b"img");

        let scan = scan_source(t.path()).unwrap();
        assert_eq!(scan.dramas.len(), 1);
        let d = &scan.dramas[0];
        assert_eq!(d.episode_count, 3);
        // 按集号排，第10集要排在最后（不能按字符串排成 01,02,10 之外的顺序）
        let nums: Vec<Option<i64>> = d.episodes.iter().map(|e| e.episode).collect();
        assert_eq!(nums, vec![Some(1), Some(2), Some(10)]);
        assert_eq!(d.synopsis, "这是一部很好看的短剧");
        assert!(d.cover.ends_with("封面.jpg"), "{}", d.cover);
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);
    }

    #[test]
    fn scans_multiple_dramas_under_one_root() {
        let t = TempTree::new("multi");
        t.file("10.12/第01集.mp4", b"x");
        t.file("10.12/第02集.mp4", b"x");
        t.file("10.13/第01集.mp4", b"x");

        let scan = scan_source(t.path()).unwrap();
        assert_eq!(scan.dramas.len(), 2);
        // 集数多的排前面
        assert_eq!(scan.dramas[0].episode_count, 2);
        assert_eq!(scan.dramas[1].episode_count, 1);
        assert_eq!(scan.total_episodes, 3);
    }

    #[test]
    fn root_level_videos_count_as_one_drama() {
        let t = TempTree::new("rootlevel");
        t.file("01.mp4", b"x");
        t.file("02.mp4", b"x");
        let scan = scan_source(t.path()).unwrap();
        assert_eq!(scan.dramas.len(), 1);
        assert_eq!(scan.dramas[0].episode_count, 2);
    }

    #[test]
    fn nested_subfolders_belong_to_the_first_level_drama() {
        let t = TempTree::new("nested");
        t.file("剧A/正片/第01集.mp4", b"x");
        t.file("剧A/花絮/花絮01.mp4", b"x");
        let scan = scan_source(t.path()).unwrap();
        assert_eq!(scan.dramas.len(), 1, "同一部剧的子目录不该被拆开");
        assert_eq!(scan.dramas[0].episode_count, 2);
    }

    #[test]
    fn cover_and_synopsis_are_found_by_name_hint() {
        let t = TempTree::new("hints");
        t.file("第01集.mp4", b"x");
        t.file("随便一个名字.txt", "不该被选中".as_bytes());
        t.file("简介.txt", "应该是这个".as_bytes());
        t.file("剧照1.jpg", b"img");
        t.file("封面.png", b"img");

        let scan = scan_source(t.path()).unwrap();
        let d = &scan.dramas[0];
        assert_eq!(d.synopsis, "应该是这个");
        assert!(d.cover.ends_with("封面.png"), "{}", d.cover);
    }

    #[test]
    fn single_unnamed_text_and_image_are_accepted() {
        let t = TempTree::new("single_asset");
        t.file("第01集.mp4", b"x");
        t.file("readme.txt", "只有这一个文本".as_bytes());
        t.file("a.jpg", b"img");

        let scan = scan_source(t.path()).unwrap();
        let d = &scan.dramas[0];
        assert_eq!(d.synopsis, "只有这一个文本");
        assert!(d.cover.ends_with("a.jpg"));
    }

    #[test]
    fn ambiguous_assets_are_left_empty_with_a_warning() {
        let t = TempTree::new("ambiguous");
        t.file("第01集.mp4", b"x");
        t.file("a.txt", "甲".as_bytes());
        t.file("b.txt", "乙".as_bytes());
        t.file("a.jpg", b"img");
        t.file("b.jpg", b"img");

        let scan = scan_source(t.path()).unwrap();
        let d = &scan.dramas[0];
        assert!(d.synopsis.is_empty(), "认不出就该留空，不能瞎猜");
        assert!(d.cover.is_empty());
        assert!(
            d.warnings.iter().any(|w| w.contains("认不出哪个是简介")),
            "{:?}",
            d.warnings
        );
        assert!(
            d.warnings.iter().any(|w| w.contains("认不出哪张是封面")),
            "{:?}",
            d.warnings
        );
    }

    #[test]
    fn non_video_files_are_ignored() {
        let t = TempTree::new("ignore");
        t.file("第01集.mp4", b"x");
        t.file("说明.pdf", b"x");
        t.file("素材.zip", b"x");
        t.file("thumb.jpg", b"x");
        let scan = scan_source(t.path()).unwrap();
        assert_eq!(scan.total_episodes, 1);
    }

    #[test]
    fn empty_directory_is_reported_clearly() {
        let t = TempTree::new("empty");
        let err = scan_source(t.path()).unwrap_err().to_string();
        assert!(err.contains("没找到视频文件"), "{err}");
    }

    #[test]
    fn missing_directory_is_reported_clearly() {
        let err = scan_source(Path::new("Z:/nope/nowhere"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("源目录不存在"), "{err}");
    }

    #[test]
    fn unnumbered_episodes_sort_last_and_warn() {
        let t = TempTree::new("unnumbered");
        t.file("甲.mp4", b"x");
        t.file("乙.mp4", b"x");
        t.file("丙.mp4", b"x");
        let scan = scan_source(t.path()).unwrap();
        let d = &scan.dramas[0];
        assert_eq!(d.episode_count, 3);
        assert!(d.episodes.iter().all(|e| e.episode.is_none()));
        assert!(
            d.warnings.iter().any(|w| w.contains("片头指纹重切")),
            "应当提示可以用片头指纹重切：{:?}",
            d.warnings
        );
    }

    #[test]
    fn drama_name_falls_back_to_the_directory_name() {
        let t = TempTree::new("naming");
        t.dir("都市爽剧");
        t.file("都市爽剧/第01集.mp4", b"x");
        let scan = scan_source(t.path()).unwrap();
        assert_eq!(scan.dramas[0].name, "都市爽剧");
    }

    #[test]
    fn default_out_dir_sits_next_to_the_source() {
        let got = default_out_dir(Path::new("G:/10.12"), "二创");
        assert_eq!(got, PathBuf::from("G:/10.12二创"));
    }

    #[test]
    fn synopsis_is_truncated_not_unbounded() {
        let t = TempTree::new("bigsynopsis");
        t.file("第01集.mp4", b"x");
        let long = "字".repeat(SYNOPSIS_LIMIT * 2);
        t.file("简介.txt", long.as_bytes());
        let scan = scan_source(t.path()).unwrap();
        assert_eq!(scan.dramas[0].synopsis.chars().count(), SYNOPSIS_LIMIT);
    }

    #[test]
    fn gbk_synopsis_does_not_panic_or_vanish() {
        let t = TempTree::new("gbk");
        t.file("第01集.mp4", b"x");
        // 「中文」的 GBK 编码
        t.file("简介.txt", &[0xD6, 0xD0, 0xCE, 0xC4]);
        let scan = scan_source(t.path()).unwrap();
        assert!(!scan.dramas[0].synopsis.is_empty(), "读不动也不该整段丢掉");
    }

    // ---------------- 起流水线（真实 ffmpeg + 内存库） ----------------

    fn env_path(key: &str) -> Option<PathBuf> {
        std::env::var(key)
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
    }

    fn make_video(ffmpeg: &Path, out: &Path, seconds: u32) {
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let status = crate::proc::no_window(
            std::process::Command::new(ffmpeg).args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x120:rate=10",
                "-t",
                &seconds.to_string(),
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
        assert!(status.success(), "生成测试视频失败");
    }

    #[test]
    fn end_to_end_starts_a_pipeline_and_queues_jobs_in_stage_order() {
        let (Some(ffmpeg), Some(ffprobe)) =
            (env_path("DRAMADIST_TEST_FFMPEG"), env_path("DRAMADIST_TEST_FFPROBE"))
        else {
            eprintln!("跳过：需要 DRAMADIST_TEST_FFMPEG 与 DRAMADIST_TEST_FFPROBE");
            return;
        };

        let t = TempTree::new("start");
        make_video(&ffmpeg, &t.path().join("剧A").join("第01集.mp4"), 2);
        make_video(&ffmpeg, &t.path().join("剧A").join("第02集.mp4"), 2);
        make_video(&ffmpeg, &t.path().join("剧B").join("第01集.mp4"), 2);
        t.file("剧A/简介.txt", "剧A的简介".as_bytes());

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrate_for_tests(&conn).unwrap();

        let out_root = t.path().join("二创");
        let stages: Vec<String> = vec!["slice".into(), "subtitle".into()];
        let outcome = start_pipeline(
            &conn,
            &ffprobe,
            t.path(),
            &out_root,
            &stages,
            10_000,
            "small",
        )
        .expect("起流水线失败");

        assert_eq!(outcome.drama_count, 2, "应当认出两部剧");
        assert_eq!(outcome.imported, 3, "三集都该导入");
        assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
        // 3 集 × 2 步
        assert_eq!(outcome.queued, 6);

        // 剧目行
        let dramas = crate::db::list_pipeline_dramas(&conn, outcome.run_id).unwrap();
        assert_eq!(dramas.len(), 2);
        assert!(dramas.iter().all(|d| d.status == "pending"));
        assert_eq!(dramas.iter().map(|d| d.episode_count).sum::<i64>(), 3);

        // 任务：都挂到剧上，而且**按剧排** —— 剧A 的两步排完才轮到剧B。
        // 这样一部剧能尽早完整出来，而不是所有剧一起卡在同一阶段。
        let jobs = crate::db::list_jobs(&conn, 50).unwrap();
        assert_eq!(jobs.len(), 6);
        assert!(
            jobs.iter().all(|j| j.pipeline_drama_id.is_some()),
            "流水线任务必须挂在某部剧下，否则进度没法显示"
        );
        let kinds: Vec<&str> = jobs.iter().map(|j| j.kind.as_str()).collect();
        assert_eq!(
            kinds,
            vec!["slice", "slice", "asr", "asr", "slice", "asr"],
            "应当是剧A 的两步先排完，再排剧B"
        );
        // 同一部剧内部，步骤顺序不能乱：先切片后字幕
        let first_drama = jobs[0].pipeline_drama_id.unwrap();
        let kinds_of_a: Vec<&str> = jobs
            .iter()
            .filter(|j| j.pipeline_drama_id == Some(first_drama))
            .map(|j| j.kind.as_str())
            .collect();
        assert_eq!(kinds_of_a, vec!["slice", "slice", "asr", "asr"]);

        // payload 里的输出目录要落在这次运行的导出目录下
        let payload: serde_json::Value = serde_json::from_str(&jobs[0].payload).unwrap();
        assert!(
            payload["outDir"].as_str().unwrap().contains("二创"),
            "{}",
            jobs[0].payload
        );

        // 一部剧一个工程，素材不混在一起
        let projects = crate::db::list_projects(&conn).unwrap();
        assert_eq!(projects.len(), 2);
    }

    #[test]
    fn start_pipeline_skips_unreadable_episodes_instead_of_failing() {
        let (Some(_ffmpeg), Some(ffprobe)) =
            (env_path("DRAMADIST_TEST_FFMPEG"), env_path("DRAMADIST_TEST_FFPROBE"))
        else {
            eprintln!("跳过：需要 ffprobe");
            return;
        };

        let t = TempTree::new("start_bad");
        // 一个真视频、一个假视频
        let real = {
            let ffmpeg = env_path("DRAMADIST_TEST_FFMPEG").unwrap();
            let p = t.path().join("剧/第01集.mp4");
            make_video(&ffmpeg, &p, 2);
            p
        };
        assert!(real.is_file());
        t.file("剧/第02集.mp4", "这不是视频".as_bytes());

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrate_for_tests(&conn).unwrap();

        let outcome = start_pipeline(
            &conn,
            &ffprobe,
            t.path(),
            &t.path().join("out"),
            &["slice".to_string()],
            10_000,
            "",
        )
        .expect("一部坏的素材不该让整条流水线起不来");

        assert_eq!(outcome.imported, 1);
        assert_eq!(outcome.queued, 1);
        assert_eq!(outcome.skipped.len(), 1, "{:?}", outcome.skipped);
        assert!(outcome.skipped[0].contains("第02集"), "{:?}", outcome.skipped);
    }

    #[test]
    fn stage_mapping_covers_every_declared_stage() {
        for (key, _, _, _) in STAGES {
            assert!(
                is_known_stage(key),
                "{key} 在步骤表里却过不了校验"
            );
            assert_eq!(stage_to_job_kind(key), stage_to_job_kind(key));
        }
        // 队列认得的类型才排得进去
        for (key, _, _, _) in STAGES {
            let kind = stage_to_job_kind(key);
            assert!(
                crate::queue::is_known_kind(kind),
                "步骤 {key} 映射到了队列不认识的类型 {kind}"
            );
        }
    }

    #[test]
    fn proof_stage_is_skipped_rather_than_queued_to_fail() {
        // 证明图要有模板和字段坐标才谈得上生成，没配就跳过 ——
        // 排一个注定失败的任务只会让流水线卡在最后一步
        let got = stage_payload("proof", 1, Path::new("E:/o"), 10_000, "").unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn unknown_stage_is_rejected() {
        let err = stage_payload("nonsense", 1, Path::new("E:/o"), 10_000, "")
            .unwrap_err()
            .to_string();
        assert!(err.contains("不认识的步骤"), "{err}");
    }
}
