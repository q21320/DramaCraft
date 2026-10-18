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
import { useRoute, useRouter } from "vue-router";
import { api } from "@/api";
import type {
  HeaderLabel,
  LabelPosition,
  PipelineRun,
  PipelineStageOption,
  SourceScan,
} from "@/types";

const message = useMessage();
const router = useRouter();
const route = useRoute();

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

/**
 * 产出位置：**每部剧自己的目录下，固定叫「二创」**。
 * 留空时把这条约定算出来给用户看，免得他到处找产出。
 *
 * 注意**必须声明在 root 之后**：写在前面虽然因为 computed 是
 * 延迟求值而侥幸能用，但那是踩在暂时性死区的边上，没必要。
 */
const effectiveOutDir = computed(() => {
  const r = root.value.trim();
  if (!r) return "";
  // 和后端 `pipeline::OUTPUT_DIR_NAME` **必须一个规则**：
  // 产出在每部剧目录下面，叫 `二创`。
  // 跟着源目录的分隔符风格走，别混出 `D:/a\二创`。
  const sep = r.includes("/") && !r.includes("\\") ? "/" : "\\";
  const base = r.replace(/[\\/]+$/, "");
  return base + sep + "<剧名>" + sep + "二创";
});
const sliceSeconds = ref(10);
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
/**
 * 全程显示，还是只显示片头几秒？
 *
 * 用户问「要一直在视频左上方吧」—— 两种做法法规上都被认作「显著标识」：
 * - **片头几秒**：对观感影响最小，是常见做法
 * - **全程显示**：最保险，但一直压在画面上
 *
 * 选哪种是用户的事，工具不替他定。所以做成开关，默认沿用片头几秒。
 */
const labelAlways = ref(false);

const labelPositions: { label: string; value: LabelPosition }[] = [
  { label: "左上角", value: "topLeft" },
  { label: "右上角", value: "topRight" },
  { label: "左下角", value: "bottomLeft" },
  { label: "右下角", value: "bottomRight" },  { label: "顶部居中", value: "topCenter" },
  { label: "底部居中", value: "bottomCenter" },
];

const scan = ref<SourceScan | null>(null);
/**
 * 扫描结果默认只列前几部。
 *
 * 下载根里常坐着几十部旧剧，全展开会把页面拉得很长；默认收成 6 行，
 * 需要时点「展开全部」。不用内部滚动 ——— 页面上已经只有一条滚动条，
 * 再嵌一个反而更难滚。
 */
const SCAN_PREVIEW_ROWS = 6;
const scanShowAll = ref(false);
const scanRows = computed(() =>
  scanShowAll.value
    ? scan.value?.dramas ?? []
    : (scan.value?.dramas ?? []).slice(0, SCAN_PREVIEW_ROWS),
);
/**
 * 从下载页跳过来时带的「刚下载的剧名」。
 *
 * 下载根里通常还躺着以前下的剧 —— 不过滤的话，用户只下了一部，
 * 整个库却被重跑一遍（实测 1 部 20 集变成 3 部 81 集、117 个任务）。
 */
const fromDownload = ref<string[]>([]);
/** 扫描结果里勾选要跑的剧（按剧名） */
const chosen = ref<string[]>([]);
const scanning = ref(false);
const starting = ref(false);
const runs = ref<PipelineRun[]>([]);
const activeRunId = ref<number | null>(null);

const activeRun = computed(() => runs.value.find((r) => r.id === activeRunId.value) ?? null);

/**
 * 这次运行到底怎么结束的。
 *
 * 只显示「空闲」曾经把用户搞糊涂过：他点了「停止」，界面只写「空闲」，
 * 而主按钮一直是「停止」—— 于是他问「点了停止 不应该更改状态？怎么还显示停止？」。
 *
 * 后端 `pipeline_cancel` 写的是 `canceled`，队列中途停下也会收口成 `canceled`；
 * `failed` 是不同的意思，得分开说。
 */
function runStatusText(status: string): string {
  switch (status) {
    case "running":
      return "正在跑";
    case "done":
      return "已完成";
    case "canceled":
      return "已停止";
    case "failed":
      return "有步骤失败";
    default:
      return status;
  }
}

