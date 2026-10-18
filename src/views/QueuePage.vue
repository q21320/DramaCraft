<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { NAlert, NButton, NCheckbox, NInput, NInputNumber, NSelect, NSpace, NTag, useMessage } from "naive-ui";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { api } from "@/api";
import { useAppStore } from "@/stores/app";
import type { Job, QueueProgress, QueueReport, QueueState } from "@/types";
import { formatDuration } from "@/utils/format";

const store = useAppStore();
const message = useMessage();

const state = ref<QueueState | null>(null);
const jobs = ref<Job[]>([]);
const busy = ref(false);
const lastReport = ref<QueueReport | null>(null);
const currentJob = ref<QueueProgress | null>(null);

// 批量入队
const pickedMedia = ref<number[]>([]);
const kind = ref("slice");
const params = ref({
  parts: 10,
  outDir: "",
  intensity: "medium",
  ocrOverwrite: false,
  asrModel: "small",
  asrLanguage: "zh",
  exportAspect: "verticalBlur",
  exportBurn: true,
  draftRoot: "",
  draftName: "",
  draftFps: 30,
  platform: "douyin",
  publishOutRoot: "",
});

const kindOptions = [
  { label: "切片", value: "slice" },
  { label: "画面处理", value: "transform" },
  { label: "硬字幕识别", value: "ocr" },
  { label: "语音转字幕（本地）", value: "asr" },
  { label: "云端转字幕", value: "cloud" },
  { label: "导出成片", value: "export" },
  { label: "剪映草稿", value: "draft" },
  { label: "发布包", value: "publish" },
  { label: "效果验证", value: "verify" },
];

const mediaOptions = computed(() =>
  store.media.map((m) => ({
    label: `${m.name}（${formatDuration(m.durationMs)}）`,
    value: m.id,
  })),
);

const statusLabels: Record<string, string> = {
  pending: "待跑",
  running: "进行中",
  done: "完成",
  failed: "失败",
  canceled: "已取消",
};

const statusTypes: Record<string, "default" | "info" | "success" | "error" | "warning"> = {
  pending: "default",
  running: "info",
  done: "success",
  failed: "error",
  canceled: "warning",
};

function kindLabel(k: string): string {
  return kindOptions.find((o) => o.value === k)?.label ?? k;
}

function mediaName(id: number | null): string {
  if (id === null) return "—";
  return store.media.find((m) => m.id === id)?.name ?? `#${id}`;
}

async function refresh() {
  try {
    const [s, list] = await Promise.all([api.queueState(), api.listJobs(300)]);
    state.value = s;
    jobs.value = list;
  } catch (e) {
    message.error((e as Error).message);
  }
}

function buildPayload(): Record<string, unknown> {
  const p = params.value;
  switch (kind.value) {
    case "slice":
      return { mode: { mode: "equalParts", parts: p.parts }, outDir: p.outDir };
    case "transform":
      return { intensity: p.intensity };
    case "ocr":
      return { overwrite: p.ocrOverwrite };
    case "asr":
      return { model: p.asrModel, language: p.asrLanguage };
    case "export":
      return {
        mediaId: 0,
        // 这里填的是**目录**；后端会按素材名生成各自的文件名，避免互相覆盖
        outPath: p.outDir,
        burnSubtitles: p.exportBurn,
        aspect: { mode: p.exportAspect },
        startMs: 0,
        endMs: 0,
        crf: 23,
        preset: "veryfast",
      };
    case "draft":
      return {
        draftRoot: p.draftRoot,
        draftName: p.draftName,
        fps: p.draftFps,
      };
    case "publish":
      return { platform: p.platform, outRoot: p.publishOutRoot };
    default:
      return {};
  }
}

async function pickDir(field: "outDir" | "draftRoot" | "publishOutRoot") {
  const picked = await open({ directory: true, multiple: false, title: "选择目录" });
  if (typeof picked === "string") params.value[field] = picked;
}

