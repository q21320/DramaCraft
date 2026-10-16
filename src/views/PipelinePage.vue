<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NCheckbox,
  NInput,
  NInputNumber,
  NPopconfirm,
  NSelect,
  NSpace,
  NTag,
  useMessage,
} from "naive-ui";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { useRouter } from "vue-router";
import { api } from "@/api";
import type { LabelPosition, PipelineRun, PipelineStageOption, SourceScan } from "@/types";

const message = useMessage();
const router = useRouter();

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

/** 展开「导出到」输入框吗？默认收起 —— 默认位置在源目录旁边，一般不用改。 */
const showOutDir = ref(false);

const root = ref("");
const outDir = ref("");

/**
 * 实际会导出到哪 —— 和后端 `pipeline::default_out_dir` 一个规则：
 * 源目录旁边建一个「二创」。留空时就把这个算出来给用户看，
 * 免得他到处找产出。
 *
 * 注意**必须声明在 root / outDir 之后**：写在前面虽然因为 computed 是
 * 延迟求值而侥幸能用，但那是踩在暂时性死区的边上，没必要。
 */
const effectiveOutDir = computed(() => {
  const r = root.value.trim();
  if (outDir.value.trim()) return outDir.value.trim();
  if (!r) return "";
  // 跟着源目录的分隔符风格走，别混出 `D:/a\二创`
  const sep = r.includes("/") && !r.includes("\\") ? "/" : "\\";
  return r.replace(/[\\/]+$/, "") + sep + "二创";
});
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
  { label: "右下角", value: "bottomRight" },  { label: "顶部居中", value: "topCenter" },
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

/**
 * 跳到运行日志页。
 *
 * 流水线页不再列运行记录 —— 那份列表和日志页说的是同一件事，
 * 摆两份只会互相打架。**过程去日志页看**。
 */
function goLogs() {
  router.push("/logs");
}

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

// ---------------- 记住这一页的填写状态 ----------------

/**
 * 把整页的填写状态存起来，下次打开原样回来。
 *
 * 用户说：「不保留 我每次都要从选择 好麻烦呀」——对。
 * 源目录、勾了哪几步、切片多长、片头标识怎么设，这些是**用户的选择**，
 * 不该每次重来一遍。
 *
 * 存在**后端**的 setting 表里，不是 localStorage：跟着库走，
 * 换机器复制库过去习惯还在；出问题能用 sqlite 直接看。
 */
const UI_KEY = "pipeline.form";

type UiForm = {
  root: string;
  outDir: string;
  picked: string[];
  sliceSeconds: number;
  model: string;
  labelEnabled: boolean;
  labelText: string;
  labelSeconds: number;
  labelPosition: LabelPosition;
};

function snapshot(): UiForm {
  return {
    root: root.value,
    outDir: outDir.value,
    picked: [...picked.value],
    sliceSeconds: sliceSeconds.value,
    model: model.value,
    labelEnabled: labelEnabled.value,
    labelText: labelText.value,
    labelSeconds: labelSeconds.value,
    labelPosition: labelPosition.value,
  };
}

/** 读回来。**读不到就当第一次用**，不报错、不弹窗。 */
async function loadForm(): Promise<boolean> {
  try {
    const raw = await api.loadUiState(UI_KEY);
    if (!raw) return false;
    const f = JSON.parse(raw) as Partial<UiForm>;
    if (typeof f.root === "string") root.value = f.root;
    if (typeof f.outDir === "string") outDir.value = f.outDir;
    if (Array.isArray(f.picked)) picked.value = f.picked;
    if (typeof f.sliceSeconds === "number") sliceSeconds.value = f.sliceSeconds;
    if (typeof f.model === "string") model.value = f.model;
    if (typeof f.labelEnabled === "boolean") labelEnabled.value = f.labelEnabled;
    if (typeof f.labelText === "string") labelText.value = f.labelText;
    if (typeof f.labelSeconds === "number") labelSeconds.value = f.labelSeconds;
    if (f.labelPosition) labelPosition.value = f.labelPosition;
    return true;
  } catch {
    // 记不住设置不该让页面打不开
    return false;
  }
}

