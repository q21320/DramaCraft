import { invoke } from "@tauri-apps/api/core";
import type {
  AppInfo,
  AsrOutcome,
  CloudConfigView,
  CloudTestResult,
  CloudTranscribeReport,
  Cue,
  DraftReport,
  EpisodeBoundary,
  EpisodeDetection,
  ExportOptions,
  ExportOutcome,
  Job,
  Media,
  OcrOptions,
  OcrReport,
  PlatformOption,
  Project,
  ProofLayout,
  ProofPreview,
  ProofRenderReport,
  PublishAccount,
  PublishPackageReport,
  PublishPlan,
  QueueState,
  ResplitReport,
  RuntimeStatus,
  Segment,
  SeriesGroup,
  SliceMode,
  SlicePlan,
  SrtImportOutcome,
  ToolPaths,
  ToolReport,
  TransformOptions,
  TransformPreview,
  TransformReport,
  VerifyReport,
} from "@/types";

/**
 * 统一封装 Tauri 调用。
 *
 * Rust 侧的错误序列化为 `{ code, message }`，这里统一抽成 Error(message)，
 * 让视图层只需要 `catch (e) { message.error(e.message) }`。
 */
export async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(cmd, args);
  } catch (raw: unknown) {
    const err = raw as { message?: string; code?: string } | string;
    if (typeof err === "string") throw new Error(err);
    throw new Error(err?.message || "调用失败");
  }
}