async function enqueue() {
  if (pickedMedia.value.length === 0) {
    message.warning("请先选择要处理的素材");
    return;
  }
  busy.value = true;
  try {
    const count = await api.enqueueBatch(pickedMedia.value, kind.value, buildPayload());
    await refresh();
    message.success(`已加入队列 ${count} 条`);
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

async function start() {
  busy.value = true;
  try {
    await api.startQueue();
    await refresh();
    message.success("队列已启动");
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

async function togglePause() {
  if (!state.value) return;
  try {
    await api.setQueuePaused(!state.value.paused);
    await refresh();
  } catch (e) {
    message.error((e as Error).message);
  }
}

async function stop() {
  try {
    await api.requestQueueStop();
    stopRequested.value = true;
    message.info("已请求停止，当前任务跑完后会停下");
  } catch (e) {
    message.error((e as Error).message);
  }
}

async function cancelPending() {
  try {
    const n = await api.cancelPendingJobs();
    await refresh();
    message.success(`已取消 ${n} 条待跑任务`);
  } catch (e) {
    message.error((e as Error).message);
  }
}

async function clearFinished() {
  try {
    const n = await api.clearFinishedJobs();
    await refresh();
    message.success(`已清理 ${n} 条已结束任务`);
  } catch (e) {
    message.error((e as Error).message);
  }
}

let unlisten: UnlistenFn | null = null;
let finishedUnlisten: UnlistenFn | null = null;
let timer: number | null = null;

/**
 * 用户已经按过「停止」。
 *
 * 停止不是立刻生效（当前任务会跑完），这段时间队列还是 `running`。
 * 不把「已经请求停了」显示出来，界面就还是「运行中」——
 * 用户会以为按钮没反应。
 */
const stopRequested = ref(false);

onMounted(async () => {
  if (store.media.length > 0) pickedMedia.value = [store.media[0].id];
  await refresh();

  unlisten = await listen<QueueProgress>("queue:progress", (event) => {
    currentJob.value = event.payload;
    void refresh();
  });
  finishedUnlisten = await listen<QueueReport & { error?: string }>("queue:finished", (event) => {
    currentJob.value = null;
    stopRequested.value = false;
    if (event.payload?.error) {
      message.error(event.payload.error);
    } else {
      lastReport.value = event.payload as QueueReport;
      message.success(
        `队列结束：完成 ${event.payload.done}，失败 ${event.payload.failed}，剩余 ${event.payload.remaining}`,
      );
    }
    void refresh();
  });

  // 轮询兜底。
  //
  // 只靠事件不够：队列停下的那条 `queue:finished` 只要漏一次（切页面、
  // 组件刚挂载、事件先到），这一页就会永远停在上一次的快照上 ——
  // 用户看到的就是「日志已经不跑了，页面还写着运行中、按钮还是停止」。
  timer = window.setInterval(() => void refresh(), 2000);
});

onUnmounted(() => {
  unlisten?.();
  finishedUnlisten?.();
  if (timer !== null) window.clearInterval(timer);
});
</script>

<template>
  <div class="dd-card">
    <div class="dd-row" style="justify-content: space-between">
      <div>
        <div class="dd-card-title">任务队列</div>
        <div class="dd-card-desc">
          把长任务排成一队顺序跑。状态存在本地数据库里，关掉应用再打开会接着跑。
        </div>
      </div>
      <n-space :size="8">
        <n-tag v-if="state?.running" size="small" :type="stopRequested ? 'warning' : 'info'" :bordered="false">
          {{ stopRequested ? "正在停（跑完这条就停）" : "运行中" }}
        </n-tag>
        <n-tag v-if="state?.paused" size="small" type="warning" :bordered="false">已暂停</n-tag>
        <n-tag v-if="currentJob" size="small" :bordered="false">
          正在跑：{{ kindLabel(currentJob.kind) }} #{{ currentJob.jobId }}
        </n-tag>
      </n-space>
    </div>

    <div v-if="state" class="dd-row" style="margin-bottom: 12px">
      <n-tag size="small" :bordered="false">待跑 {{ state.counts.pending }}</n-tag>
      <n-tag size="small" type="info" :bordered="false">进行中 {{ state.counts.running }}</n-tag>
      <n-tag size="small" type="success" :bordered="false">完成 {{ state.counts.done }}</n-tag>
      <n-tag size="small" type="error" :bordered="false">失败 {{ state.counts.failed }}</n-tag>
      <n-tag v-if="state.counts.canceled > 0" size="small" :bordered="false">
        已取消 {{ state.counts.canceled }}
      </n-tag>
    </div>

    <n-space>
      <n-button size="small" type="primary" :loading="busy" :disabled="state?.running" @click="start">
        启动队列
      </n-button>
      <n-button size="small" :disabled="!state?.running" @click="togglePause">
        {{ state?.paused ? "继续" : "暂停" }}
      </n-button>
      <n-button size="small" :disabled="!state?.running" @click="stop">停止</n-button>
      <n-button size="small" :disabled="(state?.counts.pending ?? 0) === 0" @click="cancelPending">
        取消待跑
      </n-button>
      <n-button size="small" @click="clearFinished">清理已结束</n-button>
    </n-space>

    <n-alert type="info" style="margin-top: 12px" title="关于「停止」">
      停止是跑完当前这条就不再取新的，不会把正在编码的 ffmpeg 砍掉 —— 半途杀进程只会留下半个文件。
      如果需要立刻中断，直接关掉应用；下次启动时这条任务会被退回队列重跑。
    </n-alert>
  </div>

  <div class="dd-card">
    <div class="dd-card-title">批量加入队列</div>
    <div class="dd-card-desc">选多个素材、选一个操作，一次排好队。</div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 64px">素材</span>
      <n-select
        v-model:value="pickedMedia"
        multiple
        :options="mediaOptions"
        style="width: 460px"
        placeholder="可多选"
      />
      <span class="dd-muted">已选 {{ pickedMedia.length }} 个</span>
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 64px">操作</span>
      <n-select v-model:value="kind" :options="kindOptions" style="width: 180px" />
    </div>

    <!-- 各操作的参数 -->
    <div v-if="kind === 'slice'" class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 64px">参数</span>
      <span class="dd-muted">等分</span>
      <n-input-number v-model:value="params.parts" size="small" :min="1" :max="2000" style="width: 120px" />
      <span class="dd-muted">份，输出到</span>
      <n-input v-model:value="params.outDir" size="small" style="width: 300px" placeholder="输出目录" />
      <n-button size="small" @click="pickDir('outDir')">选择</n-button>
    </div>

    <div v-else-if="kind === 'transform'" class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 64px">参数</span>
      <n-select
        v-model:value="params.intensity"
        size="small"
        style="width: 220px"
        :options="[
          { label: '轻微', value: 'light' },
          { label: '中等', value: 'medium' },
          { label: '明显', value: 'strong' },
        ]"
      />
      <span class="dd-muted">每个片段的参数按序号不同，同一批可复现</span>
    </div>

    <div v-else-if="kind === 'ocr'" class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 64px">参数</span>
      <n-checkbox v-model:checked="params.ocrOverwrite">覆盖已有字幕</n-checkbox>
      <span class="dd-muted">不勾选时，已有字幕的素材会失败而不是被冲掉</span>
    </div>

    <div v-else-if="kind === 'asr'" class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 64px">参数</span>
      <n-select
        v-model:value="params.asrModel"
        size="small"
        style="width: 160px"
        :options="[
          { label: 'tiny', value: 'tiny' },
          { label: 'base', value: 'base' },
          { label: 'small', value: 'small' },
          { label: 'medium', value: 'medium' },
        ]"
      />
      <n-select
        v-model:value="params.asrLanguage"
        size="small"
        style="width: 130px"
        :options="[
          { label: '中文', value: 'zh' },
          { label: '英文', value: 'en' },
          { label: '自动', value: 'auto' },
        ]"
      />
    </div>

    <div v-else-if="kind === 'export'" class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 64px">参数</span>
      <n-select
        v-model:value="params.exportAspect"
        size="small"
        style="width: 210px"
        :options="[
          { label: '保持原画幅', value: 'keep' },
          { label: '竖屏（模糊背景）', value: 'verticalBlur' },
          { label: '竖屏（裁切填充）', value: 'verticalCrop' },
        ]"
      />
      <n-checkbox v-model:checked="params.exportBurn">烧字幕</n-checkbox>
      <n-input v-model:value="params.outDir" size="small" style="width: 260px" placeholder="输出目录" />
      <n-button size="small" @click="pickDir('outDir')">选择</n-button>
      <span class="dd-muted">每个素材导出为 out.mp4 / 002.mp4 … 会按素材名区分</span>
    </div>

    <div v-else-if="kind === 'draft'" class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 64px">参数</span>
      <n-input v-model:value="params.draftRoot" size="small" style="width: 300px" placeholder="剪映草稿根目录" />
      <n-button size="small" @click="pickDir('draftRoot')">选择</n-button>
      <n-input v-model:value="params.draftName" size="small" style="width: 180px" placeholder="草稿名前缀（可空）" />
      <span class="dd-muted">会自动按素材名区分，避免互相覆盖</span>
    </div>

    <div v-else-if="kind === 'publish'" class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 64px">参数</span>
      <n-select
        v-model:value="params.platform"
        size="small"
        style="width: 180px"
        :options="[
          { label: '抖音', value: 'douyin' },
          { label: '微信视频号', value: 'wechat_channels' },
          { label: 'B站', value: 'bilibili' },
          { label: '快手', value: 'kuaishou' },
        ]"
      />
      <n-input v-model:value="params.publishOutRoot" size="small" style="width: 300px" placeholder="发布包输出目录" />
      <n-button size="small" @click="pickDir('publishOutRoot')">选择</n-button>
    </div>

    <div v-else class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 64px">参数</span>
      <span class="dd-muted">这个操作不需要额外参数</span>
    </div>

    <n-space>
      <n-button size="small" type="primary" :loading="busy" @click="enqueue">
        加入队列
      </n-button>
    </n-space>
  </div>

  <div v-if="lastReport" class="dd-card">
    <div class="dd-card-title">上次运行结果</div>
    <div class="dd-card-desc">
      处理 {{ lastReport.processed }} 条：完成 {{ lastReport.done }}，失败 {{ lastReport.failed }}，
      剩余 {{ lastReport.remaining }}。停止原因：{{ lastReport.stoppedBecause }}
    </div>
    <n-alert v-if="lastReport.messages.length > 0" type="default" title="明细（最多 50 条）">
      <ul style="margin: 0; padding-left: 18px">
        <li v-for="(m, i) in lastReport.messages" :key="i" class="dd-wrap">{{ m }}</li>
      </ul>
    </n-alert>
  </div>

  <div class="dd-card">
    <div class="dd-card-title">任务列表</div>
    <div v-if="jobs.length === 0" class="dd-empty">队列是空的。上面选素材和操作，加入队列。</div>
    <div v-else class="dd-scroll" style="max-height: 420px">
      <table class="dd-table">
        <thead>
          <tr>
            <th style="width: 60px">#</th>
            <th style="width: 110px">操作</th>
            <th style="width: 200px">素材</th>
            <th style="width: 80px">状态</th>
            <th style="width: 60px">尝试</th>
            <th>结果 / 原因</th>
            <th style="width: 150px">更新时间</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="j in jobs" :key="j.id">
            <td>{{ j.id }}</td>
            <td>{{ kindLabel(j.kind) }}</td>
            <td class="dd-wrap">{{ mediaName(j.mediaId) }}</td>
            <td>
              <n-tag size="tiny" :type="statusTypes[j.status] ?? 'default'" :bordered="false">
                {{ statusLabels[j.status] ?? j.status }}
              </n-tag>
            </td>
            <td class="dd-muted">{{ j.attempts }}</td>
            <td class="dd-wrap">{{ j.message || "—" }}</td>
            <td class="dd-muted">{{ j.updatedAt }}</td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>
</template>
