<script setup lang="ts">
/**
 * 运行日志页。
 *
 * ## 为什么只有日志，没有单独的「产出」视图
 *
 * 用户的话：「产出的内容在日志里尽量能直接点击播放 你说呢 是不是效果更好呢？」
 *
 * **是更好。** 日志本身就是「发生过什么」的完整记录，产出属于其中的某一行。
 * 单开一个产出列表等于把同一件事说两遍 —— 而且两份说法不一致时，
 * 用户不知道该信哪个。所以：**有产出文件的那一行，直接点开就播**。
 *
 * ## 为什么没有「选哪次运行」
 *
 * 实际要看的永远是**当前这一次**。少一个选择就少一处要解释的东西。
 * 新的一次运行开始时，视图自动跟过去。
 *
 * ## 为什么这么在意并发
 *
 * 触发源有两个（轮询定时器 + `queue:progress` 事件），并行跑任务时
 * 事件密集触发。之前没防并发，好几个请求同时在飞、各自读到同一个
 * `since`、取回同一批、各追加一次 —— 同一行被渲染了 5 遍，
 * 看起来像「程序重复跑了 5 次」。**日志页是最不能骗人的地方。**
 */
import { computed, onMounted, onUnmounted, ref, nextTick } from "vue";
import { NButton, NCheckbox, NModal, NSpace, NTag, useMessage } from "naive-ui";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { convertFileSrc } from "@tauri-apps/api/core";
import { api } from "@/api";
import type { PipelineRun, RunLog } from "@/types";

const message = useMessage();

const logs = ref<RunLog[]>([]);
const runs = ref<PipelineRun[]>([]);
/** 当前在看哪次运行 —— 自动跟最新那次，用户不用选 */
let latestRunId: number | null = null;

const follow = ref(true);
const autoScroll = ref(true);
const boxRef = ref<HTMLElement | null>(null);

/** 正在播的那一片 */
const playing = ref<{ path: string; title: string } | null>(null);

const busy = computed(() => runs.value.some((r) => r.status === "running"));

const levelType: Record<string, "default" | "success" | "warning" | "error"> = {
  info: "default",
  ok: "success",
  warn: "warning",
  error: "error",
};

/** 时间戳只显示时分秒 —— 日期对看日志没帮助，还占地方 */
function clockOf(iso: string): string {
  const m = iso.match(/(\d{2}):(\d{2}):(\d{2})/);
  return m ? `${m[1]}:${m[2]}:${m[3]}` : iso;
}

function baseName(path: string): string {
  const parts = path.split(/[\\/]/);
  return parts[parts.length - 1] || path;
}

function fileUrl(path: string): string {
  try {
    return convertFileSrc(path);
  } catch {
    return path;
  }
}

/** 这一行有没有可播的产出 */
function playable(l: RunLog): boolean {
  return !!l.artifact && l.artifact.length > 0;
}

