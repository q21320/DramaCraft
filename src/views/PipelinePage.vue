<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NCheckbox,
  NInput,
  NInputNumber,
  NModal,
  NSelect,
  NSpace,
  NTag,
  useMessage,
} from "naive-ui";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { convertFileSrc } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { api } from "@/api";
import type { OutputEpisode, PipelineRun, PipelineStageOption, SourceScan } from "@/types";

const message = useMessage();

const stages = ref<PipelineStageOption[]>([]);
const picked = ref<string[]>([]);
const root = ref("");
const outDir = ref("");
const sliceSeconds = ref(40);
const model = ref("small");
const scan = ref<SourceScan | null>(null);
const scanning = ref(false);
const starting = ref(false);
const runs = ref<PipelineRun[]>([]);
const activeRunId = ref<number | null>(null);

/** 产出：集 → 切片 */
const outputs = ref<OutputEpisode[]>([]);
const outputsLoading = ref(false);
/** 展开的集（看它的切片列表） */
const openEpisodes = ref<number[]>([]);
/** 正在内嵌播放的那一片 */
const playing = ref<{ path: string; title: string; cues: string[] } | null>(null);

const activeRun = computed(() => runs.value.find((r) => r.id === activeRunId.value) ?? null);

const totalSlices = computed(() =>
  outputs.value.reduce((n, e) => n + e.slices.length, 0),
);

/**
 * 最后一次成功刷新的时刻。
 *
 * 为什么要显示它：光让数字悄悄变，用户根本看不出来界面是活的
 * —— 上一轮就是这个毛病，库里任务一直在涨，用户以为程序卡死了。
 * 摆一个走动的时钟，「在实时更新」这件事才看得见。
 */
const lastUpdated = ref<Date | null>(null);

/** 有没有正在跑的流水线 —— 决定刷新频率，也决定页面上要不要显示「正在跑」 */
const busy = computed(() => runs.value.some((r) => r.status === "running"));

const lastUpdatedText = computed(() => {
  if (!lastUpdated.value) return "";
  const d = lastUpdated.value;
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
});

function humanSize(bytes: number): string {
  if (bytes <= 0) return "—";
  const mb = bytes / 1024 / 1024;
  return mb >= 1 ? `${mb.toFixed(1)} MB` : `${(bytes / 1024).toFixed(0)} KB`;
}

