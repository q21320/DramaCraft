//! 前后端唯一边界。前端只认这里的命令名与参数形状。

use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::{
    AppInfo, AsrOutcome, AsrRequest, Cue, EpisodeBoundary, EpisodeDetection, ExportOptions,
    ExportOutcome, Job, Media, OcrFrame, OcrOptions, OcrReport, PlatformOption, Project,
    PublishClipEntry, PublishPackageReport, PublishPlan, QueueState, ResplitReport, RuntimeStatus,
    Segment, SeriesGroup, SliceMode, SlicePlan, ToolPaths, ToolReport, TransformOptions,
    TransformReport, VerifyItem, VerifyReport,
};
use crate::{
    asr, export, jy, media, ocr, publish, queue, series, subtitle, tools, transform, verify,
    AppState,
};
use std::sync::atomic::Ordering;
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
pub fn asr_status(state: tauri::State<'_, AppState>) -> AppResult<RuntimeStatus> {
    let configured = lock_tools(&state)?.clone();
    Ok(asr::status(&configured.python))
}

/// OCR 硬字幕识别的运行时状态
#[tauri::command]
pub fn ocr_status(state: tauri::State<'_, AppState>) -> AppResult<RuntimeStatus> {
    let configured = lock_tools(&state)?.clone();
    Ok(ocr::status(&configured.python))
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

// ---------------- OCR 硬字幕识别 ----------------

/// 识别画面里烧死的硬字幕，生成字幕轴。
///
/// 已有字幕时默认**拒绝执行**（`overwrite = false`），避免把 ASR 的成果静默冲掉。
#[tauri::command]
pub async fn recognize_hard_subtitles(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    media_id: i64,
    options: Option<OcrOptions>,
    overwrite: Option<bool>,
) -> AppResult<OcrReport> {
    let opts = ocr::normalize_options(&options.unwrap_or_else(ocr::default_options));
    let overwrite = overwrite.unwrap_or(false);

    let (ffmpeg, python, worker, input, media_duration, existing) = {
        let configured = lock_tools(&state)?.clone();
        let ffmpeg = tools::require(&configured.ffmpeg, "ffmpeg")?;
        let python = tools::require(&configured.python, "python")?;
        let conn = lock_db(&state)?;
        let media = db::get_media(&conn, media_id)?;
        let existing = db::list_cues(&conn, media_id)?.len() as i64;
        (
            ffmpeg,
            python,
            worker_script(&app, "ocr_worker.py"),
            media.path,
            media.duration_ms,
            existing,
        )
    };

    if existing > 0 && !overwrite {
        return Err(AppError::Msg(format!(
            "这个素材已经有 {existing} 条字幕了。确认要覆盖的话，勾选「覆盖现有字幕」再执行；\
             或者先把现有字幕导出成 SRT 备份。"
        )));
    }

    let frames_dir = std::env::temp_dir().join(format!("dramadist_ocr_{media_id}"));
    let opts_task = opts.clone();
    let frames_dir_task = frames_dir.clone();
    let ffmpeg_task = ffmpeg.clone();
    let input_task = input.clone();
    let python_task = python.clone();
    let worker_task = worker.clone();

    let outcome = tauri::async_runtime::spawn_blocking(
        move || -> AppResult<(Vec<OcrFrame>, String, usize)> {
            let frames =
                ocr::extract_frames(&ffmpeg_task, Path::new(&input_task), &frames_dir_task, &opts_task)?;
            let total = frames.len();
            let (recognized, engine) = ocr::run_worker(
                &python_task,
                &worker_task,
                &frames,
                &opts_task,
                ocr::default_timeout(),
            )?;
            Ok((recognized, engine, total))
        },
    )
    .await
    .map_err(|e| AppError::Msg(format!("OCR 线程异常终止：{e}")))?;

    // 抽出来的帧是中间产物，无论成败都清掉
    let _ = std::fs::remove_dir_all(&frames_dir);

    let (recognized, engine, total_frames) = outcome?;
    let interval = (1000.0 / opts.sample_fps).round() as i64;
    let cues = ocr::merge_frames(&recognized, interval, opts.min_duration_ms, opts.merge_threshold);

    if cues.is_empty() {
        return Err(AppError::Msg(
            "识别跑完了，但没有拼出任何字幕。可以试试：调低置信度阈值、把「只识别底部」调成 1.0（整帧）、或确认素材里确实有硬字幕。"
                .into(),
        ));
    }

    let with_text = recognized
        .iter()
        .filter(|f| !f.text.trim().is_empty())
        .count() as i64;
    let cue_count = {
        let mut conn = lock_db(&state)?;
        db::replace_cues(&mut conn, media_id, &cues)?
    };

    let mut warnings: Vec<String> = Vec::new();
    if total_frames >= opts.max_frames {
        warnings.push(format!(
            "抽帧达到上限 {} 帧，视频后面可能没被识别到。可以降低抽帧频率后重试。",
            opts.max_frames
        ));
    }
    if (with_text as f64) / (total_frames.max(1) as f64) < 0.05 {
        warnings.push(
            "只有很少的帧识别到文字。如果素材确实带硬字幕，把「识别区域」调成 1.0（整帧）再试一次。"
                .into(),
        );
    }
    warnings.push(
        "硬字幕识别是逐帧推断，断句按相邻帧文本相似度合并，可能需要人工校对时间轴。".into(),
    );

    Ok(OcrReport {
        frames_sampled: total_frames as i64,
        frames_with_text: with_text,
        cue_count,
        engine,
        duration_ms: media_duration,
        warnings,
    })
}

// ---------------- 剧目与集 ----------------

/// 扫描目录，按文件名把素材分成「剧目 → 集」。
#[tauri::command]
pub fn scan_series_dir(dir: String) -> AppResult<Vec<SeriesGroup>> {
    let trimmed = dir.trim();
    if trimmed.is_empty() {
        return Err(AppError::Msg("请先选择要扫描的目录".into()));
    }
    let files = series::list_video_files(Path::new(trimmed))?;
    Ok(series::group_files(&files))
}

/// 文件名实在没规律时，用**片头指纹**把同一部剧的素材聚到一起。
///
/// 原理：同一部剧的片头画面是一样的，dHash 指纹能把它们找回来。
/// 读不出来的文件会被跳过，不影响其余文件。
#[tauri::command]
pub async fn cluster_series_by_intro(
    state: tauri::State<'_, AppState>,
    files: Vec<String>,
    intro_seconds: Option<f64>,
    tolerance: Option<u32>,
) -> AppResult<Vec<SeriesGroup>> {
    if files.is_empty() {
        return Err(AppError::Msg("没有要聚类的文件".into()));
    }
    let (ffmpeg, ffprobe) = {
        let configured = lock_tools(&state)?.clone();
        (
            tools::require(&configured.ffmpeg, "ffmpeg")?,
            tools::require(&configured.ffprobe, "ffprobe")?,
        )
    };
    let intro_ms = (intro_seconds.unwrap_or(3.0).clamp(0.5, 60.0) * 1000.0).round() as i64;
    let tolerance = tolerance.unwrap_or(10).min(32);

    tauri::async_runtime::spawn_blocking(move || -> AppResult<Vec<SeriesGroup>> {
        let mut usable: Vec<String> = Vec::new();
        let mut fingerprints: Vec<Vec<u64>> = Vec::new();

        for path in &files {
            let file = PathBuf::from(path);
            if !file.is_file() {
                continue;
            }
            let Ok(probe) = media::probe(&ffprobe, &file) else {
                continue;
            };
            if probe.duration_ms <= 0 {
                continue;
            }
            if let Ok(fingerprint) =
                series::intro_fingerprint(&ffmpeg, &file, probe.duration_ms, intro_ms, 2.0)
            {
                if !fingerprint.is_empty() {
                    usable.push(path.clone());
                    fingerprints.push(fingerprint);
                }
            }
        }

        if usable.is_empty() {
            return Err(AppError::Msg(
                "这些文件一个都没读出来，确认它们是不是有效的视频文件".into(),
            ));
        }

        let clusters = series::cluster_by_fingerprint(&fingerprints, tolerance);
        let mut groups: Vec<SeriesGroup> = Vec::new();

        for (i, members) in clusters.iter().enumerate() {
            let paths: Vec<String> = members.iter().map(|k| usable[*k].clone()).collect();
            let mut entries = series::files_to_entries(&paths);
            series::sort_episode_entries(&mut entries);

            // 组名优先用文件名解析出的剧目名（若全组一致），否则用序号
            let mut names: Vec<String> = entries.iter().map(|e| e.series.clone()).collect();
            names.sort();
            names.dedup();
            let label = if names.len() == 1 && !names[0].is_empty() && names[0] != "未命名" {
                format!("{}（按片头聚类）", names[0])
            } else {
                format!("未命名分组 {}", i + 1)
            };

            groups.push(SeriesGroup {
                series: label,
                episodes: entries,
            });
        }

        groups.sort_by(|a, b| {
            b.episodes
                .len()
                .cmp(&a.episodes.len())
                .then_with(|| a.series.cmp(&b.series))
        });
        Ok(groups)
    })
    .await
    .map_err(|e| AppError::Msg(format!("聚类线程异常终止：{e}")))?
}

/// 用片头指纹在全片里找每一集的起点。
///
/// 思路：把开头一段（默认 3 秒）当片头模板，在全片采样里找它每次出现的位置 ——
/// 那就是每集的开头。不依赖任何外部信息，只依赖"同一部剧片头一样"这个事实。
#[tauri::command]
pub async fn detect_episodes(
    state: tauri::State<'_, AppState>,
    media_id: i64,
    sample_fps: Option<f64>,
    intro_seconds: Option<f64>,
    tolerance: Option<u32>,
) -> AppResult<EpisodeDetection> {
    let (ffmpeg, input, duration_ms) = {
        let configured = lock_tools(&state)?.clone();
        let ffmpeg = tools::require(&configured.ffmpeg, "ffmpeg")?;
        let conn = lock_db(&state)?;
        let target = db::get_media(&conn, media_id)?;
        (ffmpeg, target.path, target.duration_ms)
    };
    if duration_ms <= 0 {
        return Err(AppError::Msg(
            "这个素材的时长是 0，先到「素材库」重新导入一次让它被正确探测".into(),
        ));
    }

    let fps = sample_fps.unwrap_or(2.0).clamp(0.2, 10.0);
    let intro_ms = (intro_seconds.unwrap_or(3.0).clamp(0.5, 60.0) * 1000.0).round() as i64;
    let tolerance = tolerance.unwrap_or(6).min(24);

    let outcome = tauri::async_runtime::spawn_blocking(
        move || -> AppResult<(Vec<(i64, u64)>, Vec<u64>)> {
            let samples =
                series::sample_timeline(&ffmpeg, Path::new(&input), duration_ms, fps, 20_000)?;
            let template = series::intro_template(&samples, intro_ms);
            Ok((samples, template))
        },
    )
    .await
    .map_err(|e| AppError::Msg(format!("采样线程异常终止：{e}")))??;

    let (samples, template) = outcome;
    let mut warnings: Vec<String> = Vec::new();
    if template.len() < 3 {
        warnings.push(
            "片头模板太短（不足 3 帧），结果可能不稳。把「片头时长」调大或提高「采样频率」再试。"
                .into(),
        );
    }

    // 两集之间不可能太近：按素材长度取一个下界
    let min_gap = (duration_ms / 100).clamp(3_000, 120_000);
    let hits = series::find_intro_positions(&samples, &template, tolerance, min_gap);
    let boundaries = series::boundaries_from_hits(&hits, duration_ms, template.len());

    if boundaries.len() <= 1 {
        warnings.push(
            "只找到一处片头。可能这段素材本来就只有一集，或者片头不够明显 —— 可以调大「容忍度」、调小「片头时长」再试。"
                .into(),
        );
    }
    if boundaries.len() > 200 {
        warnings.push(format!(
            "找到 {} 处片头，数量偏多，多半是容忍度太宽，建议调小。",
            boundaries.len()
        ));
    }
    warnings.push(
        "边界由画面指纹推断，误差通常在半个采样间隔内；切之前请先看一眼下面的边界列表。".into(),
    );

    Ok(EpisodeDetection {
        boundaries,
        sampled_frames: samples.len() as i64,
        template_frames: template.len() as i64,
        total_duration_ms: duration_ms,
        warnings,
    })
}

/// 按集边界切分原片，每集一个文件。
#[tauri::command]
pub async fn resplit_episodes(
    state: tauri::State<'_, AppState>,
    media_id: i64,
    boundaries: Vec<EpisodeBoundary>,
    out_dir: String,
) -> AppResult<ResplitReport> {
    if boundaries.is_empty() {
        return Err(AppError::Msg("没有集边界可切，请先执行「检测集边界」".into()));
    }
    let out_root = PathBuf::from(out_dir.trim());
    if out_root.as_os_str().is_empty() {
        return Err(AppError::Msg("请先选择输出目录".into()));
    }

    let (ffmpeg, input) = {
        let configured = lock_tools(&state)?.clone();
        let ffmpeg = tools::require(&configured.ffmpeg, "ffmpeg")?;
        let conn = lock_db(&state)?;
        let target = db::get_media(&conn, media_id)?;
        (ffmpeg, target.path)
    };

    let total_duration_ms: i64 = boundaries.iter().map(|b| b.duration_ms.max(0)).sum();
    let out_root_task = out_root.clone();
    let files = tauri::async_runtime::spawn_blocking(move || {
        series::resplit_by_episodes(&ffmpeg, Path::new(&input), &boundaries, &out_root_task)
    })
    .await
    .map_err(|e| AppError::Msg(format!("切分线程异常终止：{e}")))??;

    Ok(ResplitReport {
        episode_count: files.len() as i64,
        out_dir: out_root.to_string_lossy().to_string(),
        files: files
            .iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect(),
        total_duration_ms,
    })
}

// ---------------- 任务队列 ----------------

#[tauri::command]
pub fn queue_state(state: tauri::State<'_, AppState>) -> AppResult<QueueState> {
    let conn = lock_db(&state)?;
    Ok(QueueState {
        running: state.queue_running.load(Ordering::SeqCst),
        paused: state.queue_paused.load(Ordering::SeqCst),
        counts: db::job_counts(&conn)?,
    })
}

#[tauri::command]
pub fn list_jobs(state: tauri::State<'_, AppState>, limit: Option<i64>) -> AppResult<Vec<Job>> {
    let conn = lock_db(&state)?;
    db::list_jobs(&conn, limit.unwrap_or(300))
}

#[tauri::command]
pub fn enqueue_job(
    state: tauri::State<'_, AppState>,
    media_id: Option<i64>,
    kind: String,
    payload: Option<String>,
) -> AppResult<Job> {
    let kind = kind.trim().to_string();
    if !queue::is_known_kind(&kind) {
        return Err(AppError::Msg(format!(
            "不支持的任务类型：{kind}（支持：{}）",
            queue::KINDS.map(|(k, _)| k).join("/")
        )));
    }
    let payload = payload.unwrap_or_else(|| "{}".to_string());
    // 入队时先验一次 JSON，参数写错立刻报，而不是排到一半才炸
    serde_json::from_str::<serde_json::Value>(&payload)
        .map_err(|e| AppError::Msg(format!("参数不是合法 JSON：{e}")))?;

    let conn = lock_db(&state)?;
    db::enqueue_job(&conn, media_id, &kind, &payload)
}

/// 批量入队：同一份参数模板套用到多个素材（会自动写入各自的 `mediaId`）。
#[tauri::command]
pub fn enqueue_batch(
    state: tauri::State<'_, AppState>,
    media_ids: Vec<i64>,
    kind: String,
    payload: Option<String>,
) -> AppResult<i64> {
    let kind = kind.trim().to_string();
    if !queue::is_known_kind(&kind) {
        return Err(AppError::Msg(format!("不支持的任务类型：{kind}")));
    }
    if media_ids.is_empty() {
        return Err(AppError::Msg("没有选择素材".into()));
    }

    let mut template: serde_json::Map<String, serde_json::Value> = match payload {
        Some(text) if !text.trim().is_empty() => serde_json::from_str(&text)
            .map_err(|e| AppError::Msg(format!("参数必须是 JSON 对象：{e}")))?,
        _ => serde_json::Map::new(),
    };

    let conn = lock_db(&state)?;
    let mut count = 0i64;
    for media_id in &media_ids {
        template.insert("mediaId".to_string(), serde_json::json!(media_id));

        // 批量任务里凡是要落地成文件名的，都必须按素材区分，否则会互相覆盖
        if kind == "draft" || kind == "export" {
            let media = db::get_media(&conn, *media_id)?;
            let stem = media
                .name
                .rsplit_once('.')
                .map(|(s, _)| s.to_string())
                .unwrap_or_else(|| media.name.clone());

            if kind == "draft" {
                let base = template
                    .get("draftName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                let name = if base.is_empty() {
                    format!("{stem}_切片")
                } else {
                    format!("{base}_{stem}")
                };
                template.insert(
                    "draftName".to_string(),
                    serde_json::json!(jy::safe_draft_name(&name)),
                );
            } else {
                let raw = template
                    .get("outPath")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if raw.is_empty() {
                    return Err(AppError::Msg("导出任务需要指定输出目录".into()));
                }
                let given = PathBuf::from(&raw);
                let dir = if given.extension().is_some() {
                    // 给的是具体文件名 → 取它所在目录，文件名按素材名重算
                    given
                        .parent()
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|| PathBuf::from("."))
                } else {
                    given
                };
                template.insert(
                    "outPath".to_string(),
                    serde_json::json!(dir
                        .join(format!("{stem}_out.mp4"))
                        .to_string_lossy()
                        .to_string()),
                );
            }
        }

        let text = serde_json::Value::Object(template.clone()).to_string();
        db::enqueue_job(&conn, Some(*media_id), &kind, &text)?;
        count += 1;
    }
    Ok(count)
}

