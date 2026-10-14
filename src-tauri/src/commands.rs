//! 前后端唯一边界。前端只认这里的命令名与参数形状。

use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::{
    AppInfo, AsrOutcome, AsrRequest, AsrStatus, Cue, ExportOptions, ExportOutcome, Media, Project,
    Segment, SliceMode, SlicePlan, ToolPaths, ToolReport, TransformOptions, TransformReport,
};
use crate::{asr, export, jy, media, subtitle, tools, transform, AppState};
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

// ---------------- 字幕 ----------------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SrtImportOutcome {
    pub count: i64,
    pub warnings: Vec<String>,
}

#[tauri::command]
pub fn list_cues(state: tauri::State<'_, AppState>, media_id: i64) -> AppResult<Vec<Cue>> {
    let conn = lock_db(&state)?;
    db::list_cues(&conn, media_id)
}

/// 整体覆盖某个素材的字幕（前端编辑后保存走这里）。
#[tauri::command]
pub fn replace_cues(
    state: tauri::State<'_, AppState>,
    media_id: i64,
    cues: Vec<Cue>,
) -> AppResult<i64> {
    let mut conn = lock_db(&state)?;
    db::replace_cues(&mut conn, media_id, &cues)
}

/// 导入 SRT。解析告警会一并返回，前端必须展示 —— 不静默丢内容。
#[tauri::command]
pub fn import_srt(
    state: tauri::State<'_, AppState>,
    media_id: i64,
    path: String,
) -> AppResult<SrtImportOutcome> {
    let text = std::fs::read_to_string(path.trim())
        .map_err(|e| AppError::Msg(format!("读取字幕文件失败：{e}")))?;
    let (cues, warnings) = subtitle::parse_srt(&text);
    if cues.is_empty() {
        return Err(AppError::Msg(format!(
            "没有解析出任何字幕（{} 条告警）",
            warnings.len()
        )));
    }
    let mut conn = lock_db(&state)?;
    let count = db::replace_cues(&mut conn, media_id, &cues)?;
    Ok(SrtImportOutcome { count, warnings })
}

#[tauri::command]
pub fn export_srt(
    state: tauri::State<'_, AppState>,
    media_id: i64,
    path: String,
) -> AppResult<i64> {
    let cues = {
        let conn = lock_db(&state)?;
        db::list_cues(&conn, media_id)?
    };
    if cues.is_empty() {
        return Err(AppError::Msg("这个素材还没有字幕".into()));
    }
    let path = PathBuf::from(path.trim());
    if path.as_os_str().is_empty() {
        return Err(AppError::Msg("请先设置字幕保存路径".into()));
    }
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    std::fs::write(&path, subtitle::to_srt(&cues))?;
    Ok(cues.len() as i64)
}

// ---------------- ASR ----------------

