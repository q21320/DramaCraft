import { invoke } from "@tauri-apps/api/core";
import type {
  AppInfo,
  AsrStatus,
  Media,
  Project,
  Segment,
  SliceMode,
  SlicePlan,
  ToolPaths,
  ToolReport,
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
};
