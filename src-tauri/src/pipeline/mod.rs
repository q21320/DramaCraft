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
use crate::models::{HeaderLabel, SourceDrama, SourceEpisode, SourceScan, StartOutcome};
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

/// 认封面时优先这些关键词。
///
/// `poster` 不是拍脑袋加的 —— 真实素材包里封面就叫 `poster.jpg`。
const COVER_HINTS: [&str; 6] = ["封面", "cover", "poster", "海报", "folder", "thumb"];
/// 认简介时优先这些关键词。
///
/// `intro` 同理：真实素材包里简介叫 `intro.txt`。这类素材包常用英文文件名，
/// 只认中文关键词会漏掉。
const SYNOPSIS_HINTS: [&str; 7] = ["简介", "说明", "介绍", "intro", "synopsis", "desc", "readme"];

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
pub const STAGES: [(&str, &str, bool, &str); 8] = [
    (
        "rewrite",
        "AI 改剧名简介",
        false,
        "先改简介，再按新简介起剧名（整部剧一步，需先在「AI 服务」配好）",
    ),
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
        "cover",
        "生成封面",
        false,
        "用原封面 + 新剧名合成封面（整部剧一张）",
    ),
    (
        "proof",
        "证明材料",
        false,
        "按模板生成报备用的证明图与成本文档",
    ),
];

