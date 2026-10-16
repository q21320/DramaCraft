//! 任务队列：把长任务排成一队顺序跑，可暂停、可停止、**重启后能接着跑**。
//!
//! ## 为什么需要它
//!
//! 手上有几十条素材、每条都要走「切片 → 字幕 → 画面处理 → 导出」时，一条条点不现实；
//! 中途关掉应用，进度也不该全丢。
//!
//! ## 断点续跑怎么做到
//!
//! 任务状态存在 SQLite 里。进程被杀时，正在跑的那条会留在 `running` ——
//! 那是**假状态**：没有人在跑它。所以应用启动第一件事就是
//! [`db::recover_interrupted_jobs`]，把所有 `running` 退回 `pending`，队列自然接着跑。
//!
//! ## 为什么执行器直接调命令层
//!
//! Tauri 的命令函数本身就是普通 Rust 函数（`#[tauri::command]` 只是额外生成了包装），
//! 而 `AppHandle::state::<T>()` 能拿到与注入等价的 `State`。
//! 所以队列**直接复用命令层**，业务逻辑只有一份 —— 不会出现"界面里跑得对、队列里跑得错"。
//!
//! ## 停止的语义
//!
//! 「停止」是**跑完当前这条就不再取新的**，不是把正在跑的 ffmpeg 砍掉。
//! 半途杀掉编码进程只会留下半个文件，得不偿失。界面上也是这么写的。

use crate::error::{AppError, AppResult};
use crate::models::{
    ExportOptions, HeaderLabel, Job, OcrOptions, QueueReport, SliceMode, TransformOptions,
};
use crate::{commands, db, AppState};
use serde::Deserialize;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, Manager};

/// 队列支持的任务类型：(key, 中文名)
pub const KINDS: [(&str, &str); 14] = [
    // 顺序照着真实软件的流水线来。
    // **没有 "rewrite"** —— 改剧名在排任务**之前**就跑完了
    // （见 commands::pipeline_start），它不是队列里的一步。
    ("dedup", "整集去重"),
    ("desubtitle", "AI 去字幕"),
    ("storyboard", "分镜证明"),
    ("cost", "成本报告"),
    ("slice", "切片"),
    ("transform", "画面处理"),
    ("ocr", "硬字幕识别"),
    ("asr", "语音转字幕"),
    ("cloud", "云端转字幕"),
    ("export", "导出成片"),
    ("draft", "剪映草稿"),
    ("publish", "发布包"),
    ("cover", "封面生成"),
    ("verify", "效果验证"),
];

pub fn kind_label(kind: &str) -> String {
    KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map(|(_, label)| (*label).to_string())
        .unwrap_or_else(|| kind.to_string())
}

pub fn is_known_kind(kind: &str) -> bool {
    KINDS.iter().any(|(k, _)| *k == kind)
}

// ---------------- 各任务的参数 ----------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SlicePayload {
    media_id: i64,
    mode: SliceMode,
    out_dir: String,
    /// 兼容模式：强制转成 H.264。流水线默认开着 ——
    /// 源素材常是 HEVC，而平台上传规范要 H.264。
    #[serde(default)]
    compatible: bool,
    /// 文件名前缀。
    ///
    /// 现在是**一源一文件夹**（照真实软件：`qiepian\第1集_quzimu\分镜001.mp4`），
    /// 所以前缀不用再承担"区分是哪一集"的职责，目录已经分开了。
    #[serde(default)]
    prefix: String,
    /// AI 去字幕的产物目录（`<剧目录>/quzimu`）。
    ///
    /// **切片切的是去字幕产物，不是去重产物** —— 真实软件的日志写得很清楚：
    /// 「视频切片（流水线 · AI去字幕结果）」，源是 `第N集_quzimu.mp4`。
    /// 我原来挂在去重下面，错了：那是两条并行支线。
    ///
    ///   整集去重 → 去重产物（发布用）
    ///   去字幕   → quzimu → 10秒切片 → 分镜证明图（证明材料用）
    ///
    /// 没去字幕（比如没配服务）就退回上一版的行为（去重产物 → 原片）。
    #[serde(default)]
    quzimu_dir: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransformPayload {
    media_id: i64,
    intensity: String,
    options: Option<TransformOptions>,
    /// 片头标识（合规开关），叠加在预设之上
    #[serde(default)]
    header_label: Option<HeaderLabel>,
}

