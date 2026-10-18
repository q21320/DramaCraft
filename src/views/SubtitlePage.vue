<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
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
import { open, save } from "@tauri-apps/plugin-dialog";
import { api } from "@/api";
import { useAppStore } from "@/stores/app";
import type { Cue } from "@/types";
import { formatDuration, formatTimecode } from "@/utils/format";

const store = useAppStore();
const message = useMessage();

const mediaId = ref<number | null>(null);
const cues = ref<Cue[]>([]);
const warnings = ref<string[]>([]);
const dirty = ref(false);
const loading = ref(false);
const busy = ref(false);
const asrLog = ref("");
const model = ref("small");
const language = ref("zh");
const asrOverwrite = ref(false);
const cloudBusy = ref(false);
const cloudLog = ref("");

// 硬字幕识别
const ocrFps = ref(2);
const ocrRegion = ref<"bottom30" | "bottom50" | "full">("bottom30");
const ocrOverwrite = ref(false);
const ocrBusy = ref(false);
const ocrLog = ref("");

const ocrBottomRatio = computed(() =>
  ocrRegion.value === "full" ? 1.0 : ocrRegion.value === "bottom50" ? 0.5 : 0.3,
);

const mediaOptions = computed(() =>
  store.media.map((m) => ({
    label: `${m.name}（${formatDuration(m.durationMs)}）`,
    value: m.id,
  })),
);

const bundledModels = ref<string[]>([]);
const modelOptions = computed(() =>
  [
    { label: "tiny（最快，草稿用）", value: "tiny" },
    { label: "base", value: "base" },
    { label: "small（推荐）", value: "small" },
    { label: "medium（更准更慢）", value: "medium" },
  ].map((o) => ({
    ...o,
    label: bundledModels.value.includes(o.value)
      ? `${o.label} · 已内置`
      : `${o.label} · 需联网下载`,
  })),
);

const languageOptions = [
  { label: "中文", value: "zh" },
  { label: "英文", value: "en" },
  { label: "自动识别", value: "auto" },
];

const selected = computed(() => store.media.find((m) => m.id === mediaId.value) ?? null);
const totalMs = computed(() => cues.value.reduce((s, c) => s + Math.max(0, c.endMs - c.startMs), 0));

async function load() {
  if (mediaId.value === null) {
    cues.value = [];
    return;
  }
  loading.value = true;
  try {
    cues.value = await api.listCues(mediaId.value);
    dirty.value = false;
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    loading.value = false;
  }
}

function renumber() {
  cues.value.sort((a, b) => a.startMs - b.startMs);
  cues.value.forEach((c, i) => (c.indexNo = i + 1));
}