/// 这一步是**整部剧一步**吗？
///
/// 改剧名、生成封面、出证明材料都是针对整部剧的，不该按集排 ——
/// 60 集排 60 个「改剧名」任务，会调 120 次模型，钱白花。
pub fn is_drama_level(stage: &str) -> bool {
    matches!(stage, "rewrite" | "cover" | "proof")
}

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
    header_label: Option<HeaderLabel>,
) -> AppResult<StartOutcome> {
    let scan = scan_source(root)?;
    let run_id = crate::db::create_pipeline_run(
        conn,
        &scan.root,
        &out_root.to_string_lossy(),
        stages,
    )?;
    // 顺手裁掉太老的日志，免得攒到几万行拖慢界面
    let _ = crate::db::trim_logs(conn, 5000);

    let mut imported = 0i64;
    let mut skipped: Vec<String> = Vec::new();
    let mut queued = 0i64;
    let mut drama_ids: Vec<i64> = Vec::new();
    // 复用了多少 —— 报给用户看，让他知道这次没有再重复导入
    let mut reused_projects = 0i64;
    let mut reused_media = 0i64;

    // 日志开个头：源头扫到了什么。日志页第一眼看到的就是这行。
    crate::db::append_log(
        conn,
        Some(run_id),
        None,
        "info",
        &format!(
            "扫描「{}」：{} 部剧、共 {} 集。步骤：{}",
            scan.root,
            scan.dramas.len(),
            scan.total_episodes,
            stages
                .iter()
                .map(|s| stage_label(s))
                .collect::<Vec<_>>()
                .join(" → ")
        ),
    );
    for w in &scan.warnings {
        crate::db::append_log(conn, Some(run_id), None, "warn", w);
    }

    for drama in &scan.dramas {
        // **复用已有工程，不重复建。**
        //
        // 原来每次跑都新建一个，同一批素材被导入好几遍：实测三个工程指向
        // 同一个源目录，163 条素材里只有 65 个不同文件，产出页面因此错乱。
        let project = match crate::db::find_project_by_root(conn, &drama.dir)? {
            Some(existing) => {
                reused_projects += 1;
                existing
            }
            None => crate::db::create_project(conn, &drama.name, &drama.dir)?,
        };
        let drama_out = out_root.join(&drama.name);
        let drama_id = crate::db::add_pipeline_drama(
            conn,
            run_id,
            &drama.name,
            &drama.dir,
            &drama_out.to_string_lossy(),
            drama.episode_count,
            Some(project.id),
        )?;
        drama_ids.push(drama_id);

        // 日志：这部剧扫到了什么、是新建还是复用。
        // 用户回头看日志时，最想知道的就是「它到底认出了什么」。
        crate::db::append_log(
            conn,
            Some(run_id),
            Some(drama_id),
            "info",
            &format!(
                "剧场「{}」：{} 集{}",
                drama.name,
                drama.episode_count,
                if drama.synopsis.trim().is_empty() {
                    "，无简介"
                } else {
                    "，有简介"
                }
            ),
        );
        for w in &drama.warnings {
            crate::db::append_log(conn, Some(run_id), Some(drama_id), "warn", w);
        }

        // 每集单独一个子目录。
        //
        // 这不是美观问题：切片是按「这一集的第几片」编号的（001、002…），
        // 16 集共用一个目录就会互相覆盖 —— 真实素材那轮 27 片只剩最后 3 片，
        // 21 片被后来的覆盖掉了，而数据库里还显示"有"（路径确实存在，
        // 只是内容是别人的）。
        let mut media_groups: Vec<(i64, String)> = Vec::new();
        let mut media_ids: Vec<i64> = Vec::new();
        for episode in &drama.episodes {
            let path = PathBuf::from(&episode.path);
            // 同一个文件不重复导入：已经有记录就直接用
            if let Some(existing) =
                crate::db::find_media_by_path(conn, project.id, &episode.path)?
            {
                media_ids.push(existing.id);
                media_groups.push((existing.id, safe_dir_name(&episode.file_name)));
                reused_media += 1;
                continue;
            }
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
            media_groups.push((record.id, safe_dir_name(&episode.file_name)));
            imported += 1;
        }

        if media_ids.is_empty() {
            crate::db::update_pipeline_drama(conn, drama_id, "", "failed", "没有一集能读出来")?;
            continue;
        }

        for stage in stages {
            // 整部剧一步的（改剧名、封面）：**只排一个任务**，不按集排。
            // 60 集排 60 个「改剧名」= 调 120 次模型，钱白花。
            if is_drama_level(stage) {
                match drama_payload(stage, drama, &drama_out)? {
                    Some(payload) => {
                        crate::db::enqueue_job_for(
                            conn,
                            None,
                            stage_to_job_kind(stage),
                            &payload,
                            Some(drama_id),
                        )?;
                        queued += 1;
                    }
                    None => {
                        // 跳过要说清楚为什么，否则用户只看到「这步没跑」
                        let why = match stage.as_str() {
                            "rewrite" => "没找到简介",
                            "cover" => "没找到封面图",
                            _ => "没配模板",
                        };
                        skipped.push(format!(
                            "{} / {}：{why}，这一步跳过",
                            drama.name,
                            stage_label(stage)
                        ));
                    }
                }
                continue;
            }
            for (media_id, group) in &media_groups {
                let Some(payload) =
                    stage_payload(stage, *media_id, &drama_out, slice_ms, model, group, header_label.as_ref())?
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

    // 收尾日志：这次一共排了多少活、复用了多少。
    // 「复用」要写出来 —— 否则用户看到"没导入"会以为出错了（那个 bug 真发生过）。
    crate::db::append_log(
        conn,
        Some(run_id),
        None,
        "ok",
        &format!(
            "已排 {} 个任务（新增素材 {} 条，复用 {} 条，复用工程 {} 个）",
            queued, imported, reused_media, reused_projects
        ),
    );
    for s in &skipped {
        crate::db::append_log(conn, Some(run_id), None, "warn", s);
    }

    Ok(StartOutcome {
        run_id,
        drama_count: drama_ids.len() as i64,
        imported,
        queued,
        skipped,
        reused_projects,
        reused_media,
    })
}

/// 步骤 → 队列任务类型。
///
/// 名字不一样是因为队列的种类更细（比如「出字幕」这一步既可能走 asr 也可能走 ocr）。
pub fn stage_to_job_kind(stage: &str) -> &'static str {
    match stage {
        "rewrite" => "rewrite",
        "slice" => "slice",
        "subtitle" => "asr",
        "dedup" => "transform",
        "export" => "export",
        "package" => "publish",
        "cover" => "cover",
        _ => "verify",
    }
}

/// 整部剧一步的步骤要排什么任务。返回 `None` 表示这一步这部剧跳过。
///
/// 缺素材就跳过而不是排一个注定失败的任务 —— 但要**说清楚为什么**，
/// 不然用户只看到「步骤没跑」，不知道是缺简介还是缺封面。
fn drama_payload(stage: &str, drama: &SourceDrama, drama_out: &Path) -> AppResult<Option<String>> {
    match stage {
        "rewrite" => {
            if drama.synopsis.trim().is_empty() {
                return Ok(None);
            }
            Ok(Some(
                serde_json::json!({
                    "title": drama.name,
                    "synopsis": drama.synopsis,
                    "outDir": drama_out.to_string_lossy(),
                })
                .to_string(),
            ))
        }
        "cover" => {
            if drama.cover.trim().is_empty() {
                return Ok(None);
            }
            Ok(Some(
                serde_json::json!({
                    "poster": drama.cover,
                    // 新剧名由 rewrite 写在这里；读不到就退回原剧名
                    "titleDir": drama_out.to_string_lossy(),
                    "fallbackTitle": drama.name,
                    "outPath": drama_out.join("封面.jpg").to_string_lossy(),
                    // 3:4 海报，和真实软件一致
                    "width": 1080,
                    "height": 1440,
                })
                .to_string(),
            ))
        }
        // 证明图要有模板和字段坐标才谈得上生成，没配就跳过
        "proof" => Ok(None),
        other => Err(AppError::Msg(format!("不是整部剧级别的步骤：{other}"))),
    }
}

/// 把文件名变成一个能当目录名的字符串。
///
/// 去掉扩展名，并把 Windows 不允许出现在路径里的字符换掉。
fn safe_dir_name(file_name: &str) -> String {
    let stem = Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| file_name.to_string());
    let cleaned: String = stem
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            other => other,
        })
        .collect();
    // 首尾的点都去掉：结尾的点 Windows 会丢，开头的点在 Unix 上会变隐藏目录，
    // 两边都不是我们想要的。
    // 注意 `.mp4` 这种情况 —— Rust 把它当成「隐藏文件、没有扩展名」，
    // file_stem 原样返回 `.mp4`，去掉点之后就空了，落到「未命名」。
    let trimmed = cleaned.trim().trim_matches('.').trim().to_string();
    if trimmed.is_empty() {
        "未命名".to_string()
    } else {
        trimmed
    }
}

