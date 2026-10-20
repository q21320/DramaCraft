import { defineStore } from "pinia";
import { listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { api } from "@/api";
import type { FanqieDownloadState } from "@/types";

/** 列表里挑出来要下载的剧 —— 只需要下载用得到的字段。 */
export interface DownloadPick {
  seriesId: string;
  title: string;
  episodeCount?: number;
}

/**
 * 进度事件监听只需要装一次。运行时句柄不用存 —— 面板和 store
 * 跟着应用生命周期活，不存在局部卸载后要解绑的情况。
 */
let bindStarted = false;
/** running 时的兜底轮询：事件万一漏了，面板也不会卡在旧进度上 */
let pollTimer: number | null = null;

/**
 * 批量下载的状态与流程。
 *
 * ## 为什么推进的循环在后端
 *
 * 以前这里是前端 `for` 循环逐部调命令 —— 循环活在页面里，刷新一下
 * 循环就没了：右下角面板消失、进度归零，看起来像下载被取消了。
 * 现在整批交给 `fanqie_download_batch`，Rust 那边一部部推；
 * 状态存在 `AppState.download` 并落一份到 setting：
 *
 * - 页面刷新：重新挂载时 `restore()` 读回状态，面板接着显示；
 * - 切页面：本来就不影响；
 * - 应用关掉：下次启动读到中断标记，告诉用户停在哪。
 */
export const useDownloadStore = defineStore("download", {
  state: () => ({
    /** 面板是否可见：下载中一直显示；整批成功几秒后自动收起，失败时保留等用户关 */
    active: false,
    /** 是否已经跟后端对过一次状态 */
    restored: false,
    /** 流程是否还在跑 */
    running: false,
    seriesIndex: 0,
    seriesTotal: 0,
    title: "",
    /** 当前这部剧已处理完的集数 */
    episode: 0,
    /** 当前这部剧的总集数 */
    episodes: 0,
    /** 整批累计「下好 + 跳过」 */
    done: 0,
    /** 整批累计失败项 */
    failed: 0,
    message: "",
    root: "",
    error: "",
    /** 实际落盘的剧名，下载完交给流水线只跑这几部 */
    landed: [] as string[],
    /** 此刻正在下载的集：并发 3 时会同时列出三集 */
    inflight: [] as string[],
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
    /** 把后端给的整份状态套进来。事件、轮询、恢复都走这一条路。 */
    apply(s: FanqieDownloadState) {
      this.running = s.running;
      this.seriesIndex = s.seriesIndex;
      this.seriesTotal = s.seriesTotal;
      this.title = s.title;
      this.episode = s.episode;
      this.episodes = s.episodes;
      this.done = s.done;
      this.failed = s.failed;
      this.message = s.message;
      this.root = s.root;
      this.error = s.error;
      this.landed = s.landed ?? [];
      this.inflight = s.inflight ?? [];
    },

    /** 监听后端进度；重复调用无副作用。 */
    bind() {
      if (bindStarted) return;
      bindStarted = true;
      void listen<FanqieDownloadState>("fanqie:download", (e) => {
        const s = e.payload;
        this.apply(s);
        if (s.running) {
          this.active = true;
          this.startPolling();
          return;
        }
        this.stopPolling();
        // 结束：有错误就留在屏幕上让人看清原因；全部成功则几秒后自己收起
        if (s.error) this.active = true;
        else if (this.done > 0 && this.failed === 0) this.hideLater();
      })
        .catch(() => {
          // 监听挂了不影响下载本身，轮询还会兜底
          bindStarted = false;
        });
    },

    /**
     * 页面刷新 / 应用重开后的第一次对齐。
     *
     * 正在跑就把面板重新摆出来；上次被关掉时后端留了中断说明，
     * 也弹出来告诉用户「再点一次下载会只补缺的集」。
     */
    async restore() {
      if (this.restored) return;
      this.restored = true;
      this.bind();
      try {
        const s = await api.fanqieDownloadState();
        this.apply(s);
        if (s.running) {
          this.active = true;
          this.startPolling();
        } else if (s.error) {
          this.active = true;
        }
      } catch {
        // 读不到（例如在浏览器里预览）就当没下过
      }
    },

    /** running 时的兜底轮询：事件漏了也能把进度接上。 */
    startPolling() {
      if (pollTimer !== null) return;
      pollTimer = window.setInterval(() => void this.sync(), 1500);
    },

    stopPolling() {
      if (pollTimer !== null) {
        window.clearInterval(pollTimer);
        pollTimer = null;
      }
    },

    async sync() {
      try {
        const s = await api.fanqieDownloadState();
        this.apply(s);
        if (s.running) {
          this.active = true;
          return;
        }
        this.stopPolling();
        if (s.error) this.active = true;
        else if (this.done > 0 && this.failed === 0) this.hideLater();
      } catch {
        // 一次读不到不算错，下一轮再说
      }
    },

    /**
     * 下载选中的剧：整批交给后端，一部部落到 `<下载目录>/<剧名>/`。
     * 全部落地后调用 `onDone(root, titles)` —— titles 是**实际落盘的剧名**，
     * 由页面带去流水线，让它只跑刚下的这几部（下载根里还有以前下的剧）。
     */
    async run(list: DownloadPick[], onDone?: (root: string, titles: string[]) => void) {
      // 刷新后还没跟后端对过状态：先对一次，否则可能把正在跑的那批
      // 当成「没在下载」又发起一批（后端会拒，但用户会白看到一句错）。
      if (!this.restored) await this.restore();
      this.bind();
      // 已经有一批在跑时再点下载 → 不开第二份
      if (this.running) return;
      if (list.length === 0) {
        this.error = "先勾选要下载的短剧";
        this.active = true;
        return;
      }

      // 面板先摆出来：后端第一条进度可能在一两秒之后
      this.active = true;
      this.error = "";
      this.message = "正在准备下载目录…";
      this.landed = [];
      this.done = 0;
      this.failed = 0;
      this.episode = 0;
      this.episodes = 0;
      this.title = "";
      this.seriesIndex = 0;
      this.seriesTotal = list.length;

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

        this.running = true;
        this.startPolling();

        // 这一行会一直等到整批结束 —— 但推进的是 Rust：
        // 页面刷新、切走都不影响它继续跑，回来读状态就能接上。
        await api.fanqieDownloadBatch(
          list.map((v) => ({
            seriesId: v.seriesId,
            title: v.title,
            episodeCount: v.episodeCount ?? 0,
          })),
          root,
          // 锁死 1080p：平台最高档，给后期裁切/去重留余量。
          // 平台没有 1080p 时后端按高清逐档退（720 → 540 …），不会悄悄落成 480p；
          // 目录里以前按低清下过的集会被验出分辨率不够，自动删掉重下。
          "1080p",
        );

        // 以后端状态为准：事件可能错过最后一两条
        const final = await api.fanqieDownloadState();
        this.apply(final);

        // 一集都没落地 / 有失败：别往流水线送，空目录或缺集只会让后面全白干
        if (final.landed.length === 0 || final.failed > 0 || final.error) return;

        this.message = "下载完成，正在打开整剧处理…";
        this.active = true;
        onDone?.(final.root || root, final.landed);
      } catch (e) {
        this.message = "下载中断";
        this.error = `下载失败：${(e as Error).message}`;
        this.active = true;
      } finally {
        this.running = false;
        this.stopPolling();
        // 整批成功后面板自己收起，留几秒把「下载完成」看清；
        // 有失败/错误时留着，得让人看到原因
        if (!this.error && this.failed === 0) this.hideLater();
      }
    },

    /** 延迟收起面板。定时器到点时会重新确认状态 —— 这中间可能又开了一批。 */
    hideLater(delay = 3000) {
      window.setTimeout(() => {
        if (!this.running && !this.error && this.failed === 0) this.active = false;
      }, delay);
    },

    /** 收起面板：下载中不收，否则用户又看不到进度了。 */
    dismiss() {
      if (this.running) return;
      this.active = false;
      this.error = "";
      // 后端那份也清掉，否则刷新页面又会把旧错误弹出来
      void api.fanqieDownloadDismiss().catch(() => {});
    },
  },
});
