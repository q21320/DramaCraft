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
}

export interface AsrStatus {
  available: boolean;
  python: string;
  detail: string;
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
  title: string;
  description: string;
  tags: string;
  coverPath: string;
  /** draft | ready | published | failed */
  status: string;
  note: string;
  updatedAt: string;
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