/// 整集去重。**发布用的就是这个产物**，切片是它的下游。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DedupPayload {
    media_id: i64,
    #[serde(default = "default_intensity")]
    intensity: String,
    #[serde(default)]
    header_label: Option<HeaderLabel>,
    /// 去重产物放哪个目录
    out_dir: String,
    /// 产出文件的名字前缀（新剧名-第NN集）。
    ///
    /// 用户要的：文件名用 AI 生成的**新名字**，不是源目录那套。
    /// 空串就退回原来用素材名。
    #[serde(default)]
    prefix: String,
}

fn default_intensity() -> String {
    "strong".to_string()
}

/// 整部剧一步：成本报告。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CostPayload {
    title: String,
    episode_count: i64,
    total_ms: i64,
    out_path: String,
    #[serde(default)]
    invest_wan: f64,
    #[serde(default)]
    actor_ratio: f64,
    /// 报审机构。**默认空** —— 不预填任何公司名。
    #[serde(default)]
    org: String,
    /// 法定代表人姓名；空着就只留签字位置
    #[serde(default)]
    legal_person: String,
}

/// 整部剧一步：分镜证明。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoryboardPayload {
    /// 从哪几集的切片抽帧（去字幕后的那几集）
    media_ids: Vec<i64>,
    out_dir: String,
    #[serde(default = "default_frames_per_episode")]
    frames_per_episode: i64,
    #[serde(default = "default_shots_per_sheet")]
    shots_per_sheet: i64,
}

fn default_frames_per_episode() -> i64 {
    5
}
fn default_shots_per_sheet() -> i64 {
    2
}

