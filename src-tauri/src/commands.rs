//! 前后端唯一边界。前端只认这里的命令名与参数形状。

use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::{
    AppInfo, AsrOutcome, AsrRequest, Asset, AssetApplyReport, AssetImportReport, AssetKindOption,
    AssetSummary, CloudConfigView, CloudTestResult, CloudTranscribeReport, CostDocRequest, Cue,
    DiscoveredAsset, DocxReport, EpisodeBoundary, EpisodeDetection, ExportOptions, ExportOutcome,
    AiConfigView, AiProvider, AiProviderView, AiTestResult, AiUsage, CostReportRow, DedupReport,
    DesubtitleReport, RewriteOutcome, StoryboardReport, CoverReport, CoverRequest, HeaderLabel,
    IpDocRequest, Job, Media, MediaProbe, OcrFrame, OcrOptions, OcrReport, PipelineRun,
    PipelineStage, PlatformOption, Project,
    ProofLayout, ProofPreview, ProofRenderReport, PublishAccount, PublishClipEntry,
    PublishPackageReport, PublishPlan, QueueState, ResplitReport, RunLog, RuntimeStatus, Segment,
    OutputEpisode, SeriesGroup, SliceMode, SlicePlan, SourceScan, ToolPaths, ToolReport,
    TransformOptions, TransformReport, VerifyItem, VerifyReport,
};
use crate::{
    ai, asr, asset, cloud, cover, docx, export, jy, media, ocr, pipeline, proc, proof, publish, queue,
    series, subtitle, tools, transform, verify, AppState,
};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;
use tauri::{Emitter, Manager};

pub(crate) fn lock_db<'a>(
    state: &'a tauri::State<'_, AppState>,
) -> AppResult<MutexGuard<'a, Connection>> {
    state
        .db
        .lock()
        .map_err(|_| AppError::Msg("数据库锁被污染，请重启应用".into()))
}

