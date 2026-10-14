export interface Project {
  id: number;
  name: string;
  rootDir: string;
  createdAt: string;
}

export interface Media {
  id: number;
  projectId: number;
  path: string;
  name: string;
  durationMs: number;
  width: number;
  height: number;
  hasAudio: boolean;
  sizeBytes: number;
  formatName: string;
  createdAt: string;
}

export interface SlicePlan {
  index: number;
  startMs: number;
  endMs: number;
}

/** 与 Rust 侧 `SliceMode` 的 serde tag 形状一一对应 */
export type SliceMode =
  | { mode: "everySeconds"; seconds: number }
  | { mode: "equalParts"; parts: number }
  | { mode: "manual"; points: number[] };

export interface Segment {
  id: number;
  mediaId: number;
  indexNo: number;
  startMs: number;
  endMs: number;
  outPath: string;
  sizeBytes: number;
  status: string;
  message: string;
  /** 画面处理前的原始文件；为空表示没处理过 */
  originalPath: string;
}

export interface ToolPaths {
  ffmpeg: string;
  ffprobe: string;
  python: string;
}

export interface ToolReport {
  name: string;
  path: string;
  available: boolean;
  version: string;
  detail: string;
  /** 是否来自随程序分发的内置目录 */
  bundled: boolean;
}

/** 某个可选 Python 运行时的能力状态（ASR / OCR / 草稿各自一份，形状相同） */
export interface RuntimeStatus {
  available: boolean;
  python: string;
  detail: string;
}

// ---------------- 流水线 ----------------

export interface SourceEpisode {
  path: string;
  fileName: string;
  episode: number | null;
  sizeBytes: number;
}

export interface SourceDrama {
  name: string;
  dir: string;
  episodeCount: number;
  episodes: SourceEpisode[];
  synopsis: string;
  synopsisPath: string;
  cover: string;
  warnings: string[];
}

export interface SourceScan {
  root: string;
  totalEpisodes: number;
  dramas: SourceDrama[];
  warnings: string[];
}

export interface PipelineStageOption {
  key: string;
  label: string;
  enabledByDefault: boolean;
  detail: string;
}

export interface PipelineDrama {
  id: number;
  runId: number;
  name: string;
  srcDir: string;
  outDir: string;
  episodeCount: number;
  /** 最后完成的步骤 key；空串表示还没开始 */
  stage: string;
  /** pending | running | done | failed | skipped */
  status: string;
  message: string;
  updatedAt: string;
}

export interface PipelineRun {
  id: number;
  rootDir: string;
  outDir: string;
  /** 勾选的步骤，JSON 数组字符串 */
  stages: string;
  status: string;
  dramaCount: number;
  createdAt: string;
  updatedAt: string;
  dramas: PipelineDrama[];
}

/** 流水线跑出来的一个切片 */
export interface OutputSlice {
  segmentId: number;
  indexNo: number;
  /** 当前文件（画面处理后指向处理版） */
  path: string;
  sizeBytes: number;
  startMs: number;
  endMs: number;
  status: string;
  message: string;
  /** 这一段里的字幕文本 */
  cues: string[];
}

/** 一部剧的产出：一集一行，下面挂着它的切片 */
export interface OutputEpisode {
  mediaId: number;
  name: string;
  durationMs: number;
  width: number;
  height: number;
  slices: OutputSlice[];
}

// ---------------- 用户自备素材库 ----------------

/** 素材库里的一条素材。全部由用户导入，程序不内置任何第三方素材。 */
export interface Asset {
  id: number;
  /** audio | lut */
  kind: string;
  name: string;
  path: string;
  category: string;
  tags: string;
  durationMs: number;
  sizeBytes: number;
  /** LUT 的尺寸等额外信息，JSON 字符串 */
  meta: string;
  note: string;
  createdAt: string;
  updatedAt: string;
}

export interface AssetKindOption {
  key: string;
  label: string;
}

export interface AssetSummary {
  audio: number;
  lut: number;
  total: number;
  totalBytes: number;
}

export interface DiscoveredAsset {
  path: string;
  fileName: string;
  kind: string;
  sizeBytes: number;
  /** 扫描时就发现的问题（例如 .cube 被截断） */
  problem: string;
}

