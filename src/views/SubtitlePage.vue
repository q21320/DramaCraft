<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import {
  NAlert,
  NButton,
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

const mediaOptions = computed(() =>
  store.media.map((m) => ({
    label: `${m.name}（${formatDuration(m.durationMs)}）`,
    value: m.id,
  })),
);

const modelOptions = [
  { label: "tiny（最快，草稿用）", value: "tiny" },
  { label: "base", value: "base" },
  { label: "small（推荐）", value: "small" },
  { label: "medium（更准更慢）", value: "medium" },
];

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
  busy.value = true;
  asrLog.value = "";
  try {
    const outcome = await api.asrTranscribe(mediaId.value, model.value, language.value);
    asrLog.value = outcome.log;
    await load();
    warnings.value = [];
    message.success(`识别完成，共 ${outcome.cueCount} 条字幕`);
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
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
          生成字幕（ASR）
        </n-button>
        <n-tag v-if="store.asr" size="small" :type="store.asr.available ? 'success' : 'warning'" :bordered="false">
          {{ store.asr.available ? "运行时就绪" : "运行时未就绪" }}
        </n-tag>
      </div>

      <n-alert
        v-if="store.asr && !store.asr.available"
        type="warning"
        style="margin-top: 12px"
        title="ASR 不可用"
      >
        {{ store.asr.detail }}
      </n-alert>

      <n-alert v-if="asrLog" type="success" style="margin-top: 12px" title="上次识别结果">
        {{ asrLog }}
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