async function saveCues() {
  if (mediaId.value === null) return;
  renumber();
  busy.value = true;
  try {
    const count = await api.replaceCues(mediaId.value, cues.value);
    dirty.value = false;
    message.success(`已保存 ${count} 条字幕`);
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

async function runAsr() {
  if (mediaId.value === null) {
    message.warning("请先选择素材");
    return;
  }
  if (!store.asr?.available) {
    message.warning("ASR 运行时未就绪，请先到「设置」指定装有 faster-whisper 的 Python");
    return;
  }
  if (cues.value.length > 0 && dirty.value) {
    message.warning("当前修改尚未保存，请先保存再生成");
    return;
  }
  if (cues.value.length > 0 && !asrOverwrite.value) {
    message.warning(`当前已有 ${cues.value.length} 条字幕。要覆盖请先勾选「覆盖现有字幕」`);
    return;
  }
  busy.value = true;
  asrLog.value = "";
  try {
    const outcome = await api.asrTranscribe(
      mediaId.value,
      model.value,
      language.value,
      asrOverwrite.value,
    );
    asrLog.value = outcome.log;
    await load();
    warnings.value = [];
    asrOverwrite.value = false;
    message.success(`识别完成，共 ${outcome.cueCount} 条字幕`);
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

/** 交给用户自备 Key 的云端服务转写（快，按量计费） */
async function runCloud() {
  if (mediaId.value === null) {
    message.warning("请先选择素材");
    return;
  }
  if (!store.cloud?.hasKey) {
    message.warning("还没配置云端 API Key，请到「AI 服务 → 云端字幕」填写");
    return;
  }
  if (cues.value.length > 0 && !asrOverwrite.value) {
    message.warning(`当前已有 ${cues.value.length} 条字幕。要覆盖请先勾选「覆盖现有字幕」`);
    return;
  }
  cloudBusy.value = true;
  cloudLog.value = "";
  try {
    const report = await api.cloudTranscribe(mediaId.value, asrOverwrite.value);
    cloudLog.value =
      `云端返回 ${report.cueCount} 条字幕（语言 ${report.language || "未知"}，` +
      `上传音频 ${(report.audioBytes / 1024).toFixed(0)} KB）`;
    await load();
    asrOverwrite.value = false;
    message.success(`云端转写完成，共 ${report.cueCount} 条字幕`);
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    cloudBusy.value = false;
  }
}

/** 用 OCR 把画面里烧死的硬字幕读出来 */
async function runOcr() {
  if (mediaId.value === null) {
    message.warning("请先选择素材");
    return;
  }
  if (!store.ocr?.available) {
    message.warning("OCR 运行时未就绪，请到「设置」指定装有 rapidocr_onnxruntime 的 Python");
    return;
  }
  if (cues.value.length > 0 && !ocrOverwrite.value) {
    message.warning(`当前已有 ${cues.value.length} 条字幕。要覆盖请先勾选「覆盖现有字幕」`);
    return;
  }

  ocrBusy.value = true;
  ocrLog.value = "";
  try {
    const report = await api.recognizeHardSubtitles(
      mediaId.value,
      {
        sampleFps: ocrFps.value,
        bottomRatio: ocrBottomRatio.value,
        minConfidence: 0.5,
        mergeThreshold: 0.85,
        minDurationMs: 400,
        maxFrames: 4000,
      },
      ocrOverwrite.value,
    );
    ocrLog.value =
      `抽帧 ${report.framesSampled} 帧，其中 ${report.framesWithText} 帧读到文字；` +
      `生成 ${report.cueCount} 条字幕（引擎：${report.engine}）`;
    await load();
    ocrOverwrite.value = false;
    message.success(`识别完成，共 ${report.cueCount} 条字幕`);
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    ocrBusy.value = false;
  }
}

async function importSrtFile() {
  if (mediaId.value === null) {
    message.warning("请先选择素材");
    return;
  }
  const picked = await open({
    multiple: false,
    title: "选择 SRT 字幕",
    filters: [{ name: "字幕文件", extensions: ["srt"] }],
  });
  if (typeof picked !== "string") return;

  busy.value = true;
  try {
    const outcome = await api.importSrt(mediaId.value, picked);
    warnings.value = outcome.warnings;
    await load();
    if (outcome.warnings.length > 0) {
      message.warning(`导入 ${outcome.count} 条，另有 ${outcome.warnings.length} 处问题已列出`);
    } else {
      message.success(`导入 ${outcome.count} 条字幕`);
    }
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

async function exportSrtFile() {
  if (mediaId.value === null) {
    message.warning("请先选择素材");
    return;
  }
  if (dirty.value) {
    message.warning("请先保存修改，再导出");
    return;
  }
  const target = await save({
    title: "保存字幕",
    defaultPath: `${selected.value?.name.replace(/\.[^.]+$/, "") ?? "subtitle"}.srt`,
    filters: [{ name: "字幕文件", extensions: ["srt"] }],
  });
  if (typeof target !== "string") return;
  busy.value = true;
  try {
    const count = await api.exportSrt(mediaId.value, target);
    message.success(`已导出 ${count} 条字幕`);
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

function addCue() {
  const last = cues.value[cues.value.length - 1];
  const start = last ? last.endMs : 0;
  cues.value.push({
    id: 0,
    mediaId: mediaId.value ?? 0,
    indexNo: cues.value.length + 1,
    startMs: start,
    endMs: start + 2_000,
    text: "",
  });
  dirty.value = true;
}

function removeCue(index: number) {
  cues.value.splice(index, 1);
  renumber();
  dirty.value = true;
}

watch(cues, () => {
  if (!loading.value) dirty.value = true;
}, { deep: true });

watch(mediaId, () => {
  asrLog.value = "";
  warnings.value = [];
  void load();
});

onMounted(() => {
  void api
    .asrModels()
    .then((list) => {
      bundledModels.value = list;
    })
    .catch(() => {
      bundledModels.value = [];
    });
  if (store.media.length > 0) mediaId.value = store.media[0].id;
  else void load();
});
</script>

<template>
  <div v-if="store.media.length === 0" class="dd-card">
    <div class="dd-card-title">没有可配音的素材</div>
    <div class="dd-card-desc">先到「素材库」导入视频。</div>
  </div>

  <template v-else>
    <div class="dd-card">
      <div class="dd-row" style="justify-content: space-between">
        <div>
          <div class="dd-card-title">字幕轴</div>
          <div class="dd-card-desc">
            共 {{ cues.length }} 条，覆盖 {{ formatDuration(totalMs) }}
            <span v-if="dirty" style="color: #d03050">（有未保存的修改）</span>
          </div>
        </div>
        <n-space :size="8">
          <n-button size="small" :loading="busy" @click="importSrtFile">导入 SRT</n-button>
          <n-button size="small" :loading="busy" :disabled="cues.length === 0" @click="exportSrtFile">
            导出 SRT
          </n-button>
          <n-button size="small" type="primary" :loading="busy" :disabled="!dirty" @click="saveCues">
            保存
          </n-button>
        </n-space>
      </div>

      <div class="dd-row" style="margin-bottom: 12px">
        <span class="dd-muted" style="width: 64px">素材</span>
        <n-select v-model:value="mediaId" :options="mediaOptions" style="width: 380px" />
      </div>

      <div class="dd-row">
        <span class="dd-muted" style="width: 64px">识别</span>
        <n-select v-model:value="model" :options="modelOptions" size="small" style="width: 190px" />
        <n-select v-model:value="language" :options="languageOptions" size="small" style="width: 130px" />
        <n-button size="small" type="primary" :loading="busy" @click="runAsr">
          生成字幕（本地）
        </n-button>
        <n-button
          size="small"
          :loading="cloudBusy"
          :disabled="!store.cloud?.hasKey"
          @click="runCloud"
        >
          云端转写
        </n-button>
        <n-tag v-if="store.asr" size="small" :type="store.asr.available ? 'success' : 'warning'" :bordered="false">
          {{ store.asr.available ? "本地就绪" : "本地未就绪" }}
        </n-tag>
        <n-tag
          v-if="store.cloud"
          size="small"
          :type="store.cloud.hasKey ? 'success' : 'warning'"
          :bordered="false"
        >
          {{ store.cloud.hasKey ? `云端 ${store.cloud.keyHint}` : "云端未配置" }}
        </n-tag>
      </div>

      <div class="dd-row" style="margin-top: 10px">
        <span class="dd-muted" style="width: 64px"></span>
        <n-checkbox v-model:checked="asrOverwrite">
          覆盖现有字幕（当前 {{ cues.length }} 条）
        </n-checkbox>
        <span class="dd-muted">本地 ASR 与云端转写共用这一项；不勾选时已有字幕的素材会被拒绝</span>
      </div>

      <n-alert
        v-if="store.asr?.available && bundledModels.length > 0"
        type="info"
        style="margin-top: 10px"
        title="关于模型规格"
      >
        本地已内置 {{ bundledModels.join(" / ") }}。选「需联网下载」的规格时，程序会去
        HuggingFace 取模型 —— 在部分网络下这会超时，建议先用已内置的。
        识别结果会自动繁转简、标点转全角。
      </n-alert>

      <n-alert
        v-if="store.asr && !store.asr.available"
        type="warning"
        style="margin-top: 12px"
        title="ASR 不可用"
      >
        {{ store.asr.detail }}
      </n-alert>

      <n-alert v-if="asrLog" type="success" style="margin-top: 12px" title="上次本地识别结果">
        {{ asrLog }}
      </n-alert>

      <n-alert v-if="cloudLog" type="success" style="margin-top: 12px" title="上次云端转写结果">
        {{ cloudLog }}
      </n-alert>

      <n-alert
        v-if="warnings.length > 0"
        type="warning"
        style="margin-top: 12px"
        :title="`导入时有 ${warnings.length} 处问题（已尽量保留可用内容）`"
      >
        <ul style="margin: 0; padding-left: 18px">
          <li v-for="(w, i) in warnings.slice(0, 8)" :key="i">{{ w }}</li>
        </ul>
      </n-alert>
    </div>

    <div class="dd-card">
      <div class="dd-row" style="justify-content: space-between">
        <div>
          <div class="dd-card-title">硬字幕识别（OCR）</div>
          <div class="dd-card-desc">
            素材把字幕烧在画面上时，用 OCR 把它们读出来变成可编辑的字幕轴。本地 ONNX 推理，不联网。
          </div>
        </div>
        <n-tag
          v-if="store.ocr"
          size="small"
          :type="store.ocr.available ? 'success' : 'warning'"
          :bordered="false"
        >
          {{ store.ocr.available ? "运行时就绪" : "运行时未就绪" }}
        </n-tag>
      </div>

      <div class="dd-row" style="margin-bottom: 10px">
        <span class="dd-muted" style="width: 76px">抽帧频率</span>
        <n-input-number v-model:value="ocrFps" size="small" :min="0.5" :max="5" :step="0.5" style="width: 120px" />
        <span class="dd-muted">帧/秒。硬字幕一般停留 1 秒以上，2 帧/秒足够</span>
      </div>

      <div class="dd-row" style="margin-bottom: 10px">
        <span class="dd-muted" style="width: 76px">识别区域</span>
        <n-select
          v-model:value="ocrRegion"
          size="small"
          style="width: 230px"
          :options="[
            { label: '画面底部 30%（推荐）', value: 'bottom30' },
            { label: '画面底部 50%', value: 'bottom50' },
            { label: '整帧（字幕位置不固定）', value: 'full' },
          ]"
        />
        <span class="dd-muted">只认底部能少认招牌、水印、弹幕</span>
      </div>

      <div class="dd-row" style="margin-bottom: 14px">
        <span class="dd-muted" style="width: 76px">覆盖</span>
        <n-checkbox v-model:checked="ocrOverwrite">
          覆盖现有字幕（当前 {{ cues.length }} 条）
        </n-checkbox>
        <n-button size="small" type="primary" :loading="ocrBusy" @click="runOcr">
          识别硬字幕
        </n-button>
      </div>

      <n-alert v-if="store.ocr && !store.ocr.available" type="warning" title="OCR 不可用">
        {{ store.ocr.detail }}
      </n-alert>

      <n-alert v-if="ocrLog" type="success" style="margin-top: 12px" title="上次识别结果">
        {{ ocrLog }}
      </n-alert>
    </div>

    <div class="dd-card">
      <div class="dd-row" style="justify-content: space-between; margin-bottom: 10px">
        <div class="dd-card-title">逐条编辑</div>
        <n-button size="small" @click="addCue">新增一条</n-button>
      </div>

      <div v-if="cues.length === 0" class="dd-empty">
        还没有字幕。可以「生成字幕（ASR）」自动识别，或者「导入 SRT」。
      </div>

      <div v-else class="dd-scroll" style="max-height: 460px">
        <table class="dd-table">
          <thead>
            <tr>
              <th style="width: 52px">#</th>
              <th style="width: 130px">起点(ms)</th>
              <th style="width: 130px">终点(ms)</th>
              <th>文本</th>
              <th style="width: 56px"></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(c, i) in cues" :key="i">
              <td class="dd-muted">{{ i + 1 }}</td>
              <td>
                <n-input-number v-model:value="c.startMs" size="tiny" :min="0" :show-button="false" />
                <div class="dd-mono dd-muted">{{ formatTimecode(c.startMs) }}</div>
              </td>
              <td>
                <n-input-number v-model:value="c.endMs" size="tiny" :min="0" :show-button="false" />
                <div class="dd-mono dd-muted">{{ formatTimecode(c.endMs) }}</div>
              </td>
              <td class="dd-wrap">
                <n-input
                  v-model:value="c.text"
                  type="textarea"
                  size="tiny"
                  :autosize="{ minRows: 1, maxRows: 3 }"
                  placeholder="字幕文本"
                />
              </td>
              <td>
                <n-button size="tiny" quaternary type="error" @click="removeCue(i)">删</n-button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>
  </template>
</template>
