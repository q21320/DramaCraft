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
use crate::models::{ExportOptions, Job, OcrOptions, QueueReport, SliceMode, TransformOptions};
use crate::{commands, db, AppState};
use serde::Deserialize;
use std::sync::atomic::Ordering;
use tauri::{AppHandle, Emitter, Manager};

/// 队列支持的任务类型：(key, 中文名)
pub const KINDS: [(&str, &str); 8] = [
    ("slice", "切片"),
    ("transform", "画面处理"),
    ("ocr", "硬字幕识别"),
    ("asr", "语音转字幕"),
    ("export", "导出成片"),
    ("draft", "剪映草稿"),
    ("publish", "发布包"),
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
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransformPayload {
    media_id: i64,
    intensity: String,
    options: Option<TransformOptions>,
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
    out_root: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VerifyPayload {
    media_id: i64,
    samples: Option<usize>,
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
        "slice" => {
            let p: SlicePayload = parse_payload(job)?;
            let segments =
                commands::run_slice(app.clone(), state, p.media_id, p.mode, p.out_dir).await?;
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
                commands::transform_segments(state, p.media_id, p.intensity, p.options).await?;
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
            )
            .await?;
            Ok(format!("生成 {} 条字幕（{}）", outcome.cue_count, outcome.model))
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
            let report =
                commands::export_publish_package(state, p.media_id, p.platform, p.out_root).await?;
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

async fn run_inner(app: &AppHandle, limit: i64) -> AppResult<QueueReport> {
    let limit = limit.max(1);
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

        let job = {
            let state = app.state::<AppState>();
            let conn = state
                .db
                .lock()
                .map_err(|_| AppError::Msg("数据库锁被污染，请重启应用".into()))?;
            db::next_pending_job(&conn)?
        };
        let Some(job) = job else {
            break;
        };

        {
            let state = app.state::<AppState>();
            let conn = state
                .db
                .lock()
                .map_err(|_| AppError::Msg("数据库锁被污染，请重启应用".into()))?;
            db::mark_job_running(&conn, job.id)?;
        }
        let _ = app.emit(
            "queue:progress",
            serde_json::json!({
                "jobId": job.id,
                "kind": job.kind,
                "phase": "running",
            }),
        );

        // 关键：这一句 await 期间不持有任何数据库锁
        let outcome = execute(app, &job).await;

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

    if processed >= limit {
        stopped_because = format!("达到本次上限（{limit} 条）");
    }

    let remaining = {
        let state = app.state::<AppState>();
        let conn = state
            .db
            .lock()
            .map_err(|_| AppError::Msg("数据库锁被污染，请重启应用".into()))?;
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
