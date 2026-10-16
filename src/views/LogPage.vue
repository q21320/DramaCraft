<script setup lang="ts">
/**
 * 运行日志页。
 *
 * 为什么单独一页：流水线跑起来是几十上百个任务，用户需要能**回头看**
 * 「刚才到底做了什么、卡在哪一步」。任务表看的是状态，日志看的是过程。
 *
 * 两个视图：
 * - 日志：每一步的文字输出（扫到什么、导入多少、哪一步成功/失败）
 * - 产出：跑出来的片子，点开就能播
 *
 * 刻意从流水线页搬出来 —— 那页只该管「开始跑」，不该又长又杂。
 */
import { computed, onMounted, onUnmounted, ref, watch, nextTick } from "vue";
import {
  NAlert,
  NButton,
  NCheckbox,
  NModal,
  NSelect,
  NSpace,
  NTag,
  useMessage,
} from "naive-ui";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { convertFileSrc } from "@tauri-apps/api/core";
import { api } from "@/api";
import type { OutputEpisode, PipelineRun, RunLog } from "@/types";

const message = useMessage();

const tab = ref<"log" | "output">("log");

// ---- 日志 ----
const logs = ref<RunLog[]>([]);
const runs = ref<PipelineRun[]>([]);
const pickedRun = ref<number | null>(null);
const follow = ref(true);
const autoScroll = ref(true);
const boxRef = ref<HTMLElement | null>(null);

// ---- 产出 ----
const outputs = ref<OutputEpisode[]>([]);
const openEpisodes = ref<number[]>([]);
const playing = ref<{ path: string; title: string; cues: string[] } | null>(null);

const busy = computed(() => runs.value.some((r) => r.status === "running"));
const totalSlices = computed(() => outputs.value.reduce((n, e) => n + e.slices.length, 0));

const runOptions = computed(() => [
  { label: "全部运行", value: 0 },
  ...runs.value.map((r) => ({
    label: `#${r.id} ${r.status === "done" ? "已完成" : r.status === "running" ? "运行中" : r.status} · ${r.dramaCount} 部`,
    value: r.id,
  })),
]);

const levelType: Record<string, "default" | "success" | "warning" | "error"> = {
  info: "default",
  ok: "success",
  warn: "warning",
  error: "error",
};

function humanSize(bytes: number): string {
  if (bytes <= 0) return "—";
  const mb = bytes / 1024 / 1024;
  return mb >= 1 ? `${mb.toFixed(1)} MB` : `${(bytes / 1024).toFixed(0)} KB`;
}

