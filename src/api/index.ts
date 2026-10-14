import { invoke } from "@tauri-apps/api/core";
import type {
  AppInfo,
  AsrOutcome,
  AsrStatus,
  Cue,
  DraftReport,
  ExportOptions,
  ExportOutcome,
  Media,
  Project,
  Segment,
  SliceMode,
  SlicePlan,
  SrtImportOutcome,
  ToolPaths,
  ToolReport,
  TransformOptions,
  TransformPreview,
  TransformReport,
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
  asrStatus: () => call<AsrStatus>("asr_status"),

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
  asrTranscribe: (mediaId: number, model: string, language: string) =>
    call<AsrOutcome>("asr_transcribe", { mediaId, model, language }),

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
};