export const api = {
  appInfo: () => call<AppInfo>("app_info"),

  // 工具
  getTools: () => call<ToolPaths>("get_tools"),
  detectTools: () => call<ToolReport[]>("detect_tools"),
  setTools: (paths: ToolPaths) => call<ToolReport[]>("set_tools", { paths }),
  asrStatus: () => call<RuntimeStatus>("asr_status"),
  ocrStatus: () => call<RuntimeStatus>("ocr_status"),

  // 项目
  createProject: (name: string, rootDir: string) =>
    call<Project>("create_project", { name, rootDir }),
  listProjects: () => call<Project[]>("list_projects"),
  deleteProject: (id: number) => call<void>("delete_project", { id }),

  // 素材
  addMedia: (projectId: number, path: string) => call<Media>("add_media", { projectId, path }),
  listMedia: (projectId: number) => call<Media[]>("list_media", { projectId }),
  removeMedia: (id: number) => call<void>("remove_media", { id }),

  // 切片
  buildPlan: (mediaId: number, mode: SliceMode) =>
    call<SlicePlan[]>("build_slice_plan", { mediaId, mode }),
  runSlice: (mediaId: number, mode: SliceMode, outDir: string) =>
    call<Segment[]>("run_slice", { mediaId, mode, outDir }),
  listSegments: (mediaId: number) => call<Segment[]>("list_segments", { mediaId }),

  // 字幕
  listCues: (mediaId: number) => call<Cue[]>("list_cues", { mediaId }),
  replaceCues: (mediaId: number, cues: Cue[]) =>
    call<number>("replace_cues", { mediaId, cues }),
  importSrt: (mediaId: number, path: string) =>
    call<SrtImportOutcome>("import_srt", { mediaId, path }),
  exportSrt: (mediaId: number, path: string) => call<number>("export_srt", { mediaId, path }),
  asrTranscribe: (mediaId: number, model: string, language: string, overwrite: boolean) =>
    call<AsrOutcome>("asr_transcribe", { mediaId, model, language, overwrite }),
  recognizeHardSubtitles: (mediaId: number, options: OcrOptions, overwrite: boolean) =>
    call<OcrReport>("recognize_hard_subtitles", { mediaId, options, overwrite }),

  // 云端字幕
  cloudConfig: () => call<CloudConfigView>("cloud_config"),
  saveCloudConfig: (
    baseUrl: string,
    apiKey: string | null,
    model: string,
    language: string,
  ) => call<CloudConfigView>("save_cloud_config", { baseUrl, apiKey, model, language }),
  cloudTest: () => call<CloudTestResult>("cloud_test"),
  cloudTranscribe: (mediaId: number, overwrite: boolean) =>
    call<CloudTranscribeReport>("cloud_transcribe", { mediaId, overwrite }),

  // 证明材料
  proofDefaultLayout: () => call<ProofLayout>("proof_default_layout"),
  proofLoadLayout: (path: string) => call<ProofLayout>("proof_load_layout", { path }),
  proofSaveLayout: (path: string, layout: ProofLayout) =>
    call<void>("proof_save_layout", { path, layout }),
  proofPreview: (layout: ProofLayout, values: Record<string, string>) =>
    call<ProofPreview>("proof_preview", { layout, values }),
  proofRender: (layout: ProofLayout, values: Record<string, string>, outPath: string) =>
    call<ProofRenderReport>("proof_render", { layout, values, outPath }),

  // 导出
  exportVideo: (opts: ExportOptions) => call<ExportOutcome>("export_video", { opts }),

  // 剪映草稿
  exportDraft: (mediaId: number, draftRoot: string, draftName: string, fps: number) =>
    call<DraftReport>("export_draft", { mediaId, draftRoot, draftName, fps }),

  // 画面处理
  transformPreview: (
    options: TransformOptions | null,
    intensity: string,
    width?: number,
    height?: number,
  ) =>
    call<TransformPreview>("transform_preview", {
      options,
      intensity,
      width: width ?? null,
      height: height ?? null,
    }),
  transformSegments: (mediaId: number, intensity: string, options: TransformOptions | null) =>
    call<TransformReport>("transform_segments", { mediaId, intensity, options }),
  revertSegments: (mediaId: number) => call<TransformReport>("revert_segments", { mediaId }),

  // 发布
  listPlatforms: () => call<PlatformOption[]>("list_platforms"),
  listPublishPlans: (mediaId: number) => call<PublishPlan[]>("list_publish_plans", { mediaId }),
  listPublishAccounts: () => call<PublishAccount[]>("list_publish_accounts"),
  savePublishPlan: (plan: PublishPlan) => call<PublishPlan>("save_publish_plan", { plan }),
  markPublishStatus: (
    mediaId: number,
    platform: string,
    account: string,
    status: string,
    note: string,
  ) =>
    call<PublishPlan>("mark_publish_status", { mediaId, platform, account, status, note }),
  exportPublishPackage: (mediaId: number, platform: string, account: string, outRoot: string) =>
    call<PublishPackageReport>("export_publish_package", { mediaId, platform, account, outRoot }),

  // 去重效果验证
  verifyTransform: (mediaId: number, samples?: number) =>
    call<VerifyReport>("verify_transform", { mediaId, samples: samples ?? null }),

  // 剧目与集
  scanSeriesDir: (dir: string) => call<SeriesGroup[]>("scan_series_dir", { dir }),
  clusterSeriesByIntro: (files: string[], introSeconds?: number, tolerance?: number) =>
    call<SeriesGroup[]>("cluster_series_by_intro", {
      files,
      introSeconds: introSeconds ?? null,
      tolerance: tolerance ?? null,
    }),
  detectEpisodes: (
    mediaId: number,
    sampleFps?: number,
    introSeconds?: number,
    tolerance?: number,
  ) =>
    call<EpisodeDetection>("detect_episodes", {
      mediaId,
      sampleFps: sampleFps ?? null,
      introSeconds: introSeconds ?? null,
      tolerance: tolerance ?? null,
    }),
  resplitEpisodes: (mediaId: number, boundaries: EpisodeBoundary[], outDir: string) =>
    call<ResplitReport>("resplit_episodes", { mediaId, boundaries, outDir }),

  // 任务队列
  queueState: () => call<QueueState>("queue_state"),
  listJobs: (limit?: number) => call<Job[]>("list_jobs", { limit: limit ?? null }),
  enqueueJob: (mediaId: number | null, kind: string, payload: Record<string, unknown>) =>
    call<Job>("enqueue_job", { mediaId, kind, payload: JSON.stringify(payload) }),
  enqueueBatch: (mediaIds: number[], kind: string, payload: Record<string, unknown>) =>
    call<number>("enqueue_batch", {
      mediaIds,
      kind,
      payload: JSON.stringify(payload),
    }),
  startQueue: (maxJobs?: number) => call<void>("start_queue", { maxJobs: maxJobs ?? null }),
  setQueuePaused: (paused: boolean) => call<boolean>("set_queue_paused", { paused }),
  requestQueueStop: () => call<boolean>("request_queue_stop"),
  cancelPendingJobs: () => call<number>("cancel_pending_jobs"),
  clearFinishedJobs: () => call<number>("clear_finished_jobs"),
  deleteJob: (id: number) => call<void>("delete_job", { id }),
};