/** 勾选的剧一共多少集 —— 让用户确认这次到底要跑多少活 */
const chosenEpisodes = computed(() => {
  const s = scan.value;
  if (!s) return 0;
  return s.dramas
    .filter((d) => chosen.value.includes(d.name))
    .reduce((n, d) => n + d.episodeCount, 0);
});

/** 取路径最后一段：扫出来的剧名就是目录名，两者对得上 */
function folderName(p: string): string {
  return p.split(/[\\/]/).filter(Boolean).pop() || p;
}

/** 扫完后默认勾谁：从下载页来的只勾刚下的，否则全勾（保持老行为） */
function applyChosenFromScan() {
  const s = scan.value;
  if (!s) {
    chosen.value = [];
    return;
  }
  if (fromDownload.value.length === 0) {
    chosen.value = s.dramas.map((d) => d.name);
    return;
  }
  const wanted = new Set(fromDownload.value);
  const hit = s.dramas.filter((d) => wanted.has(d.name) || wanted.has(folderName(d.dir)));
  if (hit.length > 0) {
    chosen.value = hit.map((d) => d.name);
    return;
  }
  // 刚下的剧没在目录里（改名、挪走了）—— 不替用户做主
  chosen.value = [];
  message.warning("没在源目录里找到刚下载的剧，请手动勾选要跑的剧");
}

function toggleChosen(name: string, on: boolean) {
  const next = new Set(chosen.value);
  if (on) next.add(name);
  else next.delete(name);
  chosen.value = [...next];
}

/** 「改为全部」：把从下载页带过来的限制撤掉 */
function chooseAll() {
  chosen.value = scan.value?.dramas.map((d) => d.name) ?? [];
  fromDownload.value = [];
}

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
}