pub(crate) fn lock_tools<'a>(
    state: &'a tauri::State<'_, AppState>,
) -> AppResult<MutexGuard<'a, ToolPaths>> {
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
    compatible: Option<bool>,
    // 文件名前缀。空串就用纯编号。
    prefix: String,
    // 指定源文件。给了就用它，否则按「去重产物 → 原片」自己找。
    // 切片吃的是**去字幕产物**（quzimu），由队列算好传进来。
    source_override: Option<String>,
) -> AppResult<Vec<Segment>> {
    let compatible = compatible.unwrap_or(false);
    let (input_path, plans, ffmpeg) = {
        let configured = lock_tools(&state)?.clone();
        let ffmpeg = tools::require(&configured.ffmpeg, "ffmpeg")?;
        let ffprobe = configured.ffprobe.clone();
        let conn = lock_db(&state)?;
        let target = db::get_media(&conn, media_id)?;
        // 源文件的优先级：
        //
        // 1. **AI 去字幕产物**（`第N集_quzimu.mp4`）—— 真实软件就是切它
        // 2. 去重产物（`media.processed_path`）
        // 3. 原片
        //
        // 第 1 条由调用方算好传进来（它才知道剧目录在哪）；
        // 第 2、3 条 `media_source_path` 自己处理。
        let source = match source_override {
            Some(p) if std::path::Path::new(&p).is_file() => p,
            _ => db::media_source_path(&conn, media_id)?,
        };

        // **切片计划用量「实际时长」，不是数据库里记的那个。**
        //
        // 踩过：数据库记 81 秒，但 `quzimu` 实际只有 80 秒出头 ——
        // 计划里的最后一片（80~81 秒）落在正文之外，ffmpeg 只写出一个
        // **321 字节的空容器**。状态还是 ok、文件也在，下游读不出时长就崩。
        //
        // 探不出来就退回数据库的值（探测本身失败不该阻断切片）。
        let actual_ms = media::probe(Path::new(&ffprobe), Path::new(&source))
            .map(|p| p.duration_ms)
            .unwrap_or(0);
        let plan_ms = if actual_ms > 0 { actual_ms } else { target.duration_ms };
        let plans = media::plan::build_plan(plan_ms, &mode);
        (source, plans, ffmpeg)
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
    let prefix_task = prefix.clone();
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
                // 带上集名：切片编号是「这一集的第几片」，多集平铺一个目录
                // 就会互相覆盖 —— 真实素材那轮 27 片只剩 3 片，就是这么丢的。
                let stem = if prefix_task.is_empty() {
                    format!("{:03}", plan.index)
                } else {
                    // **前缀和编号之间不加下划线** —— 真实软件是 `Flova AI_分镜001`，
                // 不是 `Flova AI_分镜_001`。下划线已经在 prefix 里了。
                format!("{}{:03}", prefix_task, plan.index)
                };
                let out_path = out_task.join(format!("{stem}.mp4"));
                match media::slice(&ffmpeg_task, Path::new(&input_task), &out_path, plan, compatible) {
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

/// 把成本报告渲染成 JPG（照真实软件的产出：`zhengming/<剧名>_成本配置比例情况报告.jpg`）。
///
/// 真实软件那份产出就是 **JPG**（1241×1754，A4 竖版），不是 docx。
/// 我们两份都出：docx 好改，jpg 直接能交。
///
/// 版式由 `resources/cost_report_render.py` 用 PIL 画 —— 打包环境里没有
/// LibreOffice/Word，docx 转图片这条路走不通。
///
/// **报审机构 / 法人 / 公章 / 签名都由调用方传，这里不预填任何公司名。**
/// 真实软件那份里的公司名和签名是用户自己的材料，不能写死在代码里。
pub fn render_cost_report_jpg(
    app: &tauri::AppHandle,
    python: &Path,
    req: &serde_json::Value,
) -> AppResult<()> {
    let worker = worker_script(app, "cost_report_render.py");
    if !worker.is_file() {
        return Err(AppError::Msg(format!(
            "找不到成本报告渲染脚本：{}",
            worker.to_string_lossy()
        )));
    }
    let value = proc::run_json_worker(python, &worker, req, std::time::Duration::from_secs(120))?;
    if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
        let msg = value
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("未知错误");
        return Err(AppError::Msg(format!("成本报告渲染失败：{msg}")));
    }
    Ok(())
}
/// 跑一次本地 ASR 并把结果整片覆盖到该素材的字幕上。长任务，走 blocking 线程池。
///
/// 与 OCR / 云端转写一致：已有字幕时默认**拒绝执行**，必须显式 `overwrite`。
/// 否则一次误点就会把人工校对过的字幕冲掉。
#[tauri::command]
pub async fn asr_transcribe(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    media_id: i64,
    model: String,
    language: String,
    overwrite: Option<bool>,
) -> AppResult<AsrOutcome> {
    let overwrite = overwrite.unwrap_or(false);

    let (python, worker, audio, existing) = {
        let configured = lock_tools(&state)?.clone();
        let python = tools::require(&configured.python, "python")?;
        let conn = lock_db(&state)?;
        let target = db::get_media(&conn, media_id)?;
        let existing = db::list_cues(&conn, media_id)?.len() as i64;
        (python, worker_script(&app, "asr_worker.py"), target.path, existing)
    };

    if existing > 0 && !overwrite {
        return Err(AppError::Msg(format!(
            "这个素材已经有 {existing} 条字幕了。确认要覆盖的话，勾选「覆盖现有字幕」再执行。"
        )));
    }

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

    // 随程序分发了这个规格的模型就直接用本地路径 —— 不走网络下载
    let model = match asr::bundled_model_dir(&model) {
        Some(dir) => dir.to_string_lossy().to_string(),
        None => model,
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
                video_filter: transform::build_video_filter(&normalized, target)?,
                audio_filter: transform::build_audio_filter(&normalized),
                samples: vec![normalized],
            })
        }
        None => {
            // 预览也摇一套强度 —— 预览看到的就是实际会跑出来的样子
            let strengths = transform::Strengths::fresh();
            let samples: Vec<TransformOptions> = (0..3)
                .map(|i| transform::normalize(&transform::preset_for(level, i, &strengths)))
                .collect();
            let first = samples[0].clone();
            Ok(TransformPreview {
                video_filter: transform::build_video_filter(&first, target)?,
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
    header_label: Option<HeaderLabel>,
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

    // **一次运行摇一套强度**（各项 115~125），这一批片段共用。
    // 真实软件也是这么做的：强度打一行日志说明，每片在此基础上
    // 再随机自己的去头尾/变速/B 帧。
    let strengths = transform::Strengths::fresh();

    // 给了 options 就所有片段用同一组；否则按序号生成确定性预设，每片各不相同
    //
    // 片头标识**叠加**在两者之上：它是一条合规开关，和"怎么去重"无关。
    // 放在这里而不是塞进预设，是为了让预设保持纯粹（好测、好复现）。
    let label = header_label.clone();
    let plan: Vec<(i64, String, String, TransformOptions)> = segments
        .iter()
        .enumerate()
        .map(|(index, seg)| {
            let mut opts = match &options {
                Some(raw) => transform::normalize(raw),
                None => transform::preset_for(level, index, &strengths),
            };
            if let Some(l) = &label {
                opts.header_label = Some(l.clone());
            }
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
    // **并行处理。**
    //
    // 原来是逐个跑，一部 60 集的剧要跑接近一小时。真实软件的运行日志显示
    // 它用 4 路并行，28 个文件 7 分钟就跑完 —— 差距主要在并行度，不在编码器。
    //
    // 编码仍是 CPU libx264（本来就是）：**消费级显卡的 NVENC 并发槽位有限**
    // （那边的日志实测「探测并发 3 路」），4 路并行时第 4 路要排队，
    // 反而退化成串行。
    let workers = transform::transform_concurrency();
    let results = tauri::async_runtime::spawn_blocking(move || {
        let queue = std::sync::Arc::new(std::sync::Mutex::new(plan));
        let acc: std::sync::Arc<std::sync::Mutex<Vec<(i64, String, i64, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        let mut handles = Vec::with_capacity(workers);
        for _ in 0..workers {
            let queue = std::sync::Arc::clone(&queue);
            let acc = std::sync::Arc::clone(&acc);
            let ff = ffmpeg_task.clone();
            handles.push(std::thread::spawn(move || loop {
                // 取一件就放开锁，别攥着它跑 ffmpeg
                let item = match queue.lock() {
                    Ok(mut q) => q.pop(),
                    Err(_) => return,
                };
                let Some((id, input, out, opts)) = item else { break };
                let res = match transform::apply(
                    &ff,
                    Path::new(&input),
                    Path::new(&out),
                    &opts,
                    target,
                ) {
                    Ok(size) => (id, out, size, String::new()),
                    Err(e) => (id, out, 0, e.to_string()),
                };
                if let Ok(mut a) = acc.lock() {
                    a.push(res);
                }
            }));
        }
        for h in handles {
            let _ = h.join();
        }
        match std::sync::Arc::try_unwrap(acc) {
            Ok(m) => m.into_inner().unwrap_or_default(),
            Err(_) => Vec::new(),
        }
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

    // 出片之后过一遍平台规范（≤500MB / ≥720p / ≥4Mbps / ≥30s）。
    // 这几条是上传被拒的常见原因，早说比传上去被退回来强。
    let spec_problems = {
        let ffprobe = lock_tools(&state)?.ffprobe.clone();
        if Path::new(&ffprobe).is_file() {
            results
                .iter()
                .filter(|(_, _, size, message)| message.is_empty() && *size > 0)
                .filter_map(|(_, out, _, _)| {
                    let problems = transform::check_spec(Path::new(&ffprobe), Path::new(out));
                    if problems.is_empty() {
                        None
                    } else {
                        Some(transform::SpecProblem {
                            file: out.clone(),
                            problems,
                        })
                    }
                })
                .collect()
        } else {
            Vec::new()
        }
    };

    let conn = lock_db(&state)?;
    Ok(TransformReport {
        total,
        processed,
        failed,
        errors,
        segments: db::list_segments(&conn, media_id)?,
        spec_problems,
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
        spec_problems: Vec::new(),
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
    // 保留参数以兼容旧调用；现在按素材时长固定抽 12 帧定位
    #[allow(unused)] sample_fps: Option<f64>,
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

/// 输出路径统一带上 .docx 后缀 —— 少了它 Word 不会把文件认成文档。
fn ensure_docx_extension(path: &Path) -> PathBuf {
    let is_docx = path
        .extension()
        .map(|e| e.eq_ignore_ascii_case("docx"))
        .unwrap_or(false);
    if is_docx {
        return path.to_path_buf();
    }
    let mut text = path.as_os_str().to_string_lossy().to_string();
    text.push_str(".docx");
    PathBuf::from(text)
}

fn docx_style() -> docx::DocStyle {
    docx::DocStyle {
        east_asian_font: "仿宋".to_string(),
        latin_font: "Times New Roman".to_string(),
        ..Default::default()
    }
}

fn write_proof_docx(
    kind: &str,
    blocks: Vec<docx::DocBlock>,
    title: &str,
    out_path: &str,
) -> AppResult<DocxReport> {
    let out = ensure_docx_extension(Path::new(out_path.trim()));
    if out.as_os_str().is_empty() {
        return Err(AppError::Msg("请先选择输出路径".into()));
    }
    let block_count = blocks.len() as i64;
    let meta = docx::DocMeta {
        title: title.to_string(),
        author: "DramaDist".to_string(),
        created: docx::now_iso(),
    };
    let size_bytes = docx::write_docx(&out, &blocks, &docx_style(), &meta)?;

    let mut warnings: Vec<String> = Vec::new();
    warnings.push(
        "文档只包含你录入的数据，请自行核对金额与权属表述后再对外提交。".to_string(),
    );
    if out_path.trim() != out.to_string_lossy() {
        warnings.push(format!("已自动补上 .docx 后缀：{}", out.to_string_lossy()));
    }

    Ok(DocxReport {
        out_path: out.to_string_lossy().to_string(),
        size_bytes,
        kind: kind.to_string(),
        block_count,
        warnings,
    })
}

/// 生成「成本明细表」docx。
#[tauri::command]
pub fn generate_cost_docx(request: CostDocRequest, out_path: String) -> AppResult<DocxReport> {
    if request.drama_name.trim().is_empty() {
        return Err(AppError::Msg("剧目名称不能为空".into()));
    }
    let rows: Vec<docx::CostRow> = request
        .rows
        .iter()
        .map(|r| docx::CostRow {
            item: r.item.clone(),
            quantity: r.quantity.clone(),
            unit_price: r.unit_price.clone(),
            amount: r.amount.clone(),
        })
        .collect();
    let blocks = docx::build_cost_blocks(
        request.drama_name.trim(),
        &rows,
        request.total.trim(),
        request.producer.trim(),
        request.date.trim(),
    );
    write_proof_docx(
        "cost",
        blocks,
        &format!("{} - 成本明细表", request.drama_name.trim()),
        &out_path,
    )
}

/// 本地硬字幕去除：抽帧 OCR 找位置 → `delogo` 盖掉。
///
/// 真实软件走的是开拍 `videoscreenclear` 在线服务。那条路要接口文档 + 额度 +
/// 网络；这条**全本地**，不依赖任何外部服务，也不用填坐标 —— 位置是扫出来的。
///
/// **找不到硬字幕就明确报出来**，不硬盖一块。盖错了比不盖更糟：
/// 观众看到画面中间糊一块，会以为片子坏了。
#[tauri::command]
pub async fn desubtitle_media(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    media_id: i64,
    out_path: Option<String>,
    // 保留参数以兼容旧调用；现在按素材时长固定抽 12 帧定位
    #[allow(unused)] sample_fps: Option<f64>,
) -> AppResult<DesubtitleReport> {
    let (ffmpeg, ffprobe, python, worker, input, media, out) = {
        let configured = lock_tools(&state)?.clone();
        let ffmpeg = tools::require(&configured.ffmpeg, "ffmpeg")?;
        // 探实际文件的尺寸用 —— **不能用素材记录里的**，见下面 subtitle_band 那段
        let ffprobe = configured.ffprobe.clone();
        let python = tools::require(&configured.python, "python")?;
        let conn = lock_db(&state)?;
        let media = db::get_media(&conn, media_id)?;
        let out = match out_path.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            Some(p) => PathBuf::from(p),
            None => {
                // 没给就放在源文件旁边：`第1集.mp4` → `第1集_quzimu.mp4`
                let stem = Path::new(&media.path)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| media.name.clone());
                Path::new(&media.path)
                    .parent()
                    .unwrap_or_else(|| Path::new("."))
                    .join(format!("{stem}_quzimu.mp4"))
            }
        };
        (
            ffmpeg,
            ffprobe,
            python,
            worker_script(&app, "ocr_worker.py"),
            // **取去重产物，不是原片。**
            //
            // 分镜证明是拿去**证明这条片子**的 —— 它该展示的是**实际发布
            // 出去的那个画面**，不是源片。用源片的话，证明里的画面和发出去
            // 的画面对不上（去重改过裁切、变速、调色，可能还有片头标识）。
            //
            // `media_source_path` 自己处理优先级：去重产物存在就用它，
            // 没有（没跑去重）才退回原片。
            PathBuf::from(db::media_source_path(&conn, media_id)?),
            media,
            out,
        )
    };

    if !input.is_file() {
        return Err(AppError::Msg(format!(
            "素材文件不存在：{}",
            input.to_string_lossy()
        )));
    }

    let mut opts = ocr::default_options();

    // **抽帧不要裁图。**
    //
    // `bottom_ratio` 是给「识别字幕」用的：让 ffmpeg 只抽下半部分，OCR 快一点。
    // 但**定位**要的是**全画面坐标** —— 裁过之后 OCR 返回的框是相对裁剪图的
    // （0~864），而 `subtitle_band` 按全画面判「在不在下方」
    // （要求 y ≥ 1920×0.55 = 1056），结果**所有框都被滤掉**，
    // 报「没找到硬字幕」—— 明明有。下方筛选在 `subtitle_band` 里自己做。
    opts.bottom_ratio = 1.0;

    // **少抽几帧。**
    //
    // 定位只需要知道「字幕大概在哪一行」，不必逐秒采样。
    // 原来按 0.5 帧/秒抽（82 秒 = 42 帧），而 RapidOCR 跑在 CPU 上，
    // 一帧几百毫秒 —— 4 路并发直接把 CPU 打满（用户原话「CPU 爆了」）。
    //
    // 12 帧足够：字幕位置在一集里是固定的，样本再多只是重复同一个结论。
    let duration_sec = (media.duration_ms as f64 / 1000.0).max(1.0);
    const LOCALIZE_FRAMES: f64 = 12.0;
    opts.sample_fps = (LOCALIZE_FRAMES / duration_sec).clamp(0.02, 0.5);
    opts.max_frames = LOCALIZE_FRAMES as usize;

    opts.min_confidence = 0.6;

    let frames_dir = std::env::temp_dir().join(format!("dramadist_desub_{media_id}"));
    let _ = std::fs::remove_dir_all(&frames_dir);
    std::fs::create_dir_all(&frames_dir)?;

    let frames = ocr::extract_frames(&ffmpeg, &input, &frames_dir, &opts)?;
    if frames.is_empty() {
        let _ = std::fs::remove_dir_all(&frames_dir);
        return Err(AppError::Msg("抽不出帧，无法定位字幕（素材可能损坏）".into()));
    }
    let sampled = frames.len() as i64;

    let (ocr_frames, engine) = ocr::run_worker(&python, &worker, &frames, &opts, ocr::default_timeout())?;
    let _ = std::fs::remove_dir_all(&frames_dir);

    // **尺寸要从实际处理的这个文件探，不能用素材记录里的。**
    //
    // 踩过一次：去字幕改成吃「去重产物」之后又抓不到字幕了。原因是
    // 去重产物被**缩到了 720p**（404×720），而素材记录里存的是源片的
    // 1080×1920 —— OCR 在 720p 上量出来的字幕框 y≈453，被判成
    // 「不在下半部分」（要求 y ≥ 1920×0.55 = 1056），全被滤掉。
    //
    // **手上正在处理哪个文件，就问哪个文件的尺寸。**
    let probe = media::probe(&PathBuf::from(&ffprobe), &input)?;
    let (fw, fh) = (probe.width, probe.height);
    if fw <= 0 || fh <= 0 {
        return Err(AppError::Msg("探不到画面尺寸，无法定位字幕".into()));
    }
    let band = crate::desubtitle::subtitle_band(&ocr_frames, fw, fh);
    let Some(band) = band else {
        // **找不到就说找不到。** 不硬盖一块 —— 那会毁掉画面。
        return Ok(DesubtitleReport {
            out_path: String::new(),
            band: None,
            sampled_frames: sampled,
            engine,
            removed: false,
            note: "没找到硬字幕（下半部分没扫到稳定的文字）。片源可能本来就没硬字幕，\
                   或者字幕位置比较特殊。"
                .to_string(),
        });
    };

    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let filter = crate::desubtitle::delogo_filter(&band);

    // **这一段用 GPU 编码。**
    //
    // 实测一集 82 秒的素材（1080×1920）：
    //
    //     libx264 CPU  57.8s      ← 原来
    //     h264_nvenc   17.1s      ← 快 3.4 倍
    //     解码+delogo   3.3s      ← 只占 6%
    //
    // 也就是**时间几乎全花在编码上**（54 秒），滤镜本身可以忽略。
    // 这和「去重」正好相反：去重的滤镜链很重（tmix 那些），所以那边
    // 特意用 CPU；去字幕只有 `delogo` 一个轻滤镜，瓶颈纯粹是编码。
    //
    // 探测用现成的 `pick_h264_encoder` —— 它**真的编一帧**来验，
    // 而不是看 `-encoders` 列表（列表里有 nvenc 不代表这台机器真有 N 卡）。
    let enc = media::pick_h264_encoder(&ffmpeg);

    let mut args: Vec<String> = vec![
        "-y".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-i".into(),
        input.to_string_lossy().to_string(),
        "-vf".into(),
        filter.clone(),
        "-c:v".into(),
        enc.name().to_string(),
    ];
    args.extend(enc.quality_args());
    args.extend(
        [
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "copy",
            "-movflags",
            "+faststart",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    args.push(out.to_string_lossy().to_string());
    let mut cmd = std::process::Command::new(&ffmpeg);
    proc::no_window(cmd.args(&args));
    let output = cmd
        .output()
        .map_err(|e| AppError::Msg(format!("调 ffmpeg 失败：{e}")))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(AppError::Msg(format!(
            "去字幕失败：{}",
            proc::tail(err.trim())
        )));
    }

    Ok(DesubtitleReport {
        out_path: out.to_string_lossy().to_string(),
        band: Some(band),
        sampled_frames: sampled,
        engine,
        removed: true,
        note: format!(
            "字幕带 {}×{} @ ({}, {})，扫了 {sampled} 帧命中 {} 帧",
            band.w, band.h, band.x, band.y, band.hit_frames
        ),
    })
}

/// 生成「成本配置比例情况报告」。
///
/// **版式照 `zhengming/成本图表.docx` 那份模板做**（用户指定的成本模板）：
/// 标题 + 一句提审说明 + 5 列表格 + 符合性声明 + 版权承诺 + 报审机构签字。
///
/// 和模板唯一的差别：**报审机构不预填公司名**。模板里印着模板作者的公司，
/// 照抄会把别人的主体信息打进用户的申报材料里 —— 那是很严重的事。
#[tauri::command]
pub fn generate_cost_report(
    rows: Vec<CostReportRow>,
    org: String,
    legal_person: String,
    date: String,
    out_path: String,
) -> AppResult<DocxReport> {
    if rows.is_empty() {
        return Err(AppError::Msg("至少要有一部剧才能出成本报告".into()));
    }
    for r in &rows {
        if r.name.trim().is_empty() {
            return Err(AppError::Msg("剧目名称不能为空".into()));
        }
        // 表单上那两句提示是**硬性要求**，超了就是不符合，早点说清楚
        if r.invest_wan > 100.0 {
            return Err(AppError::Msg(format!(
                "「{}」的总投资额 {} 万元超过 100 万 —— 这张表要求「需＜100万」",
                r.name, r.invest_wan
            )));
        }
        if r.actor_ratio > 40.0 {
            return Err(AppError::Msg(format!(
                "「{}」的演员总片酬占比 {}% 超过 40% —— 这张表要求「需＜40%」",
                r.name, r.actor_ratio
            )));
        }
    }

    let req = docx::CostReportRequest {
        rows: rows
            .iter()
            .map(|r| docx::CostReportRow {
                name: r.name.trim().to_string(),
                episodes: r.episodes,
                total_minutes: r.total_minutes,
                invest_wan: r.invest_wan,
                actor_ratio: r.actor_ratio,
            })
            .collect(),
        org,
        legal_person,
        date,
    };
    let blocks = docx::build_cost_report_blocks(&req);
    let block_count = blocks.len() as i64;
    let out = ensure_docx_extension(Path::new(out_path.trim()));
    if out.as_os_str().is_empty() {
        return Err(AppError::Msg("请先选择输出路径".into()));
    }
    let meta = docx::DocMeta {
        title: "成本配置比例情况报告".to_string(),
        author: "DramaDist".to_string(),
        created: docx::now_iso(),
    };
    // **用成本报告专用的字号**（正文 16pt），不是默认那套 12pt
    let size_bytes = docx::write_docx(&out, &blocks, &docx::cost_report_style(), &meta)?;

    let mut warnings = vec![
        "文档只包含你录入的数据，请自行核对金额、机构名称与权属表述后再对外提交。".to_string(),
        "报审机构、法定代表人、日期都留给你自己填 —— 这些不该由工具代写。".to_string(),
    ];
    if req.org.trim().is_empty() {
        warnings.push("报审机构还是空的，记得填上并盖章。".to_string());
    }
    Ok(DocxReport {
        kind: "cost_report".to_string(),
        out_path: out.to_string_lossy().to_string(),
        size_bytes,
        block_count,
        warnings,
    })
}

/// 生成「作品权属声明」docx。
#[tauri::command]
pub fn generate_ip_docx(request: IpDocRequest, out_path: String) -> AppResult<DocxReport> {
    if request.drama_name.trim().is_empty() {
        return Err(AppError::Msg("作品名称不能为空".into()));
    }
    if request.episode_count <= 0 {
        return Err(AppError::Msg("集数要大于 0".into()));
    }
    let blocks = docx::build_ip_blocks(
        request.drama_name.trim(),
        request.episode_count,
        request.total_duration.trim(),
        request.owner.trim(),
        request.date.trim(),
    );
    write_proof_docx(
        "ip",
        blocks,
        &format!("{} - 作品权属声明", request.drama_name.trim()),
        &out_path,
    )
}

// ---------------- 用户自备素材库 ----------------

/// 素材类型选项。程序**不内置任何素材**，全靠用户导入。
#[tauri::command]
pub fn asset_kinds() -> AppResult<Vec<AssetKindOption>> {
    Ok(asset::KINDS
        .iter()
        .map(|(key, label)| AssetKindOption {
            key: (*key).to_string(),
            label: (*label).to_string(),
        })
        .collect())
}

/// 扫描目录，列出可导入的素材（不写库）。
#[tauri::command]
pub fn scan_asset_dir(dir: String) -> AppResult<Vec<DiscoveredAsset>> {
    asset::scan_dir(Path::new(dir.trim()))
}

/// 导入素材。音频会读时长，LUT 会解析文件头并核对数据行数。
#[tauri::command]
pub fn import_assets(
    state: tauri::State<'_, AppState>,
    paths: Vec<String>,
    category: String,
) -> AppResult<AssetImportReport> {
    if paths.is_empty() {
        return Err(AppError::Msg("没有选择要导入的文件".into()));
    }
    let ffprobe = {
        let configured = lock_tools(&state)?.clone();
        configured.ffprobe
    };
    let category = category.trim().to_string();
    let mut imported = 0i64;
    let mut skipped: Vec<String> = Vec::new();
    let supported: Vec<String> = asset::KINDS
        .iter()
        .map(|(k, _)| format!("{}（{}）", k, asset::kind_label(k)))
        .collect();

    for raw in &paths {
        let path = PathBuf::from(raw.trim());
        let Some(kind) = asset::classify(&path) else {
            skipped.push(format!(
                "{}：不是支持的素材类型，只认 {}",
                raw,
                supported.join("、")
            ));
            continue;
        };
        if !path.is_file() {
            skipped.push(format!("{raw}：文件不存在"));
            continue;
        }

        let name = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "未命名".to_string());
        let size_bytes = std::fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(0);

        // 按类型取各自的元信息；取不到就跳过，不塞一条残缺记录进库
        let (duration_ms, meta) = if kind == "lut" {
            match asset::read_cube(&path) {
                Ok(info) => (0, serde_json::to_string(&info).unwrap_or_default()),
                Err(e) => {
                    skipped.push(format!("{}：{e}", raw));
                    continue;
                }
            }
        } else {
            match media::probe(Path::new(&ffprobe), &path) {
                Ok(info) if info.has_audio => (info.duration_ms, String::new()),
                Ok(_) => {
                    skipped.push(format!("{raw}：这个文件里没有音轨"));
                    continue;
                }
                Err(e) => {
                    skipped.push(format!("{raw}：读取音频信息失败（{e}）"));
                    continue;
                }
            }
        };

        let conn = lock_db(&state)?;
        db::upsert_asset(
            &conn,
            &Asset {
                id: 0,
                kind: kind.to_string(),
                name,
                path: raw.trim().to_string(),
                category: category.clone(),
                tags: String::new(),
                duration_ms,
                size_bytes,
                meta,
                note: String::new(),
                created_at: String::new(),
                updated_at: String::new(),
            },
        )?;
        imported += 1;
    }

    Ok(AssetImportReport {
        imported,
        skipped,
        summary: {
            let conn = lock_db(&state)?;
            db::asset_summary(&conn)?
        },
    })
}

#[tauri::command]
pub fn list_assets(
    state: tauri::State<'_, AppState>,
    kind: Option<String>,
) -> AppResult<Vec<Asset>> {
    let filter = kind.unwrap_or_default();
    let filter = filter.trim();
    // 类型写错时直接报错，而不是静默返回空列表让人以为"素材没了"
    if !filter.is_empty() && !asset::is_known_kind(filter) {
        return Err(AppError::Msg(format!(
            "不认识的素材类型：{filter}（支持 {}）",
            asset::KINDS.map(|(k, _)| k).join("/")
        )));
    }
    let conn = lock_db(&state)?;
    db::list_assets(&conn, if filter.is_empty() { None } else { Some(filter) })
}

#[tauri::command]
pub fn update_asset(
    state: tauri::State<'_, AppState>,
    id: i64,
    name: String,
    category: String,
    tags: String,
    note: String,
) -> AppResult<Asset> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Msg("素材名称不能为空".into()));
    }
    let conn = lock_db(&state)?;
    db::update_asset(&conn, id, &name, category.trim(), tags.trim(), note.trim())
}

/// 从库里移除记录。**不会删磁盘上的文件** —— 那是你自己的素材。
#[tauri::command]
pub fn delete_asset(state: tauri::State<'_, AppState>, id: i64) -> AppResult<()> {
    let conn = lock_db(&state)?;
    db::delete_asset(&conn, id)
}

#[tauri::command]
pub fn asset_summary(state: tauri::State<'_, AppState>) -> AppResult<AssetSummary> {
    let conn = lock_db(&state)?;
    db::asset_summary(&conn)
}

/// 给某个素材配 BGM / 音效，产出 `<原名>_bgm.mp4`。
#[tauri::command]
pub async fn apply_bgm(
    state: tauri::State<'_, AppState>,
    media_id: i64,
    asset_id: i64,
    mode: String,
    bgm_volume: Option<f64>,
    original_volume: Option<f64>,
    loop_bgm: Option<bool>,
) -> AppResult<AssetApplyReport> {
    let (ffmpeg, ffprobe, video, audio, probe) = {
        let configured = lock_tools(&state)?.clone();
        let conn = lock_db(&state)?;
        let media = db::get_media(&conn, media_id)?;
        let record = db::get_asset(&conn, asset_id)?;
        if record.kind != "audio" {
            return Err(AppError::Msg(format!(
                "「{}」不是音频素材，不能当 BGM 用",
                record.name
            )));
        }
        (
            tools::require(&configured.ffmpeg, "ffmpeg")?,
            tools::require(&configured.ffprobe, "ffprobe")?,
            PathBuf::from(media.path),
            PathBuf::from(record.path),
            MediaProbe {
                duration_ms: media.duration_ms,
                width: media.width,
                height: media.height,
                has_audio: media.has_audio,
                size_bytes: media.size_bytes,
                format_name: String::new(),
            },
        )
    };

    let out = asset::output_path(&video, "bgm");
    let mode = asset::parse_bgm_mode(&mode);
    let bgm_volume = bgm_volume.unwrap_or(0.35);
    let original_volume = original_volume.unwrap_or(1.0);
    let loop_bgm = loop_bgm.unwrap_or(false);

    tauri::async_runtime::spawn_blocking(move || {
        asset::apply_bgm(
            &ffmpeg,
            &ffprobe,
            &video,
            &audio,
            &out,
            mode,
            bgm_volume,
            original_volume,
            loop_bgm,
            Some(&probe),
        )
    })
    .await
    .map_err(|e| AppError::Msg(format!("配 BGM 线程异常终止：{e}")))?
}

/// 给某个素材套 LUT，产出 `<原名>_lut.mp4`。
#[tauri::command]
pub async fn apply_lut(
    state: tauri::State<'_, AppState>,
    media_id: i64,
    asset_id: i64,
    intensity: Option<f64>,
) -> AppResult<AssetApplyReport> {
    let (ffmpeg, video, lut) = {
        let configured = lock_tools(&state)?.clone();
        let ffmpeg = tools::require(&configured.ffmpeg, "ffmpeg")?;
        let conn = lock_db(&state)?;
        let media = db::get_media(&conn, media_id)?;
        let record = db::get_asset(&conn, asset_id)?;
        if record.kind != "lut" {
            return Err(AppError::Msg(format!(
                "「{}」不是 LUT 素材",
                record.name
            )));
        }
        (
            ffmpeg,
            PathBuf::from(media.path),
            PathBuf::from(record.path),
        )
    };

    let out = asset::output_path(&video, "lut");
    let intensity = intensity.unwrap_or(1.0);

    tauri::async_runtime::spawn_blocking(move || {
        asset::apply_lut(&ffmpeg, &video, &lut, &out, intensity)
    })
    .await
    .map_err(|e| AppError::Msg(format!("套用 LUT 线程异常终止：{e}")))?
}

// ---------------- 证明材料 ----------------

/// 一份起步布局：三个最常用字段 + 本机能找到的中文字体。
#[tauri::command]
pub fn proof_default_layout() -> AppResult<ProofLayout> {
    Ok(proof::default_layout())
}

#[tauri::command]
pub fn proof_load_layout(path: String) -> AppResult<ProofLayout> {
    proof::load_layout(Path::new(path.trim()))
}

#[tauri::command]
pub fn proof_save_layout(path: String, layout: ProofLayout) -> AppResult<()> {
    proof::save_layout(Path::new(path.trim()), &layout)
}

/// 只算不跑：先看清每个字段会怎么排、有没有溢出，再决定要不要生成。
#[tauri::command]
pub fn proof_preview(
    layout: ProofLayout,
    values: HashMap<String, String>,
) -> AppResult<ProofPreview> {
    Ok(proof::preview(&layout, &values))
}

#[tauri::command]
pub async fn proof_render(
    state: tauri::State<'_, AppState>,
    layout: ProofLayout,
    values: HashMap<String, String>,
    out_path: String,
) -> AppResult<ProofRenderReport> {
    let ffmpeg = {
        let configured = lock_tools(&state)?.clone();
        tools::require(&configured.ffmpeg, "ffmpeg")?
    };
    let out = PathBuf::from(out_path.trim());
    if out.as_os_str().is_empty() {
        return Err(AppError::Msg("请先选择输出路径".into()));
    }
    tauri::async_runtime::spawn_blocking(move || proof::render(&ffmpeg, &layout, &values, &out))
        .await
        .map_err(|e| AppError::Msg(format!("渲染线程异常终止：{e}")))?
}

/// 本地已经内置了哪些模型规格。
///
/// 界面据此标注「已内置 / 需联网下载」—— 让用户选一个要下载的规格、
/// 而网络又连不上（HuggingFace 在这类网络下常常超时），是最糟的体验。
#[tauri::command]
pub fn asr_models() -> AppResult<Vec<String>> {
    Ok(asr::bundled_model_sizes())
}

// ---------------- 云端字幕（用户自备 Key） ----------------

#[tauri::command]
pub fn cloud_config(state: tauri::State<'_, AppState>) -> AppResult<CloudConfigView> {
    let conn = lock_db(&state)?;
    let cfg = cloud::load_config(&conn)?;
    Ok(cloud::view(&cfg))
}

/// 保存云端配置。`api_key` 传 `null` 表示不改动已有 Key（前端不该拿到原 Key）。
#[tauri::command]
pub fn save_cloud_config(
    state: tauri::State<'_, AppState>,
    base_url: String,
    api_key: Option<String>,
    model: String,
    language: String,
) -> AppResult<CloudConfigView> {
    let conn = lock_db(&state)?;
    let cfg = cloud::save_config(&conn, &base_url, api_key.as_deref(), &model, &language)?;
    Ok(cloud::view(&cfg))
}

/// 连通性自检：地址、鉴权、模型名对不对，一次问清楚。
#[tauri::command]
pub async fn cloud_test(state: tauri::State<'_, AppState>) -> AppResult<CloudTestResult> {
    let cfg = {
        let conn = lock_db(&state)?;
        cloud::load_config(&conn)?
    };
    let endpoint = cloud::transcribe_endpoint(&cfg.base_url);
    Ok(match cloud::test_connection(&cfg).await {
        Ok(detail) => CloudTestResult {
            ok: true,
            endpoint,
            detail,
        },
        Err(e) => CloudTestResult {
            ok: false,
            endpoint,
            detail: e.to_string(),
        },
    })
}

/// 用云端服务转写某个素材，结果覆盖到它的字幕上。
#[tauri::command]
pub async fn cloud_transcribe(
    state: tauri::State<'_, AppState>,
    media_id: i64,
    overwrite: Option<bool>,
) -> AppResult<CloudTranscribeReport> {
    let overwrite = overwrite.unwrap_or(false);

    let (ffmpeg, cfg, input, existing) = {
        let configured = lock_tools(&state)?.clone();
        let ffmpeg = tools::require(&configured.ffmpeg, "ffmpeg")?;
        let conn = lock_db(&state)?;
        let media = db::get_media(&conn, media_id)?;
        let cfg = cloud::load_config(&conn)?;
        let existing = db::list_cues(&conn, media_id)?.len() as i64;
        (ffmpeg, cfg, media.path, existing)
    };

    if existing > 0 && !overwrite {
        return Err(AppError::Msg(format!(
            "这个素材已经有 {existing} 条字幕了。确认要覆盖的话，勾选「覆盖现有字幕」再执行。"
        )));
    }

    // 1) 抽音频（本地，快）
    let audio = cloud::temp_audio_path(media_id);
    let ffmpeg_task = ffmpeg.clone();
    let input_task = input.clone();
    let audio_task = audio.clone();
    let audio_bytes = tauri::async_runtime::spawn_blocking(move || {
        cloud::extract_audio(&ffmpeg_task, Path::new(&input_task), &audio_task)
    })
    .await
    .map_err(|e| AppError::Msg(format!("抽音频线程异常终止：{e}")))??;

    // 2) 上传转写
    let endpoint = cloud::transcribe_endpoint(&cfg.base_url);
    let response = cloud::transcribe(&cfg, &audio).await;
    // 中间产物无论成败都清掉
    let _ = std::fs::remove_file(&audio);
    let response = response?;

    let cues = cloud::to_cues(&response, media_id);
    if cues.is_empty() {
        return Err(AppError::Msg(
            "云端跑完了但没有返回任何字幕。确认素材里有人声，或换一个模型再试。".into(),
        ));
    }

    let cue_count = {
        let mut conn = lock_db(&state)?;
        db::replace_cues(&mut conn, media_id, &cues)?
    };

    Ok(CloudTranscribeReport {
        cue_count,
        language: response.language.clone(),
        duration_ms: (response.duration * 1000.0).round() as i64,
        endpoint,
        audio_bytes,
        warnings: vec![
            "准确率与断句由服务商模型决定，时间轴可能需要人工校对。".to_string(),
            "上传的是抽取出的音频，不是整个视频文件。".to_string(),
        ],
    })
}

// ---------------- 流水线 ----------------

/// 流水线能做哪些步骤（界面据此渲染勾选项）
#[tauri::command]
pub fn pipeline_stages() -> AppResult<Vec<PipelineStage>> {
    Ok(pipeline::stage_list())
}

/// 扫描源目录：一个目录进去，一批剧目出来。
///
/// 这是整条流水线的入口 —— 指一个目录，剩下的交给后面的步骤。
#[tauri::command]
pub fn pipeline_scan(root: String) -> AppResult<SourceScan> {
    let root = root.trim();
    if root.is_empty() {
        return Err(AppError::Msg("请先选择源目录".into()));
    }
    pipeline::scan_source(Path::new(root))
}

/// 默认导出目录：**源目录里面的 `<源目录名>二创`**，一部剧的东西放在一个文件夹里。
#[tauri::command]
pub fn pipeline_default_out_dir(root: String) -> AppResult<String> {
    if root.trim().is_empty() {
        return Err(AppError::Msg("请先选择源目录".into()));
    }
    Ok(pipeline::default_out_dir(Path::new(root.trim()), "二创")
        .to_string_lossy()
        .to_string())
}

/// 开始一条流水线。
///
/// 做四件事：
/// 1. 扫描源目录（快）
/// 2. **如果勾了「AI 改剧名简介」，先调 AI 拿到新剧名** ——
///    必须在建目录、排任务**之前**做完，否则输出路径就得事后改名
/// 3. 建工程、导入素材
/// 4. 按步骤顺序把任务排进队列
///
/// 顺序很关键：真实软件是「AI重写简介 → AI重写剧名 → 整剧重组 → …」，
/// 前两步跑完，后面所有步骤用的都是新剧名。我一开始把「改剧名」做成了
/// 队列里的一个任务 —— 而任务是**一次性全部排好**的，排的时候还不知道新名字，
/// 结果就得事后改目录名、改 payload 里的路径，绕了一大圈。
#[tauri::command]
pub async fn pipeline_start(
    state: tauri::State<'_, AppState>,
    root: String,
    out_dir: String,
    stages: Vec<String>,
    slice_seconds: Option<f64>,
    model: Option<String>,
    header_label: Option<HeaderLabel>,
) -> AppResult<PipelineRun> {
    let root_path = PathBuf::from(root.trim());
    if root_path.as_os_str().is_empty() {
        return Err(AppError::Msg("请先选择源目录".into()));
    }

    let enabled: Vec<String> = stages
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if enabled.is_empty() {
        return Err(AppError::Msg("至少要勾选一个步骤".into()));
    }
    for key in &enabled {
        if !pipeline::is_known_stage(key) {
            return Err(AppError::Msg(format!("不认识的步骤：{key}")));
        }
    }

    let out_root = if out_dir.trim().is_empty() {
        pipeline::default_out_dir(&root_path, "二创")
    } else {
        PathBuf::from(out_dir.trim())
    };
    let slice_ms = ((slice_seconds.unwrap_or(40.0)).clamp(3.0, 600.0) * 1000.0).round() as i64;
    let asr_model = model.unwrap_or_default();

    let ffprobe = {
        let configured = lock_tools(&state)?.clone();
        tools::require(&configured.ffprobe, "ffprobe")?
    };

    // ---- 第 2 步：先在扫描结果上把剧名改好 ----
    //
    // 这一步**不持数据库锁**（要等网络），所以放在 lock_db 之前。
    // 但它写的日志属于「这次运行」，而 run 记录还没建 —— 所以先划一条线，
    // 等 run 建好之后再把线之后的日志挂上去（attach_recent_logs_to_run）。
    // 不挂的话，用户按运行过滤时看不到「改剧名失败」，像什么都没发生。
    let log_mark = {
        let conn = lock_db(&state)?;
        db::max_log_id(&conn)?
    };
    let mut renames: std::collections::HashMap<String, pipeline::NewName> =
        std::collections::HashMap::new();
    if enabled.iter().any(|s| s == "rewrite") {
        let scan = pipeline::scan_source(&root_path)?;
        let provider = {
            let conn = lock_db(&state)?;
            ai::usable_provider(&conn)?
        };
        match provider {
            None => {
                log_line(
                    &state,
                    "warn",
                    "勾了「AI 改剧名简介」但没有可用的 AI 服务（或用途开关没开），本次跳过改剧名",
                );
            }
            Some(p) => {
                for drama in &scan.dramas {
                    if drama.synopsis.trim().is_empty() {
                        log_line(
                            &state,
                            "warn",
                            &format!("「{}」没找到简介，跳过改剧名", drama.name),
                        );
                        continue;
                    }

                    // **每次跑都重新起名，不沿用上次的。**
                    //
                    // 用户说「ai生成的文件名不要固定呀」—— 对，AI 起名的价值
                    // 就在多样性，固定下来就没意义了。
                    //
                    // （我一度改成沿用，是为了避免导出目录越跑越多 ——
                    //  那是**用错的办法解决对的问题**。目录重复该在目录层面
                    //  解决：新名字定了之后，把上一次的目录改名，见下面。）
                    //
                    // **失败不中断整条流水线** —— 用原剧名继续跑，
                    // 总比因为一次网络抖动就什么都出不来强。
                    match ai::rewrite_title_and_synopsis(&p, &drama.name, &drama.synopsis).await {
                        Ok((new_title, new_synopsis)) => {
                            // **每次跑都新建一个剧目录，旧的不动。**
                            //
                            // 用户要的：AI 起了新剧名，就在「二创」下**新建**一个
                            // 以新剧名命名的文件夹，**保留之前生成的，不覆盖任何东西**。
                            //
                            // 之前我做反了 —— 把上一次的目录改名成新剧名，
                            // 结果旧产出被「搬走」、`jy\` 里几轮的草稿混在一起，
                            // 看着像数据错乱。

                            log_line(
                                &state,
                                "ok",
                                &format!(
                                    "改剧名：「{}」→「{new_title}」（{} 字简介）",
                                    drama.name,
                                    new_synopsis.chars().count()
                                ),
                            );
                            renames.insert(
                                drama.name.clone(),
                                pipeline::NewName {
                                    title: new_title,
                                    synopsis: new_synopsis,
                                },
                            );
                        }
                        Err(e) => {
                            log_line(
                                &state,
                                "warn",
                                &format!("「{}」改剧名失败，用原名继续：{e}", drama.name),
                            );
                        }
                    }
                }
            }
        }
    }

    let conn = lock_db(&state)?;
    // 整剧重组：把碎片拼成 60~90 秒的集。**在导入素材之前跑** ——
    // 重组产物才是后面所有步骤的输入。放到队列里做的话，素材早按原片段
    // 导进去了，重组出来也没人用。
    let regroup = if enabled.iter().any(|s| s == "regroup") {
        let ffmpeg = {
            let configured = lock_tools(&state)?.clone();
            tools::require(&configured.ffmpeg, "ffmpeg")?
        };
        Some(pipeline::RegroupOptions {
            ffmpeg,
            // 75 秒落在真实软件的 60~90 秒区间中间
            target_ms: 75_000,
        })
    } else {
        None
    };
    let outcome = pipeline::start_pipeline(
        &conn,
        &ffprobe,
        &root_path,
        &out_root,
        &enabled,
        slice_ms,
        &asr_model,
        header_label,
        &renames,
        regroup.as_ref(),
    )?;

    // 把开始前写的那几条日志（扫描、改剧名成没成）挂到这次运行上。
    // 用户按运行过滤时才看得到「改剧名失败了」。
    let _ = db::attach_recent_logs_to_run(&conn, outcome.run_id, log_mark);

    if !outcome.skipped.is_empty() {
        // 读不出来的文件不能悄悄咽掉 —— 但要让它可查，而不是弹一堆错误
        eprintln!(
            "DramaDist: 流水线有 {} 个文件没能导入：{}",
            outcome.skipped.len(),
            outcome.skipped.join("；")
        );
    }
    // 「一集都没用上」才算失败。判断逻辑在 StartOutcome 上（可测）。
    if outcome.imported_nothing() {
        let why = outcome
            .skipped
            .first()
            .cloned()
            .unwrap_or_else(|| "源目录里没有可用的视频".to_string());
        return Err(AppError::Msg(format!("一部剧的素材都没能导入。{why}")));
    }

    db::get_pipeline_run(&conn, outcome.run_id)?
        .ok_or_else(|| AppError::Msg("流水线建好了却读不出来".into()))
}

#[tauri::command]
pub fn pipeline_list(state: tauri::State<'_, AppState>, limit: Option<i64>) -> AppResult<Vec<PipelineRun>> {
    let conn = lock_db(&state)?;
    db::list_pipeline_runs(&conn, limit.unwrap_or(20))
}

#[tauri::command]
pub fn pipeline_get(state: tauri::State<'_, AppState>, id: i64) -> AppResult<Option<PipelineRun>> {
    let conn = lock_db(&state)?;
    db::get_pipeline_run(&conn, id)
}

/// 一次流水线跑出来了什么 —— 供「看效果」用。
///
/// 产出必须能当场看：光把文件写到磁盘上，用户还得自己去资源管理器里翻，
/// 那和没做完差不多。
#[tauri::command]
pub fn pipeline_outputs(
    state: tauri::State<'_, AppState>,
    run_id: i64,
) -> AppResult<Vec<OutputEpisode>> {
    let conn = lock_db(&state)?;
    db::run_outputs(&conn, run_id)
}

/// 用系统默认播放器打开这个文件。
#[tauri::command]
pub fn open_in_player(path: String) -> AppResult<()> {
    let p = PathBuf::from(path.trim());
    if !p.is_file() {
        return Err(AppError::Msg(format!("文件不在了：{}", p.to_string_lossy())));
    }
    proc::open_with_system(&p)
}

/// 在资源管理器里定位这个文件（选中它）。
#[tauri::command]
pub fn reveal_in_explorer(path: String) -> AppResult<()> {
    let p = PathBuf::from(path.trim());
    if !p.exists() {
        return Err(AppError::Msg(format!("文件不在了：{}", p.to_string_lossy())));
    }
    proc::reveal_in_explorer(&p)
}

/// 取消一条流水线：把它所有还没跑的任务撤掉。
#[tauri::command]
pub fn pipeline_cancel(state: tauri::State<'_, AppState>, id: i64) -> AppResult<i64> {
    let conn = lock_db(&state)?;
    let dramas = db::list_pipeline_dramas(&conn, id)?;
    let mut canceled = 0i64;
    for drama in dramas {
        canceled += db::cancel_jobs_of_drama(&conn, drama.id)?;
        if drama.status != "done" {
            db::update_pipeline_drama(&conn, drama.id, &drama.stage, "skipped", "已取消")?;
        }
    }
    db::set_pipeline_run_status(&conn, id, db::RUN_CANCELED)?;
    Ok(canceled)
}

// ---------------- AI 服务配置 ----------------

/// 读取界面上记住的东西（源目录、勾了哪些步骤…）。
///
/// 用户说：「不保留 我每次都要从选择 好麻烦呀」——对。
/// 每次打开都要重选一遍源目录、重勾一遍步骤，是很烦。
///
/// 存在 `setting` 表里（不是 localStorage）：跟着库走，
/// 换台机器复制库过去，习惯也还在；出问题能用 sqlite 直接看。
#[tauri::command]
pub fn load_ui_state(
    state: tauri::State<'_, AppState>,
    key: String,
) -> AppResult<Option<String>> {
    let conn = lock_db(&state)?;
    db::get_setting(&conn, &format!("ui.{key}"))
}

/// 记住界面上的一些东西。失败不抛错 —— 记不住设置不该让页面卡住。
#[tauri::command]
pub fn save_ui_state(
    state: tauri::State<'_, AppState>,
    key: String,
    value: String,
) -> AppResult<()> {
    // 单个键值不该无限膨胀（有人手动塞了个巨大 JSON 会拖慢启动）
    if value.len() > 64 * 1024 {
        return Err(AppError::Msg("要记的东西太大了".into()));
    }
    let conn = lock_db(&state)?;
    db::set_setting(&conn, &format!("ui.{key}"), &value)
}

/// 读 AI 服务配置。**API Key 一律打码** —— 真 Key 不出后端。
#[tauri::command]
pub fn ai_config(state: tauri::State<'_, AppState>) -> AppResult<AiConfigView> {
    let conn = lock_db(&state)?;
    ai::view(&conn)
}

/// 保存 AI 服务配置。
///
/// Key 留空 = 不改动原来的（界面上显示的是打码值，用户没动它就不该覆盖）。
#[tauri::command]
pub fn save_ai_config(
    state: tauri::State<'_, AppState>,
    providers: Vec<AiProvider>,
    active: String,
    usage: AiUsage,
) -> AppResult<AiConfigView> {
    ai::validate(&providers, &active)?;
    let conn = lock_db(&state)?;
    ai::apply_view(&conn, &providers, &active, &usage)?;
    ai::view(&conn)
}

/// 测试某个服务商通不通。
///
/// **只做一次廉价的探测请求，不触发任何生成，不产生费用。**
#[tauri::command]
pub async fn ai_test(
    state: tauri::State<'_, AppState>,
    provider_id: String,
) -> AppResult<AiTestResult> {
    let provider = {
        let conn = lock_db(&state)?;
        ai::load_providers(&conn)?
            .into_iter()
            .find(|p| p.id == provider_id)
            .ok_or_else(|| AppError::Msg(format!("没有这个服务商：{provider_id}")))?
    };
    Ok(ai::test_provider(&provider).await)
}

/// 把某个服务商的 Base URL / 模型恢复成内置预设。
///
/// 用户把字段清空或者填乱了之后，不用去翻文档抄。
/// **不动 API Key** —— 那不是预设能决定的东西。
#[tauri::command]
pub fn ai_reset_preset(
    state: tauri::State<'_, AppState>,
    provider_id: String,
) -> AppResult<AiProviderView> {
    let conn = lock_db(&state)?;
    let mut providers = ai::load_providers(&conn)?;
    let target = providers
        .iter_mut()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| AppError::Msg(format!("没有这个服务商：{provider_id}")))?;
    ai::apply_preset(target);
    // 预设里 Base URL 为空的那个（自定义），恢复后保持为空，不编造
    ai::save_providers(&conn, &providers)?;
    Ok(ai::view(&conn)?
        .providers
        .into_iter()
        .find(|p| p.id == provider_id)
        .ok_or_else(|| AppError::Msg("刚保存的服务商不见了".into()))?)
}

/// 往运行日志里写一行（没有 run 归属的那种，比如"开始前"的提示）。
///
/// 拿不到锁就静默跳过 —— 日志写不进去不该让主流程挂掉。
fn log_line(state: &tauri::State<'_, AppState>, level: &str, message: &str) {
    if let Ok(conn) = lock_db(state) {
        db::append_log(&conn, None, None, level, message);
    }
}
/// 列出账号能看到的模型。
///
/// 直接用 `GET {base}/models`。为什么值得做：用户填的模型名常常是错的
/// （写 `Doubao-Seedream-5.0-flash`，真名是 `doubao-seedream-5-0-260128`），
/// 或者账号根本没开通 —— 与其让他一次次试错，不如把列表摆出来让他选。
#[tauri::command]
pub async fn ai_list_models(
    state: tauri::State<'_, AppState>,
    provider_id: String,
) -> AppResult<Vec<ai::ModelInfo>> {
    let provider = {
        let conn = lock_db(&state)?;
        ai::load_providers(&conn)?
            .into_iter()
            .find(|p| p.id == provider_id)
            .ok_or_else(|| AppError::Msg(format!("没有这个服务商：{provider_id}")))?
    };
    ai::list_models(&provider).await
}
// ---------------- AI 改写剧名 / 简介 ----------------

/// 改写剧名与简介。
///
/// 顺序和真实软件一致：**先改简介，再按新简介起新剧名** ——
/// 反过来的话，剧名是根据旧简介起的，两者会对不上。
///
/// 结果写进 `out_dir` 的 `剧名.txt` / `详细简介.txt`，
/// 并追加一行到 `处理过的数据.txt`（台账，避免同一部剧重复做）。
#[tauri::command]
pub async fn ai_rewrite(
    state: tauri::State<'_, AppState>,
    title: String,
    synopsis: String,
    out_dir: Option<String>,
) -> AppResult<RewriteOutcome> {
    let provider = {
        let conn = lock_db(&state)?;
        ai::usable_provider(&conn)?
            .ok_or_else(|| AppError::Msg("还没有可用的 AI 服务，先到「AI 服务」页配好".into()))?
    };

    let original_title = title.trim().to_string();
    let original_synopsis = synopsis.trim().to_string();
    if original_synopsis.is_empty() {
        return Err(AppError::Msg(
            "没有原简介 —— 改写要先有素材。源目录里放一个 intro.txt 或简介.txt".into(),
        ));
    }

    // 第一步：改简介
    let (sys1, user1) = ai::synopsis_prompt(&original_title, &original_synopsis);
    let new_synopsis = ai::clean_synopsis(&ai::chat(&provider, &sys1, &user1).await?);
    if new_synopsis.is_empty() {
        return Err(AppError::Msg("模型返回了空简介，没改成".into()));
    }

    // 第二步：按**新简介**起剧名
    let (sys2, user2) = ai::title_prompt(&new_synopsis);
    let new_title = ai::clean_title(&ai::chat(&provider, &sys2, &user2).await?);
    if new_title.is_empty() {
        return Err(AppError::Msg("模型返回了空剧名，没改成".into()));
    }

    // 落盘 + 记台账
    let mut saved: Vec<String> = Vec::new();
    if let Some(dir) = out_dir.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppError::Msg(format!("建不了输出目录：{e}")))?;
        let title_file = dir.join("剧名.txt");
        let syn_file = dir.join("详细简介.txt");
        std::fs::write(&title_file, &new_title)
            .map_err(|e| AppError::Msg(format!("写剧名失败：{e}")))?;
        std::fs::write(&syn_file, &new_synopsis)
            .map_err(|e| AppError::Msg(format!("写简介失败：{e}")))?;
        saved.push(title_file.to_string_lossy().to_string());
        saved.push(syn_file.to_string_lossy().to_string());

        // 台账：一行一部剧，重复做的时候能查
        let ledger = dir.join("处理过的数据.txt");
        let line = format!(
            "{}\t原剧名={}\t新剧名={}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
            original_title,
            new_title
        );
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&ledger)
        {
            use std::io::Write;
            let _ = f.write_all(line.as_bytes());
            saved.push(ledger.to_string_lossy().to_string());
        }
    }

    Ok(RewriteOutcome {
        original_title,
        new_title,
        original_synopsis,
        new_synopsis,
        provider: provider.label,
        saved,
    })
}

/// 对**整集**做画面处理（去重）。
///
/// 真实软件的流程是「**先去重整集、再切片**」：去重产物（它叫 `yiquchong`）
/// 才是发布用的东西，切片只是它的下游（做证明图用）。
///
/// 我原来是反的 —— 先切再对每一片去重。同一部剧要多跑十几倍的去重，
/// 而且去重后的切片还得再拼回去。改过来之后：
/// - 去重次数 = 集数（61 次），不是片数（150+ 次）
/// - 去重产物是一个完整文件，直接就能发
/// - 切片时自动读这个产物（`media_source_path`），不用手动传
#[tauri::command]
pub async fn dedup_media(
    state: tauri::State<'_, AppState>,
    media_id: i64,
    intensity: Option<String>,
    header_label: Option<HeaderLabel>,
    out_dir: Option<String>,
    // 产出文件的名字前缀（新剧名-第NN集）。空串就退回素材名。
    prefix: String,
    // 强度种子。同一个种子结果相同 —— 队列把「这次运行」的种子传进来，
    // 一部剧里每集共享一套强度。0 表示用当前时间（每次都不一样）。
    seed: u64,
) -> AppResult<DedupReport> {
    let (ffmpeg, ffprobe, media) = {
        let configured = lock_tools(&state)?.clone();
        let ffmpeg = tools::require(&configured.ffmpeg, "ffmpeg")?;
        let conn = lock_db(&state)?;
        let media = db::get_media(&conn, media_id)?;
        (ffmpeg, configured.ffprobe, media)
    };

    let input = PathBuf::from(&media.path);
    if !input.is_file() {
        return Err(AppError::Msg(format!("原片不在了：{}", input.to_string_lossy())));
    }

    let out = match out_dir.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            std::fs::create_dir_all(&dir)
                .map_err(|e| AppError::Msg(format!("建不了输出目录：{e}")))?;
            // **文件名用新剧名**（用户要的），没给前缀才退回素材名
            let stem = if prefix.trim().is_empty() {
                media.name.trim_end_matches(".mp4").to_string()
            } else {
                prefix.trim().to_string()
            };
            dir.join(format!("{stem}_去重.mp4"))
        }
        None => input.with_file_name(format!(
            "{}_去重.mp4",
            input
                .file_stem()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default()
        )),
    };

    let level = transform::Intensity::parse(intensity.as_deref().unwrap_or("strong"));
    let strengths = if seed == 0 {
        transform::Strengths::fresh()
    } else {
        transform::Strengths::from_seed(seed)
    };
    let mut opts = transform::preset_for(level, media_id as usize, &strengths);
    opts.header_label = header_label;
    // 先把「改了什么」记下来 —— opts 马上要 move 进闭包
    let changes = transform::describe(&opts);

    let target = if media.width > 0 && media.height > 0 {
        Some((media.width as u32, media.height as u32))
    } else {
        None
    };

    let id = media.id;
    let path_for_task = out.clone();
    let size = tauri::async_runtime::spawn_blocking(move || {
        transform::apply(&ffmpeg, &input, &path_for_task, &opts, target)
    })
    .await
    .map_err(|e| AppError::Msg(format!("去重线程异常终止：{e}")))??;

    // 记下来：切片时优先读它
    {
        let conn = lock_db(&state)?;
        db::set_media_processed(&conn, id, &out.to_string_lossy())?;
    }

    let spec_problems = if Path::new(&ffprobe).is_file() {
        let problems = transform::check_spec(Path::new(&ffprobe), &out);
        if problems.is_empty() {
            Vec::new()
        } else {
            vec![transform::SpecProblem {
                file: out.to_string_lossy().to_string(),
                problems,
            }]
        }
    } else {
        Vec::new()
    };

    Ok(DedupReport {
        media_id: id,
        out_path: out.to_string_lossy().to_string(),
        size_bytes: size,
        changes,
        spec_problems,
    })
}

