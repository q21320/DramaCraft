//! DramaDist —— 短剧切片分发工作台。
//!
//! 分层：`commands`（前后端边界）→ `db` / `media` / `asr` / `tools`（能力）→ `models`（数据结构）。
//! 重活一律走子进程，主进程只负责编排、落库、上报进度。

mod asr;
mod asset;
mod cloud;
mod commands;
mod db;
mod docx;
mod error;
mod export;
mod jy;
mod media;
mod models;
mod ocr;
mod pipeline;
mod proc;
mod proof;
mod publish;
mod queue;
mod series;
mod subtitle;
mod tools;
mod transform;
mod verify;

use models::ToolPaths;
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use tauri::{Emitter, Manager};

/// 去掉 Windows 的扩展长度路径前缀 `\\?\`。
///
/// Tauri 的 `resource_dir()` 在 Windows 上会返回带这个前缀的路径。
/// Python 自己能处理（worker 脚本找得到），但**ctranslate2 是 C++ 的，
/// 不认这个前缀** —— 直接把带前缀的模型目录传给它，会报
/// `Unable to open file 'model.bin'`。
fn strip_extended_prefix(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

pub struct AppState {
    pub db: Mutex<Connection>,
    pub tools: Mutex<ToolPaths>,
    /// 队列是否正在跑（同时只允许一个 runner）
    pub queue_running: AtomicBool,
    /// 暂停标记：跑完当前这条后不再取新的
    pub queue_paused: AtomicBool,
    /// 停止请求：同样在当前任务跑完后生效
    pub queue_cancel: AtomicBool,
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            // 内置运行时：安装后在资源目录下，开发时在源码的 resources/ 下。
            // 登记之后 tools::resolve 就会优先用它 —— 用户不必自己装 ffmpeg 或 Python。
            // 两个候选都试：打包器可能保留 `resources/` 这一层，也可能不保留。
            let bundled = app
                .path()
                .resource_dir()
                .ok()
                .map(|dir| {
                    [
                        dir.join("resources"),
                        dir.clone(),
                    ]
                })
                .and_then(|roots| {
                    roots
                        .into_iter()
                        .find(|root| root.join(tools::BUNDLED_DIR).is_dir())
                })
                .or_else(|| {
                    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources");
                    dev.is_dir().then_some(dev)
                });

            let bundled_dirs: Vec<PathBuf> = bundled
                .map(|root| {
                    let root = strip_extended_prefix(root);
                    [tools::BUNDLED_DIR, tools::ASR_RUNTIME_DIR]
                        .iter()
                        .map(|sub| root.join(sub))
                        .filter(|dir| dir.is_dir())
                        .collect()
                })
                .unwrap_or_default();

            for dir in &bundled_dirs {
                eprintln!("DramaDist: 内置运行时目录 {}", dir.display());
            }
            tools::set_bundled_dirs(bundled_dirs);

            // 启动时把解析结果打出来：出问题时第一时间就知道用的是哪一份
            for name in ["ffmpeg", "ffprobe", "python"] {
                match tools::resolve("", name) {
                    Some(path) => eprintln!(
                        "DramaDist: {name} = {}（{}）",
                        path.display(),
                        if tools::is_bundled(&path) {
                            "内置"
                        } else {
                            "外部"
                        }
                    ),
                    None => eprintln!("DramaDist: 没找到 {name}"),
                }
            }

            let conn = db::open(&data_dir.join("dramadist.db"))?;

            // 断点续跑第一步：进程被杀后库里会留下假的 running —— 没有人在跑它。
            // 必须退回 pending，否则这些任务永远不会再被拾起。
            let recovered = db::recover_interrupted_jobs(&conn)?;
            let recovered_pipelines = db::recover_interrupted_pipelines(&conn)?;

            // 自动探测一遍，再用用户上次保存的路径覆盖
            let mut paths = tools::initial_paths();
            for (key, slot) in [("tool.ffmpeg", 0u8), ("tool.ffprobe", 1), ("tool.python", 2)] {
                if let Some(saved) = db::get_setting(&conn, key)? {
                    let saved = saved.trim().to_string();
                    if !saved.is_empty() {
                        match slot {
                            0 => paths.ffmpeg = saved,
                            1 => paths.ffprobe = saved,
                            _ => paths.python = saved,
                        }
                    }
                }
            }

            app.manage(AppState {
                db: Mutex::new(conn),
                tools: Mutex::new(paths),
                queue_running: AtomicBool::new(false),
                queue_paused: AtomicBool::new(false),
                queue_cancel: AtomicBool::new(false),
            });

            if recovered > 0 {
                eprintln!("DramaDist: {recovered} 条队列任务在上次运行中被中断，已退回待跑");
            }
            if recovered_pipelines > 0 {
                eprintln!("DramaDist: {recovered_pipelines} 部剧的流水线在上次运行中被中断，已退回待跑");
            }

            // 断点续跑第二步：退回队列还不够，得**真的接着跑**。
            // 只把状态改回 pending 而等用户手点「启动队列」，那叫断点，不叫续跑。
            // 用户主动暂停过的队列不会自动重启 —— 那是有意的选择，不该被覆盖。
            let (pending, paused_before) = {
                let state = app.state::<AppState>();
                let conn = state
                    .db
                    .lock()
                    .map_err(|_| crate::error::AppError::Msg("数据库锁被污染".into()))?;
                let counts = db::job_counts(&conn)?;
                let paused = db::get_setting(&conn, "queue.auto_resume")?
                    .map(|v| v.trim() == "off")
                    .unwrap_or(false);
                (counts.pending, paused)
            };

            if pending > 0 && !paused_before {
                eprintln!("DramaDist: 队列里有 {pending} 条待跑任务，自动接着跑");
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    match queue::run(handle.clone(), None).await {
                        Ok(report) => {
                            eprintln!(
                                "DramaDist: 队列结束 —— 完成 {}，失败 {}，剩余 {}（{}）",
                                report.done, report.failed, report.remaining, report.stopped_because
                            );
                            let _ = handle.emit("queue:finished", &report);
                        }
                        Err(e) => {
                            eprintln!("DramaDist: 队列启动失败：{e}");
                            let _ = handle.emit(
                                "queue:finished",
                                serde_json::json!({ "error": e.to_string() }),
                            );
                        }
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::get_tools,
            commands::detect_tools,
            commands::set_tools,
            commands::asr_status,
            commands::asr_models,
            commands::create_project,
            commands::list_projects,
            commands::delete_project,
            commands::add_media,
            commands::list_media,
            commands::remove_media,
            commands::build_slice_plan,
            commands::run_slice,
            commands::list_segments,
            commands::list_cues,
            commands::replace_cues,
            commands::import_srt,
            commands::export_srt,
            commands::asr_transcribe,
            commands::export_video,
            commands::export_draft,
            commands::transform_preview,
            commands::transform_segments,
            commands::revert_segments,
            commands::list_platforms,
            commands::list_publish_plans,
            commands::list_publish_accounts,
            commands::save_publish_plan,
            commands::mark_publish_status,
            commands::export_publish_package,
            commands::verify_transform,
            commands::ocr_status,
            commands::recognize_hard_subtitles,
            commands::cloud_config,
            commands::save_cloud_config,
            commands::cloud_test,
            commands::cloud_transcribe,
            commands::proof_default_layout,
            commands::proof_load_layout,
            commands::proof_save_layout,
            commands::proof_preview,
            commands::proof_render,
            commands::generate_cost_docx,
            commands::generate_ip_docx,
            commands::asset_kinds,
            commands::scan_asset_dir,
            commands::import_assets,
            commands::list_assets,
            commands::update_asset,
            commands::delete_asset,
            commands::asset_summary,
            commands::apply_bgm,
            commands::apply_lut,
            commands::scan_series_dir,
            commands::cluster_series_by_intro,
            commands::detect_episodes,
            commands::resplit_episodes,
            commands::queue_state,
            commands::list_jobs,
            commands::enqueue_job,
            commands::enqueue_batch,
            commands::start_queue,
            commands::set_queue_paused,
            commands::request_queue_stop,
            commands::cancel_pending_jobs,
            commands::clear_finished_jobs,
            commands::delete_job,
            commands::pipeline_stages,
            commands::pipeline_scan,
            commands::pipeline_default_out_dir,
            commands::pipeline_start,
            commands::pipeline_list,
            commands::pipeline_get,
            commands::pipeline_cancel,
            commands::pipeline_outputs,
            commands::open_in_player,
            commands::reveal_in_explorer,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended_length_prefix_is_stripped() {
        // Tauri 的 resource_dir() 在 Windows 上返回带 \\?\ 的路径。
        // ctranslate2 打不开这种路径 —— 真实素材上撞到过：
        //   RuntimeError: Unable to open file 'model.bin'
        assert_eq!(
            strip_extended_prefix(PathBuf::from(r"\\?\E:\a\b\models\small")),
            PathBuf::from(r"E:\a\b\models\small")
        );
        // UNC 要还原成 \\server\share，不能变成 server\share
        assert_eq!(
            strip_extended_prefix(PathBuf::from(r"\\?\UNC\server\share\x")),
            PathBuf::from(r"\\server\share\x")
        );
        // 本来就没前缀的不动
        assert_eq!(
            strip_extended_prefix(PathBuf::from(r"E:\a\b")),
            PathBuf::from(r"E:\a\b")
        );
        // 相对路径也不动
        assert_eq!(
            strip_extended_prefix(PathBuf::from("models/small")),
            PathBuf::from("models/small")
        );
    }
}