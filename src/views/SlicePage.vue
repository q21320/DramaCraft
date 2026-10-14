<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NInput,
  NInputNumber,
  NProgress,
  NRadioButton,
  NRadioGroup,
  NSelect,
  NTag,
  useMessage,
} from "naive-ui";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { useRoute } from "vue-router";
import { api } from "@/api";
import { useAppStore } from "@/stores/app";
import type { Segment, SliceMode, SlicePlan, SliceProgress } from "@/types";
import { formatDuration, formatSize, formatTimecode } from "@/utils/format";

const store = useAppStore();
const route = useRoute();
const message = useMessage();

const mediaId = ref<number | null>(null);
const kind = ref<"everySeconds" | "equalParts" | "manual">("equalParts");
const seconds = ref(60);
const parts = ref(10);
const manualText = ref("");
const outDir = ref("");
const plans = ref<SlicePlan[]>([]);
const segments = ref<Segment[]>([]);
const previewing = ref(false);
const running = ref(false);
const progress = ref({ index: 0, total: 0, failed: 0 });

const mediaOptions = computed(() =>
  store.media.map((m) => ({
    label: `${m.name}（${formatDuration(m.durationMs)}）`,
    value: m.id,
  })),
);

const selected = computed(() => store.media.find((m) => m.id === mediaId.value) ?? null);

const mode = computed<SliceMode>(() => {
  if (kind.value === "everySeconds") {
    return { mode: "everySeconds", seconds: Number(seconds.value) || 0 };
  }
  if (kind.value === "equalParts") {
    return { mode: "equalParts", parts: Number(parts.value) || 0 };
  }
  const points = manualText.value
    .split(/[\s,，、]+/)
    .filter((t) => t.length > 0)
    .map((t) => Math.round(Number(t) * 1000))
    .filter((n) => Number.isFinite(n) && n > 0);
  return { mode: "manual", points };
});

const totalPlanned = computed(() => plans.value.reduce((sum, p) => sum + (p.endMs - p.startMs), 0));

const percent = computed(() => {
  if (progress.value.total === 0) return 0;
  return Math.round((progress.value.index / progress.value.total) * 100);
});

let unlisten: UnlistenFn | null = null;
let timer: number | null = null;

async function preview() {
  if (mediaId.value === null) {
    plans.value = [];
    return;
  }
  previewing.value = true;
  try {
    plans.value = await api.buildPlan(mediaId.value, mode.value);
  } catch (e) {
    plans.value = [];
    message.error((e as Error).message);
  } finally {
    previewing.value = false;
  }
}

function schedulePreview() {
  if (timer !== null) window.clearTimeout(timer);
  timer = window.setTimeout(() => void preview(), 260);
}

async function pickOutDir() {
  const picked = await open({ directory: true, multiple: false, title: "选择输出目录" });
  if (typeof picked === "string") {
    outDir.value = picked;
    store.setOutDir(picked);
  }
}

async function loadSegments() {
  if (mediaId.value === null) {
    segments.value = [];
    return;
  }
  try {
    segments.value = await api.listSegments(mediaId.value);
  } catch {
    segments.value = [];
  }
}

async function run() {
  if (mediaId.value === null) {
    message.warning("请先选择素材");
    return;
  }
  if (!outDir.value.trim()) {
    message.warning("请先选择输出目录");
    return;
  }
  if (plans.value.length === 0) {
    message.warning("切片计划为空，请调整参数后重试");
    return;
  }
  running.value = true;
  progress.value = { index: 0, total: plans.value.length, failed: 0 };
  try {
    segments.value = await api.runSlice(mediaId.value, mode.value, outDir.value.trim());
    const failed = segments.value.filter((s) => s.status !== "ok").length;
    if (failed > 0) {
      message.warning(`完成，但有 ${failed} 片失败，详见结果表的说明列`);
    } else {
      message.success(`已生成 ${segments.value.length} 个片段`);
    }
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    running.value = false;
  }
}

onMounted(async () => {
  outDir.value = store.outDir;
  const fromQuery = Number(route.query.mediaId ?? "");
  if (Number.isFinite(fromQuery) && fromQuery > 0) {
    mediaId.value = fromQuery;
  } else if (store.media.length > 0) {
    mediaId.value = store.media[0].id;
  }
  await preview();
  await loadSegments();

  unlisten = await listen<SliceProgress>("slice:progress", (event) => {
    const p = event.payload;
    progress.value = { index: p.index, total: p.total, failed: p.failed ?? 0 };
  });
});

onUnmounted(() => {
  unlisten?.();
  if (timer !== null) window.clearTimeout(timer);
});

watch([mediaId, mode], () => {
  schedulePreview();
  void loadSegments();
});
</script>