/// 某一步在某一集上要排什么任务。返回 `None` 表示这一步这集跳过。
///
/// `group` 是这一集的目录名 —— **每一步的输出都要放进它自己的子目录**。
/// 少了这个，同一部剧的每一集都会往同一个目录里写同名文件，互相覆盖。
pub fn stage_payload(
    stage: &str,
    media_id: i64,
    out_dir: &Path,
    slice_ms: i64,
    model: &str,
    group: &str,
    label: Option<&HeaderLabel>,
) -> AppResult<Option<String>> {
    let here = out_dir.join(group);
    let value = match stage {
        "slice" => serde_json::json!({
            "mediaId": media_id,
            // 形状要对：`mode` 这一层装的是 SliceMode，而 SliceMode 自己是
            // 内部标签枚举（tag = "mode"），所以会出现两层 mode。
            // 变体名是 everySeconds。写错不会当场报，而是排进队列、
            // 跑到它的时候才炸 —— 真实素材那轮就是这么栽的。
            "mode": { "mode": "everySeconds", "seconds": slice_ms as f64 / 1000.0 },
            // 每集一个子目录：切片按「这一集的第几片」编号，
            // 共用目录会让 16 集互相覆盖
            "outDir": here.join("切片").to_string_lossy(),
            // 切出来要能直接上传：源素材常是 HEVC，而平台要 H.264
            "compatible": true,
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
            // options 留空 → 用按序号生成的确定性预设，每片各不相同
            "options": serde_json::Value::Null,
            // 片头标识**叠加**在预设之上：它是合规开关，和"怎么去重"是两件事
            "headerLabel": label,
        }),
        "export" => serde_json::json!({
            "mediaId": media_id,
            // 原来用 {media_id}.mp4 当文件名 —— 对用户毫无意义，
            // 而且换一次运行就换一批名字。用剧集名更好认。
            "outPath": here.join(format!("{group}_成片.mp4")).to_string_lossy(),
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
            "outRoot": here.join("发布包").to_string_lossy(),
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

/// 文件名是不是"重复下载留下的副本"。
///
/// 认两种最常见的形态：
/// - 结尾的括号序号：`第01集(1).mp4`、`第01集 (2).mp4`
/// - 副本题字：`第01集 - 副本.mp4`、`第01集 副本.mp4`、`第01集 copy.mp4`
///
/// 刻意**不**把 `_1`、`-1` 这类当副本 —— 那可能是合法的分卷命名，
/// 误判会把正常的集数吃掉。
fn looks_like_duplicate(file_name: &str) -> bool {
    let stem = Path::new(file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| file_name.to_string());
    let lowered = stem.to_lowercase();

    // 结尾的 (数字)
    if lowered.ends_with(')') {
        if let Some(open) = lowered.rfind('(') {
            let inner = &lowered[open + 1..lowered.len() - 1];
            if !inner.is_empty() && inner.chars().all(|c| c.is_ascii_digit()) {
                return true;
            }
        }
    }
    // 副本题字
    for marker in ["副本", "copy", "拷贝"] {
        if lowered.ends_with(marker) {
            return true;
        }
        // `- 副本` / `_副本` 也常见
        if lowered.ends_with(&format!("-{marker}")) || lowered.ends_with(&format!("_{marker}")) {
            return true;
        }
    }
    false
}

/// 同一集号只保留一个，返回被丢掉的文件名。
///
/// 保留规则（按优先级）：
/// 1. **不是副本的优先** —— 正常命名的那个才是"正主"
/// 2. 体积大的优先 —— 副本可能下了一半
/// 3. 文件名靠前的优先 —— 保证结果稳定可复现
///
/// 只处理**认得出集号**的；认不出集号的一律保留（宁多勿少，
/// 让用户自己看警告决定）。
fn drop_duplicate_episodes(episodes: &mut Vec<SourceEpisode>) -> Vec<String> {
    use std::collections::HashMap;

    let mut best: HashMap<i64, usize> = HashMap::new();
    let mut drop_at: Vec<usize> = Vec::new();

    for (i, ep) in episodes.iter().enumerate() {
        let Some(n) = ep.episode else { continue };
        match best.get(&n) {
            None => {
                best.insert(n, i);
            }
            Some(&keep) => {
                let keep_ep = &episodes[keep];
                let challenger_is_copy = looks_like_duplicate(&ep.file_name);
                let keeper_is_copy = looks_like_duplicate(&keep_ep.file_name);
                let challenger_wins = match (keeper_is_copy, challenger_is_copy) {
                    (true, false) => true,   // 正主换上来
                    (false, true) => false,  // 副本别想上位
                    _ => ep.size_bytes > keep_ep.size_bytes,
                };
                if challenger_wins {
                    drop_at.push(keep);
                    best.insert(n, i);
                } else {
                    drop_at.push(i);
                }
            }
        }
    }

    drop_at.sort_unstable();
    let mut ignored: Vec<String> = Vec::new();
    for i in drop_at.into_iter().rev() {
        ignored.push(episodes.remove(i).file_name);
    }
    ignored.reverse();
    ignored
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

    // 同一集号出现多个文件时只留一个。
    //
    // 素材包里这种情况很常见：同一个文件下过两次，系统自动加上 `(1)`、
    // `- 副本` 之类的后缀。它们**时长和字节数完全一样**，是同一份内容。
    // 不处理的话，一部 61 集的剧会被当成 65 集（实测就是这个数）。
    let ignored = drop_duplicate_episodes(&mut episodes);
    if !ignored.is_empty() {
        let shown: Vec<String> = ignored.iter().take(5).cloned().collect();
        let more = if ignored.len() > shown.len() {
            format!(" 等 {} 个", ignored.len())
        } else {
            String::new()
        };
        warnings.push(format!(
            "忽略了 {} 个重复副本：{}{more}。同一集只保留一个。",
            ignored.len(),
            shown.join("、")
        ));
    }

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
            None,
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

        // 产出查询：链路是 pipeline_drama.src_dir ↔ project.root_dir ↔ media
        // —— 这个 join 不直观，所以用测试锁住它
        let outputs = crate::db::run_outputs(&conn, outcome.run_id).unwrap();
        assert_eq!(outputs.len(), 3, "三集应当各有一行");
        assert!(
            outputs.iter().all(|e| e.name.ends_with(".mp4")),
            "每行应当是一集：{:?}",
            outputs.iter().map(|e| &e.name).collect::<Vec<_>>()
        );
        // 这时候还没切片，所以每集的 slices 是空的，但行必须在
        assert!(outputs.iter().all(|e| e.slices.is_empty()));
    }

    #[test]
    fn each_episode_writes_into_its_own_folder() {
        // 这是真实素材上踩到的坑：16 集共用一个「切片」目录，
        // 而切片是按「这一集的第几片」编号的（001、002…），
        // 于是后面的集把前面的全覆盖了 —— 27 片只剩最后 3 片，
        // 数据库里还显示"有"（路径确实存在，内容却是别人的）。
        let out = Path::new("D:/二创/某剧");
        let a = stage_payload("slice", 1, out, 40_000, "small", "某剧-第01集", None)
            .unwrap()
            .unwrap();
        let b = stage_payload("slice", 2, out, 40_000, "small", "某剧-第02集", None)
            .unwrap()
            .unwrap();
        let dir_a: serde_json::Value = serde_json::from_str(&a).unwrap();
        let dir_b: serde_json::Value = serde_json::from_str(&b).unwrap();
        assert_ne!(
            dir_a["outDir"], dir_b["outDir"],
            "两集的输出目录必须不同，否则会互相覆盖"
        );
        assert!(
            dir_a["outDir"].as_str().unwrap().contains("第01集"),
            "{}",
            dir_a["outDir"]
        );
        assert!(
            dir_b["outDir"].as_str().unwrap().contains("第02集"),
            "{}",
            dir_b["outDir"]
        );
    }

    #[test]
    fn all_stages_of_one_episode_stay_under_its_own_folder() {
        // 切片 / 导出 / 发布包 都要落在这一集自己的目录下
        let out = Path::new("D:/二创/某剧");
        let group = "某剧-第01集";
        for stage in ["slice", "export", "package"] {
            let raw = stage_payload(stage, 1, out, 40_000, "small", group, None)
                .unwrap()
                .unwrap();
            let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
            let key = if stage == "slice" { "outDir" } else if stage == "export" { "outPath" } else { "outRoot" };
            let path = v[key].as_str().unwrap();
            assert!(
                path.contains(group),
                "{stage} 的输出没有落在这一集自己的目录下：{path}"
            );
        }
    }

    #[test]
    fn file_names_are_made_safe_for_directories() {
        assert_eq!(safe_dir_name("某剧-第01集.mp4"), "某剧-第01集");
        // `.mp4`：Rust 把它当「隐藏文件、没有扩展名」，file_stem 原样返回。
        // 去掉前导点后剩 `mp4`，是个能用的目录名 —— 不该退化成「未命名」。
        assert_eq!(safe_dir_name(".mp4"), "mp4");
        // 全是点才真的没名字可用：`...mp4` 的 file_stem 是 `..`
        assert_eq!(safe_dir_name("...mp4"), "未命名");
        // 结尾的点在 Windows 上会被丢掉
        assert_eq!(safe_dir_name("name...mp4"), "name");
        // 非法字符换成下划线
        assert_eq!(safe_dir_name(r#"a"b|c?.mp4"#), "a_b_c_");
        // 注意入参**永远是裸文件名**（来自 SourceEpisode::file_name），
        // 不会是路径。所以 Path::file_stem 把 / 当分隔符这件事在实际调用里
        // 不会发生 —— 这里把这个事实写下来，免得以后有人误传路径进来。
        assert_eq!(safe_dir_name("a/b.mp4"), "b");
    }

    #[test]
    fn running_twice_does_not_duplicate_projects_or_media() {
        // 实测踩到的：每次跑流水线都新建工程、重新导入素材，
        // 结果三个工程指向同一个源目录，163 条素材里只有 65 个不同文件，
        // 产出页面因此错乱（同一集出现两三次，有的有字幕有的没有）。
        let (Some(ffmpeg), Some(ffprobe)) = (
            env_path("DRAMADIST_TEST_FFMPEG"),
            env_path("DRAMADIST_TEST_FFPROBE"),
        ) else {
            eprintln!("跳过：需要 DRAMADIST_TEST_FFMPEG 与 DRAMADIST_TEST_FFPROBE");
            return;
        };

        let t = TempTree::new("rerun");
        make_video(&ffmpeg, &t.path().join("剧/第01集.mp4"), 2);
        make_video(&ffmpeg, &t.path().join("剧/第02集.mp4"), 2);

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrate_for_tests(&conn).unwrap();
        let out = t.path().join("二创");
        let stages = vec!["slice".to_string()];

        let first = start_pipeline(&conn, &ffprobe, t.path(), &out, &stages, 10_000, "", None)
            .expect("第一次跑失败");
        assert_eq!(first.imported, 2);
        assert_eq!(first.reused_projects, 0);
        assert_eq!(first.reused_media, 0);

        let second = start_pipeline(&conn, &ffprobe, t.path(), &out, &stages, 10_000, "", None)
            .expect("第二次跑失败");

        // 第二次一条都不该新建
        assert_eq!(second.imported, 0, "第二次不该再导入素材");
        assert_eq!(second.reused_projects, 1, "应当复用已有工程");
        assert_eq!(second.reused_media, 2, "两集都该复用已有素材");
        assert_eq!(second.queued, 2, "任务还是要照排 —— 复用素材不等于跳过这一步");

        // 库里的实际行数才是最终判据
        assert_eq!(crate::db::list_projects(&conn).unwrap().len(), 1, "只能有一个工程");
        let media = crate::db::list_media(&conn, 1).unwrap();
        assert_eq!(media.len(), 2, "只能有两条素材，不能重复导入");
    }

    #[test]
    fn dropped_copies_never_reach_the_pipeline() {
        let (Some(ffmpeg), Some(ffprobe)) = (
            env_path("DRAMADIST_TEST_FFMPEG"),
            env_path("DRAMADIST_TEST_FFPROBE"),
        ) else {
            eprintln!("跳过：需要 ffmpeg / ffprobe");
            return;
        };
        let t = TempTree::new("dup_pipeline");
        make_video(&ffmpeg, &t.path().join("剧").join("剧-第01集.mp4"), 2);
        // 同名副本：内容一样，不该被当成第二集
        std::fs::copy(
            t.path().join("剧").join("剧-第01集.mp4"),
            t.path().join("剧").join("剧-第01集(1).mp4"),
        )
        .unwrap();

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
            None,
        )
        .expect("起流水线失败");

        assert_eq!(outcome.imported, 1, "副本不该被导入");
        assert_eq!(outcome.queued, 1);
    }

    #[test]
    fn outputs_never_leak_across_runs() {
        // 实测踩到的：靠 src_dir 关联工程时，一次查询把几次运行的数据全捞出来
        // （163 行里只有 65 个不同文件，界面同一集出现两三次）。
        // v9 起按 project_id 精确查，这条测试把它锁住。
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrate_for_tests(&conn).unwrap();
        let src = "D:/某剧";

        // 造出"同一个源目录被跑过两次、各建了一个工程"的现场
        let run_a = crate::db::create_pipeline_run(&conn, src, "D:/out", &["slice".into()]).unwrap();
        let proj_a = crate::db::create_project(&conn, "某剧", src).unwrap();
        let drama_a = crate::db::add_pipeline_drama(
            &conn, run_a, "某剧", src, "D:/out/某剧", 2, Some(proj_a.id),
        )
        .unwrap();

        let run_b = crate::db::create_pipeline_run(&conn, src, "D:/out", &["slice".into()]).unwrap();
        let proj_b = crate::db::create_project(&conn, "某剧", src).unwrap();
        let _drama_b = crate::db::add_pipeline_drama(
            &conn, run_b, "某剧", src, "D:/out/某剧", 3, Some(proj_b.id),
        )
        .unwrap();

        let probe = crate::models::MediaProbe {
            duration_ms: 1000,
            width: 720,
            height: 1280,
            has_audio: true,
            size_bytes: 10,
            format_name: "mp4".into(),
        };
        for n in 1..=2 {
            crate::db::add_media(
                &conn,
                proj_a.id,
                &format!("{src}/第0{n}集.mp4"),
                &format!("第0{n}集.mp4"),
                &probe,
            )
            .unwrap();
        }
        for n in 1..=3 {
            crate::db::add_media(
                &conn,
                proj_b.id,
                &format!("{src}/第0{n}集.mp4"),
                &format!("第0{n}集.mp4"),
                &probe,
            )
            .unwrap();
        }

        // A 这次只该看见自己的 2 集，绝不能把 B 的 3 集也算进来
        let a = crate::db::run_outputs(&conn, run_a).unwrap();
        assert_eq!(a.len(), 2, "run A 应当只有 2 集，实际 {}", a.len());
        let b = crate::db::run_outputs(&conn, run_b).unwrap();
        assert_eq!(b.len(), 3, "run B 应当只有 3 集，实际 {}", b.len());

        // drama_id 用一下，免得被当成未使用变量
        assert!(drama_a > 0);
    }

    #[test]
    fn rerunning_is_not_mistaken_for_importing_nothing() {
        // 加「复用已有素材」之后踩过的坑：第二次跑同一批素材，
        // imported 全是 0（都复用了），只看 imported 会把正常情况当成失败，
        // 用户看到「一部剧的素材都没能导入」。
        let fresh = StartOutcome {
            run_id: 1,
            drama_count: 1,
            imported: 3,
            queued: 3,
            skipped: Vec::new(),
            reused_projects: 0,
            reused_media: 0,
        };
        assert!(!fresh.imported_nothing());

        let reused = StartOutcome {
            imported: 0,
            reused_projects: 1,
            reused_media: 3,
            ..fresh.clone()
        };
        assert!(
            !reused.imported_nothing(),
            "全部复用不算失败 —— 这正是那个 bug"
        );

        let nothing = StartOutcome {
            imported: 0,
            reused_media: 0,
            ..fresh
        };
        assert!(nothing.imported_nothing(), "真的没有才算失败");
    }

    #[test]
    fn every_stage_payload_actually_parses() {
        // 这条测试是为了防住一类坑：payload 的**字段名或枚举变体写错**，
        // 编译期查不出来，入队也不报，要跑到那个任务才炸。
        // 真实素材那轮：切片方式写成 fixedDuration（枚举里是 everySeconds），
        // 32 个切片任务全废，还连累了后面的画面处理。
        let drama = SourceDrama {
            name: "某剧".into(),
            dir: "D:/某剧".into(),
            episode_count: 2,
            episodes: Vec::new(),
            synopsis: "一段简介".into(),
            synopsis_path: "D:/某剧/intro.txt".into(),
            cover: "D:/某剧/poster.jpg".into(),
            warnings: Vec::new(),
        };
        let mut checked = 0;
        for (stage, _, _, _) in STAGES {
            let kind = stage_to_job_kind(stage);
            if is_drama_level(stage) {
                // 整部剧一步：参数里带齐简介和封面，否则会被跳过（那也算正常）
                let Some(payload) = drama_payload(stage, &drama, Path::new("E:/out"))
                    .unwrap_or_else(|e| panic!("{stage} 生成参数失败：{e}"))
                else {
                    continue;
                };
                crate::queue::validate_payload(kind, &payload).unwrap_or_else(|e| {
                    panic!("步骤 {stage}（队列类型 {kind}）的参数过不了校验：{e}")
                });
                checked += 1;
                continue;
            }
            let payload = stage_payload(stage, 7, Path::new("E:/out"), 40_000, "small", "第01集", None)
                .unwrap_or_else(|e| panic!("{stage} 生成参数失败：{e}"));
            let Some(payload) = payload else {
                continue; // 这一步没配就不排任务，正常
            };
            crate::queue::validate_payload(kind, &payload)
                .unwrap_or_else(|e| panic!("步骤 {stage}（队列类型 {kind}）的参数过不了校验：{e}"));
            checked += 1;
        }
        assert!(
            checked >= 6,
            "步骤表里应当有至少 6 步能生成参数，实际 {checked}"
        );
    }

    #[test]
    fn drama_level_stages_are_queued_once_per_drama_not_per_episode() {
        // 改剧名、生成封面都是整部剧一步。按集排的话，60 集会调 120 次模型。
        assert!(is_drama_level("rewrite"));
        assert!(is_drama_level("cover"));
        assert!(is_drama_level("proof"));
        assert!(!is_drama_level("slice"));
        assert!(!is_drama_level("subtitle"));
        assert!(!is_drama_level("dedup"));
    }

    #[test]
    fn drama_level_stages_skip_cleanly_when_material_is_missing() {
        // 缺简介就跳过改剧名，缺封面就跳过封面 —— 但不能排一个注定失败的任务
        let mut drama = SourceDrama {
            name: "某剧".into(),
            dir: "D:/某剧".into(),
            episode_count: 1,
            episodes: Vec::new(),
            synopsis: String::new(),
            synopsis_path: String::new(),
            cover: String::new(),
            warnings: Vec::new(),
        };
        let out = Path::new("E:/out");
        assert!(drama_payload("rewrite", &drama, out).unwrap().is_none(), "没简介该跳过");
        assert!(drama_payload("cover", &drama, out).unwrap().is_none(), "没封面该跳过");

        drama.synopsis = "简介".into();
        drama.cover = "D:/某剧/poster.jpg".into();
        assert!(drama_payload("rewrite", &drama, out).unwrap().is_some());
        assert!(drama_payload("cover", &drama, out).unwrap().is_some());

        // 封面要带上兜底剧名：rewrite 没跑或没写出 剧名.txt 时还能用
        let raw = drama_payload("cover", &drama, out).unwrap().unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["fallbackTitle"], "某剧");
        assert_eq!(v["poster"], "D:/某剧/poster.jpg");
    }

    #[test]
    fn wrong_slice_mode_is_rejected_at_enqueue_time() {
        // 反例：把切片方式写成不存在的变体，必须当场被拦下
        let bad = serde_json::json!({
            "mediaId": 1,
            "mode": { "mode": "fixedDuration", "seconds": 40.0 },
            "outDir": "E:/out",
        })
        .to_string();
        let err = crate::queue::validate_payload("slice", &bad)
            .unwrap_err()
            .to_string();
        assert!(err.contains("参数不对"), "{err}");
        assert!(err.contains("everySeconds"), "错误里要提示正确写法：{err}");
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
            None,
        )
        .expect("一部坏的素材不该让整条流水线起不来");

        assert_eq!(outcome.imported, 1);
        assert_eq!(outcome.queued, 1);
        assert_eq!(outcome.skipped.len(), 1, "{:?}", outcome.skipped);
        assert!(outcome.skipped[0].contains("第02集"), "{:?}", outcome.skipped);
    }

    #[test]
    fn duplicate_download_copies_are_merged_into_one_episode() {
        // 真实素材上撞到的：同一集下过两次，系统加了 (1) 后缀，
        // 结果一部 61 集的剧被当成 65 集。
        let t = TempTree::new("dup_episodes");
        const DRAMA: &str = "某剧";
        for n in 1..=3 {
            t.file(&format!("{DRAMA}/某剧-第{n:02}集.mp4"), b"x");
        }
        // 第 1~2 集各多一个 (1) 副本
        t.file(&format!("{DRAMA}/某剧-第01集(1).mp4"), b"x");
        t.file(&format!("{DRAMA}/某剧-第02集(1).mp4"), b"x");
        // 第 3 集多一个"副本"
        t.file(&format!("{DRAMA}/某剧-第03集 - 副本.mp4"), b"x");

        let scan = scan_source(t.path()).unwrap();
        let d = &scan.dramas[0];
        assert_eq!(d.episode_count, 3, "三集就该是三集，副本不算");
        let names: Vec<&str> = d.episodes.iter().map(|e| e.file_name.as_str()).collect();
        assert!(names.contains(&"某剧-第01集.mp4"), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("(1)")), "{names:?}");
        assert!(!names.iter().any(|n| n.contains("副本")), "{names:?}");
        // 要如实告诉用户忽略了什么，不能悄悄咽掉
        assert!(
            d.warnings.iter().any(|w| w.contains("重复副本")),
            "{:?}",
            d.warnings
        );
    }

    #[test]
    fn the_normally_named_file_wins_even_if_the_copy_is_bigger() {
        let t = TempTree::new("dup_bigger");
        t.file("某剧/某剧-第01集.mp4", b"small");
        t.file("某剧/某剧-第01集(1).mp4", b"this one is much bigger");
        let scan = scan_source(t.path()).unwrap();
        let d = &scan.dramas[0];
        assert_eq!(d.episode_count, 1);
        assert_eq!(d.episodes[0].file_name, "某剧-第01集.mp4");
    }

    #[test]
    fn underscored_suffixes_are_not_treated_as_copies() {
        // `_1`、`-1` 可能是合法的分卷命名，误判会把正常集数吃掉
        assert!(!looks_like_duplicate("某剧-第01集_1.mp4"));
        assert!(!looks_like_duplicate("某剧-第01集-1.mp4"));
        assert!(!looks_like_duplicate("某剧-第01集.mp4"));
        // 但这些是
        assert!(looks_like_duplicate("某剧-第01集(1).mp4"));
        assert!(looks_like_duplicate("某剧-第01集 (12).mp4"));
        assert!(looks_like_duplicate("某剧-第01集 - 副本.mp4"));
        assert!(looks_like_duplicate("某剧-第01集 copy.mp4"));
        assert!(looks_like_duplicate("某剧-第01集(2).MP4"));
        // 括号里不是纯数字的不算
        assert!(!looks_like_duplicate("某剧-第01集(高清).mp4"));
    }

    #[test]
    fn unnumbered_files_are_never_dropped() {
        // 认不出集号的一律保留 —— 宁多勿少，让用户看警告自己决定
        let t = TempTree::new("dup_unnumbered");
        t.file("某剧/甲(1).mp4", b"a");
        t.file("某剧/甲.mp4", b"b");
        let scan = scan_source(t.path()).unwrap();
        assert_eq!(scan.dramas[0].episode_count, 2);
    }

    #[test]
    fn real_world_material_pack_is_scanned_correctly() {
        // 照着一份真实的短剧素材包写的（百度网盘下的）：
        //   intro.txt        ← 英文名，不是「简介.txt」
        //   poster.jpg       ← 英文名，不是「封面.jpg」
        //   <剧名>-第01集.mp4 … 第10集.mp4
        //   <剧名>-第11集.mp4.baiduyun.p.downloading  ← 没下完的临时文件
        let t = TempTree::new("realpack");
        const DRAMA: &str = "弟弟结婚，我的新车竟成了贺礼";
        for n in 1..=10 {
            t.file(
                &format!("{DRAMA}/{DRAMA}-第{n:02}集.mp4"),
                b"x",
            );
        }
        // 百度网盘没下完的临时文件：扩展名是 .downloading，不能被当成视频
        t.file(
            &format!("{DRAMA}/{DRAMA}-第11集.mp4.baiduyun.p.downloading"),
            b"partial",
        );
        t.file(&format!("{DRAMA}/intro.txt"), "简介正文".as_bytes());
        t.file(&format!("{DRAMA}/poster.jpg"), b"img");

        // 以素材包所在的那一层为根目录（里面只有一个剧的子目录）
        let root = t.dir("pack");
        // 把剧目录挪进 pack 下
        let from = t.path().join(DRAMA);
        let to = root.join(DRAMA);
        std::fs::rename(&from, &to).unwrap();

        let scan = scan_source(&root).unwrap();
        assert_eq!(scan.dramas.len(), 1);

        let d = &scan.dramas[0];
        assert_eq!(d.name, DRAMA, "剧名应当取目录名");
        assert_eq!(
            d.episode_count, 10,
            "没下完的 .baiduyun.p.downloading 不能被算成一集"
        );
        assert_eq!(d.synopsis, "简介正文", "intro.txt 要认出来（英文名）");
        assert!(d.cover.ends_with("poster.jpg"), "{}", d.cover);
        assert!(d.warnings.is_empty(), "{:?}", d.warnings);

        // 集号要连续排好
        let nums: Vec<i64> = d.episodes.iter().filter_map(|e| e.episode).collect();
        assert_eq!(nums, (1..=10).collect::<Vec<i64>>());
    }

    #[test]
    fn downloading_temp_files_are_not_treated_as_video() {
        // 素材包里常混着网盘的下载中文件，扩展名千奇百怪
        for name in [
            "第01集.mp4.baiduyun.p.downloading",
            "第02集.mp4.crdownload",
            "第03集.mp4.part",
            "第04集.mp4.!ut",
        ] {
            let t = TempTree::new("dl");
            t.file(&format!("剧/{name}"), b"partial");
            // 只有这一个文件时应当报"没找到视频"，而不是把它当成一集
            let err = scan_source(t.path()).unwrap_err().to_string();
            assert!(err.contains("没找到视频文件"), "{name} 被误当成了视频");
        }
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
    fn label_reaches_the_dedup_payload() {
        // 合规开关必须真的进到任务参数里。
        // 之前踩过一次同类坑：payload 字段名写错，编译期查不出、入队也不报，
        // 要跑到那个任务才炸。
        let label = HeaderLabel::default();
        let raw = stage_payload(
            "dedup",
            7,
            Path::new("E:/out"),
            40_000,
            "small",
            "第01集",
            Some(&label),
        )
        .unwrap()
        .expect("dedup 应当产出参数");
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["headerLabel"]["text"], "AI创作");
        assert_eq!(v["headerLabel"]["seconds"], 3.0);
        assert_eq!(v["headerLabel"]["position"], "topLeft");
        // 参数本身要能过队列的校验
        crate::queue::validate_payload("transform", &raw).expect("参数过不了队列校验");

        // 没配就是 null，不能凭空加标识
        let raw2 = stage_payload("dedup", 7, Path::new("E:/out"), 40_000, "small", "第01集", None)
            .unwrap()
            .unwrap();
        let v2: serde_json::Value = serde_json::from_str(&raw2).unwrap();
        assert!(v2["headerLabel"].is_null());
    }

    #[test]
    fn proof_stage_is_skipped_rather_than_queued_to_fail() {        // 证明图要有模板和字段坐标才谈得上生成，没配就跳过 ——
        // 排一个注定失败的任务只会让流水线卡在最后一步
        let got = stage_payload("proof", 1, Path::new("E:/o"), 10_000, "", "第01集", None).unwrap();
        assert!(got.is_none());
    }

    #[test]
    fn unknown_stage_is_rejected() {
        let err = stage_payload("nonsense", 1, Path::new("E:/o"), 10_000, "", "第01集", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("不认识的步骤"), "{err}");
    }
}