export interface AssetImportReport {
  imported: number;
  skipped: string[];
  summary: AssetSummary;
}

export interface AssetApplyReport {
  outPath: string;
  sizeBytes: number;
  durationMs: number;
  filter: string;
  warnings: string[];
}

// ---------------- 证明材料 ----------------

export interface ProofField {
  key: string;
  label: string;
  /** 先擦除的矩形 [x1,y1,x2,y2]；null 表示不擦除 */
  eraseRect: number[] | null;
  eraseColor: string;
  /** 文字绘制区域 [x,y,w,h] */
  boxRect: number[];
  fontSize: number;
  minFontSize: number;
  color: string;
  align: string;
  valign: string;
  wrap: boolean;
  lineSpacing: number;
}

export interface ProofLayout {
  template: string;
  imageSize: number[];
  fontPath: string;
  fields: ProofField[];
}

export interface ProofFieldPreview {
  key: string;
  label: string;
  lines: string[];
  fontSize: number;
  shrunk: boolean;
  overflow: boolean;
}

export interface ProofPreview {
  fields: ProofFieldPreview[];
  filter: string;
  warnings: string[];
}

export interface ProofRenderReport {
  outPath: string;
  sizeBytes: number;
  fieldCount: number;
  filter: string;
  warnings: string[];
}

// ---------------- 证明文档（docx） ----------------

export interface DocxReport {
  outPath: string;
  sizeBytes: number;
  /** cost | ip */
  kind: string;
  blockCount: number;
  warnings: string[];
}

export interface CostDocRow {
  item: string;
  quantity: string;
  unitPrice: string;
  amount: string;
}

export interface CostDocRequest {
  dramaName: string;
  producer: string;
  date: string;
  total: string;
  rows: CostDocRow[];
}

export interface IpDocRequest {
  dramaName: string;
  episodeCount: number;
  totalDuration: string;
  owner: string;
  date: string;
}

// ---------------- 云端字幕（用户自备 Key） ----------------

/** 前端能看到的云端配置：**永远不含完整 API Key** */
export interface CloudConfigView {
  baseUrl: string;
  model: string;
  language: string;
  hasKey: boolean;
  /** 打码后的 Key，例如 sk-****abcd */
  keyHint: string;
  warnings: string[];
}

export interface CloudTranscribeReport {
  cueCount: number;
  language: string;
  durationMs: number;
  endpoint: string;
  audioBytes: number;
  warnings: string[];
}

export interface CloudTestResult {
  ok: boolean;
  endpoint: string;
  detail: string;
}

// ---------------- 任务队列 ----------------

export interface Job {
  id: number;
  mediaId: number | null;
  /** slice | transform | ocr | asr | export | draft | publish | verify */
  kind: string;
  payload: string;
  /** pending | running | done | failed | canceled */
  status: string;
  message: string;
  attempts: number;
  createdAt: string;
  updatedAt: string;
}

export interface JobCounts {
  pending: number;
  running: number;
  done: number;
  failed: number;
  canceled: number;
  total: number;
}

export interface QueueState {
  running: boolean;
  paused: boolean;
  counts: JobCounts;
}

export interface QueueReport {
  processed: number;
  done: number;
  failed: number;
  remaining: number;
  stoppedBecause: string;
  messages: string[];
}

export interface QueueProgress {
  jobId: number;
  kind: string;
  phase: "running" | "done" | "failed";
  message?: string;
}

// ---------------- 剧目与集 ----------------

export interface SeriesFile {
  path: string;
  fileName: string;
  /** 解析出的剧目名 */
  series: string;
  /** 解析出的集号；认不出为 null */
  episode: number | null;
}

export interface SeriesGroup {
  series: string;
  episodes: SeriesFile[];
}

export interface EpisodeBoundary {
  indexNo: number;
  startMs: number;
  endMs: number;
  durationMs: number;
  /** high / low */
  confidence: string;
}

export interface EpisodeDetection {
  boundaries: EpisodeBoundary[];
  sampledFrames: number;
  templateFrames: number;
  totalDurationMs: number;
  warnings: string[];
}

export interface ResplitReport {
  episodeCount: number;
  outDir: string;
  files: string[];
  totalDurationMs: number;
}

// ---------------- OCR 硬字幕识别 ----------------