<template>
  <div v-if="store.media.length === 0" class="dd-card">
    <div class="dd-card-title">没有可切片的素材</div>
    <div class="dd-card-desc">先到「素材库」导入视频，再回来切片。</div>
  </div>

  <template v-else>
    <div class="dd-card">
      <div class="dd-card-title">切片设置</div>
      <div class="dd-card-desc">
        先用「计划预览」确认切点，再执行。执行时优先流拷贝（无损、快），失败才回退转码。
      </div>

      <div class="dd-row" style="margin-bottom: 12px">
        <span class="dd-muted" style="width: 64px">素材</span>
        <n-select
          v-model:value="mediaId"
          :options="mediaOptions"
          style="width: 420px"
          placeholder="选择素材"
        />
      </div>

      <div class="dd-row" style="margin-bottom: 12px">
        <span class="dd-muted" style="width: 64px">切分</span>
        <n-radio-group v-model:value="kind" size="small">
          <n-radio-button value="equalParts">按份数等分</n-radio-button>
          <n-radio-button value="everySeconds">每 N 秒一片</n-radio-button>
          <n-radio-button value="manual">手动切点</n-radio-button>
        </n-radio-group>
      </div>

      <div class="dd-row" style="margin-bottom: 12px">
        <span class="dd-muted" style="width: 64px">参数</span>
        <template v-if="kind === 'equalParts'">
          <n-input-number v-model:value="parts" :min="1" :max="2000" size="small" style="width: 160px" />
          <span class="dd-muted">份（等分 {{ selected ? formatDuration(selected.durationMs) : "—" }}）</span>
        </template>
        <template v-else-if="kind === 'everySeconds'">
          <n-input-number v-model:value="seconds" :min="1" :max="3600" size="small" style="width: 160px" />
          <span class="dd-muted">秒 / 片（尾部不足 0.5 秒并入上一片）</span>
        </template>
        <template v-else>
          <n-input
            v-model:value="manualText"
            size="small"
            style="width: 420px"
            placeholder="切点，单位秒，用逗号或空格分隔，例如：30, 65.5, 120"
          />
        </template>
      </div>

      <div class="dd-row">
        <span class="dd-muted" style="width: 64px">输出</span>
        <n-input v-model:value="outDir" size="small" style="width: 420px" placeholder="输出目录" />
        <n-button size="small" @click="pickOutDir">选择目录</n-button>
        <n-button size="small" type="primary" :loading="running" @click="run">
          执行切片
        </n-button>
      </div>

      <n-progress
        v-if="running || progress.index > 0"
        type="line"
        :percentage="percent"
        :height="8"
        style="margin-top: 14px"
        :indicator-placement="'inside'"
      />
    </div>

    <div class="dd-card">
      <div class="dd-row" style="justify-content: space-between">
        <div>
          <div class="dd-card-title">计划预览</div>
          <div class="dd-card-desc">
            共 {{ plans.length }} 片，覆盖
            {{ formatDuration(totalPlanned) }}
            <span v-if="selected"> / 素材 {{ formatDuration(selected.durationMs) }}</span>
          </div>
        </div>
        <n-tag v-if="previewing" size="small" :bordered="false">计算中…</n-tag>
      </div>

      <div v-if="plans.length === 0" class="dd-empty">
        计划为空。多半是参数不合理：份数过多、步长过短，或素材本身时长异常。
      </div>

      <div v-else class="dd-scroll" style="max-height: 260px">
        <table class="dd-table">
          <thead>
            <tr>
              <th style="width: 70px">序号</th>
              <th>起点</th>
              <th>终点</th>
              <th>时长</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="p in plans" :key="p.index">
              <td>{{ p.index }}</td>
              <td class="dd-mono">{{ formatTimecode(p.startMs) }}</td>
              <td class="dd-mono">{{ formatTimecode(p.endMs) }}</td>
              <td>{{ formatDuration(p.endMs - p.startMs) }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>

    <div class="dd-card">
      <div class="dd-card-title">切片结果</div>
      <div class="dd-card-desc">
        重跑会覆盖同一素材的上一次结果。失败的片段会保留原因，不会静默丢弃。
      </div>

      <div v-if="segments.length === 0" class="dd-empty">还没有执行过切片。</div>

      <div v-else class="dd-scroll">
        <table class="dd-table">
          <thead>
            <tr>
              <th style="width: 70px">序号</th>
              <th>区间</th>
              <th>体积</th>
              <th style="width: 80px">状态</th>
              <th>说明 / 输出</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="s in segments" :key="s.id">
              <td>{{ s.indexNo }}</td>
              <td class="dd-mono">
                {{ formatTimecode(s.startMs) }} → {{ formatTimecode(s.endMs) }}
              </td>
              <td>{{ formatSize(s.sizeBytes) }}</td>
              <td>
                <n-tag size="tiny" :type="s.status === 'ok' ? 'success' : 'error'" :bordered="false">
                  {{ s.status === "ok" ? "成功" : "失败" }}
                </n-tag>
              </td>
              <td class="dd-wrap">
                <div v-if="s.status !== 'ok'" style="color: var(--dd-muted)">{{ s.message }}</div>
                <div class="dd-mono dd-muted">{{ s.outPath }}</div>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>

    <n-alert v-if="!store.ffmpegReady" type="warning" title="ffmpeg 未就绪" style="margin-top: 4px">
      切片依赖 ffmpeg 与 ffprobe，请先到「设置」指定它们的路径。
    </n-alert>
  </template>
</template>