async function doScan() {
  if (!root.value.trim()) {
    message.warning("请先选择源目录");
    return;
  }
  scanning.value = true;
  scan.value = null;
  scanShowAll.value = false;
  try {
    scan.value = await api.pipelineScan(root.value.trim());
    applyChosenFromScan();
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
  // 没扫过就先扫一遍：让「开始」跑的就是界面上看到的那几部，
  // 而不是后端自己再扫一遍、跑出用户没见过的剧。
  if (!scan.value) {
    await doScan();
    if (!scan.value) return;
  }
  // 一部都没勾就不跑 —— 不然传到后端就是「整目录全跑」，正是这次要防的事
  if (chosen.value.length === 0) {
    message.warning("至少要勾选一部剧");
    return;
  }
  const only = chosen.value;
  starting.value = true;
  try {
    // **片头标识要真的传下去。**
    //
    // 这里漏过一次：`api.pipelineStart` 的签名一直支持 headerLabel，
    // 但调用时只传了前 5 个参数 —— 于是开关勾了、存了、界面也显示勾着，
    // 后端收到的却是 `headerLabel: null`，成片上什么都没有。
    // 用户的话：「我还是没看到…怎么一下都没看到呢？」
    //
    // 教训：**参数在签名里有，不等于调用处传了。** 中间少一环就是静默失效。
    const headerLabel: HeaderLabel | null = labelEnabled.value
      ? {
          text: labelText.value.trim() || "AI创作",
          // 0 = 全程显示（后端认这个语义）
          seconds: labelAlways.value ? 0 : labelSeconds.value,
          position: labelPosition.value,
          sizeRatio: 0.03,
        }
      : null;

    const run = await api.pipelineStart(
      root.value.trim(),
      // **永远传空** —— 导出位置是设计决定，不再给用户改。
      // 传空后端就用 `default_out_dir`：`<源目录>/<源目录名>二创`。
      "",
      picked.value,
      sliceSeconds.value,
      model.value,
      headerLabel,
      only,
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
    // 数字不说人话：0 个待跑却还有剧在跑时，只报「已取消 0 个任务」
    // 等于告诉用户「什么都没发生」，而它明明是他按下的那个停止。
    const left = runs.value.find((r) => r.id === id)?.dramas.filter(
      (d) => d.status === "running",
    ).length ?? 0;
    if (left > 0) {
      message.success(
        `已停止：撤掉 ${n} 个待跑任务。正在跑的那部会做完这一步再停，别再点「开始」，不然会重新排一批`,
      );
    } else {
      message.success(`已停止：撤掉 ${n} 个待跑任务`);
    }
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
  picked: string[];
  sliceSeconds: number;
  model: string;
  labelEnabled: boolean;
  labelText: string;
  labelSeconds: number;
  labelPosition: LabelPosition;
  labelAlways: boolean;
};

function snapshot(): UiForm {
  return {
    root: root.value,
    picked: [...picked.value],
    sliceSeconds: sliceSeconds.value,
    model: model.value,
    labelEnabled: labelEnabled.value,
    labelText: labelText.value,
    labelSeconds: labelSeconds.value,
    labelPosition: labelPosition.value,
    labelAlways: labelAlways.value,
  };
}

/** 读回来。**读不到就当第一次用**，不报错、不弹窗。 */
async function loadForm(): Promise<boolean> {
  try {
    const raw = await api.loadUiState(UI_KEY);
    if (!raw) return false;
    const f = JSON.parse(raw) as Partial<UiForm>;
    if (typeof f.root === "string") root.value = f.root;
    if (Array.isArray(f.picked)) picked.value = f.picked;
    if (typeof f.sliceSeconds === "number") sliceSeconds.value = f.sliceSeconds;
    if (typeof f.model === "string") model.value = f.model;
    if (typeof f.labelEnabled === "boolean") labelEnabled.value = f.labelEnabled;
    if (typeof f.labelText === "string") labelText.value = f.labelText;
    if (typeof f.labelSeconds === "number") labelSeconds.value = f.labelSeconds;
    if (f.labelPosition) labelPosition.value = f.labelPosition;
    if (typeof f.labelAlways === 'boolean') labelAlways.value = f.labelAlways;
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
  // 从列表页下载完跳过来：`?root=<下载根>&only=<刚下的剧名>&autostart=1`
  const autoRoot = typeof route.query.root === "string" ? route.query.root : "";
  const autostart = route.query.autostart === "1";
  // 下载页带来的「只跑这几部」—— 必须先读，下面的 replace 会把 query 清掉
  const onlyRaw = typeof route.query.only === "string" ? route.query.only : "";
  if (onlyRaw) {
    try {
      const arr: unknown = JSON.parse(onlyRaw);
      if (Array.isArray(arr)) {
        fromDownload.value = arr.filter(
          (x): x is string => typeof x === "string" && x.trim().length > 0,
        );
      }
    } catch {
      // 坏参数当作没有，别让页面打不开
    }
  }
  // 先清掉 query，刷新页面不会重复自动开跑
  if (autoRoot || autostart || onlyRaw) void router.replace({ name: "pipeline" });

  // **先把上次填的读回来，再谈默认值。**
  // 顺序反了的话，默认勾选会把用户上次的选择盖掉。
  const restored = await loadForm();
  // 下载完跳过来的目录优先 —— 这次要跑的就是刚落的原片
  if (autoRoot) root.value = autoRoot;
  try {
    stages.value = await api.pipelineStages();
    const defaults = () =>
      stages.value.filter((s) => s.enabledByDefault).map((s) => s.key);
    if (!restored) {
      picked.value = defaults();
    } else {
      // 只保留后端这次真有的步骤（万一以后删了某一步，旧勾选不该留着）
      const valid = new Set(stages.value.map((s) => s.key));
      picked.value = picked.value.filter((k) => valid.has(k));
      // 自动开跑时不能一步都不跑；上次全取消过就退回默认勾选
      if (autostart && picked.value.length === 0) picked.value = defaults();
    }
  } catch (e) {
    message.error((e as Error).message);
  }
  // 任何一项变了就记下来
  watch(
    [root, picked, sliceSeconds, model, labelEnabled, labelText, labelSeconds, labelPosition, labelAlways],
    () => scheduleSave(),
    { deep: true },
  );
  // 手动改了源目录：旧的扫描结果、勾选、刚下载的剧名都不作数了
  // （不然会把不在新目录里的剧名传给后端，直接报「没找到」）
  watch(root, () => {
    scan.value = null;
    scanShowAll.value = false;
    chosen.value = [];
    fromDownload.value = [];
  });
  await refreshRuns();
  startPolling();
  // 忙闲切换时调整刷新频率
  watch(busy, () => startPolling());
  unlisten = await listen("queue:progress", () => {
    void refreshRuns();
  });

  // 下载完直接开跑。
  // **先扫一遍再开跑**：一是让用户看到这次只跑哪几部，
  // 二是确认刚下的剧真在目录里 —— 名字对不上就停下来让他手动勾选，
  // 而不是默默把整个目录重跑一遍。
  if (autostart && autoRoot) {
    await doScan();
    if (scan.value) await start();
  }
});

onUnmounted(() => {
  unlisten?.();
  stopPolling();
});
</script>

<template>
  <div class="dd-card">
    <div class="dd-card-title">整剧处理</div>
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
      **导出位置不给改，只显示。**
      用户说：「导出的输入框直接删除就好了啊 默认在原视频路径」——
      对。产出固定在源目录里面的 `<源目录名>二创`，这是设计决定，不是选项。
      留一个输入框反而让人以为要填、还可能填错（填到源目录里就会互相踩）。
    -->
    <div class="dd-row" style="margin-bottom: 14px; font-size: 12px">
      <span style="width: 78px"></span>
      <span class="dd-muted dd-wrap" style="flex: 1">
        产出会放在：{{ effectiveOutDir || "（选好源目录后自动确定）" }}
      </span>
    </div>

    <n-alert v-if="scan" type="success" title="扫描结果" style="margin-bottom: 14px">
      <div class="dd-wrap">
        共 <b>{{ scan.dramas.length }}</b> 部剧、<b>{{ scan.totalEpisodes }}</b> 集；
        本次勾选 <b>{{ chosen.length }}</b> 部、<b>{{ chosenEpisodes }}</b> 集
        <n-button
          v-if="fromDownload.length > 0"
          text
          size="tiny"
          style="margin-left: 8px"
          @click="chooseAll"
        >
          改为处理全部
        </n-button>
      </div>
      <div v-if="fromDownload.length > 0" class="dd-muted" style="margin-top: 2px">
        只跑刚下载的剧；目录里其他的剧不会动
      </div>
      <table class="dd-table" style="margin-top: 8px">
        <thead>
          <tr>
            <th style="width: 44px">本次</th>
            <th style="width: 200px">剧目</th>
            <th style="width: 70px">集数</th>
            <th style="width: 90px">简介</th>
            <th style="width: 90px">封面</th>
            <th>提示</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="d in scanRows" :key="d.dir">
            <td>
              <n-checkbox
                :checked="chosen.includes(d.name)"
                @update:checked="(v: boolean) => toggleChosen(d.name, v)"
              />
            </td>
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
      <div
        v-if="scan.dramas.length > SCAN_PREVIEW_ROWS"
        class="dd-muted"
        style="margin-top: 6px; display: flex; align-items: center; gap: 8px"
      >
        <span>
          显示 {{ scanRows.length }} / {{ scan.dramas.length }} 部
        </span>
        <n-button text size="tiny" @click="scanShowAll = !scanShowAll">
          {{ scanShowAll ? "收起" : `展开全部 ${scan.dramas.length} 部` }}
        </n-button>
      </div>
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
        <!--
          片头几秒 vs 全程显示，两者都算「显著标识」。
          用户问过「要一直在视频左上方吧」—— 想要就一直显示，做成开关。
        -->
        <n-checkbox v-model:checked="labelAlways">全程显示</n-checkbox>
        <template v-if="!labelAlways">
          <span class="dd-muted">显示</span>
          <n-input-number
            v-model:value="labelSeconds"
            size="small"
            :min="0.5"
            :max="15"
            :step="0.5"
            style="width: 100px"
          />
          <span class="dd-muted">秒</span>
        </template>
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
        {{ starting && willRewrite ? "正在调 AI 起新剧名…" : "开始整剧处理" }}
      </n-button>
      <n-popconfirm v-else @positive-click="cancelRun(activeRunId!)">
        <template #trigger>
          <n-button size="small" type="warning" secondary>停止</n-button>
        </template>
        <!-- 要说清楚停下来意味着什么，不然用户不敢点 -->
        停止后：已经跑完的会留着，还没跑的不再跑，这次运行标成「已停止」，按钮换回「开始整剧处理」。
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
      <!--
        停下之后得显示「怎么停的」。只写「空闲」的话，用户看不到自己那次
        停止到底生效了没有 —— 而那两个按钮没换回「开始」，看着就像没生效。
      -->
      <span v-else-if="activeRun" class="dd-muted">{{ runStatusText(activeRun.status) }}</span>
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