// ---------------- 分镜证明 ----------------

/// 从切片抽帧 → 反推分镜 → 拼成证明图。
///
/// 照真实软件的做法：每集抽若干帧，交给**视觉模型**反推出"这一镜拍了什么"，
/// 再把若干镜头拼进一张图，就是报备用的分镜证明。
///
/// 没有配 AI 服务时**退回无描述版**：只把抽出来的帧拼成图 ——
/// 证明"这些画面确实存在"这一点仍然成立，只是没有文字说明。
/// 排一个注定失败的任务不如给一个能用的降级产物。
/// 把剧名变成合法的文件名片段（去掉路径分隔符和 Windows 不允许的字符）。
fn safe_file_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    let t = cleaned.trim().trim_end_matches('.').to_string();
    if t.is_empty() {
        "未命名".to_string()
    } else {
        t
    }
}

/// 秒 → `MM:SS`（分镜时间段显示用）。
fn mmss(sec: f64) -> String {
    let s = sec.max(0.0) as i64;
    format!("{:02}:{:02}", s / 60, s % 60)
}

#[tauri::command]
pub async fn build_storyboard(
    state: tauri::State<'_, AppState>,
    media_ids: Vec<i64>,
    out_dir: String,
    _frames_per_episode: i64,
    _shots_per_sheet: i64,
) -> AppResult<StoryboardReport> {
    //! 分镜证明：取 4 集去字幕产物 → 切镜头 → 每镜头抽 5 帧 → AI 反推
    //! → 每 2 镜头排 1 页 JPG，同时写一份 Seedance 可用的 JSON。
    //!
    //! **算法照真实软件的日志**：
    //!
    //! ```text
    //! 工程图 AI 辅助用片（AI去字幕后同一批）：第1集_quzimu.mp4 … 第4集_quzimu.mp4
    //! 共 4 张图 × 每张 2 镜头（共 8 镜头，Seedance）
    //! Ark 语言模型[分镜反推]：（附图 5 张）
    //!   镜头01 反推 OK（第1集_quzimu.mp4 00:00–00:42）
    //!   …
    //! → 第1张（镜头01+02）…_分镜证明_01.jpg
    //! ```
    //!
    //! 两个要点：
    //! - **镜头按 ~42 秒切**，不是「每集固定 2 段」 —— 真实软件里第3集
    //!   出了 3 个镜头（最后一个只剩 5 秒）、第4集只有 1 个
    //! - **每镜头喂 5 张帧**（真实软件写着「附图 5 张」）
    if media_ids.is_empty() {
        return Err(AppError::Msg(
            "没有可用于做证明的集 —— 先跑「AI 去字幕」，它决定用哪几集".into(),
        ));
    }
    let out_dir = PathBuf::from(out_dir.trim());
    if out_dir.as_os_str().is_empty() {
        return Err(AppError::Msg("没指定证明图输出目录".into()));
    }
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| AppError::Msg(format!("建不了输出目录：{e}")))?;

    let (ffmpeg, ffprobe, provider, title, python) = {
        let configured = lock_tools(&state)?.clone();
        let ffmpeg = tools::require(&configured.ffmpeg, "ffmpeg")?;
        let ffprobe = configured.ffprobe.clone();
        let python = tools::require(&configured.python, "python")?;
        let conn = lock_db(&state)?;
        let provider = ai::usable_provider(&conn)?;
        // 剧名：rewrite 写过就用新的。`剧名.txt` 在**剧目录**下，
        // 而分镜证明输出在 `<剧目录>/分镜证明/` —— 本级和父级都找一遍。
        let title = std::fs::read_to_string(out_dir.join("剧名.txt"))
            .or_else(|_| {
                out_dir
                    .parent()
                    .map(|p| std::fs::read_to_string(p.join("剧名.txt")))
                    .unwrap_or(Err(std::io::Error::from(std::io::ErrorKind::NotFound)))
            })
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "未命名".to_string());
        (ffmpeg, ffprobe, provider, title, python)
    };

    // ---------- ① 定镜头：每集按 ~42 秒切，凑够 8 个 ----------
    const SHOT_SECS: f64 = 42.0;
    const MAX_SHOTS: usize = 8;
    const FRAMES_PER_SHOT: usize = 5;

    struct Plan {
        source: PathBuf,
        ep_label: String,
        start: f64,
        end: f64,
    }
    let mut plans: Vec<Plan> = Vec::new();
    for mid in &media_ids {
        if plans.len() >= MAX_SHOTS {
            break;
        }
        let (source, ep_label, dur) = {
            let conn = lock_db(&state)?;
            let media = db::get_media(&conn, *mid)?;
            let src = PathBuf::from(db::media_source_path(&conn, *mid)?);
            // **时长要用实际文件的，不是数据库记的。**
            //
            // 踩过：数据库记 200 秒、实际去重产物只有 104.7 秒 ——
            // 按数据库排出来的第 5 个镜头落在 170 秒之后，抽帧时
            // `-ss 170` 早就越过文件末尾，ffmpeg 直接失败，
            // 整个「分镜证明」0 产出。
            let actual = crate::media::probe(std::path::Path::new(&ffprobe), &src)
                .map(|m| m.duration_ms as f64 / 1000.0)
                .unwrap_or(0.0);
            let d = if actual > 0.0 {
                actual
            } else {
                media.duration_ms as f64 / 1000.0
            };
            (src, media.name.clone(), d)
        };
        if !source.is_file() || dur <= 1.0 {
            continue;
        }
        let mut at = 0.0f64;
        while at < dur && plans.len() < MAX_SHOTS {
            let end = (at + SHOT_SECS).min(dur);
            // 太短的尾巴（不到 3 秒）并进上一段，别单独出一个 5 秒的镜头
            if end - at < 3.0 {
                if let Some(last) = plans.last_mut() {
                    last.end = end;
                }
                break;
            }
            plans.push(Plan {
                source: source.clone(),
                ep_label: ep_label.clone(),
                start: at,
                end,
            });
            at = end;
        }
    }
    if plans.is_empty() {
        return Err(AppError::Msg(
            "切不出镜头 —— 集太短或文件不在".into(),
        ));
    }

    // ---------- ② 每镜头抽 5 帧 ----------
    let frame_root = out_dir.join("_frames");
    std::fs::create_dir_all(&frame_root)
        .map_err(|e| AppError::Msg(format!("建不了抽帧目录：{e}")))?;

    let mut frames_of: Vec<Vec<PathBuf>> = Vec::new();
    for (i, p) in plans.iter().enumerate() {
        let shot_dir = frame_root.join(format!("shot{:02}", i + 1));
        std::fs::create_dir_all(&shot_dir)
            .map_err(|e| AppError::Msg(format!("建不了镜头帧目录：{e}")))?;
        let mut got = Vec::new();
        for k in 0..FRAMES_PER_SHOT {
            // 均匀分布在这个镜头内：第 k 帧取在 (k+0.5)/n 处
            let frac = (k as f64 + 0.5) / FRAMES_PER_SHOT as f64;
            let at = p.start + (p.end - p.start) * frac;
            let name = if k == FRAMES_PER_SHOT - 1 {
                // 最后一张兼作预览图（真实软件 `_frames\shotNN\preview.jpg`）
                "preview.jpg".to_string()
            } else {
                format!("kf{}.jpg", k + 1)
            };
            let dst = shot_dir.join(&name);
            let ok = crate::proc::no_window(
                std::process::Command::new(&ffmpeg).args(["-hide_banner", "-loglevel", "error", "-y", "-ss"]),
            )
            .arg(format!("{at:.3}"))
            .arg("-i")
            .arg(&p.source)
            .args(["-frames:v", "1", "-q:v", "3"])
            .arg(&dst)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
            if !ok || !dst.is_file() {
                return Err(AppError::Msg(format!(
                    "抽第 {} 个镜头的第 {} 帧失败（{} {:.1}s）",
                    i + 1,
                    k + 1,
                    p.source.to_string_lossy(),
                    at
                )));
            }
            if name != "preview.jpg" {
                got.push(dst);
            }
        }
        frames_of.push(got);
    }
    let preview_of: Vec<PathBuf> = (0..plans.len())
        .map(|i| frame_root.join(format!("shot{:02}", i + 1)).join("preview.jpg"))
        .collect();

    // ---------- ③ AI 反推 ----------
    let provider = provider.ok_or_else(|| {
        AppError::Msg("分镜反推要 AI 服务 —— 先去「AI 服务」配一个，别的是纯本地".into())
    })?;
    let mut shots: Vec<ai::ShotReverse> = Vec::new();
    for (i, p) in plans.iter().enumerate() {
        let tr = format!("{}-{}", mmss(p.start), mmss(p.end));
        let r = ai::reverse_shot(
            &provider,
            &frames_of[i],
            &p.ep_label,
            i + 1,
            plans.len(),
            &tr,
            p.end - p.start,
        )
        .await
        .map_err(|e| {
            AppError::Msg(format!(
                "镜头{:02} 反推失败（{} {}）：{}",
                i + 1,
                p.ep_label,
                tr,
                crate::proc::tail(&e.to_string())
            ))
        })?;
        shots.push(r);
    }

    // ---------- ④ 每 2 镜头排 1 页 + 写 JSON ----------
    const SHOTS_PER_SHEET: usize = 2;
    let total_pages = shots.len().div_ceil(SHOTS_PER_SHEET);
    let mut pages = 0i64;
    // **文件名用剧名，不是输出目录名。**
    //
    // 踩过：输出目录叫「分镜证明」，取它的 `file_name()` 当剧名，
    // 结果产出叫 `分镜证明_分镜证明_01.jpg` —— 剧名没了，还重复一遍。
    //
    // `title` 是从 `剧名.txt` 读出来的（rewrite 写过就是新剧名），那才是对的。
    let safe_title = safe_file_name(&title);
    for page in 0..total_pages {
        let from = page * SHOTS_PER_SHEET;
        let to = (from + SHOTS_PER_SHEET).min(shots.len());
        let mut page_shots = Vec::new();
        let mut json_shots = Vec::new();
        for idx in from..to {
            let s = &shots[idx];
            let r = &shots[idx];
            let _ = r;
            page_shots.push(serde_json::json!({
                "shot_index": idx + 1,
                "time_range": format!("{}-{}", mmss(plans[idx].start), mmss(plans[idx].end)),
                "preview": preview_of[idx].to_string_lossy(),
                "keyframes": frames_of[idx].iter().map(|p| p.to_string_lossy().to_string()).collect::<Vec<_>>(),
                "data": {
                    "prompt": s.prompt,
                    "dialogue": s.dialogue,
                    "sfx": s.sfx,
                    "lighting": s.lighting,
                    "scriptExcerpt": s.script_excerpt,
                    "characters": s.characters,
                    "checks": s.checks,
                    "params": {
                        "model": "Seedance 2.0",
                        "seed": s.seed,
                        "resolution": s.resolution,
                        "guidance": s.guidance,
                        "durationSec": s.duration_sec,
                        "fps": s.fps,
                    }
                }
            }));
            // 和真实软件一致的 JSON 形状：page + shots[].{shot_index, data}
            json_shots.push(serde_json::json!({
                "shot_index": idx + 1,
                "data": {
                    "prompt": s.prompt,
                    "dialogue": s.dialogue,
                    "sfx": s.sfx,
                    "lighting": s.lighting,
                    "script_excerpt": s.script_excerpt,
                    "characters": s.characters,
                    "checks": s.checks,
                    "params": {
                        "model": "Seedance 2.0",
                        "seed": s.seed,
                        "resolution": s.resolution,
                        "guidance": s.guidance,
                        "duration_sec": s.duration_sec,
                        "fps": s.fps,
                    }
                }
            }));
        }
        let jpg = out_dir.join(format!("{}_分镜证明_{:02}.jpg", safe_title, page + 1));
        let json_path = out_dir.join(format!("{}_分镜证明_{:02}.json", safe_title, page + 1));
        // **key 必须和 `storyboard_render.py` 读的一致（snake_case）。**
        //
        // 这里踩过一次：Rust 写 `outPath`、Python 读 `out_path`，
        // 渲染器报「没给 out_path」，而报错完全看不出是 key 名对不上。
        // **跨语言的边界上，字段名就是契约，两边得逐字一样。**
        let req = serde_json::json!({
            "out_path": jpg.to_string_lossy(),
            "title": safe_title,
            "page": page + 1,
            "total_pages": total_pages,
            "shots": page_shots,
        });
        let worker = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("storyboard_render.py");
        let value = crate::proc::run_json_worker(
            &python,
            &worker,
            &req,
            std::time::Duration::from_secs(120),
        )?;
        if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let msg = value
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("未知错误");
            return Err(AppError::Msg(format!("分镜证明页渲染失败：{msg}")));
        }
        let doc = serde_json::json!({
            "page": page + 1,
            "title": safe_title,
            "shots": json_shots,
        });
        std::fs::write(
            &json_path,
            serde_json::to_string_pretty(&doc).unwrap_or_default(),
        )
        .map_err(|e| AppError::Msg(format!("写不了分镜 JSON：{e}")))?;
        pages += 1;
    }

    Ok(StoryboardReport {
        sheets: pages,
        shots: shots.len() as i64,
        used_ai: true,
        frames_dir: frame_root.to_string_lossy().to_string(),
        notes: vec![format!(
            "共 {} 张证明图 × 每张 {} 镜头（共 {} 镜头）｜反推模型：{}",
            pages,
            SHOTS_PER_SHEET,
            shots.len(),
            provider.label
        )],
    })
}