/** 存。**攒 500ms 再存** —— 勾选框会连着变，别每一下都写库。 */
let saveTimer: number | null = null;
async function saveForm() {
  try {
    await api.saveUiState(UI_KEY, JSON.stringify(snapshot()));
  } catch {
    /* 同上：存不上就算了，不影响使用 */
  }
}
function scheduleSave() {
  if (saveTimer !== null) window.clearTimeout(saveTimer);
  saveTimer = window.setTimeout(() => {
    saveTimer = null;
    void saveForm();
  }, 500);
}
onMounted(async () => {
  // **先把上次填的读回来，再谈默认值。**
  // 顺序反了的话，默认勾选会把用户上次的选择盖掉。
  const restored = await loadForm();
  try {
    stages.value = await api.pipelineStages();
    if (!restored) {
      picked.value = stages.value.filter((s) => s.enabledByDefault).map((s) => s.key);
    } else {
      // 只保留后端这次真有的步骤（万一以后删了某一步，旧勾选不该留着）
      const valid = new Set(stages.value.map((s) => s.key));
      picked.value = picked.value.filter((k) => valid.has(k));
    }
  } catch (e) {
    message.error((e as Error).message);
  }
  // 任何一项变了就记下来
  watch(
    [root, outDir, picked, sliceSeconds, model, labelEnabled, labelText, labelSeconds, labelPosition],
    () => scheduleSave(),
    { deep: true },
  );
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

    <div class="dd-row" style="margin-bottom: 8px">
      <span class="dd-muted" style="width: 78px">源目录</span>
      <n-input v-model:value="root" size="small" style="width: 420px" placeholder="放着各部剧的文件夹" />
      <n-button size="small" @click="pickRoot">选择</n-button>
      <n-button size="small" :loading="scanning" @click="doScan">扫描</n-button>
    </div>

    <!--
      导出目录默认就在源目录旁边，绝大多数情况不用改。
      所以平时只留一行灰字说明「放哪」，需要改的时候才展开输入框 ——
      摆一行空输入框只是占地方，还让人以为必须填。
    -->
    <div class="dd-row" style="margin-bottom: 14px; font-size: 12px">
      <span style="width: 78px"></span>
      <span class="dd-muted dd-wrap" style="flex: 1">
        导出到 {{ effectiveOutDir || "（选好源目录后自动确定）" }}
      </span>
      <n-button text size="tiny" @click="showOutDir = !showOutDir">
        {{ showOutDir ? "收起" : "改" }}
      </n-button>
    </div>
    <div v-if="showOutDir" class="dd-row" style="margin-bottom: 14px">
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
      <!--
        运行中这个按钮就变成「停止」。
        用户问「开始流水线之后 是否改成停止 或者 取消呢？」—— 该改。
        开始之后最主要的动作就是停下来；让它一直叫「开始流水线」、
        而取消藏在下面那行状态里，等于两处说同一件事。
      -->
      <n-button
        v-if="!busy"
        size="small"
        type="primary"
        :loading="starting"
        @click="start"
      >
        {{ starting && willRewrite ? "正在调 AI 起新剧名…" : "开始流水线" }}
      </n-button>
      <n-popconfirm v-else @positive-click="cancelRun(activeRunId!)">
        <template #trigger>
          <n-button size="small" type="warning" secondary>停止</n-button>
        </template>
        <!-- 要说清楚停下来意味着什么，不然用户不敢点 -->
        停止后：已经跑完的会留着，还没跑的不再跑。想接着跑就再点一次「开始流水线」。
      </n-popconfirm>
      <span v-if="willRewrite" class="dd-muted">
        会先调 AI 改简介、起新剧名（约十几秒），<b>再用新剧名建目录、排任务</b>；
        改失败了会用原剧名继续，不会卡住
      </span>
      <span v-else class="dd-muted">排完队会自动开跑；关掉应用再打开会接着跑</span>
    </n-space>
  </div>

  <!--
    只留一行状态，不再列运行记录。

    用户说「运行记录不要了 我只看日志就好了」—— 对：那份列表和日志页
    说的是同一件事，摆两份只会互相打架。一份事实来源就够了。

    但「正在跑」还是得看得见：光让数字悄悄变，用户会以为卡死了。
  -->
  <div class="dd-row" style="justify-content: space-between; padding: 2px 4px">
    <n-space :size="10" align="center">
      <span v-if="busy" class="dd-live"><span class="dd-dot" />正在跑</span>
      <span v-else class="dd-muted">空闲</span>
      <span v-if="activeRun" class="dd-muted">{{ activeRun.dramaCount }} 部剧</span>
      <span class="dd-muted dd-clock">最后更新 {{ lastUpdatedText || "—" }}</span>
    </n-space>
    <n-space :size="8" align="center">
      <!-- 「停止」已经在上面主按钮那儿了，这里不再放第二个取消 —— 两处说同一件事 -->
      <n-button size="small" type="primary" ghost @click="goLogs">看运行日志 →</n-button>
    </n-space>
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
