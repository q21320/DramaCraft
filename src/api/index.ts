import { invoke } from "@tauri-apps/api/core";
import type {
  AiConfigView,
  AiProvider,
  AiModelInfo,
  AiTestResult,
  AiUsage,
  AppInfo,
  AsrOutcome,
  Asset,
  AssetApplyReport,
  AssetImportReport,
  AssetKindOption,
  AssetSummary,
  CloudConfigView,
  CloudTestResult,
  CloudTranscribeReport,
  CostDocRequest,
  CostReportRow,
  CoverReport,
  DedupReport,
  RewriteOutcome,
  StoryboardReport,
  RunLog,
  CoverRequest,
  Cue,
  DiscoveredAsset,
  DocxReport,
  DraftReport,
  EpisodeBoundary,
  EpisodeDetection,
  ExportOptions,
  ExportOutcome,
  Job,
  Media,
  IpDocRequest,
  OcrOptions,
  OcrReport,
  PlatformOption,
  Project,
  PipelineRun,
  HeaderLabel,
  OutputEpisode,
  PipelineStageOption,
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
  SourceScan,
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
  asrModels: () => call<string[]>("asr_models"),
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

  // 界面上记住的东西（源目录、勾选…）
  loadUiState: (key: string) => call<string | null>("load_ui_state", { key }),
  saveUiState: (key: string, value: string) => call<void>("save_ui_state", { key, value }),

  // 证明文档（docx）
  generateCostDocx: (request: CostDocRequest, outPath: string) =>
    call<DocxReport>("generate_cost_docx", { request, outPath }),
  /**
   * 生成「成本配置比例情况报告」—— 申报用的那张表。
   *
   * 版式照 `zhengming/成本图表.docx` 那份模板：标题 + 提审说明 + 5 列表格
   * + 符合性声明 + 版权承诺 + 报审机构签字。
   * **报审机构和法人故意留空** —— 不该由工具代写主体信息。
   */
  generateCostReport: (
    rows: CostReportRow[],
    org: string,
    legalPerson: string,
    date: string,
    outPath: string,
  ) => call<DocxReport>("generate_cost_report", { rows, org, legalPerson, date, outPath }),
  generateIpDocx: (request: IpDocRequest, outPath: string) =>
    call<DocxReport>("generate_ip_docx", { request, outPath }),

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

  // 用户自备素材库
  assetKinds: () => call<AssetKindOption[]>("asset_kinds"),
  scanAssetDir: (dir: string) => call<DiscoveredAsset[]>("scan_asset_dir", { dir }),
  importAssets: (paths: string[], category: string) =>
    call<AssetImportReport>("import_assets", { paths, category }),
  listAssets: (kind?: string) => call<Asset[]>("list_assets", { kind: kind ?? null }),
  updateAsset: (id: number, name: string, category: string, tags: string, note: string) =>
    call<Asset>("update_asset", { id, name, category, tags, note }),
  deleteAsset: (id: number) => call<void>("delete_asset", { id }),
  assetSummary: () => call<AssetSummary>("asset_summary"),
  applyBgm: (
    mediaId: number,
    assetId: number,
    mode: string,
    bgmVolume: number,
    originalVolume: number,
    loopBgm: boolean,
  ) =>
    call<AssetApplyReport>("apply_bgm", {
      mediaId,
      assetId,
      mode,
      bgmVolume,
      originalVolume,
      loopBgm,
    }),
  applyLut: (mediaId: number, assetId: number, intensity: number) =>
    call<AssetApplyReport>("apply_lut", { mediaId, assetId, intensity }),

  // 流水线
  pipelineStages: () => call<PipelineStageOption[]>("pipeline_stages"),
  pipelineScan: (root: string) => call<SourceScan>("pipeline_scan", { root }),
  pipelineDefaultOutDir: (root: string) =>
    call<string>("pipeline_default_out_dir", { root }),
  pipelineStart: (
    root: string,
    outDir: string,
    stages: string[],
    sliceSeconds: number,
    model: string,
    headerLabel?: HeaderLabel | null,
  ) =>
    call<PipelineRun>("pipeline_start", {
      root,
      outDir,
      stages,
      sliceSeconds,
      headerLabel: headerLabel ?? null,
      model,
    }),
  pipelineList: (limit?: number) => call<PipelineRun[]>("pipeline_list", { limit: limit ?? 20 }),
  pipelineGet: (id: number) => call<PipelineRun | null>("pipeline_get", { id }),
  pipelineCancel: (id: number) => call<number>("pipeline_cancel", { id }),
  /** 一次流水线跑出来了什么（产出必须能当场看） */
  pipelineOutputs: (runId: number) => call<OutputEpisode[]>("pipeline_outputs", { runId }),
  /** 用系统默认播放器打开 */
  openInPlayer: (path: string) => call<void>("open_in_player", { path }),
  /** 在资源管理器里定位 */
  revealInExplorer: (path: string) => call<void>("reveal_in_explorer", { path }),

  /**
   * 整集去重 —— **发布用的就是这个产物**，切片是它的下游。
   * 真实软件的流程是「先去重整集、再切片」。
   */
  dedupMedia: (mediaId: number, intensity: string, headerLabel: HeaderLabel | null, outDir: string) =>
    call<DedupReport>("dedup_media", { mediaId, intensity, headerLabel, outDir }),

  /** 分镜证明：抽帧 → 视觉模型反推 → 拼成报备用的证明图 */
  buildStoryboard: (mediaIds: number[], outDir: string, framesPerEpisode: number, shotsPerSheet: number) =>
    call<StoryboardReport>("build_storyboard", { mediaIds, outDir, framesPerEpisode, shotsPerSheet }),

  // 运行日志：afterId 用来增量拉取，只要新行
  listLogs: (runId: number | null, afterId: number, limit?: number) =>
    call<RunLog[]>("list_logs", { runId, afterId, limit: limit ?? 800 }),
  clearLogs: () => call<number>("clear_logs"),
  /** 清空运行记录。**只清记录，磁盘上的产出不动**；正在跑的那次也保留。 */
  clearPipelineRuns: () => call<number>("clear_pipeline_runs"),

  // AI 服务配置（Key 只存在本机，读回来的是打码值）
  aiConfig: () => call<AiConfigView>("ai_config"),
  saveAiConfig: (providers: AiProvider[], active: string, usage: AiUsage) =>
    call<AiConfigView>("save_ai_config", { providers, active, usage }),
  /** 只发一次廉价探测，不触发生成、不产生费用 */
  aiTest: (providerId: string) => call<AiTestResult>("ai_test", { providerId }),
  /** 把 Base URL / 模型恢复成内置预设；不动 Key */
  /** 列出账号能看到的模型（GET {base}/models）。名字写错/没开通都能在这里看出来。 */
  aiListModels: (providerId: string) => call<AiModelInfo[]>("ai_list_models", { providerId }),

  aiResetPreset: (providerId: string) => call<AiProvider>("ai_reset_preset", { providerId }),

  /** 改写剧名/简介：先改简介，再按新简介起剧名。会调用两次模型。 */
  aiRewrite: (title: string, synopsis: string, outDir: string | null) =>
    call<RewriteOutcome>("ai_rewrite", { title, synopsis, outDir }),

  // 封面生成：配好 AI 走图生图，否则本地合成
  renderCover: (req: CoverRequest) => call<CoverReport>("render_cover", { req }),
};
