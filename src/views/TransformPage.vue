<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import {
  NAlert,
  NButton,
  NInputNumber,
  NPopconfirm,
  NRadioButton,
  NRadioGroup,
  NSelect,
  NSlider,
  NSpace,
  NSwitch,
  NTag,
  useMessage,
} from "naive-ui";
import { api } from "@/api";
import { useAppStore } from "@/stores/app";
import type { Segment, TransformOptions, TransformReport, VerifyReport } from "@/types";
import { formatDuration } from "@/utils/format";

const store = useAppStore();
const message = useMessage();

const mediaId = ref<number | null>(null);
const mode = ref<"preset" | "manual">("preset");
const intensity = ref("medium");
const segments = ref<Segment[]>([]);
const videoFilter = ref("");
const audioFilter = ref("");
const busy = ref(false);
const report = ref<TransformReport | null>(null);
const verifying = ref(false);
const verifyReport = ref<VerifyReport | null>(null);

const manual = ref<TransformOptions>({
  mirror: false,
  cropRatio: 0,
  scaleRatio: 1,
  brightness: 0,
  contrast: 1,
  saturation: 1,
  noise: 0,
  speed: 1,
  volume: 1,
});

const mediaOptions = computed(() =>
  store.media.map((m) => ({
    label: `${m.name}（${formatDuration(m.durationMs)}）`,
    value: m.id,
  })),
);

const selected = computed(() => store.media.find((m) => m.id === mediaId.value) ?? null);
const okSegments = computed(() => segments.value.filter((s) => s.status === "ok"));
const processedCount = computed(() => okSegments.value.filter((s) => s.originalPath !== "").length);

const isManualNoop = computed(() => {
  const o = manual.value;
  return (
    !o.mirror &&
    o.cropRatio === 0 &&
    o.scaleRatio === 1 &&
    o.brightness === 0 &&
    o.contrast === 1 &&
    o.saturation === 1 &&
    o.noise === 0 &&
    o.speed === 1 &&
    o.volume === 1
  );
});

const canRun = computed(() =>
  okSegments.value.length > 0 && (mode.value === "preset" || !isManualNoop.value),
);

let timer: number | null = null;