export interface OcrOptions {
  sampleFps: number;
  bottomRatio: number;
  minConfidence: number;
  mergeThreshold: number;
  minDurationMs: number;
  maxFrames: number;
}

export interface OcrReport {
  framesSampled: number;
  framesWithText: number;
  cueCount: number;
  engine: string;
  durationMs: number;
  warnings: string[];
}

// ---------------- 字幕 ----------------

/** 一条字幕。`id` 为 0 表示尚未落库 */
export interface Cue {
  id: number;
  mediaId: number;
  indexNo: number;
  startMs: number;
  endMs: number;
  text: string;
}

export interface AsrOutcome {
  cueCount: number;
  language: string;
  model: string;
  durationMs: number;
  log: string;
}

export interface SrtImportOutcome {
  count: number;
  warnings: string[];
}

// ---------------- 导出 ----------------

/** 与 Rust 侧 `AspectMode` 的 serde tag 形状一一对应 */
export type AspectMode =
  | { mode: "keep" }
  | { mode: "verticalBlur" }
  | { mode: "verticalCrop" };

export interface ExportOptions {
  mediaId: number;
  outPath: string;
  burnSubtitles: boolean;
  aspect: AspectMode;
  startMs: number;
  endMs: number;
  crf: number;
  preset: string;
}

export interface ExportOutcome {
  outPath: string;
  sizeBytes: number;
  durationMs: number;
  filter: string;
  log: string;
}

// ---------------- 剪映草稿 ----------------

export interface DraftReport {
  draftPath: string;
  draftName: string;
  clipCount: number;
  cueCount: number;
  durationMs: number;
  warnings: string[];
}

// ---------------- 画面处理（差异化） ----------------

export interface TransformOptions {
  mirror: boolean;
  cropRatio: number;
  scaleRatio: number;
  brightness: number;
  contrast: number;
  saturation: number;
  noise: number;
  speed: number;
  volume: number;
}

export interface TransformPreview {
  videoFilter: string;
  audioFilter: string;
  samples: TransformOptions[];
}

export interface TransformReport {
  total: number;
  processed: number;
  failed: number;
  errors: string[];
  segments: Segment[];
}

// ---------------- 发布 ----------------

export interface PlatformOption {
  key: string;
  label: string;
  /** 该平台是否已有公开发布接口 */
  hasPublicApi: boolean;
  note: string;
}

export interface PublishPlan {
  id: number;
  mediaId: number;
  platform: string;
  /** 账号名。同一平台可以有多个账号，各自独立记账 */
  account: string;
  title: string;
  description: string;
  tags: string;
  coverPath: string;
  /** draft | ready | published | failed */
  status: string;
  note: string;
  updatedAt: string;
}

/** 一个「平台 × 账号」的台账汇总 */
export interface PublishAccount {
  platform: string;
  platformLabel: string;
  account: string;
  total: number;
  draft: number;
  ready: number;
  published: number;
  failed: number;
  lastUpdated: string;
}

export interface PublishClipEntry {
  indexNo: number;
  file: string;
  startMs: number;
  endMs: number;
  durationMs: number;
  subtitle: string;
  suggestedTitle: string;
}

export interface PublishPackageReport {
  outDir: string;
  clipCount: number;
  subtitleCount: number;
  coverPath: string;
  metadataPath: string;
  readmePath: string;
  warnings: string[];
}

// ---------------- 去重效果验证 ----------------

export interface SimilaritySample {
  index: number;
  distance: number;
  similarity: number;
}

export interface SimilarityReport {
  before: string;
  after: string;
  samplesCompared: number;
  meanDistance: number;
  meanSimilarity: number;
  verdict: string;
  samples: SimilaritySample[];
  warnings: string[];
}

export interface VerifyItem {
  segmentId: number;
  indexNo: number;
  originalPath: string;
  currentPath: string;
  report: SimilarityReport;
}

export interface VerifyReport {
  checked: number;
  unchanged: number;
  failed: number;
  meanSimilarity: number;
  verdict: string;
  items: VerifyItem[];
  errors: string[];
  warnings: string[];
}

export interface AppInfo {
  name: string;
  version: string;
  dataDir: string;
}

export interface SliceProgress {
  mediaId: number;
  index: number;
  total: number;
  phase: "running" | "done";
  failed?: number;
}
