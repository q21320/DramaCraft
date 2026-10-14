//! DramaDist —— 短剧切片分发工作台。
//!
//! 分层：`commands`（前后端边界）→ `db` / `media` / `asr` / `tools`（能力）→ `models`（数据结构）。
//! 重活一律走子进程，主进程只负责编排、落库、上报进度。

mod asr;
mod commands;
mod db;
mod error;
mod export;
mod jy;
mod media;
mod models;
mod proc;
mod subtitle;
mod tools;
mod transform;

use models::ToolPaths;
use rusqlite::Connection;
use std::sync::Mutex;
use tauri::Manager;

pub struct AppState {
    pub db: Mutex<Connection>,
    pub tools: Mutex<ToolPaths>,
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
            let conn = db::open(&data_dir.join("dramadist.db"))?;

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
            });
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