async function refreshSegments() {
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

async function refreshPreview() {
  try {
    const payload = mode.value === "manual" ? manual.value : null;
    const preview = await api.transformPreview(
      payload,
      intensity.value,
      selected.value?.width,
      selected.value?.height,
    );
    videoFilter.value = preview.videoFilter;
    audioFilter.value = preview.audioFilter;
  } catch {
    videoFilter.value = "";
    audioFilter.value = "";
  }
}

function schedulePreview() {
  if (timer !== null) window.clearTimeout(timer);
  timer = window.setTimeout(() => void refreshPreview(), 200);
}

async function run() {
  if (mediaId.value === null) {
    message.warning("请先选择素材");
    return;
  }
  if (!canRun.value) {
    message.warning("没有选择任何处理项，至少勾选或调整一项");
    return;
  }
  busy.value = true;
  report.value = null;
  try {
    report.value = await api.transformSegments(
      mediaId.value,
      intensity.value,
      mode.value === "manual" ? manual.value : null,
    );
    await refreshSegments();
    if (report.value.failed > 0) {
      message.warning(`完成 ${report.value.processed} 个，失败 ${report.value.failed} 个`);
    } else {
      message.success(`已处理 ${report.value.processed} 个片段`);
    }
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

async function revert() {
  if (mediaId.value === null) return;
  busy.value = true;
  try {
    const outcome = await api.revertSegments(mediaId.value);
    await refreshSegments();
    message.success(`已还原 ${outcome.processed} 个片段`);
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

function resetManual() {
  manual.value = {
    mirror: false,
    cropRatio: 0,
    scaleRatio: 1,
    brightness: 0,
    contrast: 1,
    saturation: 1,
    noise: 0,
    speed: 1,
    volume: 1,
  };
}

/** 差异越明显，对这个工具来说越是「好结果」 */
function verdictType(v: string): "success" | "warning" | "error" | "default" {
  if (v === "差异明显") return "success";
  if (v === "有差异") return "warning";
  if (v === "几乎没变") return "error";
  return "default";
}

function percent(value: number): string {
  return `${(value * 100).toFixed(1)}%`;
}

async function runVerify() {
  if (mediaId.value === null) {
    message.warning("请先选择素材");
    return;
  }
  if (processedCount.value === 0) {
    message.warning("还没有处理过的片段，请先执行画面处理");
    return;
  }
  verifying.value = true;
  verifyReport.value = null;
  try {
    verifyReport.value = await api.verifyTransform(mediaId.value);
    message.success(`已比对 ${verifyReport.value.checked} 个片段`);
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    verifying.value = false;
  }
}

watch(mediaId, () => {
  report.value = null;
  verifyReport.value = null;
  void refreshSegments();
  schedulePreview();
});

watch([mode, intensity, manual], schedulePreview, { deep: true });

onMounted(async () => {
  if (store.media.length > 0) mediaId.value = store.media[0].id;
  await refreshSegments();
  await refreshPreview();
});
</script>

<template>
  <div v-if="store.media.length === 0" class="dd-card">
    <div class="dd-card-title">没有可处理的素材</div>
    <div class="dd-card-desc">先到「素材库」导入视频，再去「切片工作台」切片。</div>
  </div>

  <template v-else>
    <div class="dd-card">
      <div class="dd-card-title">画面处理</div>
      <div class="dd-card-desc">
        对切片做镜像、裁切、缩放、调色、噪点与变速，让每个片段彼此不同。
        产出的文件与原件同目录（<span class="dd-mono">xxx_t.mp4</span>），原件不会被删除，随时可以还原。
      </div>

      <div class="dd-row" style="margin-bottom: 10px">
        <span class="dd-muted" style="width: 64px">素材</span>
        <n-select v-model:value="mediaId" :options="mediaOptions" style="width: 380px" />
        <n-tag size="small" :type="okSegments.length > 0 ? 'success' : 'warning'" :bordered="false">
          可用切片 {{ okSegments.length }}
        </n-tag>
        <n-tag v-if="processedCount > 0" size="small" type="info" :bordered="false">
          已处理 {{ processedCount }}
        </n-tag>
      </div>

      <div class="dd-row" style="margin-bottom: 12px">
        <span class="dd-muted" style="width: 64px">方式</span>
        <n-radio-group v-model:value="mode" size="small">
          <n-radio-button value="preset">自动档位（每片参数不同）</n-radio-button>
          <n-radio-button value="manual">手动指定（所有片一致）</n-radio-button>
        </n-radio-group>
      </div>

      <div v-if="mode === 'preset'" class="dd-row" style="margin-bottom: 12px">
        <span class="dd-muted" style="width: 64px">档位</span>
        <n-select
          v-model:value="intensity"
          size="small"
          style="width: 220px"
          :options="[
            { label: '轻微（几乎看不出）', value: 'light' },
            { label: '中等（推荐）', value: 'medium' },
            { label: '明显（改动较大）', value: 'strong' },
          ]"
        />
        <span class="dd-muted">按片段序号生成确定性参数：同一批重复处理结果一致，可复现</span>
      </div>

      <template v-else>
        <div class="dd-row" style="margin-bottom: 8px">
          <span class="dd-muted" style="width: 64px">镜像</span>
          <n-switch v-model:value="manual.mirror" size="small" />
          <span class="dd-muted" style="margin-left: 24px">噪点</span>
          <n-input-number v-model:value="manual.noise" size="small" :min="0" :max="10" style="width: 110px" />
        </div>

        <div class="dd-row" style="margin-bottom: 6px">
          <span class="dd-muted" style="width: 64px">裁切</span>
          <n-slider v-model:value="manual.cropRatio" :min="0" :max="0.1" :step="0.005" style="width: 240px" />
          <span class="dd-mono" style="width: 74px">{{ (manual.cropRatio * 100).toFixed(1) }}%</span>
          <span class="dd-muted">四边各裁掉的比例（裁完自动放大回原尺寸）</span>
        </div>

        <div class="dd-row" style="margin-bottom: 6px">
          <span class="dd-muted" style="width: 64px">缩放</span>
          <n-slider v-model:value="manual.scaleRatio" :min="0.96" :max="1.04" :step="0.001" style="width: 240px" />
          <span class="dd-mono" style="width: 74px">×{{ manual.scaleRatio.toFixed(3) }}</span>
        </div>

        <div class="dd-row" style="margin-bottom: 6px">
          <span class="dd-muted" style="width: 64px">亮度</span>
          <n-slider v-model:value="manual.brightness" :min="-0.05" :max="0.05" :step="0.001" style="width: 240px" />
          <span class="dd-mono" style="width: 74px">{{ manual.brightness.toFixed(3) }}</span>
        </div>

        <div class="dd-row" style="margin-bottom: 6px">
          <span class="dd-muted" style="width: 64px">对比度</span>
          <n-slider v-model:value="manual.contrast" :min="0.95" :max="1.05" :step="0.001" style="width: 240px" />
          <span class="dd-mono" style="width: 74px">{{ manual.contrast.toFixed(3) }}</span>
        </div>

        <div class="dd-row" style="margin-bottom: 6px">
          <span class="dd-muted" style="width: 64px">饱和度</span>
          <n-slider v-model:value="manual.saturation" :min="0.95" :max="1.05" :step="0.001" style="width: 240px" />
          <span class="dd-mono" style="width: 74px">{{ manual.saturation.toFixed(3) }}</span>
        </div>

        <div class="dd-row" style="margin-bottom: 6px">
          <span class="dd-muted" style="width: 64px">速度</span>
          <n-slider v-model:value="manual.speed" :min="0.9" :max="1.1" :step="0.001" style="width: 240px" />
          <span class="dd-mono" style="width: 74px">×{{ manual.speed.toFixed(3) }}</span>
          <span class="dd-muted">音频会同步变速，保持对齐</span>
        </div>

        <div class="dd-row" style="margin-bottom: 12px">
          <span class="dd-muted" style="width: 64px">音量</span>
          <n-slider v-model:value="manual.volume" :min="0.8" :max="1.2" :step="0.001" style="width: 240px" />
          <span class="dd-mono" style="width: 74px">×{{ manual.volume.toFixed(3) }}</span>
          <n-button size="tiny" quaternary @click="resetManual">重置</n-button>
        </div>
      </template>

      <div class="dd-card" style="margin: 0 0 12px; padding: 12px 14px">
        <div class="dd-card-title">将要执行的滤镜</div>
        <div class="dd-mono dd-wrap" style="word-break: break-all">
          {{ videoFilter || "（无画面处理）" }}
        </div>
        <div v-if="audioFilter" class="dd-mono dd-wrap dd-muted" style="word-break: break-all">
          {{ audioFilter }}
        </div>
      </div>

      <n-space>
        <n-button size="small" type="primary" :loading="busy" :disabled="!canRun" @click="run">
          开始处理
        </n-button>
        <n-button
          size="small"
          :loading="verifying"
          :disabled="processedCount === 0"
          @click="runVerify"
        >
          验证处理效果
        </n-button>
        <n-popconfirm @positive-click="revert">
          <template #trigger>
            <n-button size="small" :loading="busy" :disabled="processedCount === 0">还原</n-button>
          </template>
          把所有片段指回处理前的文件？处理产物会留在磁盘上，不会删除。
        </n-popconfirm>
      </n-space>
    </div>

    <div v-if="verifyReport" class="dd-card">
      <div class="dd-row" style="justify-content: space-between">
        <div>
          <div class="dd-card-title">处理效果验证</div>
          <div class="dd-card-desc">
            已比对 {{ verifyReport.checked }} 个片段，平均相似度
            {{ percent(verifyReport.meanSimilarity) }}
            <span v-if="verifyReport.unchanged > 0">；另有 {{ verifyReport.unchanged }} 个片段未处理</span>
          </div>
        </div>
        <n-tag size="small" :type="verdictType(verifyReport.verdict)" :bordered="false">
          {{ verifyReport.verdict }}
        </n-tag>
      </div>

      <div v-if="verifyReport.items.length === 0" class="dd-empty">
        没有可比对的片段。
      </div>

      <div v-else class="dd-scroll" style="max-height: 300px">
        <table class="dd-table">
          <thead>
            <tr>
              <th style="width: 60px">#</th>
              <th style="width: 110px">相似度</th>
              <th style="width: 130px">平均汉明距离</th>
              <th style="width: 110px">结论</th>
              <th>采样帧</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="item in verifyReport.items" :key="item.segmentId">
              <td>{{ item.indexNo }}</td>
              <td class="dd-mono">{{ percent(item.report.meanSimilarity) }}</td>
              <td class="dd-mono">{{ item.report.meanDistance.toFixed(1) }} / 64</td>
              <td>
                <n-tag size="tiny" :type="verdictType(item.report.verdict)" :bordered="false">
                  {{ item.report.verdict }}
                </n-tag>
              </td>
              <td class="dd-mono dd-muted">
                {{ item.report.samplesCompared }} 帧
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <n-alert v-if="verifyReport.errors.length > 0" type="error" style="margin-top: 12px" title="失败明细">
        <ul style="margin: 0; padding-left: 18px">
          <li v-for="(e, i) in verifyReport.errors" :key="i" class="dd-wrap">{{ e }}</li>
        </ul>
      </n-alert>

      <n-alert v-if="verifyReport.warnings.length > 0" type="info" style="margin-top: 12px" title="说明">
        <ul style="margin: 0; padding-left: 18px">
          <li v-for="(w, i) in verifyReport.warnings" :key="i" class="dd-wrap">{{ w }}</li>
        </ul>
      </n-alert>
    </div>

    <div v-if="report" class="dd-card">
      <div class="dd-card-title">处理结果</div>
      <div class="dd-card-desc">
        共 {{ report.total }} 个片段：成功 {{ report.processed }}，失败 {{ report.failed }}
      </div>
      <n-alert v-if="report.errors.length > 0" type="error" title="失败明细">
        <ul style="margin: 0; padding-left: 18px">
          <li v-for="(e, i) in report.errors" :key="i" class="dd-wrap">{{ e }}</li>
        </ul>
      </n-alert>
    </div>

    <div class="dd-card">
      <div class="dd-card-title">片段状态</div>
      <div v-if="okSegments.length === 0" class="dd-empty">还没有成功的切片。</div>
      <div v-else class="dd-scroll" style="max-height: 320px">
        <table class="dd-table">
          <thead>
            <tr>
              <th style="width: 60px">#</th>
              <th>当前文件</th>
              <th style="width: 90px">状态</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="s in okSegments" :key="s.id">
              <td>{{ s.indexNo }}</td>
              <td class="dd-wrap dd-mono">{{ s.outPath }}</td>
              <td>
                <n-tag size="tiny" :type="s.originalPath ? 'info' : 'default'" :bordered="false">
                  {{ s.originalPath ? "已处理" : "原始" }}
                </n-tag>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>

    <n-alert type="warning" title="使用前提">
      这类处理用于二次创作时的画面差异化。请确认你对素材拥有相应权利（例如已获得短剧方的切片推广授权），
      平台规则与著作权责任由使用者承担。
    </n-alert>
  </template>
</template>
