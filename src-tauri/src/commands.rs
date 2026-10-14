//! 前后端唯一边界。前端只认这里的命令名与参数形状。

use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::{
    AppInfo, AsrStatus, Media, Project, Segment, SliceMode, SlicePlan, ToolPaths, ToolReport,
};
use crate::{asr, media, tools, AppState};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;
use tauri::{Emitter, Manager};

fn lock_db<'a>(state: &'a tauri::State<'_, AppState>) -> AppResult<MutexGuard<'a, Connection>> {
    state
        .db
        .lock()
        .map_err(|_| AppError::Msg("数据库锁被污染，请重启应用".into()))
}

fn lock_tools<'a>(state: &'a tauri::State<'_, AppState>) -> AppResult<MutexGuard<'a, ToolPaths>> {
    state
        .tools
        .lock()
        .map_err(|_| AppError::Msg("工具配置锁被污染，请重启应用".into()))
}

// ---------------- 应用与设置 ----------------

#[tauri::command]
pub fn app_info(app: tauri::AppHandle) -> AppResult<AppInfo> {
    let data_dir = app
        .path()
        .app_data_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(AppInfo {
        name: "DramaDist".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        data_dir,
    })
}

#[tauri::command]
pub fn get_tools(state: tauri::State<'_, AppState>) -> AppResult<ToolPaths> {
    Ok(lock_tools(&state)?.clone())
}

#[tauri::command]
pub fn detect_tools(state: tauri::State<'_, AppState>) -> AppResult<Vec<ToolReport>> {
    let configured = lock_tools(&state)?.clone();
    Ok(tools::probe_all(&configured))
}

#[tauri::command]
pub fn set_tools(state: tauri::State<'_, AppState>, paths: ToolPaths) -> AppResult<Vec<ToolReport>> {
    {
        let conn = lock_db(&state)?;
        db::set_setting(&conn, "tool.ffmpeg", &paths.ffmpeg)?;
        db::set_setting(&conn, "tool.ffprobe", &paths.ffprobe)?;
        db::set_setting(&conn, "tool.python", &paths.python)?;
    }
    {
        let mut guard = lock_tools(&state)?;
        *guard = paths.clone();
    }
    Ok(tools::probe_all(&paths))
}

#[tauri::command]
pub fn asr_status(state: tauri::State<'_, AppState>) -> AppResult<AsrStatus> {
    let configured = lock_tools(&state)?.clone();
    Ok(asr::status(&configured.python))
}

// ---------------- 项目 ----------------

#[tauri::command]
pub fn create_project(
    state: tauri::State<'_, AppState>,
    name: String,
    root_dir: String,
) -> AppResult<Project> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Msg("项目名不能为空".into()));
    }
    let conn = lock_db(&state)?;
    db::create_project(&conn, &name, root_dir.trim())
}

#[tauri::command]
pub fn list_projects(state: tauri::State<'_, AppState>) -> AppResult<Vec<Project>> {
    let conn = lock_db(&state)?;
    db::list_projects(&conn)
}

#[tauri::command]
pub fn delete_project(state: tauri::State<'_, AppState>, id: i64) -> AppResult<()> {
    let conn = lock_db(&state)?;
    db::delete_project(&conn, id)
}

// ---------------- 素材 ----------------

#[tauri::command]
pub fn add_media(
    state: tauri::State<'_, AppState>,
    project_id: i64,
    path: String,
) -> AppResult<Media> {
    let path = path.trim().to_string();
    if path.is_empty() {
        return Err(AppError::Msg("素材路径不能为空".into()));
    }

    let ffprobe = {
        let configured = lock_tools(&state)?.clone();
        tools::require(&configured.ffprobe, "ffprobe")?
    };

    let file = PathBuf::from(&path);
    {
        let conn = lock_db(&state)?;
        if db::list_media(&conn, project_id)?.iter().any(|m| m.path == path) {
            return Err(AppError::Msg("这个素材已经在项目里了".into()));
        }
    }

    let probe = media::probe(&ffprobe, &file)?;
    let name = file
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| path.clone());

    let conn = lock_db(&state)?;
    db::add_media(&conn, project_id, &path, &name, &probe)
}

#[tauri::command]
pub fn list_media(state: tauri::State<'_, AppState>, project_id: i64) -> AppResult<Vec<Media>> {
    let conn = lock_db(&state)?;
    db::list_media(&conn, project_id)
}

