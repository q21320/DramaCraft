import { defineStore } from "pinia";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { api } from "@/api";
import type { FanqieDownloadProgress } from "@/types";

/** 列表里挑出来要下载的剧 —— 只需要下载用得到的字段。 */
export interface DownloadPick {
  seriesId: string;
  title: string;
  episodeCount?: number;
}

/**
 * 进度事件监听放在**模块级**，不放进 store state：
 * 它是运行时句柄，不需要响应式。
 */
let unlisten: UnlistenFn | null = null;

/**
 * 批量下载的状态与流程。
 *
 * ## 为什么放在 Pinia 里
 *
 * 组件内的 `reactive` 会随页面卸载一起消失 —— 用户切去别的页面再回来，
 * 看到的是全新状态：进度 0、面板消失，以为下载停了。
 *
 * 实际情况是 `downloadSelected` 的异步循环还在跑（Vue 卸载组件不会掐 Promise），
 * 只是没人记录它的进度。放大到 store 之后：
 *
 * - 流程不依赖页面，切页面下载照跑；
 * - 进度事件由 store 监听，回来直接看到当前值；
 * - 顶部导航也能显示「下载中 x/y」。
 */
export const useDownloadStore = defineStore("download", {
  state: () => ({
    /** 面板是否可见（下载结束也保留，直到用户收起） */
    active: false,
    /** 流程是否还在跑 */
    running: false,
    seriesIndex: 0,
    seriesTotal: 0,
    title: "",
    episode: 0,
    episodes: 0,
    done: 0,
    failed: 0,
    message: "",
    root: "",
    error: "",
  }),

  getters: {
    /** 总进度：按剧均分，部内再按集数推进。 */
    percent(state): number {
      if (state.seriesTotal <= 0) return 0;
      const inner = state.episodes > 0 ? Math.min(1, state.episode / state.episodes) : 0;
      return Math.round(((state.seriesIndex + inner) / state.seriesTotal) * 100);
    },
  },

  actions: {
    /**
     * 下载选中的剧：逐部拉分集、逐集解密，落到 `<下载目录>/<剧名>/`。
     * 全部落地后调用 `onDone(root, titles)` —— titles 是**实际落盘的剧名**，
     * 由页面带去流水线，让它只跑刚下的这几部（下载根里还有以前下的剧）。
     */
    async run(list: DownloadPick[], onDone?: (root: string, titles: string[]) => void) {
      // 已经有一批在跑时再点下载 → 不开第二份
      if (this.running) return;
      if (list.length === 0) {
        this.error = "先勾选要下载的短剧";
        this.active = true;
        return;
      }

      this.active = true;
      this.running = true;
      this.error = "";
      this.seriesIndex = 0;
      this.seriesTotal = list.length;
      this.title = "";
      this.episode = 0;
      this.episodes = 0;
      this.done = 0;
      this.failed = 0;
      this.message = "正在准备下载目录…";

      // 监听跟着 store 走：页面卸载不会把它带走
      if (unlisten) {
        unlisten();
        unlisten = null;
      }
      try {
        unlisten = await listen<FanqieDownloadProgress>("fanqie:download", (e) => {
          if (!this.active) return;
          const p = e.payload;
          this.title = p.title;
          this.episode = p.index;
          this.episodes = p.total;
          this.message = p.message;
          this.failed = Math.max(this.failed, p.failed);
        });
      } catch {
        // 监听挂了不影响下载本身，最终结果仍会回来
      }

      try {
        // 第一次下载先让用户指定一个固定目录；以后每次都放这里
        let root = await api.fanqieDownloadRoot();
        if (!root) {
          const picked = await open({
            directory: true,
            multiple: false,
            title: "选择下载目录（以后所有剧都放这里）",
          });
          if (typeof picked !== "string" || !picked) {
            this.message = "还没有下载目录";
            this.error = "第一次下载要先选一个目录，以后所有剧都放这里。";
            return;
          }
          root = await api.setFanqieDownloadRoot(picked);
        }
        this.root = root;

        // 真正落到盘上的剧名（详情接口的标题，目录就是拿它建的）
        const landed: string[] = [];
        for (let i = 0; i < list.length; i++) {
          const v = list[i];
          this.seriesIndex = i;
          this.title = v.title;
          this.episode = 0;
          this.episodes = v.episodeCount || 0;
          this.message = "正在获取分集列表…";

          const detailData = await api.fanqieSeriesDetail(v.seriesId);
          const episodes = detailData.episodes
            .filter((e) => !!e.vid)
            .map((e) => ({ vid: e.vid, index: e.index }));
          if (episodes.length === 0) {
            this.failed += 1;
            this.message = `《${v.title}》没有可下载的分集`;
            continue;
          }

          this.title = detailData.title || v.title;
          this.episodes = episodes.length;
          const report = await api.fanqieDownloadSeries(
            {
              seriesId: v.seriesId,
              title: this.title,
              intro: detailData.intro || "",
              cover: detailData.cover || "",
              episodes,
            },
            root,
          );
          this.done += report.downloaded + report.skipped;
          this.failed += report.failed.length;
          landed.push(report.title);
          this.message = report.failed.length
            ? `《${report.title}》有 ${report.failed.length} 集失败：${report.failed[0]}`
            : `《${report.title}》完成`;
        }

        // 一集都没落地就别往流水线送 —— 空目录只会扫出个错
        if (this.done === 0) {
          this.message = "一集都没下载成功";
          this.error = "没有可跑的源片，已停在列表页；可以重试或换个清晰度。";
          return;
        }
        // 有失败集也别自动开跑：带着缺集去二创，后面全白干。
        // 再点一次下载只会补缺的集（已存在会跳过）。
        if (this.failed > 0) {
          this.message = `有 ${this.failed} 集没下下来`;
          this.error = `有 ${this.failed} 集下载失败，已停在列表页；再点一次「下载」会只补缺的集。`;
          return;
        }

        this.message = "下载完成，正在打开流水线…";
        onDone?.(root, landed);
      } catch (e) {
        this.message = "下载中断";
        this.error = `下载失败：${(e as Error).message}`;
      } finally {
        this.running = false;
        if (unlisten) {
          unlisten();
          unlisten = null;
        }
      }
    },

    /** 收起面板：下载中不收，否则用户又看不到进度了。 */
    dismiss() {
      if (this.running) return;
      this.active = false;
      this.error = "";
    },
  },
});