function humanTime(ms: number): string {
  const s = Math.round(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/** 本地文件要经 asset 协议才能给 webview 播放 */
function fileUrl(path: string): string {
  try {
    return convertFileSrc(path);
  } catch {
    return path;
  }
}

async function loadOutputs(runId: number | null) {
  if (runId === null) {
    outputs.value = [];
    return;
  }
  outputsLoading.value = true;
  try {
    outputs.value = await api.pipelineOutputs(runId);
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    outputsLoading.value = false;
  }
}

const statusLabels: Record<string, string> = {
  pending: "待跑",
  running: "进行中",
  done: "完成",
  failed: "失败",
  skipped: "已跳过",
};

const statusTypes: Record<string, "default" | "info" | "success" | "error" | "warning"> = {
  pending: "default",
  running: "info",
  done: "success",
  failed: "error",
  skipped: "warning",
};

const runStatusLabels: Record<string, string> = {
  running: "运行中",
  done: "已完成",
  failed: "有失败",
  canceled: "已取消",
};

async function refreshRuns() {
  try {
    runs.value = await api.pipelineList(20);
    if (activeRunId.value === null && runs.value.length > 0) {
      activeRunId.value = runs.value[0].id;
    }
    // 顺带刷产出 —— 这样切片一出来就能看见，不用等整条流水线跑完
    await loadOutputs(activeRunId.value);
    lastUpdated.value = new Date();
  } catch (e) {
    message.error((e as Error).message);
  }
}

function toggleEpisode(mediaId: number) {
  openEpisodes.value = openEpisodes.value.includes(mediaId)
    ? openEpisodes.value.filter((id) => id !== mediaId)
    : [...openEpisodes.value, mediaId];
}

async function play(slice: { path: string; indexNo: number }, episodeName: string, cues: string[]) {
  playing.value = {
    path: slice.path,
    title: `${episodeName} · 第 ${slice.indexNo} 片`,
    cues,
  };
}

async function openExternal(path: string) {
  try {
    await api.openInPlayer(path);
  } catch (e) {
    message.error((e as Error).message);
  }
}

async function reveal(path: string) {
  try {
    await api.revealInExplorer(path);
  } catch (e) {
    message.error((e as Error).message);
  }
}

async function pickRoot() {
  const p = await open({ directory: true, multiple: false, title: "选择源目录" });
  if (typeof p !== "string") return;
  root.value = p;
  scan.value = null;
  try {
    outDir.value = await api.pipelineDefaultOutDir(p);
  } catch {
    outDir.value = "";
  }
}

async function pickOut() {
  const p = await open({ directory: true, multiple: false, title: "选择导出目录" });
  if (typeof p === "string") outDir.value = p;
}

async function doScan() {
  if (!root.value.trim()) {
    message.warning("请先选择源目录");
    return;
  }
  scanning.value = true;
  scan.value = null;
  try {
    scan.value = await api.pipelineScan(root.value.trim());
    message.success(
      `扫到 ${scan.value.dramas.length} 部剧，共 ${scan.value.totalEpisodes} 集`,
    );
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    scanning.value = false;
  }
}

async function start() {
  if (!root.value.trim()) {
    message.warning("请先选择源目录");
    return;
  }
  if (picked.value.length === 0) {
    message.warning("至少要勾选一个步骤");
    return;
  }
  starting.value = true;
  try {
    const run = await api.pipelineStart(
      root.value.trim(),
      outDir.value.trim(),
      picked.value,
      sliceSeconds.value,
      model.value,
    );
    activeRunId.value = run.id;
    await refreshRuns();
    // 排完队就开跑 —— 用户点「开始流水线」的意思就是现在开始
    await api.startQueue();
    message.success(`已排入 ${run.dramaCount} 部剧，队列已启动`);
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    starting.value = false;
  }
}

async function cancelRun(id: number) {
  try {
    const n = await api.pipelineCancel(id);
    await refreshRuns();
    message.success(`已取消 ${n} 个待跑任务`);
  } catch (e) {
    message.error((e as Error).message);
  }
}

const kindLabel = (key: string) => stages.value.find((s) => s.key === key)?.label ?? key;

let unlisten: UnlistenFn | null = null;
let timer: number | null = null;

/**
 * 轮询兜底。
 *
 * 只靠 `queue:progress` 事件是不够的 —— 事件丢一次、或者组件挂载晚于
 * 第一条事件，界面就会一直停在初始状态（用户就遇到过：数据库里明明
 * 已经在切片了，界面还显示「待跑·未开始」）。
 * 这个页面本来就是「看着它跑」的，定时刷新才是该有的行为。
 */
function startPolling() {
  stopPolling();
  // 有流水线在跑时快一点，空闲时慢一点省电
  timer = window.setInterval(() => {
    void refreshRuns();
  }, busy.value ? 1500 : 8000);
}

function stopPolling() {
  if (timer !== null) {
    window.clearInterval(timer);
    timer = null;
  }
}

onMounted(async () => {
  try {
    stages.value = await api.pipelineStages();
    picked.value = stages.value.filter((s) => s.enabledByDefault).map((s) => s.key);
  } catch (e) {
    message.error((e as Error).message);
  }
  await refreshRuns();
  startPolling();
  // 忙闲切换时调整刷新频率
  watch(busy, () => startPolling());
  unlisten = await listen("queue:progress", () => {
    void refreshRuns();
  });
});

onUnmounted(() => {
  unlisten?.();
  stopPolling();
});
</script>

<template>
  <div class="dd-card">
    <div class="dd-card-title">流水线</div>
    <div class="dd-card-desc">
      指一个源目录，剩下的按顺序做完：扫描 → 切片 → 出字幕 → 画面处理 → 导出 → 发布包。
      一部剧做完再做下一部。
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 78px">源目录</span>
      <n-input v-model:value="root" size="small" style="width: 420px" placeholder="放着各部剧的文件夹" />
      <n-button size="small" @click="pickRoot">选择</n-button>
      <n-button size="small" :loading="scanning" @click="doScan">扫描</n-button>
    </div>

    <div class="dd-row" style="margin-bottom: 14px">
      <span class="dd-muted" style="width: 78px">导出到</span>
      <n-input v-model:value="outDir" size="small" style="width: 420px" placeholder="留空则用源目录旁边的「二创」" />
      <n-button size="small" @click="pickOut">选择</n-button>
    </div>

    <n-alert v-if="scan" type="success" title="扫描结果" style="margin-bottom: 14px">
      <div class="dd-wrap">
        共 <b>{{ scan.dramas.length }}</b> 部剧、<b>{{ scan.totalEpisodes }}</b> 集
      </div>
      <table class="dd-table" style="margin-top: 8px">
        <thead>
          <tr>
            <th style="width: 200px">剧目</th>
            <th style="width: 70px">集数</th>
            <th style="width: 90px">简介</th>
            <th style="width: 90px">封面</th>
            <th>提示</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="d in scan.dramas" :key="d.dir">
            <td>{{ d.name }}</td>
            <td>{{ d.episodeCount }}</td>
            <td>
              <n-tag size="tiny" :type="d.synopsis ? 'success' : 'warning'" :bordered="false">
                {{ d.synopsis ? "有" : "无" }}
              </n-tag>
            </td>
            <td>
              <n-tag size="tiny" :type="d.cover ? 'success' : 'warning'" :bordered="false">
                {{ d.cover ? "有" : "无" }}
              </n-tag>
            </td>
            <td class="dd-wrap dd-muted">{{ d.warnings.join("；") || "—" }}</td>
          </tr>
        </tbody>
      </table>
      <ul v-if="scan.warnings.length > 0" style="margin: 8px 0 0; padding-left: 18px">
        <li v-for="(w, i) in scan.warnings" :key="i" class="dd-wrap">{{ w }}</li>
      </ul>
    </n-alert>

    <div class="dd-card-title" style="font-size: 13px; margin-bottom: 8px">做哪些步骤</div>
    <div class="dd-row" style="flex-wrap: wrap; gap: 10px; margin-bottom: 14px">
      <div v-for="s in stages" :key="s.key" style="min-width: 240px">
        <n-checkbox
          :checked="picked.includes(s.key)"
          @update:checked="(v: boolean) => {
            picked = v ? [...picked, s.key] : picked.filter((k) => k !== s.key);
          }"
        >
          {{ s.label }}
        </n-checkbox>
        <div class="dd-muted" style="margin-left: 22px">{{ s.detail }}</div>
      </div>
    </div>

    <div class="dd-row" style="margin-bottom: 14px">
      <span class="dd-muted" style="width: 78px">切片时长</span>
      <n-input-number v-model:value="sliceSeconds" size="small" :min="3" :max="600" style="width: 110px" />
      <span class="dd-muted">秒</span>
      <span class="dd-muted" style="margin-left: 14px">字幕模型</span>
      <n-select
        v-model:value="model"
        size="small"
        style="width: 140px"
        :options="[
          { label: 'small（推荐）', value: 'small' },
          { label: 'tiny（快）', value: 'tiny' },
          { label: 'base', value: 'base' },
        ]"
      />
    </div>

    <n-space>
      <n-button size="small" type="primary" :loading="starting" @click="start">
        开始流水线
      </n-button>
      <span class="dd-muted">排完队会自动开跑；关掉应用再打开会接着跑</span>
    </n-space>
  </div>

  <div class="dd-card">
    <div class="dd-row" style="justify-content: space-between; margin-bottom: 10px">
      <div class="dd-card-title">运行记录</div>
      <n-space :size="10" align="center">
        <!-- 「在动」要看得见：光让数字悄悄变，用户会以为卡死了 -->
        <span v-if="busy" class="dd-live">
          <span class="dd-dot" />正在跑
        </span>
        <span v-else class="dd-muted">空闲</span>
        <span class="dd-muted dd-clock">最后更新 {{ lastUpdatedText || "—" }}</span>
        <n-button size="small" @click="refreshRuns">刷新</n-button>
      </n-space>
    </div>

    <div v-if="runs.length === 0" class="dd-empty">还没跑过流水线。</div>

    <n-space v-else vertical :size="12">
      <div v-for="run in runs" :key="run.id" class="dd-card" style="margin: 0">
        <div class="dd-row" style="justify-content: space-between; margin-bottom: 8px">
          <n-space :size="8">
            <n-tag
              size="small"
              :type="run.status === 'done' ? 'success' : run.status === 'failed' ? 'error' : 'info'"
              :bordered="false"
            >
              #{{ run.id }} {{ runStatusLabels[run.status] ?? run.status }}
            </n-tag>
            <n-tag size="small" :bordered="false">{{ run.dramaCount }} 部剧</n-tag>
            <span class="dd-muted dd-mono dd-wrap">{{ run.rootDir }}</span>
          </n-space>
          <n-space :size="8">
            <n-button size="tiny" @click="activeRunId = activeRunId === run.id ? null : run.id">
              {{ activeRunId === run.id ? "收起" : "展开" }}
            </n-button>
            <n-button
              v-if="run.status === 'running'"
              size="tiny"
              @click="cancelRun(run.id)"
            >
              取消
            </n-button>
          </n-space>
        </div>

        <table v-if="activeRunId === run.id" class="dd-table">
          <thead>
            <tr>
              <th style="width: 200px">剧目</th>
              <th style="width: 70px">集数</th>
              <th style="width: 90px">状态</th>
              <th style="width: 90px">到哪一步</th>
              <th>最近一条消息</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="d in run.dramas" :key="d.id">
              <td>{{ d.name }}</td>
              <td>{{ d.episodeCount }}</td>
              <td>
                <n-tag size="tiny" :type="statusTypes[d.status] ?? 'default'" :bordered="false">
                  {{ statusLabels[d.status] ?? d.status }}
                </n-tag>
              </td>
              <td class="dd-muted">{{ d.stage ? kindLabel(d.stage) : "未开始" }}</td>
              <td class="dd-wrap dd-muted">{{ d.message || "—" }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </n-space>

    <n-alert v-if="activeRun" type="info" style="margin-top: 12px" title="说明">
      进度按「一部剧做完再做下一部」推进。某一步失败时，那部剧会标成失败，
      其余步骤不再继续 —— 一步没成，后面的产出没有意义。
      详细的任务列表在「任务队列」页。
    </n-alert>
  </div>

  <!-- 产出：跑出来什么，当场就能看 -->
  <div v-if="activeRun" class="dd-card">
    <div class="dd-row" style="justify-content: space-between; margin-bottom: 4px">
      <div class="dd-card-title">产出 · 运行 #{{ activeRun.id }}</div>
      <n-space :size="8">
        <n-tag size="small" :bordered="false">{{ outputs.length }} 集</n-tag>
        <n-tag size="small" :bordered="false" type="success">{{ totalSlices }} 片</n-tag>
        <n-button size="tiny" :loading="outputsLoading" @click="loadOutputs(activeRunId)">
          刷新
        </n-button>
      </n-space>
    </div>
    <div class="dd-card-desc">
      跑的过程中会自己更新，切出来的片一出现就能点开看。
    </div>

    <div v-if="outputs.length === 0" class="dd-empty">
      还没有产出。开始流水线后会实时出现在这里。
    </div>

    <n-space v-else vertical :size="6">
      <div v-for="ep in outputs" :key="ep.mediaId" class="dd-ep">
        <div class="dd-row dd-ep-head" @click="toggleEpisode(ep.mediaId)">
          <span class="dd-ep-toggle">{{ openEpisodes.includes(ep.mediaId) ? "▾" : "▸" }}</span>
          <span class="dd-ep-name">{{ ep.name }}</span>
          <n-tag size="tiny" :bordered="false">{{ ep.slices.length }} 片</n-tag>
          <span class="dd-muted">{{ humanTime(ep.durationMs) }} · {{ ep.width }}×{{ ep.height }}</span>
        </div>

        <div v-if="openEpisodes.includes(ep.mediaId)" class="dd-slices">
          <div v-if="ep.slices.length === 0" class="dd-muted" style="padding: 4px 10px">
            这一集还没有切片
          </div>
          <div v-for="s in ep.slices" :key="s.segmentId" class="dd-slice">
            <div class="dd-row" style="gap: 8px; flex-wrap: wrap">
              <b style="width: 62px">第 {{ s.indexNo }} 片</b>
              <span class="dd-muted" style="width: 92px">
                {{ humanTime(s.endMs - s.startMs) }}
              </span>
              <span class="dd-muted" style="width: 76px">{{ humanSize(s.sizeBytes) }}</span>
              <n-tag
                size="tiny"
                :type="s.status === 'ok' ? 'success' : 'error'"
                :bordered="false"
              >
                {{ s.status === "ok" ? "成功" : s.status }}
              </n-tag>
              <n-button size="tiny" type="primary" ghost @click="play(s, ep.name, s.cues)">
                播放
              </n-button>
              <n-button size="tiny" @click="openExternal(s.path)">系统播放器</n-button>
              <n-button size="tiny" quaternary @click="reveal(s.path)">打开文件夹</n-button>
            </div>
            <div v-if="s.cues.length > 0" class="dd-cues">
              {{ s.cues.join(" ") }}
            </div>
            <div v-else class="dd-cues dd-muted">（这一段没有字幕）</div>
          </div>
        </div>
      </div>
    </n-space>
  </div>

  <!-- 内嵌播放器：快速过一眼用 -->
  <n-modal
    v-if="playing"
    :show="true"
    preset="card"
    style="width: 720px"
    :title="playing.title"
    @close="playing = null"
  >
    <video
      :src="fileUrl(playing.path)"
      controls
      autoplay
      style="width: 100%; max-height: 60vh; background: #000"
    />
    <div v-if="playing.cues.length > 0" style="margin-top: 10px">
      <div class="dd-muted" style="margin-bottom: 4px">这一段的字幕</div>
      <div class="dd-wrap">{{ playing.cues.join(" ") }}</div>
    </div>
    <template #footer>
      <n-space justify="end">
        <n-button size="small" @click="openExternal(playing.path)">用系统播放器打开</n-button>
        <n-button size="small" @click="reveal(playing.path)">打开文件夹</n-button>
      </n-space>
    </template>
  </n-modal>
</template>

<style scoped>
/* 「正在跑」的呼吸点 */
.dd-live {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
  color: #18a058;
}
.dd-dot {
  width: 7px;
  height: 7px;
  border-radius: 50%;
  background: #18a058;
  animation: dd-pulse 1.1s ease-in-out infinite;
}
@keyframes dd-pulse {
  0%,
  100% {
    opacity: 1;
    transform: scale(1);
  }
  50% {
    opacity: 0.25;
    transform: scale(0.7);
  }
}
/* 时钟用等宽数字，跳动时不会左右晃 */
.dd-clock {
  font-size: 12px;
  font-variant-numeric: tabular-nums;
}
.dd-ep {
  border: 1px solid rgba(128, 128, 128, 0.22);
  border-radius: 6px;
  overflow: hidden;
}
.dd-ep-head {
  gap: 10px;
  padding: 7px 10px;
  cursor: pointer;
  align-items: center;
}
.dd-ep-head:hover {
  background: rgba(128, 128, 128, 0.09);
}
.dd-ep-toggle {
  width: 12px;
  opacity: 0.6;
}
.dd-ep-name {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.dd-slices {
  border-top: 1px solid rgba(128, 128, 128, 0.16);
  padding: 6px 0;
}
.dd-slice {
  padding: 6px 10px;
}
.dd-slice + .dd-slice {
  border-top: 1px dashed rgba(128, 128, 128, 0.16);
}
.dd-cues {
  margin-top: 4px;
  margin-left: 70px;
  font-size: 12px;
  opacity: 0.8;
  line-height: 1.5;
}
</style>