function humanTime(ms: number): string {
  const s = Math.round(ms / 1000);
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/** 时间戳只显示时分秒 —— 日期对看日志没帮助，还占地方 */
function clockOf(iso: string): string {
  const m = iso.match(/(\d{2}):(\d{2}):(\d{2})/);
  return m ? `${m[1]}:${m[2]}:${m[3]}` : iso;
}

function fileUrl(path: string): string {
  try {
    return convertFileSrc(path);
  } catch {
    return path;
  }
}

/**
 * 拉新日志。用 afterId 增量取，不重复搬已经看过的行。
 *
 * **必须防并发。** 触发源有两个（轮询定时器 + `queue:progress` 事件），
 * 并行跑任务时事件密集触发，于是好几个 pull 同时在飞：
 * 它们各自读到**同一个** `lastId`、取回**同一批**数据、各追加一次 ——
 * 日志页就把同一行渲染了 5 遍，看起来像"重复处理了 5 次"。
 * 时间戳一模一样就是铁证：真跑 5 次时间不可能相同。
 *
 * 三层防护：拉取中不重入、按 id 去重、事件攒一下再拉。
 */
let pulling = false;
/** 事件触发的合并定时器 */
let coalesceTimer: number | null = null;

async function pullLogs(reset = false) {
  // 已经在拉了就别叠 —— 叠上去只会重复追加
  if (pulling) return;
  pulling = true;
  try {
    const rid = pickedRun.value === 0 ? null : pickedRun.value;
    if (reset) {
      logs.value = [];
    }
    // 取值和请求放在同一段里，别跨 await
    const since = reset ? 0 : logs.value.length ? logs.value[logs.value.length - 1].id : 0;
    const fresh = await api.listLogs(rid, since, 800);
    if (fresh.length > 0) {
      // 双保险：按 id 去重。并发窗口之外也可能有重复（比如切换运行时的竞态）
      const seen = new Set(logs.value.map((l) => l.id));
      const add = fresh.filter((l) => !seen.has(l.id));
      if (add.length > 0) {
        logs.value = [...logs.value, ...add];
        // 日志太多时砍掉前面的，免得界面越用越卡
        if (logs.value.length > 4000) logs.value = logs.value.slice(-3000);
        if (autoScroll.value) {
          await nextTick();
          boxRef.value?.scrollTo({ top: boxRef.value.scrollHeight });
        }
      }
    }
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    pulling = false;
  }
}

/**
 * 事件触发用的入口：**攒 300ms 再拉**。
 *
 * 一批 4 个任务并发跑，每个任务开始/结束都发事件 —— 一次刷十几条。
 * 每条都拉一次既浪费又容易踩并发，合并成一次就够。
 */
function triggerPull() {
  if (coalesceTimer !== null) return;
  coalesceTimer = window.setTimeout(() => {
    coalesceTimer = null;
    void pullLogs();
  }, 300);
}

async function refreshRuns() {
  try {
    runs.value = await api.pipelineList(20);
  } catch {
    /* 日志页不因为这个弹错 */
  }
}

async function loadOutputs() {
  const rid = pickedRun.value === 0 ? (runs.value[0]?.id ?? null) : pickedRun.value;
  if (rid === null) {
    outputs.value = [];
    return;
  }
  try {
    outputs.value = await api.pipelineOutputs(rid);
  } catch (e) {
    message.error((e as Error).message);
  }
}

function toggleEpisode(mediaId: number) {
  openEpisodes.value = openEpisodes.value.includes(mediaId)
    ? openEpisodes.value.filter((id) => id !== mediaId)
    : [...openEpisodes.value, mediaId];
}

async function reveal(path: string) {
  try {
    await api.revealInExplorer(path);
  } catch (e) {
    message.error((e as Error).message);
  }
}

async function openExternal(path: string) {
  try {
    await api.openInPlayer(path);
  } catch (e) {
    message.error((e as Error).message);
  }
}

async function clearAll() {
  try {
    const n = await api.clearLogs();
    logs.value = [];
    message.success(`清掉了 ${n} 行日志`);
  } catch (e) {
    message.error((e as Error).message);
  }
}

let unlisten: UnlistenFn | null = null;
let timer: number | null = null;

function startPolling() {
  stopPolling();
  timer = window.setInterval(() => {
    void refreshRuns();
    if (tab.value === "log" && follow.value) void pullLogs();
  }, busy.value ? 1500 : 5000);
}

function stopPolling() {
  if (timer !== null) {
    window.clearInterval(timer);
    timer = null;
  }
}

watch(pickedRun, () => {
  void pullLogs(true);
  void loadOutputs();
});

watch(busy, () => startPolling());
watch(tab, (v) => {
  if (v === "output") void loadOutputs();
});

onMounted(async () => {
  await refreshRuns();
  await pullLogs(true);
  await loadOutputs();
  startPolling();
  unlisten = await listen("queue:progress", () => {
    void refreshRuns();
    if (tab.value === "log" && follow.value) triggerPull();
  });
});

onUnmounted(() => {
  unlisten?.();
  stopPolling();
});
</script>

<template>
  <div class="dd-card">
    <div class="dd-row" style="justify-content: space-between; margin-bottom: 4px">
      <div class="dd-card-title">运行日志</div>
      <n-space :size="10" align="center">
        <span v-if="busy" class="dd-live"><span class="dd-dot" />正在跑</span>
        <span v-else class="dd-muted">空闲</span>
        <n-select
          v-model:value="pickedRun"
          size="small"
          style="width: 190px"
          :options="runOptions"
          placeholder="全部运行"
        />
        <n-button size="small" @click="clearAll">清空</n-button>
      </n-space>
    </div>
    <div class="dd-card-desc">
      跑流水线时这里会一步步写下来：扫到什么、导入了多少、哪一步成没成。
      <b>卡住的时候先看这里</b> —— 状态只是结果，日志才有过程。
    </div>

    <n-space :size="8" style="margin-bottom: 10px">
      <n-button size="small" :type="tab === 'log' ? 'primary' : 'default'" @click="tab = 'log'">
        日志
      </n-button>
      <n-button size="small" :type="tab === 'output' ? 'primary' : 'default'" @click="tab = 'output'">
        产出
        <span v-if="totalSlices > 0" class="dd-muted">（{{ outputs.length }} 集 / {{ totalSlices }} 片）</span>
      </n-button>
    </n-space>

    <!-- 日志 -->
    <template v-if="tab === 'log'">
      <div class="dd-row" style="gap: 14px; margin-bottom: 8px">
        <n-checkbox v-model:checked="follow">实时跟随</n-checkbox>
        <n-checkbox v-model:checked="autoScroll">自动滚到底</n-checkbox>
        <span class="dd-muted">共 {{ logs.length }} 行</span>
      </div>

      <div ref="boxRef" class="dd-logbox">
        <div v-if="logs.length === 0" class="dd-muted" style="padding: 12px">
          还没有日志。跑一次流水线就有了。
        </div>
        <div v-for="l in logs" :key="l.id" class="dd-logline" :class="'lv-' + l.level">
          <span class="dd-logtime">{{ clockOf(l.createdAt) }}</span>
          <n-tag size="tiny" :type="levelType[l.level] ?? 'default'" :bordered="false">
            {{ l.level }}
          </n-tag>
          <span class="dd-logmsg">{{ l.message }}</span>
        </div>
      </div>
    </template>

    <!-- 产出 -->
    <template v-else>
      <div v-if="outputs.length === 0" class="dd-empty">这次运行还没有产出。</div>
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
                <span class="dd-muted" style="width: 92px">{{ humanTime(s.endMs - s.startMs) }}</span>
                <span class="dd-muted" style="width: 76px">{{ humanSize(s.sizeBytes) }}</span>
                <n-tag size="tiny" :type="s.status === 'ok' ? 'success' : 'error'" :bordered="false">
                  {{ s.status === "ok" ? "成功" : s.status }}
                </n-tag>
                <n-button size="tiny" type="primary" ghost @click="playing = { path: s.path, title: ep.name + ' · 第 ' + s.indexNo + ' 片', cues: s.cues }">
                  播放
                </n-button>
                <n-button size="tiny" @click="openExternal(s.path)">系统播放器</n-button>
                <n-button size="tiny" quaternary @click="reveal(s.path)">打开文件夹</n-button>
              </div>
              <div v-if="s.cues.length > 0" class="dd-cues">{{ s.cues.join(" ") }}</div>
              <div v-else class="dd-cues dd-muted">（这一段没有字幕）</div>
            </div>
          </div>
        </div>
      </n-space>
    </template>

    <n-alert v-if="tab === 'output'" type="info" style="margin-top: 12px">
      产出按「这次运行自己的工程」查 —— 同一个源目录跑过多次也不会串。
    </n-alert>
  </div>

  <n-modal
    v-if="playing"
    :show="true"
    preset="card"
    style="width: 720px"
    :title="playing.title"
    @close="playing = null"
  >
    <video :src="fileUrl(playing.path)" controls autoplay style="width: 100%; max-height: 60vh; background: #000" />
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
/* 日志框：等宽字体 + 深色底，长文本读起来不累 */
.dd-logbox {
  height: calc(100vh - 380px);
  min-height: 260px;
  overflow: auto;
  background: #1b1f24;
  border-radius: 6px;
  padding: 8px 10px;
  font-family: Consolas, "Cascadia Mono", monospace;
  font-size: 12px;
  line-height: 1.7;
}
.dd-logline {
  display: flex;
  gap: 8px;
  align-items: baseline;
  color: #c9d1d9;
  white-space: pre-wrap;
  word-break: break-all;
}
.dd-logtime {
  flex: none;
  opacity: 0.5;
}
.dd-logmsg {
  flex: 1;
  min-width: 0;
}
.dd-logline.lv-ok .dd-logmsg {
  color: #7ee787;
}
.dd-logline.lv-warn .dd-logmsg {
  color: #f0c674;
}
.dd-logline.lv-error .dd-logmsg {
  color: #ff7b72;
  font-weight: 600;
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
