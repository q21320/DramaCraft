<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NCheckbox,
  NInput,
  NInputNumber,
  NSelect,
  NSpace,
  NTag,
  useMessage,
} from "naive-ui";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { api } from "@/api";
import type { LabelPosition, PipelineRun, PipelineStageOption, SourceScan } from "@/types";

const message = useMessage();

const stages = ref<PipelineStageOption[]>([]);
const picked = ref<string[]>([]);

/**
 * 勾了「AI 改剧名简介」吗？
 *
 * 勾了的话，点「开始流水线」会**先等 AI 返回新剧名**（十几秒），
 * 然后用新剧名建目录、排任务 —— 所以按钮上要说明在等什么，
 * 否则用户会以为卡死了。
 */
const willRewrite = computed(() => picked.value.includes("rewrite"));
const root = ref("");
const outDir = ref("");
const sliceSeconds = ref(40);
const model = ref("small");

/**
 * 片头标识。
 *
 * **这是合规开关，不是去重手段。**《人工智能生成合成内容标识办法》要求
 * AI 生成 / 合成的内容显著标识。画面经过 AI 处理（去字幕、AI 生成封面
 * 或文案等）后发布，应当标识。
 *
 * 默认**关**：内容是 AI 生成的才需要标，标错了是另一种不实。
 */
const labelEnabled = ref(false);
const labelText = ref("AI创作");
const labelSeconds = ref(3);
const labelPosition = ref<LabelPosition>("topLeft");

const labelPositions: { label: string; value: LabelPosition }[] = [
  { label: "左上角", value: "topLeft" },
  { label: "右上角", value: "topRight" },
  { label: "左下角", value: "bottomLeft" },
  { label: "右下角", value: "bottomRight" },
  { label: "顶部居中", value: "topCenter" },
  { label: "底部居中", value: "bottomCenter" },
];

const scan = ref<SourceScan | null>(null);
const scanning = ref(false);
const starting = ref(false);
const runs = ref<PipelineRun[]>([]);
const activeRunId = ref<number | null>(null);

const activeRun = computed(() => runs.value.find((r) => r.id === activeRunId.value) ?? null);

/**
 * 最后一次成功刷新的时刻。
 *
 * 为什么要显示它：光让数字悄悄变，用户根本看不出来界面是活的
 * —— 有人就因此以为程序卡死了。摆一个走动的时钟，「在实时更新」才看得见。
 */
const lastUpdated = ref<Date | null>(null);

/** 有没有正在跑的流水线 —— 决定刷新频率，也决定要不要显示「正在跑」 */
const busy = computed(() => runs.value.some((r) => r.status === "running"));

const lastUpdatedText = computed(() => {
  if (!lastUpdated.value) return "";
  const d = lastUpdated.value;
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
});

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
    lastUpdated.value = new Date();
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
      指一个源目录，剩下的按顺序做完：
      <b>改剧名简介 → 整剧重组 → 整集去重 → AI 去字幕 → 视频切片 → 分镜证明 → 封面 → 成本报告</b>。
      一部剧做完再做下一部。
      <br />
      关键在顺序：<b>先对整集去重，再切片</b> —— 去重产物才是发布用的东西，
      切片是它的下游（做证明图用）。反过来的话，同一部剧要多跑十几倍的去重。
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

    <!-- 片头标识：合规功能，默认关 -->
    <div class="dd-label-box">
      <n-checkbox v-model:checked="labelEnabled">
        片头加「AI 生成内容」标识
      </n-checkbox>
      <div class="dd-muted" style="margin-left: 22px; margin-top: 2px">
        《人工智能生成合成内容标识办法》要求 AI 生成 / 合成的内容显著标识。
        <b>画面经过 AI 处理</b>（AI 去字幕、AI 生成封面或文案等）后发布，应当标识。
        没用到 AI 就别勾 —— 标错了是另一种不实。
      </div>
      <div v-if="labelEnabled" class="dd-row" style="gap: 8px; margin: 8px 0 0 22px; flex-wrap: wrap">
        <span class="dd-muted">文字</span>
        <n-input v-model:value="labelText" size="small" style="width: 140px" maxlength="16" />
        <span class="dd-muted">显示</span>
        <n-input-number v-model:value="labelSeconds" size="small" :min="0.5" :max="15" :step="0.5" style="width: 100px" />
        <span class="dd-muted">秒</span>
        <span class="dd-muted">位置</span>
        <n-select
          v-model:value="labelPosition"
          size="small"
          style="width: 120px"
          :options="labelPositions"
        />
      </div>
    </div>

    <n-space>
      <n-button size="small" type="primary" :loading="starting" @click="start">
        {{ starting && willRewrite ? "正在调 AI 起新剧名…" : "开始流水线" }}
      </n-button>
      <span v-if="willRewrite" class="dd-muted">
        会先调 AI 改简介、起新剧名（约十几秒），<b>再用新剧名建目录、排任务</b>；
        改失败了会用原剧名继续，不会卡住
      </span>
      <span v-else class="dd-muted">排完队会自动开跑；关掉应用再打开会接着跑</span>
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
/* 片头标识：合规开关，视觉上和去重参数分开 */
.dd-label-box {
  border: 1px solid rgba(128, 128, 128, 0.22);
  border-radius: 6px;
  padding: 9px 11px;
  margin-bottom: 14px;
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