// ---------------- 封面生成 ----------------

/// 生成封面。
///
/// 两条路自动选：配好 AI 且开了「封面生成」就走**图生图**（能擦掉原封面
/// 上的旧剧名）；否则走**本地合成**（用标题条盖住旧字 + 写新剧名）。
/// 结果里会说明实际用了哪条 —— 别让用户以为一定是 AI 出的。
#[tauri::command]
pub async fn render_cover(
    state: tauri::State<'_, AppState>,
    req: CoverRequest,
) -> AppResult<CoverReport> {
    let (configured, provider, use_ai) = {
        let conn = lock_db(&state)?;
        let configured = lock_tools(&state)?.clone();
        let usage = ai::load_usage(&conn)?;
        let provider = ai::usable_provider(&conn)?;
        (configured, provider, usage.cover_generation)
    };
    let ffmpeg = tools::require(&configured.ffmpeg, "ffmpeg")?;

    let out = PathBuf::from(req.out_path.trim());
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::Msg(format!("建不了输出目录：{e}")))?;
    }

    let mut notes: Vec<String> = Vec::new();
    let engine;

    match (use_ai, provider) {
        (true, Some(p)) => {
            let poster = PathBuf::from(req.poster.trim());
            let bytes =
                ai::generate_cover_image(&p, &poster, &req.title, req.width, req.height).await?;
            std::fs::write(&out, &bytes).map_err(|e| AppError::Msg(format!("写封面失败：{e}")))?;
            engine = "ai".to_string();
            notes.push(format!("由「{}」的 {} 生成", p.label, p.image_model));
            // 这一步很重要：AI 生成的封面属于 AI 生成内容，有标识义务
            notes.push("这是 AI 生成的封面 —— 按《人工智能生成合成内容标识办法》应当标识".into());
        }
        (true, None) => {
            notes.push(
                "开了「封面生成」但 AI 服务没配全（Key / Base URL / 生图模型缺一个），已退回本地合成"
                    .into(),
            );
            cover::render(&ffmpeg, &req, &out)?;
            engine = "local".to_string();
        }
        (false, _) => {
            cover::render(&ffmpeg, &req, &out)?;
            engine = "local".to_string();
            notes.push("本地合成：用标题条盖住原封面上的旧字，不擦除、不生成".into());
        }
    }

    let size_bytes = std::fs::metadata(&out).map(|m| m.len() as i64).unwrap_or(0);
    Ok(CoverReport {
        out_path: out.to_string_lossy().to_string(),
        width: req.width,
        height: req.height,
        size_bytes,
        engine,
        notes,
    })
}