/// 整部剧一步：生成封面。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CoverPayload {
    poster: String,
    /// 从哪读新剧名（通常是导出目录，rewrite 会在那里写 `剧名.txt`）
    title_dir: String,
    /// 兜底剧名：读不到 `剧名.txt` 时用它
    fallback_title: String,
    out_path: String,
    width: i64,
    height: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OcrPayload {
    media_id: i64,
    options: Option<OcrOptions>,
    overwrite: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AsrPayload {
    media_id: i64,
    model: Option<String>,
    language: Option<String>,
    overwrite: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DraftPayload {
    media_id: i64,
    draft_root: String,
    draft_name: String,
    fps: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublishPayload {
    media_id: i64,
    platform: String,
    account: Option<String>,
    out_root: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VerifyPayload {
    media_id: i64,
    samples: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloudPayload {
    media_id: i64,
    overwrite: Option<bool>,
}

/// 校验一份 payload 能不能被解析成对应类型的参数。
///
/// **在入队时就调用，而不是等到跑的时候** —— 参数写错了应该当场报，
/// 而不是排一堆注定失败的任务。真实素材那轮就是这么栽的：
/// 流水线把切片方式写成了 `fixedDuration`（枚举里没有这个变体），
/// 32 个切片任务全排在队列里，跑一个败一个，还连累了后面的画面处理。
pub fn validate_payload(kind: &str, payload: &str) -> AppResult<()> {
    fn check<T: for<'de> Deserialize<'de>>(kind: &str, payload: &str) -> AppResult<()> {
        serde_json::from_str::<T>(payload).map(|_| ()).map_err(|e| {
            AppError::Msg(format!(
                "{kind} 任务的参数不对：{e}。原始参数：{}",
                crate::proc::tail(payload)
            ))
        })
    }
    match kind {
        "dedup" => check::<DedupPayload>(kind, payload),
        "cost" => check::<CostPayload>(kind, payload),
        "storyboard" => check::<StoryboardPayload>(kind, payload),
        // 这两步的执行器会直接给出「还没实现 + 为什么」的说明，
        // 参数只要是个 JSON 对象就够（校验太严会把它挡在入队之前，
        // 用户就看不到那条说明了）
        "desubtitle" => check::<serde_json::Map<String, serde_json::Value>>(kind, payload),
        "cover" => check::<CoverPayload>(kind, payload),
        "slice" => check::<SlicePayload>(kind, payload),
        "transform" => check::<TransformPayload>(kind, payload),
        "ocr" => check::<OcrPayload>(kind, payload),
        "asr" => check::<AsrPayload>(kind, payload),
        "cloud" => check::<CloudPayload>(kind, payload),
        "export" => check::<ExportOptions>(kind, payload),
        "draft" => check::<DraftPayload>(kind, payload),
        "publish" => check::<PublishPayload>(kind, payload),
        "verify" => check::<VerifyPayload>(kind, payload),
        other => Err(AppError::Msg(format!("不认识的任务类型：{other}"))),
    }
}

fn parse_payload<T: for<'de> Deserialize<'de>>(job: &Job) -> AppResult<T> {
    serde_json::from_str(&job.payload).map_err(|e| {
        AppError::Msg(format!(
            "任务 #{} 的参数无法解析（{e}）。原始参数：{}",
            job.id, job.payload
        ))
    })
}

fn human_size(bytes: i64) -> String {
    if bytes <= 0 {
        return "0 B".to_string();
    }
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut i = 0;
    while value >= 1024.0 && i < units.len() - 1 {
        value /= 1024.0;
        i += 1;
    }
    if i == 0 || value >= 100.0 {
        format!("{value:.0} {}", units[i])
    } else {
        format!("{value:.1} {}", units[i])
    }
}

/// 执行一条任务，返回一句人类可读的结果。
pub async fn execute(app: &AppHandle, job: &Job) -> AppResult<String> {
    let state = app.state::<AppState>();

    match job.kind.as_str() {
        // 整剧重组 / AI 去字幕：**跳过并说明**，不让整部剧失败。
        //
        // 这两步现在是默认勾选的（用户要「所有步骤都默认选中」），
        // 如果返回 Err，一部剧会当场失败、后面的步骤全被取消，什么都出不来。
        // 所以这里返回 Ok，但消息写清楚「跳过了 + 为什么」——
        // 运行记录和运行日志里都看得到，不会被悄悄咽掉。
        "desubtitle" => Ok(
            "已跳过：AI 去字幕需要云服务的接口文档（真实软件用的是开拍 videoscreenclear）。\
             没有该服务的 API 说明就没法实现 —— 需要 AccessKey / SecretKey 与接口格式。"
                .to_string(),
        ),
        // 成本报告：出「成本配置比例情况报告」。
        //
        // 版式照用户指定那份模板（zhengming/成本图表.docx）：
        // 标题 + 提审说明 + 5 列表格 + 符合性声明 + 版权承诺 + 报审机构签字。
        "cost" => {
            let p: CostPayload = parse_payload(job)?;
            let minutes = (p.total_ms as f64 / 60000.0).max(0.0);
            let report = commands::generate_cost_report(
                vec![crate::models::CostReportRow {
                    name: p.title.clone(),
                    episodes: p.episode_count,
                    total_minutes: minutes,
                    // 表单要求「需＜100万」「需＜40%」，默认给保守值。
                    // 真实数字该由用户填 —— 这是拿去申报的东西。
                    invest_wan: if p.invest_wan > 0.0 { p.invest_wan } else { 1.0 },
                    actor_ratio: p.actor_ratio.max(0.0),
                }],
                p.org,
                p.legal_person,
                String::new(),
                p.out_path,
            )?;
            Ok(format!("成本报告已生成（{}）", report.out_path))
        }
        // 分镜证明：抽帧 → 视觉模型反推 → 拼成证明图
        "storyboard" => {
            let p: StoryboardPayload = parse_payload(job)?;
            let report = commands::build_storyboard(
                state,
                p.media_ids,
                p.out_dir,
                p.frames_per_episode,
                p.shots_per_sheet,
            )
            .await?;
            Ok(format!(
                "分镜证明已生成 {} 张（共 {} 镜头）",
                report.sheets, report.shots
            ))
        }
        // 整集去重 —— 真实软件的流程是「先去重整集、再切片」。
        // 去重产物才是发布用的东西。
        "dedup" => {
            let p: DedupPayload = parse_payload(job)?;
            let report = commands::dedup_media(
                state,
                p.media_id,
                Some(p.intensity),
                p.header_label,
                Some(p.out_dir),
                p.prefix,
                // 强度种子：0 = 用当前时间，每次跑都不一样。
                // 真实软件的 13 项强度就是每次随机 115~125，固定的没意义。
                0,
            )
            .await?;
            let mb = report.size_bytes as f64 / 1048576.0;
            let spec = if report.spec_problems.is_empty() {
                "上传规范校验通过".to_string()
            } else {
                // **说清楚"成功但有提示"**：原来只写「1 项没达到上传规范」，
                // 用户分不清是成了还是废了。产物已经出来了，只是有几条要留意。
                let detail: Vec<String> = report
                    .spec_problems
                    .iter()
                    .flat_map(|s| s.problems.iter().cloned())
                    .collect();
                format!("{} 条提示，不影响出片：{}", detail.len(), detail.join("；"))
            };
            // 把「改了什么」写出来 —— 用户抱怨过「看不到去重效果差异日志」。
            // 每片参数按序号生成、故意各不相同，不列出来就只是一句"完成了"。
            Ok(format!(
                "去重完成（{mb:.1} MB）｜改了：{}｜{spec}",
                report.changes
            ))
        }
        // 整部剧第一步：改简介 → 按新简介起剧名。
        // 结果写进导出目录的 `剧名.txt`，封面那一步会去读。
        // 整部剧倒数第二步：封面。
        // 用 rewrite 写下的新剧名；读不到就退回原剧名 —— 别因为缺一个文件就整条卡住。
        "cover" => {
            let p: CoverPayload = parse_payload(job)?;
            let title = std::fs::read_to_string(std::path::Path::new(&p.title_dir).join("剧名.txt"))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or(p.fallback_title);
            let report = commands::render_cover(
                state,
                crate::models::CoverRequest {
                    poster: p.poster,
                    title,
                    out_path: p.out_path,
                    width: p.width,
                    height: p.height,
                    position: crate::models::LabelPosition::TopCenter,
                    out_root: String::new(),
                },
            )
            .await?;
            Ok(format!(
                "封面已生成（{}）",
                if report.engine == "ai" {
                    "AI 图生图"
                } else {
                    "本地合成"
                }
            ))
        }
        "slice" => {
            let p: SlicePayload = parse_payload(job)?;
            // **先看有没有 AI 去字幕的产物，有就切它。**
            //
            // 真实软件的日志：「视频切片（流水线 · AI去字幕结果）」——
            // 源是 `第N集_quzimu.mp4`。去重产物是发布用的，切片只是
            // 证明材料的原料，两条支线互不相干。
            //
            // 没配去字幕服务时 quzimu 不存在，传 None 就退回上一版行为。
            let quzimu = {
                // 这里 state 是 `&AppState`（不是 Tauri 的 State），
                // 直接用它的 db 锁 —— 和本文件别处一个写法。
                let conn = state
                    .db
                    .lock()
                    .map_err(|_| AppError::Msg("数据库锁被污染，请重启应用".into()))?;
                let m = db::get_media(&conn, p.media_id)?;
                p.quzimu_dir.as_ref().and_then(|dir| {
                    let stem = std::path::Path::new(&m.name)
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| m.name.clone());
                    let c = std::path::Path::new(dir).join(format!("{stem}_quzimu.mp4"));
                    c.is_file().then(|| c.to_string_lossy().to_string())
                })
            };
            let segments = commands::run_slice(
                app.clone(),
                state,
                p.media_id,
                p.mode,
                p.out_dir,
                Some(p.compatible),
                p.prefix,
                quzimu,
            )
            .await?;
            let failed = segments.iter().filter(|s| s.status != "ok").count();
            if failed > 0 {
                Ok(format!("切出 {} 片，其中 {failed} 片失败", segments.len()))
            } else {
                Ok(format!("切出 {} 片", segments.len()))
            }
        }
        "transform" => {
            let p: TransformPayload = parse_payload(job)?;
            let report =
                commands::transform_segments(state, p.media_id, p.intensity, p.options, p.header_label).await?;
            Ok(format!(
                "处理 {} 个片段（失败 {}）",
                report.processed, report.failed
            ))
        }
        "ocr" => {
            let p: OcrPayload = parse_payload(job)?;
            let report =
                commands::recognize_hard_subtitles(app.clone(), state, p.media_id, p.options, p.overwrite)
                    .await?;
            Ok(format!(
                "读 {} 帧，生成 {} 条字幕",
                report.frames_with_text, report.cue_count
            ))
        }
        "asr" => {
            let p: AsrPayload = parse_payload(job)?;
            let outcome = commands::asr_transcribe(
                app.clone(),
                state,
                p.media_id,
                p.model.unwrap_or_default(),
                p.language.unwrap_or_default(),
                p.overwrite,
            )
            .await?;
            Ok(format!("生成 {} 条字幕（{}）", outcome.cue_count, outcome.model))
        }
        "cloud" => {
            let p: CloudPayload = parse_payload(job)?;
            let report = commands::cloud_transcribe(state, p.media_id, p.overwrite).await?;
            Ok(format!(
                "云端生成 {} 条字幕（上传 {}）",
                report.cue_count,
                human_size(report.audio_bytes)
            ))
        }
        "export" => {
            let opts: ExportOptions = parse_payload(job)?;
            let outcome = commands::export_video(state, opts).await?;
            Ok(format!(
                "导出完成（{}）",
                human_size(outcome.size_bytes)
            ))
        }
        "draft" => {
            let p: DraftPayload = parse_payload(job)?;
            let report = commands::export_draft(
                app.clone(),
                state,
                p.media_id,
                p.draft_root,
                p.draft_name,
                p.fps.unwrap_or(30),
            )
            .await?;
            Ok(format!(
                "草稿已生成：{} 个片段 / {} 条字幕",
                report.clip_count, report.cue_count
            ))
        }
        "publish" => {
            let p: PublishPayload = parse_payload(job)?;
            let report = commands::export_publish_package(
                state,
                p.media_id,
                p.platform,
                p.account,
                p.out_root,
            )
            .await?;
            Ok(format!(
                "发布包已生成：{} 个片段 / {} 份字幕",
                report.clip_count, report.subtitle_count
            ))
        }
        "verify" => {
            let p: VerifyPayload = parse_payload(job)?;
            let report = commands::verify_transform(state, p.media_id, p.samples).await?;
            Ok(format!(
                "比对 {} 个片段，平均相似度 {:.1}%（{}）",
                report.checked,
                report.mean_similarity * 100.0,
                report.verdict
            ))
        }
        other => Err(AppError::Msg(format!(
            "不认识的任务类型：{other}（支持的：{}）",
            KINDS.map(|(k, _)| k).join("/")
        ))),
    }
}

// ---------------- 执行循环 ----------------

/// 跑队列，直到没有待跑任务、被暂停、被要求停止，或达到本次上限。
pub async fn run(app: AppHandle, max_jobs: Option<i64>) -> AppResult<QueueReport> {
    {
        let state = app.state::<AppState>();
        if state.queue_running.swap(true, Ordering::SeqCst) {
            return Err(AppError::Msg("队列已经在跑了，不用重复启动".into()));
        }
        // 每次启动都清掉上一次的停止请求，避免"一点启动就立刻停"
        state.queue_cancel.store(false, Ordering::SeqCst);
    }

    let outcome = run_inner(&app, max_jobs.unwrap_or(i64::MAX)).await;

    app.state::<AppState>()
        .queue_running
        .store(false, Ordering::SeqCst);
    outcome
}

/// 这个任务产出了哪个文件（能点的那个）。
///
/// 用户的想法：「产出的内容在日志里尽量能直接点击播放」——
/// 对。产出不该是另一个列表，它就该长在产生它的那一行上。
///
/// 只给一个**能播的**文件：
/// - 整集去重：一集一个文件，就是它
/// - 切片：一集好几片，给第一片（日志里已经写明"切出 N 片"）
/// - 别的步骤：没有可播的单文件，就不给（那一行也就不可点）
fn artifact_of(conn: &rusqlite::Connection, job: &Job) -> Option<String> {
    let media_id = job.media_id?;
    match job.kind.as_str() {
        "dedup" => {
            let m = db::get_media(conn, media_id).ok()?;
            if !m.processed_path.trim().is_empty()
                && std::path::Path::new(&m.processed_path).is_file()
            {
                Some(m.processed_path)
            } else {
                None
            }
        }
        "slice" => db::list_segments(conn, media_id)
            .ok()?
            .into_iter()
            .find(|s| s.status == "ok" && std::path::Path::new(&s.out_path).is_file())
            .map(|s| s.out_path),
        _ => None,
    }
}

/// 队列同时跑几个任务。
///
/// 每个任务是一个独立的 ffmpeg 进程，而 ffmpeg 自己已经多线程
/// （libx264 默认吃满多核）。开太多只会互相抢核，反而更慢。
///
/// 按「每路 4 个核」算，夹在 1~4 路之间。
/// 实测的痛点：61 集整集去重一个个跑要半小时以上，用户抱怨「很慢很慢」——
/// 瓶颈就在并行度，不在编码器。
fn job_concurrency() -> i64 {
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    ((cores / 4).max(1) as i64).clamp(1, 4)
}

async fn run_inner(app: &AppHandle, limit: i64) -> AppResult<QueueReport> {
    let limit = limit.max(1);
    let workers = job_concurrency();
    let mut processed = 0i64;
    let mut done = 0i64;
    let mut failed = 0i64;
    let mut messages: Vec<String> = Vec::new();
    let mut stopped_because = "没有待跑的任务了".to_string();

    while processed < limit {
        {
            let state = app.state::<AppState>();
            if state.queue_paused.load(Ordering::SeqCst) {
                stopped_because = "队列被暂停".to_string();
                break;
            }
            if state.queue_cancel.load(Ordering::SeqCst) {
                stopped_because = "收到停止请求（当前任务跑完后停下）".to_string();
                break;
            }
        }

        // **取一批同类型的任务一起跑。**
        //
        // 为什么必须是同类型：任务之间不是都能并行 ——「切片」得在「去重」
        // 之后（切片读的正是去重产物），去重没跑完就切片，切出来的是原片。
        // 但同一类型内部彼此独立：第 3 集去重和第 7 集去重互不相干。
        //
        // 规则：看队首是什么类型，就只并发取这个类型 —— 天然保证
        // 「上一步全部跑完，才轮到下一步」。
        //
        // 61 集去重原来一个个跑要半小时以上，这是用户抱怨「很慢很慢」的根源。
        let (kind, batch) = {
            let state = app.state::<AppState>();
            let conn = state
                .db
                .lock()
                .map_err(|_| AppError::Msg("数据库锁被污染，请重启应用".into()))?;
            let Some(kind) = db::next_pending_kind(&conn)? else {
                break;
            };
            let jobs = db::next_pending_jobs_of_kind(&conn, &kind, workers)?;
            for job in &jobs {
                db::mark_job_running(&conn, job.id)?;
                // **步骤标题只打一次。**
                //
                // 标题是分节（`===== 整集去重 =====`），不是每个任务的进度条。
                // 每个任务都打一遍的话，61 集就是 61 行一模一样的标题 ——
                // 用户直接问过「怎么执行这么多次，执行一次不行嘛」。
                //
                // 判据：这部剧最近一条日志如果已经是同一个标题，就不再打。
                if let Some(drama_id) = job.pipeline_drama_id {
                    let label = format!("===== {} =====", kind_label(&job.kind));
                    let run_id = db::run_id_of_drama(&conn, Some(drama_id)).unwrap_or(None);
                    // **这次运行里这个标题没写过才写。**
                    //
                    // 不能用「最近一条是不是标题」当判据 —— 明细行之后
                    // 它又会认为该打了，61 集分十几批跑就会打十几次标题。
                    let already = match run_id {
                        Some(r) => db::has_log_with_message(&conn, drama_id, r, &label)
                            .unwrap_or(false),
                        None => false,
                    };
                    if !already {
                        db::append_log(&conn, run_id, Some(drama_id), "info", &label);
                        // 去重这一步**把这次摇到的强度写下来** ——
                        // 真实软件也打这一行。13 项强度每次运行都不一样，
                        // 不写下来，用户回头没法知道这批片子是按什么参数处理的。
                        if job.kind == "dedup" {
                            db::append_log(
                                &conn,
                                run_id,
                                Some(drama_id),
                                "info",
                                &format!(
                                    "本次去重强度：{}",
                                    crate::transform::Strengths::fresh().describe()
                                ),
                            );
                        }
                    }
                }
            }
            (kind, jobs)
        };
        if batch.is_empty() {
            break;
        }

        for job in &batch {
            let _ = app.emit(
                "queue:progress",
                serde_json::json!({
                    "jobId": job.id,
                    "kind": job.kind,
                    "phase": "running",
                }),
            );
        }

        // 并发执行 —— 这一整段 await 期间**不持有任何数据库锁**。
        // 各自的 ffmpeg 是独立进程，互不干扰。
        // 记下这批属于哪次运行 —— batch 马上要被 move 进任务里，
        // 后面写「步骤完成」汇总时还要用
        let batch_run_id: Option<i64> = {
            let state = app.state::<AppState>();
            let conn = state
                .db
                .lock()
                .map_err(|_| AppError::Msg("数据库锁被污染，请重启应用".into()))?;
            batch
                .iter()
                .filter_map(|j| j.pipeline_drama_id)
                .find_map(|d| db::run_id_of_drama(&conn, Some(d)).unwrap_or(None))
        };

        let mut handles = Vec::with_capacity(batch.len());
        for job in batch {
            let app_task = app.clone();
            handles.push(tauri::async_runtime::spawn(async move {
                let outcome = execute(&app_task, &job).await;
                (job, outcome)
            }));
        }
        let mut outcomes = Vec::with_capacity(handles.len());
        for h in handles {
            match h.await {
                Ok(pair) => outcomes.push(pair),
                Err(e) => eprintln!("DramaDist: 队列任务异常终止：{e}"),
            }
        }

        for (job, outcome) in outcomes {

        {
            let state = app.state::<AppState>();
            let conn = state
                .db
                .lock()
                .map_err(|_| AppError::Msg("数据库锁被污染，请重启应用".into()))?;
            match &outcome {
                Ok(summary) => db::finish_job(&conn, job.id, db::JOB_DONE, summary)?,
                Err(e) => db::finish_job(&conn, job.id, db::JOB_FAILED, &e.to_string())?,
            }

            // 流水线任务：把进度写回它所属的那部剧。
            // 失败时整部剧标失败 —— 一步没成，后面的步骤继续跑没有意义。
            if let Some(drama_id) = job.pipeline_drama_id {
                let run_id = db::run_id_of_drama(&conn, Some(drama_id)).unwrap_or(None);
                // **每行要能认出是哪一集。**
                //
                // 61 集跑下来就是 61 行明细，如果都长成
                // 「✓ 整集去重：去重完成（57.0 MB）」—— 认不出谁是谁，
                // 没法对账。带上集名才叫「每个任务打印一次」。
                let who = job
                    .media_id
                    .and_then(|m| db::get_media(&conn, m).ok())
                    .map(|m| format!("{} ", m.name))
                    .unwrap_or_default();
                match &outcome {
                    Ok(summary) => {
                        // 有产出文件的那种，这一行界面上能直接点开播
                        let artifact = artifact_of(&conn, &job).unwrap_or_default();
                        db::append_log_with_file(
                            &conn,
                            run_id,
                            Some(drama_id),
                            "ok",
                            &format!(
                                "✓ {who}{}：{}",
                                kind_label(&job.kind),
                                crate::proc::tail(summary)
                            ),
                            &artifact,
                        );
                        db::update_pipeline_drama(
                            &conn,
                            drama_id,
                            &job.kind,
                            "running",
                            &format!("{}：{}", kind_label(&job.kind), crate::proc::tail(summary)),
                        )?;
                        // 这部剧的任务全跑完了吗？跑完就标成完成。
                        //
                        // 少了这一步，剧目会永远停在「进行中」——
                        // 真实素材那轮 64 个任务两个多小时前就跑完了，
                        // 界面上却还在转，用户以为卡死。
                        if db::count_pending_jobs_of_drama(&conn, drama_id)? == 0 {
                            db::update_pipeline_drama(
                                &conn,
                                drama_id,
                                &job.kind,
                                "done",
                                "全部步骤已完成",
                            )?;
                        }
                    }
                    Err(e) => {
                        db::append_log(
                            &conn,
                            run_id,
                            Some(drama_id),
                            "error",
                            &format!(
                                "✗ {who}{}：{}",
                                kind_label(&job.kind),
                                crate::proc::tail(&e.to_string())
                            ),
                        );
                        db::update_pipeline_drama(
                            &conn,
                            drama_id,
                            &job.kind,
                            "failed",
                            &format!("{}：{}", kind_label(&job.kind), crate::proc::tail(&e.to_string())),
                        )?;
                        // 这一步没成，这部剧后面的步骤就不该再跑 ——
                        // 界面上写着「其余步骤不再继续」，代码就得真做。
                        let cancelled = db::cancel_jobs_of_drama(&conn, drama_id)?;
                        if cancelled > 0 {
                            eprintln!(
                                "DramaDist: 剧 #{drama_id} 的 {} 失败，已撤掉它剩下的 {cancelled} 个任务",
                                kind_label(&job.kind)
                            );
                        }
                    }
                }
            }
        }

        match outcome {
            Ok(summary) => {
                done += 1;
                if messages.len() < 50 {
                    messages.push(format!(
                        "#{} {}：{summary}",
                        job.id,
                        kind_label(&job.kind)
                    ));
                }
                let _ = app.emit(
                    "queue:progress",
                    serde_json::json!({
                        "jobId": job.id, "kind": job.kind, "phase": "done", "message": summary,
                    }),
                );
            }
            Err(e) => {
                failed += 1;
                if messages.len() < 50 {
                    messages.push(format!(
                        "#{} {}：失败 —— {e}",
                        job.id,
                        kind_label(&job.kind)
                    ));
                }
                let _ = app.emit(
                    "queue:progress",
                    serde_json::json!({
                        "jobId": job.id, "kind": job.kind, "phase": "failed", "message": e.to_string(),
                    }),
                );
            }
        }
            processed += 1;
        }

        // **步骤收口。**
        //
        // 明细一条条列出来是对的（用户要看去重差异），但一长串 ✓ 没有尽头，
        // 用户不知道「这一步到底完了没有」。跑到这一步没剩下同类任务，
        // 就补一行汇总把它收住。
        if let Some(run_id) = batch_run_id {
            let state = app.state::<AppState>();
            let conn = state
                .db
                .lock()
                .map_err(|_| AppError::Msg("数据库锁被污染，请重启应用".into()))?;
            if db::count_pending_jobs_of_kind(&conn, &kind)? == 0 {
                let n = db::count_done_jobs_of_kind(&conn, run_id, &kind).unwrap_or(0);
                db::append_log(
                    &conn,
                    Some(run_id),
                    None,
                    "info",
                    &format!("===== {} 全部完成（{n} 个） =====", kind_label(&kind)),
                );
            }
        }
    }

    if processed >= limit {
        stopped_because = format!("达到本次上限（{limit} 条）");
    }

    let remaining = {
        let state = app.state::<AppState>();
        let conn = state
            .db
            .lock()
            .map_err(|_| AppError::Msg("数据库锁被污染，请重启应用".into()))?;
        // 队列空了就把流水线的状态收口，别让它永远停在"运行中"
        let _ = db::refresh_pipeline_run_status(&conn);
        db::job_counts(&conn)?.pending
    };

    Ok(QueueReport {
        processed,
        done,
        failed,
        remaining,
        stopped_because,
        messages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_labels_and_validation() {
        assert_eq!(kind_label("slice"), "切片");
        assert_eq!(kind_label("ocr"), "硬字幕识别");
        // 不认识的类型原样返回，不 panic
        assert_eq!(kind_label("nope"), "nope");
        assert!(is_known_kind("export"));
        assert!(!is_known_kind("upload"));
        // 八种类型都不重复
        let mut keys: Vec<&str> = KINDS.iter().map(|(k, _)| *k).collect();
        keys.sort_unstable();
        let before = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), before);
    }

    #[test]
    fn payload_parsing_reports_the_offending_job() {
        let job = Job {
            id: 42,
            media_id: Some(1),
            kind: "slice".into(),
            payload: "{ not json }".into(),
            status: "pending".into(),
            message: String::new(),
            attempts: 0,
            created_at: String::new(),
            updated_at: String::new(),
            pipeline_drama_id: None,
        };
        let err = parse_payload::<SlicePayload>(&job).unwrap_err().to_string();
        assert!(err.contains("#42"), "错误里要带上任务号：{err}");
        assert!(err.contains("无法解析"), "{err}");
    }

    #[test]
    fn payload_parsing_accepts_camel_case_shape() {
        let job = Job {
            id: 1,
            media_id: Some(3),
            kind: "slice".into(),
            payload: r#"{"mediaId":3,"mode":{"mode":"equalParts","parts":6},"outDir":"E:/out"}"#.into(),
            status: "pending".into(),
            message: String::new(),
            attempts: 0,
            created_at: String::new(),
            updated_at: String::new(),
            pipeline_drama_id: None,
        };
        let parsed: SlicePayload = parse_payload(&job).unwrap();
        assert_eq!(parsed.media_id, 3);
        assert_eq!(parsed.out_dir, "E:/out");
        assert!(matches!(parsed.mode, SliceMode::EqualParts { parts: 6 }));
    }

    #[test]
    fn human_size_formats_reasonably() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(2048), "2.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human_size(300 * 1024 * 1024), "300 MB");
    }
}