#[tauri::command]
pub fn remove_media(state: tauri::State<'_, AppState>, id: i64) -> AppResult<()> {
    let conn = lock_db(&state)?;
    db::remove_media(&conn, id)
}

// ---------------- 切片 ----------------

/// 只算计划不落盘，用于界面上的实时预览。
#[tauri::command]
pub fn build_slice_plan(
    state: tauri::State<'_, AppState>,
    media_id: i64,
    mode: SliceMode,
) -> AppResult<Vec<SlicePlan>> {
    let conn = lock_db(&state)?;
    let target = db::get_media(&conn, media_id)?;
    Ok(media::plan::build_plan(target.duration_ms, &mode))
}

/// 执行切片。**异步**：真正的 ffmpeg 循环丢进 blocking 线程池，
/// 界面通过 `slice:progress` 事件看进度，主线程不卡。
#[tauri::command]
pub async fn run_slice(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    media_id: i64,
    mode: SliceMode,
    out_dir: String,
) -> AppResult<Vec<Segment>> {
    let (input_path, plans, ffmpeg) = {
        let configured = lock_tools(&state)?.clone();
        let ffmpeg = tools::require(&configured.ffmpeg, "ffmpeg")?;
        let conn = lock_db(&state)?;
        let target = db::get_media(&conn, media_id)?;
        let plans = media::plan::build_plan(target.duration_ms, &mode);
        (target.path, plans, ffmpeg)
    };

    if plans.is_empty() {
        return Err(AppError::Msg(
            "切片计划为空：请检查切分参数（素材过短，或份数/步长设置不合理）".into(),
        ));
    }

    let out_root = PathBuf::from(out_dir.trim());
    if out_root.as_os_str().is_empty() {
        return Err(AppError::Msg("请先选择输出目录".into()));
    }
    std::fs::create_dir_all(&out_root)?;

    let total = plans.len();
    let app_task = app.clone();
    let ffmpeg_task = ffmpeg.clone();
    let input_task = input_path.clone();
    let out_task = out_root.clone();
    let plans_task = plans.clone();

    // 每片一条结果：计划 / 落地路径 / 体积 / 状态 / 说明
    let results = tauri::async_runtime::spawn_blocking(
        move || -> Vec<(SlicePlan, String, i64, String, String)> {
            let mut acc = Vec::with_capacity(total);
            for plan in &plans_task {
                let _ = app_task.emit(
                    "slice:progress",
                    serde_json::json!({
                        "mediaId": media_id,
                        "index": plan.index,
                        "total": total,
                        "phase": "running",
                    }),
                );
                let out_path = out_task.join(format!("{:03}.mp4", plan.index));
                match media::slice(&ffmpeg_task, Path::new(&input_task), &out_path, plan) {
                    Ok(o) => acc.push((
                        plan.clone(),
                        o.out_path.to_string_lossy().to_string(),
                        o.size_bytes,
                        "ok".to_string(),
                        o.message,
                    )),
                    Err(e) => acc.push((
                        plan.clone(),
                        out_path.to_string_lossy().to_string(),
                        0,
                        "failed".to_string(),
                        e.to_string(),
                    )),
                }
            }
            acc
        },
    )
    .await
    .map_err(|e| AppError::Msg(format!("切片线程异常终止：{e}")))?;

    // 落库：先清旧结果，避免重跑后残留
    {
        let conn = lock_db(&state)?;
        db::clear_segments(&conn, media_id)?;
        for (plan, path, size, status, message) in &results {
            db::insert_segment(
                &conn,
                media_id,
                plan.index,
                plan.start_ms,
                plan.end_ms,
                path,
                *size,
                status,
                message,
            )?;
        }
    }

    let failed = results.iter().filter(|r| r.3 == "failed").count();
    let _ = app.emit(
        "slice:progress",
        serde_json::json!({
            "mediaId": media_id,
            "index": total,
            "total": total,
            "phase": "done",
            "failed": failed,
        }),
    );

    let conn = lock_db(&state)?;
    db::list_segments(&conn, media_id)
}

#[tauri::command]
pub fn list_segments(state: tauri::State<'_, AppState>, media_id: i64) -> AppResult<Vec<Segment>> {
    let conn = lock_db(&state)?;
    db::list_segments(&conn, media_id)
}