function play(l: RunLog) {
  if (!playable(l)) return;
  playing.value = { path: l.artifact, title: baseName(l.artifact) };
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

// ---------------- 拉取（三层防并发） ----------------

let pulling = false;
let coalesceTimer: number | null = null;

async function pullLogs(reset = false) {
  // 已经在拉了就别叠 —— 叠上去只会重复追加
  if (pulling) return;
  pulling = true;
  try {
    if (reset) logs.value = [];
    // 取值和请求放在同一段里，别跨 await
    const since = reset ? 0 : logs.value.length ? logs.value[logs.value.length - 1].id : 0;
    const fresh = await api.listLogs(latestRunId, since, 800);
    if (fresh.length > 0) {
      // 双保险：按 id 去重
      const seen = new Set(logs.value.map((l) => l.id));
      const add = fresh.filter((l) => !seen.has(l.id));
      if (add.length > 0) {
        logs.value = [...logs.value, ...add];
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

/** 事件触发用的入口：攒 300ms 再拉（一批 4 个任务会刷十几条事件） */
function triggerPull() {
  if (coalesceTimer !== null) return;
  coalesceTimer = window.setTimeout(() => {
    coalesceTimer = null;
    void pullLogs();
  }, 300);
}

/** 刷新运行列表并跟到最新那次；换运行了就重置日志视图 */
async function refreshRuns() {
  try {
    const list = await api.pipelineList(20);
    runs.value = list;
    const latest = list[0]?.id ?? null;
    if (latest !== latestRunId) {
      latestRunId = latest;
      void pullLogs(true);
    }
  } catch {
    /* 日志页不因为这个弹错 */
  }
}

// 复制日志 —— 贴给别人看、或者自己存档。
//
// **不用 clipboard 插件**：Tauri 的 webview 里 `navigator.clipboard` 有时被拦，
// 所以带一个 `execCommand('copy')` 的兜底 —— 那个虽然过时，但到哪都能用。
const copying = ref(false);

function logsToText(): string {
  return logs.value
    .map((l) => {
      const t = new Date(l.createdAt).toLocaleString("zh-CN", { hour12: false });
      // 对齐一点，贴出去好读：时间 + 级别 + 内容
      const lv = (l.level || "info").padEnd(5);
      return `${t}  ${lv}  ${l.message}`;
    })
    .join("\n");
}

async function copyLogs() {
  if (logs.value.length === 0) {
    message.warning("还没有日志可复制");
    return;
  }
  const text = logsToText();
  copying.value = true;
  try {
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      // 兜底：塞一个隐藏 textarea 再用 execCommand
      const ta = document.createElement("textarea");
      ta.value = text;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      const ok = document.execCommand("copy");
      document.body.removeChild(ta);
      if (!ok) throw new Error("浏览器不允许写剪贴板");
    }
    message.success(`已复制 ${logs.value.length} 行日志`);
  } catch (e) {
    message.error(`复制失败：${e}`);
  } finally {
    copying.value = false;
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

let unlistenQueue: UnlistenFn | null = null;
let unlistenDownload: UnlistenFn | null = null;
let timer: number | null = null;

function startPolling() {
  stopPolling();
  timer = window.setInterval(() => {
    void refreshRuns();
    if (follow.value) void pullLogs();
  }, busy.value ? 1500 : 5000);
}

function stopPolling() {
  if (timer !== null) {
    window.clearInterval(timer);
    timer = null;
  }
}

onMounted(async () => {
  await refreshRuns();
  await pullLogs(true);
  startPolling();
  unlistenQueue = await listen("queue:progress", () => {
    void refreshRuns();
    if (follow.value) triggerPull();
  });
  // 下载也在写日志（保存路径 / 第几集），事件到了就拉一把，
  // 不用等 5 秒轮询 —— 用户正盯着看。
  unlistenDownload = await listen("fanqie:download", () => {
    if (follow.value) triggerPull();
  });
});

onUnmounted(() => {
  unlistenQueue?.();
  unlistenDownload?.();
  stopPolling();
  if (coalesceTimer !== null) window.clearTimeout(coalesceTimer);
});
</script>

<template>
  <div class="dd-card">
    <div class="dd-row" style="justify-content: space-between; margin-bottom: 4px">
      <div class="dd-card-title">运行日志</div>
      <n-space :size="10" align="center">
        <span v-if="busy" class="dd-live"><span class="dd-dot" />正在跑</span>
        <span v-else class="dd-muted">空闲</span>
        <n-button size="small" :loading="copying" @click="copyLogs">复制日志</n-button>
        <n-button size="small" @click="clearAll">清空日志</n-button>
      </n-space>
    </div>
    <div class="dd-card-desc">
      跑整剧处理、下载原片时这里都会一步步写下来：扫到什么、保存到哪、第几集下完了、哪一步成没成。
      <b>卡住的时候先看这里</b> —— 状态只是结果，日志才有过程。
      <br />
      <b>带播放图标的行可以直接点开看</b>（产出、下好的原片都可以），不用去别处找。
    </div>

    <div class="dd-row" style="gap: 14px; margin-bottom: 8px; margin-top: 8px">
      <n-checkbox v-model:checked="follow">实时跟随</n-checkbox>
      <n-checkbox v-model:checked="autoScroll">自动滚到底</n-checkbox>
      <span class="dd-muted">共 {{ logs.length }} 行</span>
    </div>

    <div ref="boxRef" class="dd-logbox">
      <div v-if="logs.length === 0" class="dd-muted" style="padding: 12px">
        还没有日志。跑一次整剧处理或下载一部剧就有了。
      </div>
      <div
        v-for="l in logs"
        :key="l.id"
        class="dd-logline"
        :class="['lv-' + l.level, playable(l) ? 'clickable' : '']"
        :title="playable(l) ? '点一下播放：' + baseName(l.artifact) : ''"
        @click="play(l)"
      >
        <span class="dd-logtime">{{ clockOf(l.createdAt) }}</span>
        <n-tag size="tiny" :type="levelType[l.level] ?? 'default'" :bordered="false">
          {{ l.level }}
        </n-tag>
        <span class="dd-logmsg">{{ l.message }}</span>
        <span v-if="playable(l)" class="dd-play">▶</span>
      </div>
    </div>
  </div>

  <n-modal
    v-if="playing"
    :show="true"
    preset="card"
    style="width: 760px"
    :title="playing.title"
    @close="playing = null"
  >
    <video
      :src="fileUrl(playing.path)"
      controls
      autoplay
      style="width: 100%; max-height: 62vh; background: #000"
    />
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
  height: calc(100vh - 330px);
  min-height: 300px;
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
  border-radius: 4px;
  padding: 0 4px;
}
/* 有产出的行：整行可点，鼠标也变手型 —— 不然用户不知道能点 */
.dd-logline.clickable {
  cursor: pointer;
}
.dd-logline.clickable:hover {
  background: rgba(255, 255, 255, 0.08);
}
.dd-logtime {
  flex: none;
  opacity: 0.5;
}
.dd-logmsg {
  flex: 1;
  min-width: 0;
}
.dd-play {
  flex: none;
  opacity: 0.85;
  font-size: 11px;
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
</style>