/// 启动队列。后台跑、立即返回，进度通过 `queue:progress` 事件推送。
#[tauri::command]
pub fn start_queue(app: tauri::AppHandle, max_jobs: Option<i64>) -> AppResult<()> {
    {
        let state = app.state::<AppState>();
        if state.queue_running.load(Ordering::SeqCst) {
            return Err(AppError::Msg("队列已经在跑了".into()));
        }
        // 顺手清掉暂停标记，否则「暂停 → 再点启动」会立刻又停下
        state.queue_paused.store(false, Ordering::SeqCst);
    }

    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        match queue::run(handle.clone(), max_jobs).await {
            Ok(report) => {
                let _ = handle.emit("queue:finished", &report);
            }
            Err(e) => {
                let _ = handle.emit(
                    "queue:finished",
                    serde_json::json!({ "error": e.to_string() }),
                );
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub fn set_queue_paused(state: tauri::State<'_, AppState>, paused: bool) -> AppResult<bool> {
    state.queue_paused.store(paused, Ordering::SeqCst);
    Ok(paused)
}

/// 请求停止：**跑完当前这条**才停，不会把正在编码的 ffmpeg 砍掉。
#[tauri::command]
pub fn request_queue_stop(state: tauri::State<'_, AppState>) -> AppResult<bool> {
    state.queue_cancel.store(true, Ordering::SeqCst);
    Ok(true)
}

#[tauri::command]
pub fn cancel_pending_jobs(state: tauri::State<'_, AppState>) -> AppResult<i64> {
    let conn = lock_db(&state)?;
    db::cancel_pending_jobs(&conn)
}

#[tauri::command]
pub fn clear_finished_jobs(state: tauri::State<'_, AppState>) -> AppResult<i64> {
    let conn = lock_db(&state)?;
    db::clear_finished_jobs(&conn)
}

/// 删掉单条任务（正在跑的不允许删）。
#[tauri::command]
pub fn delete_job(state: tauri::State<'_, AppState>, id: i64) -> AppResult<()> {
    let conn = lock_db(&state)?;
    db::delete_job(&conn, id)
}

// ---------------- 去重效果验证 ----------------

/// 逐个比对「处理前 vs 处理后」的画面差异，给处理闭环。
///
/// 阈值是经验值：各平台判重算法不公开，这里只回答「改动幅度够不够明显」，
/// **不承诺能过任何平台的判重**。
#[tauri::command]
pub async fn verify_transform(
    state: tauri::State<'_, AppState>,
    media_id: i64,
    samples: Option<usize>,
) -> AppResult<VerifyReport> {
    let (ffmpeg, ffprobe, segments) = {
        let configured = lock_tools(&state)?.clone();
        let ffmpeg = tools::require(&configured.ffmpeg, "ffmpeg")?;
        let ffprobe = tools::require(&configured.ffprobe, "ffprobe")?;
        let conn = lock_db(&state)?;
        let segments: Vec<Segment> = db::list_segments(&conn, media_id)?
            .into_iter()
            .filter(|s| s.status == "ok" && !s.out_path.trim().is_empty())
            .collect();
        (ffmpeg, ffprobe, segments)
    };

    if segments.is_empty() {
        return Err(AppError::Msg(
            "这个素材还没有成功的切片，先到「切片工作台」执行切片".into(),
        ));
    }

    let sample_count = samples.unwrap_or(verify::DEFAULT_SAMPLES).clamp(2, 30);

    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let mut items: Vec<VerifyItem> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        let mut unchanged = 0i64;
        let mut failed = 0i64;

        for seg in segments {
            if seg.original_path.trim().is_empty() {
                unchanged += 1;
                continue;
            }
            let before = PathBuf::from(&seg.original_path);
            let after = PathBuf::from(&seg.out_path);
            if !before.is_file() || !after.is_file() {
                failed += 1;
                if errors.len() < 10 {
                    errors.push(format!("片段 {}：处理前后的文件有缺失，无法比对", seg.index_no));
                }
                continue;
            }

            let before_probe = match media::probe(&ffprobe, &before) {
                Ok(p) => p,
                Err(e) => {
                    failed += 1;
                    if errors.len() < 10 {
                        errors.push(format!("片段 {}：读取原文件失败（{e}）", seg.index_no));
                    }
                    continue;
                }
            };
            let after_probe = match media::probe(&ffprobe, &after) {
                Ok(p) => p,
                Err(e) => {
                    failed += 1;
                    if errors.len() < 10 {
                        errors.push(format!("片段 {}：读取处理后的文件失败（{e}）", seg.index_no));
                    }
                    continue;
                }
            };

            match verify::compare_files(
                &ffmpeg,
                &before,
                &before_probe,
                &after,
                &after_probe,
                sample_count,
            ) {
                Ok(report) => items.push(VerifyItem {
                    segment_id: seg.id,
                    index_no: seg.index_no,
                    original_path: seg.original_path.clone(),
                    current_path: seg.out_path.clone(),
                    report,
                }),
                Err(e) => {
                    failed += 1;
                    if errors.len() < 10 {
                        errors.push(format!("片段 {}：比对失败（{e}）", seg.index_no));
                    }
                }
            }
        }

        (items, errors, unchanged, failed)
    })
    .await
    .map_err(|e| AppError::Msg(format!("验证线程异常终止：{e}")))?;

    let (items, errors, unchanged, failed) = outcome;
    let mean = if items.is_empty() {
        0.0
    } else {
        items.iter().map(|i| i.report.mean_similarity).sum::<f64>() / items.len() as f64
    };

    let mut warnings: Vec<String> = Vec::new();
    if items.is_empty() {
        warnings.push("没有可比对的片段：这些切片还没做过画面处理".into());
    } else if mean >= 0.95 {
        warnings.push(
            "平均相似度偏高，说明改动幅度很小。可以到「画面处理」用更高档位重做一次。".into(),
        );
    }
    warnings.push(
        "结论基于感知哈希（dHash），只说明改动幅度；各平台判重算法不公开，不承诺能过任何平台的判重。"
            .into(),
    );

    Ok(VerifyReport {
        checked: items.len() as i64,
        unchanged,
        failed,
        mean_similarity: mean,
        verdict: if items.is_empty() {
            "无可比对片段".to_string()
        } else {
            verify::verdict(mean).to_string()
        },
        items,
        errors,
        warnings,
    })
}

// ---------------- 发布 ----------------

#[tauri::command]
pub fn list_platforms() -> AppResult<Vec<PlatformOption>> {
    Ok(publish::platform_options())
}
#[tauri::command]
pub fn list_publish_plans(
    state: tauri::State<'_, AppState>,
    media_id: i64,
) -> AppResult<Vec<PublishPlan>> {
    let conn = lock_db(&state)?;
    db::list_publish_plans(&conn, media_id)
}

#[tauri::command]
pub fn save_publish_plan(
    state: tauri::State<'_, AppState>,
    plan: PublishPlan,
) -> AppResult<PublishPlan> {
    let platform = plan.platform.trim().to_string();
    if !publish::is_known_platform(&platform) {
        return Err(AppError::Msg(format!("不支持的平台：{platform}")));
    }
    let title = plan.title.trim().to_string();
    if title.is_empty() {
        return Err(AppError::Msg("标题不能为空".into()));
    }
    let normalized = PublishPlan {
        id: plan.id,
        media_id: plan.media_id,
        platform,
        title,
        description: plan.description.trim().to_string(),
        tags: plan.tags.trim().to_string(),
        cover_path: plan.cover_path.trim().to_string(),
        status: publish::normalize_status(&plan.status),
        note: plan.note.trim().to_string(),
        updated_at: String::new(),
    };
    let conn = lock_db(&state)?;
    db::upsert_publish_plan(&conn, &normalized)
}

/// 标记某个平台发没发出去。这是本地台账，不联网。
#[tauri::command]
pub fn mark_publish_status(
    state: tauri::State<'_, AppState>,
    media_id: i64,
    platform: String,
    status: String,
    note: String,
) -> AppResult<PublishPlan> {
    let platform = platform.trim().to_string();
    let status = publish::normalize_status(&status);
    let conn = lock_db(&state)?;
    db::set_publish_status(&conn, media_id, &platform, &status, note.trim())?;
    db::get_publish_plan(&conn, media_id, &platform)?
        .ok_or_else(|| AppError::Msg("读取发布设置失败".into()))
}

/// 导出发布包：视频、字幕、封面、元数据、说明文件按平台整理成一个目录。
///
/// **只写本地文件，不向任何平台发送内容。**
#[tauri::command]
pub async fn export_publish_package(
    state: tauri::State<'_, AppState>,
    media_id: i64,
    platform: String,
    out_root: String,
) -> AppResult<PublishPackageReport> {
    let platform = platform.trim().to_string();
    if !publish::is_known_platform(&platform) {
        return Err(AppError::Msg(format!("不支持的平台：{platform}")));
    }
    let out_root_path = PathBuf::from(out_root.trim());
    if out_root_path.as_os_str().is_empty() {
        return Err(AppError::Msg("请先选择发布包输出目录".into()));
    }

    let (ffmpeg, meta, custom_cover, package_clips) = {
        let configured = lock_tools(&state)?.clone();
        let ffmpeg = tools::require(&configured.ffmpeg, "ffmpeg")?;
        let conn = lock_db(&state)?;

        let media = db::get_media(&conn, media_id)?;
        let stored = db::get_publish_plan(&conn, media_id, &platform)?;

        // 还没填设置也能出包：标题用素材名兜底，其余留空
        let fallback_title = media
            .name
            .rsplit_once('.')
            .map(|(stem, _)| stem.to_string())
            .unwrap_or_else(|| media.name.clone());
        let effective = PublishPlan {
            id: stored.as_ref().map(|p| p.id).unwrap_or(0),
            media_id,
            platform: platform.clone(),
            title: stored
                .as_ref()
                .map(|p| p.title.trim().to_string())
                .filter(|t| !t.is_empty())
                .unwrap_or(fallback_title),
            description: stored
                .as_ref()
                .map(|p| p.description.clone())
                .unwrap_or_default(),
            tags: stored.as_ref().map(|p| p.tags.clone()).unwrap_or_default(),
            cover_path: stored
                .as_ref()
                .map(|p| p.cover_path.trim().to_string())
                .unwrap_or_default(),
            status: stored
                .as_ref()
                .map(|p| p.status.clone())
                .unwrap_or_else(|| publish::STATUS_DRAFT.to_string()),
            note: String::new(),
            updated_at: String::new(),
        };

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
        let total = segments.len() as i64;

        let mut entries: Vec<PublishClipEntry> = Vec::with_capacity(segments.len());
        let mut package_clips: Vec<publish::PackageClip> = Vec::with_capacity(segments.len());

        for (offset, seg) in segments.iter().enumerate() {
            let index_no = offset as i64 + 1;
            let cues = subtitle::project_range(&all_cues, seg.start_ms, seg.end_ms, 200);
            let srt = if cues.is_empty() {
                None
            } else {
                Some(subtitle::to_srt(&cues))
            };
            let subtitle_text = cues
                .iter()
                .map(|c| c.text.trim())
                .collect::<Vec<_>>()
                .join(" ");

            entries.push(PublishClipEntry {
                index_no,
                file: format!("{index_no:03}.mp4"),
                start_ms: seg.start_ms,
                end_ms: seg.end_ms,
                duration_ms: (seg.end_ms - seg.start_ms).max(0),
                subtitle: subtitle_text,
                suggested_title: publish::suggested_title(&effective.title, index_no, total),
            });
            package_clips.push(publish::PackageClip {
                index_no,
                source: PathBuf::from(&seg.out_path),
                srt,
            });
        }

        let meta =
            publish::build_metadata(&effective, &media.name, media.width, media.height, entries);
        let cover = PathBuf::from(effective.cover_path.trim());
        let custom_cover = if cover.as_os_str().is_empty() {
            None
        } else {
            Some(cover)
        };

        (ffmpeg, meta, custom_cover, package_clips)
    };

    tauri::async_runtime::spawn_blocking(move || {
        publish::write_package(
            &ffmpeg,
            &out_root_path,
            &meta,
            &package_clips,
            custom_cover.as_deref(),
        )
    })
    .await
    .map_err(|e| AppError::Msg(format!("打包线程异常终止：{e}")))?
}
