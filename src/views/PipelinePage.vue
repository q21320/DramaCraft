<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
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
import type { PipelineRun, PipelineStageOption, SourceScan } from "@/types";

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

const activeRun = computed(() => runs.value.find((r) => r.id === activeRunId.value) ?? null);

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

onMounted(async () => {
  try {
    stages.value = await api.pipelineStages();
    picked.value = stages.value.filter((s) => s.enabledByDefault).map((s) => s.key);
  } catch (e) {
    message.error((e as Error).message);
  }
  await refreshRuns();
  unlisten = await listen("queue:progress", () => {
    void refreshRuns();
  });
});

onUnmounted(() => {
  unlisten?.();
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
      <n-button size="small" @click="refreshRuns">刷新</n-button>
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