/// 清空运行记录。
///
/// **不动磁盘上的产出** —— 跑出来的视频是用户的，删记录不该把文件也带走。
/// **正在跑的那次也不删**（删了队列就找不到归属）。理由见 `db::clear_pipeline_runs`。
#[tauri::command]
pub fn clear_pipeline_runs(state: tauri::State<'_, AppState>) -> AppResult<i64> {
    let conn = lock_db(&state)?;
    let n = db::clear_pipeline_runs(&conn)?;
    db::append_log(&conn, None, None, "info", &format!("清空了 {n} 次运行记录"));
    Ok(n)
}

// ---------------- 运行日志 ----------------

/// 读运行日志。
///
/// `afterId` 用来做增量拉取 —— 前端只要新行，不用每次把整段搬一遍。
#[tauri::command]
pub fn list_logs(
    state: tauri::State<'_, AppState>,
    run_id: Option<i64>,
    after_id: Option<i64>,
    limit: Option<i64>,
) -> AppResult<Vec<RunLog>> {
    let conn = lock_db(&state)?;
    db::list_logs(&conn, run_id, after_id.unwrap_or(0), limit.unwrap_or(500))
}

/// 清空日志。跑过很多轮之后会攒下不少，给用户一个清理入口。
#[tauri::command]
pub fn clear_logs(state: tauri::State<'_, AppState>) -> AppResult<i64> {
    let conn = lock_db(&state)?;
    let n = conn.execute("DELETE FROM run_log", [])?;
    Ok(n as i64)
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
        // 用户明确点了启动 → 以后可以自动续跑
        let conn = state
            .db
            .lock()
            .map_err(|_| AppError::Msg("数据库锁被污染，请重启应用".into()))?;
        db::set_setting(&conn, "queue.auto_resume", "on")?;
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
    // 把"用户主动暂停过"记下来：下次启动时不要自动把队列又跑起来。
    // 用户按下暂停是有意的选择，不该被一次重启覆盖掉。
    let conn = lock_db(&state)?;
    db::set_setting(&conn, "queue.auto_resume", if paused { "off" } else { "on" })?;
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

/// 台账总览：按「平台 × 账号」汇总，用来一眼看清每个账号发到什么程度了。
#[tauri::command]
pub fn list_publish_accounts(state: tauri::State<'_, AppState>) -> AppResult<Vec<PublishAccount>> {
    let conn = lock_db(&state)?;
    let mut accounts = db::list_publish_accounts(&conn)?;
    for account in accounts.iter_mut() {
        account.platform_label = publish::platform_label(&account.platform);
    }
    Ok(accounts)
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
        account: publish::normalize_account(&plan.account),
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

/// 标记某个平台 / 账号发没发出去。这是本地台账，不联网。
#[tauri::command]
pub fn mark_publish_status(
    state: tauri::State<'_, AppState>,
    media_id: i64,
    platform: String,
    account: String,
    status: String,
    note: String,
) -> AppResult<PublishPlan> {
    let platform = platform.trim().to_string();
    let account = publish::normalize_account(&account);
    let status = publish::normalize_status(&status);
    let conn = lock_db(&state)?;
    db::set_publish_status(&conn, media_id, &platform, &account, &status, note.trim())?;
    db::get_publish_plan(&conn, media_id, &platform, &account)?
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
    account: Option<String>,
    out_root: String,
) -> AppResult<PublishPackageReport> {
    let platform = platform.trim().to_string();
    if !publish::is_known_platform(&platform) {
        return Err(AppError::Msg(format!("不支持的平台：{platform}")));
    }
    let account = publish::normalize_account(account.as_deref().unwrap_or(""));
    let out_root_path = PathBuf::from(out_root.trim());
    if out_root_path.as_os_str().is_empty() {
        return Err(AppError::Msg("请先选择发布包输出目录".into()));
    }

    let (ffmpeg, meta, custom_cover, package_clips) = {
        let configured = lock_tools(&state)?.clone();
        let ffmpeg = tools::require(&configured.ffmpeg, "ffmpeg")?;
        let conn = lock_db(&state)?;

        let media = db::get_media(&conn, media_id)?;
        let stored = db::get_publish_plan(&conn, media_id, &platform, &account)?;

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
            account: account.clone(),
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
