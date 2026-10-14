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
use tauri::Manager;

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
            // 内置二进制：安装后在资源目录下，开发时在源码的 resources/bin 下。
            // 登记之后 tools::resolve 就会优先用它 —— 用户不必自己装 ffmpeg。
            // 两个候选都试：打包器可能保留 `resources/` 这一层，也可能不保留。
            let bundled = app
                .path()
                .resource_dir()
                .ok()
                .and_then(|dir| {
                    [
                        dir.join("resources").join(tools::BUNDLED_DIR),
                        dir.join(tools::BUNDLED_DIR),
                    ]
                    .into_iter()
                    .find(|candidate| candidate.is_dir())
                })
                .or_else(|| {
                    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("resources")
                        .join(tools::BUNDLED_DIR);
                    dev.is_dir().then_some(dev)
                });
            if let Some(dir) = &bundled {
                eprintln!("DramaDist: 使用内置工具目录 {}", dir.display());
            }
            tools::set_bundled_dir(bundled);

            // 启动时把解析结果打出来：出问题时第一时间就知道用的是哪一份
            for name in ["ffmpeg", "ffprobe"] {
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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app_info,
            commands::get_tools,
            commands::detect_tools,
            commands::set_tools,
            commands::asr_status,
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
