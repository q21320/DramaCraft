import { defineStore } from "pinia";
import { api } from "@/api";
import type { AppInfo, Media, Project, RuntimeStatus, ToolPaths, ToolReport } from "@/types";

interface State {
  info: AppInfo | null;
  projects: Project[];
  currentProjectId: number | null;
  media: Media[];
  tools: ToolPaths;
  toolReports: ToolReport[];
  asr: RuntimeStatus | null;
  ocr: RuntimeStatus | null;
  loading: boolean;
  error: string;
  /** 最近一次切片输出目录，按项目分开记，避免串台 */
  outDirs: Record<number, string>;
}

export const useAppStore = defineStore("app", {
  state: (): State => ({
    info: null,
    projects: [],
    currentProjectId: null,
    media: [],
    tools: { ffmpeg: "", ffprobe: "", python: "" },
    toolReports: [],
    asr: null,
    ocr: null,
    loading: false,
    error: "",
    outDirs: {},
  }),

  getters: {
    currentProject(state): Project | null {
      return state.projects.find((p) => p.id === state.currentProjectId) ?? null;
    },
    ffmpegReady(state): boolean {
      return state.toolReports.some((r) => r.name === "ffmpeg" && r.available);
    },
    ffprobeReady(state): boolean {
      return state.toolReports.some((r) => r.name === "ffprobe" && r.available);
    },
    outDir(state): string {
      const id = state.currentProjectId;
      return id ? state.outDirs[id] ?? "" : "";
    },
  },

  actions: {
    async bootstrap() {
      this.loading = true;
      this.error = "";
      try {
        this.info = await api.appInfo();
        this.tools = await api.getTools();
        this.toolReports = await api.detectTools();
        this.asr = await api.asrStatus();
        this.ocr = await api.ocrStatus();
        await this.reloadProjects();
      } catch (e) {
        this.error = (e as Error).message;
      } finally {
        this.loading = false;
      }
    },

    async reloadProjects() {
      this.projects = await api.listProjects();
      if (this.currentProjectId === null && this.projects.length > 0) {
        this.currentProjectId = this.projects[0].id;
      }
      if (this.currentProjectId !== null) {
        await this.reloadMedia();
      }
    },

    async selectProject(id: number) {
      this.currentProjectId = id;
      await this.reloadMedia();
    },

    async createProject(name: string, rootDir: string) {
      const created = await api.createProject(name, rootDir);
      await this.reloadProjects();
      this.currentProjectId = created.id;
      await this.reloadMedia();
      return created;
    },

    async deleteProject(id: number) {
      await api.deleteProject(id);
      if (this.currentProjectId === id) {
        this.currentProjectId = null;
        this.media = [];
      }
      await this.reloadProjects();
    },

    async reloadMedia() {
      const id = this.currentProjectId;
      this.media = id === null ? [] : await api.listMedia(id);
    },

    async addMedia(path: string) {
      const id = this.currentProjectId;
      if (id === null) throw new Error("请先选择或创建一个项目");
      await api.addMedia(id, path);
      await this.reloadMedia();
    },

    async removeMedia(mediaId: number) {
      await api.removeMedia(mediaId);
      await this.reloadMedia();
    },

    async saveTools(paths: ToolPaths) {
      this.toolReports = await api.setTools(paths);
      this.tools = await api.getTools();
      this.asr = await api.asrStatus();
      this.ocr = await api.ocrStatus();
    },

    setOutDir(dir: string) {
      const id = this.currentProjectId;
      if (id !== null) this.outDirs[id] = dir;
    },
  },
});