/// worker 脚本位置：打包后在资源目录，开发时在 `src-tauri/resources`。
fn worker_script(app: &tauri::AppHandle, file_name: &str) -> PathBuf {
    if let Ok(dir) = app.path().resource_dir() {
        for candidate in [
            dir.join("resources").join(file_name),
            dir.join(file_name),
        ] {
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources")
        .join(file_name)
}

/// 跑一次 ASR 并把结果整片覆盖到该素材的字幕上。长任务，走 blocking 线程池。
#[tauri::command]
pub async fn asr_transcribe(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    media_id: i64,
    model: String,
    language: String,
) -> AppResult<AsrOutcome> {
    let (python, worker, audio) = {
        let configured = lock_tools(&state)?.clone();
        let python = tools::require(&configured.python, "python")?;
        let conn = lock_db(&state)?;
        let target = db::get_media(&conn, media_id)?;
        (python, worker_script(&app, "asr_worker.py"), target.path)
    };

    let model = if model.trim().is_empty() {
        asr::DEFAULT_MODEL.to_string()
    } else {
        model.trim().to_string()
    };
    let language = if language.trim().is_empty() {
        "zh".to_string()
    } else {
        language.trim().to_string()
    };

    let request = AsrRequest {
        audio,
        model: model.clone(),
        language,
        model_dir: String::new(),
        device: "auto".into(),
        compute_type: String::new(),
    };

    let python_task = python.clone();
    let worker_task = worker.clone();
    let response = tauri::async_runtime::spawn_blocking(move || {
        asr::transcribe(&python_task, &worker_task, &request, asr::default_timeout())
    })
    .await
    .map_err(|e| AppError::Msg(format!("ASR 线程异常终止：{e}")))??;

    let cues = asr::response_to_cues(&response, media_id);
    if cues.is_empty() {
        return Err(AppError::Msg(
            "转写流程跑完了，但没有识别出任何语音内容（素材可能是纯音乐或静音）".into(),
        ));
    }
    let count = {
        let mut conn = lock_db(&state)?;
        db::replace_cues(&mut conn, media_id, &cues)?
    };

    Ok(AsrOutcome {
        cue_count: count,
        language: response.language.clone(),
        model: response.model.clone(),
        duration_ms: (response.duration * 1000.0).round() as i64,
        log: format!(
            "模型 {}，语言 {}，共 {} 条字幕",
            response.model,
            if response.language.is_empty() {
                "未知"
            } else {
                &response.language
            },
            count
        ),
    })
}

// ---------------- 成片导出 ----------------

/// 与成片同名的字幕文件路径（如 `final.mp4` → `final.srt`）。
fn srt_beside(out_path: &str) -> PathBuf {
    let path = PathBuf::from(out_path.trim());
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "subtitle".into());
    let dir = path
        .parent()
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    dir.join(format!("{stem}.srt"))
}

#[tauri::command]
pub async fn export_video(
    state: tauri::State<'_, AppState>,
    opts: ExportOptions,
) -> AppResult<ExportOutcome> {
    let (ffmpeg, input_path, subtitle_path) = {
        let configured = lock_tools(&state)?.clone();
        let ffmpeg = tools::require(&configured.ffmpeg, "ffmpeg")?;
        let conn = lock_db(&state)?;
        let target = db::get_media(&conn, opts.media_id)?;

        let subtitle_path = if opts.burn_subtitles {
            let cues = db::list_cues(&conn, opts.media_id)?;
            if cues.is_empty() {
                return Err(AppError::Msg(
                    "这个素材还没有字幕，先生成或导入 SRT 再烧进画面".into(),
                ));
            }
            let ranged = opts.end_ms > opts.start_ms;
            let effective = if ranged {
                subtitle::project_range(&cues, opts.start_ms, opts.end_ms, 200)
            } else {
                cues
            };
            if effective.is_empty() {
                return Err(AppError::Msg("所选区间内没有字幕".into()));
            }
            // 字幕与成片同名放在一起：既能被 ffmpeg 同目录读取，也顺手留给用户
            let srt_path = srt_beside(&opts.out_path);
            if let Some(dir) = srt_path.parent() {
                if !dir.as_os_str().is_empty() {
                    std::fs::create_dir_all(dir)?;
                }
            }
            std::fs::write(&srt_path, subtitle::to_srt(&effective))?;
            Some(srt_path)
        } else {
            None
        };

        (ffmpeg, target.path, subtitle_path)
    };

    let ffmpeg_task = ffmpeg.clone();
    let input_task = input_path.clone();
    let subtitle_task = subtitle_path.clone();
    let opts_task = opts.clone();

    let mut outcome = tauri::async_runtime::spawn_blocking(move || {
        export::export(
            &ffmpeg_task,
            Path::new(&input_task),
            &opts_task,
            subtitle_task.as_deref(),
        )
    })
    .await
    .map_err(|e| AppError::Msg(format!("导出线程异常终止：{e}")))??;

    if let Some(srt) = &subtitle_path {
        outcome.log = format!(
            "字幕已同步导出：{}\n{}",
            srt.to_string_lossy(),
            outcome.log
        );
    }
    Ok(outcome)
}

// ---------------- 剪映草稿 ----------------

/// 把某个素材的切片（连同字幕）导出成一份剪映草稿。
#[tauri::command]
pub async fn export_draft(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    media_id: i64,
    draft_root: String,
    draft_name: String,
    fps: u32,
) -> AppResult<jy::DraftReport> {
    let draft_root = draft_root.trim().to_string();
    if draft_root.is_empty() {
        return Err(AppError::Msg(
            "请先选择剪映草稿根目录（剪映「全局设置 → 草稿位置」指向的那个目录）".into(),
        ));
    }

    let (python, worker, clips, durations, cues_per_clip) = {
        let configured = lock_tools(&state)?.clone();
        let python = tools::require(&configured.python, "python")?;
        let conn = lock_db(&state)?;

        let segments: Vec<Segment> = db::list_segments(&conn, media_id)?
            .into_iter()
            .filter(|s| s.status == "ok" && !s.out_path.trim().is_empty())
            .collect();
        if segments.is_empty() {
            return Err(AppError::Msg(
                "这个素材还没有成功的切片，请先到「切片工作台」执行切片".into(),
            ));
        }

        let all_cues = db::list_cues(&conn, media_id)?;
        let mut durations = Vec::with_capacity(segments.len());
        let mut cues_per_clip = Vec::with_capacity(segments.len());
        let mut clips = Vec::with_capacity(segments.len());

        for seg in &segments {
            let duration = (seg.end_ms - seg.start_ms).max(0);
            durations.push(duration);
            // 整片字幕按切片区间裁剪平移，再交给 assemble_timeline 做顺序偏移
            cues_per_clip.push(subtitle::project_range(
                &all_cues,
                seg.start_ms,
                seg.end_ms,
                200,
            ));
            clips.push(jy::DraftClip {
                path: seg.out_path.clone(),
                duration_ms: duration,
            });
        }

        (
            python,
            worker_script(&app, "jy_worker.py"),
            clips,
            durations,
            cues_per_clip,
        )
    };

    let cues = jy::assemble_timeline(&durations, &cues_per_clip);
    let request = jy::DraftRequest {
        draft_root,
        draft_name: jy::safe_draft_name(&draft_name),
        width: jy::DEFAULT_WIDTH,
        height: jy::DEFAULT_HEIGHT,
        fps: if fps == 0 { jy::DEFAULT_FPS } else { fps },
        clips,
        cues,
        font_size: 8.0,
    };

    let python_task = python.clone();
    let worker_task = worker.clone();
    tauri::async_runtime::spawn_blocking(move || {
        jy::export_draft(&python_task, &worker_task, &request, jy::default_timeout())
    })
    .await
    .map_err(|e| AppError::Msg(format!("草稿线程异常终止：{e}")))?
}

// ---------------- 画面处理（差异化） ----------------

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformPreview {
    pub video_filter: String,
    pub audio_filter: String,
    /// 按序号生成的三组样例参数，让用户先看清"这一档会怎么处理"
    pub samples: Vec<TransformOptions>,
}

/// 只算不跑：返回将要使用的滤镜串与样例参数。
///
/// 传入 `width`/`height`（素材探测结果）时会给出精确像素的滤镜，
/// 与真正执行时的完全一致。
#[tauri::command]
pub fn transform_preview(
    options: Option<TransformOptions>,
    intensity: String,
    width: Option<u32>,
    height: Option<u32>,
) -> AppResult<TransformPreview> {
    let level = transform::Intensity::parse(&intensity);
    let target = match (width, height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => Some((w, h)),
        _ => None,
    };
    match options {
        Some(raw) => {
            let normalized = transform::normalize(&raw);
            Ok(TransformPreview {
                video_filter: transform::build_video_filter(&normalized, target),
                audio_filter: transform::build_audio_filter(&normalized),
                samples: vec![normalized],
            })
        }
        None => {
            let samples: Vec<TransformOptions> = (0..3)
                .map(|i| transform::normalize(&transform::preset_for(level, i)))
                .collect();
            let first = samples[0].clone();
            Ok(TransformPreview {
                video_filter: transform::build_video_filter(&first, target),
                audio_filter: transform::build_audio_filter(&first),
                samples,
            })
        }
    }
}

/// 批量处理某个素材的全部成功切片。产出与原件同目录的 `xxx_t.mp4`，并更新记录指向它。
#[tauri::command]
pub async fn transform_segments(
    state: tauri::State<'_, AppState>,
    media_id: i64,
    intensity: String,
    options: Option<TransformOptions>,
) -> AppResult<TransformReport> {
    let (ffmpeg, segments, target) = {
        let configured = lock_tools(&state)?.clone();
        let ffmpeg = tools::require(&configured.ffmpeg, "ffmpeg")?;
        let conn = lock_db(&state)?;
        let media = db::get_media(&conn, media_id)?;
        let segments: Vec<Segment> = db::list_segments(&conn, media_id)?
            .into_iter()
            .filter(|s| s.status == "ok" && !s.out_path.trim().is_empty())
            .collect();
        if segments.is_empty() {
            return Err(AppError::Msg(
                "这个素材还没有成功的切片，请先到「切片工作台」执行切片".into(),
            ));
        }
        // 切片是流拷贝出来的，尺寸与素材一致；有了它就能做精确缩放而不是相对缩放
        let target = if media.width > 0 && media.height > 0 {
            Some((media.width as u32, media.height as u32))
        } else {
            None
        };
        (ffmpeg, segments, target)
    };

    let level = transform::Intensity::parse(&intensity);
    let total = segments.len() as i64;

    // 给了 options 就所有片段用同一组；否则按序号生成确定性预设，每片各不相同
    let plan: Vec<(i64, String, String, TransformOptions)> = segments
        .iter()
        .enumerate()
        .map(|(index, seg)| {
            let opts = match &options {
                Some(raw) => transform::normalize(raw),
                None => transform::preset_for(level, index),
            };
            let input = PathBuf::from(&seg.out_path);
            let out = transform::processed_path(&input);
            (
                seg.id,
                seg.out_path.clone(),
                out.to_string_lossy().to_string(),
                opts,
            )
        })
        .collect();

    let ffmpeg_task = ffmpeg.clone();
    let results = tauri::async_runtime::spawn_blocking(move || {
        let mut acc: Vec<(i64, String, i64, String)> = Vec::with_capacity(plan.len());
        for (id, input, out, opts) in plan {
            match transform::apply(
                &ffmpeg_task,
                Path::new(&input),
                Path::new(&out),
                &opts,
                target,
            ) {
                Ok(size) => acc.push((id, out, size, String::new())),
                Err(e) => acc.push((id, out, 0, e.to_string())),
            }
        }
        acc
    })
    .await
    .map_err(|e| AppError::Msg(format!("画面处理线程异常终止：{e}")))?;

    let mut processed = 0i64;
    let mut failed = 0i64;
    let mut errors: Vec<String> = Vec::new();
    {
        let conn = lock_db(&state)?;
        for (id, out, size, message) in &results {
            if message.is_empty() && *size > 0 {
                db::update_segment_output(&conn, *id, out, *size)?;
                processed += 1;
            } else {
                failed += 1;
                if errors.len() < 10 {
                    let reason = if message.is_empty() {
                        "没有产出文件"
                    } else {
                        message.as_str()
                    };
                    errors.push(format!("片段 {id}：{reason}"));
                }
            }
        }
    }

    let conn = lock_db(&state)?;
    Ok(TransformReport {
        total,
        processed,
        failed,
        errors,
        segments: db::list_segments(&conn, media_id)?,
    })
}

/// 还原成处理前的文件。
#[tauri::command]
pub fn revert_segments(
    state: tauri::State<'_, AppState>,
    media_id: i64,
) -> AppResult<TransformReport> {
    let conn = lock_db(&state)?;
    let reverted = db::revert_segments(&conn, media_id)?;
    let segments = db::list_segments(&conn, media_id)?;
    Ok(TransformReport {
        total: segments.len() as i64,
        processed: reverted,
        failed: 0,
        errors: Vec::new(),
        segments,
    })
}
