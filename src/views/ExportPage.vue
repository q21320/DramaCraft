<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { NAlert, NButton, NInput, NInputNumber, NSelect, NSpace, NSwitch, NTag, useMessage } from "naive-ui";
import { save } from "@tauri-apps/plugin-dialog";
import { api } from "@/api";
import { useAppStore } from "@/stores/app";
import type { AspectMode, ExportOutcome } from "@/types";
import { formatDuration, formatSize } from "@/utils/format";

const store = useAppStore();
const message = useMessage();

const mediaId = ref<number | null>(null);
const outPath = ref("");
const aspectKind = ref<"keep" | "verticalBlur" | "verticalCrop">("verticalBlur");
const burn = ref(true);
const crf = ref(23);
const preset = ref("veryfast");
const rangeStart = ref<number | null>(null);
const rangeEnd = ref<number | null>(null);
const busy = ref(false);
const cueCount = ref(0);
const result = ref<ExportOutcome | null>(null);

const mediaOptions = computed(() =>
  store.media.map((m) => ({
    label: `${m.name}（${formatDuration(m.durationMs)}）`,
    value: m.id,
  })),
);

const presetOptions = ["ultrafast", "veryfast", "fast", "medium", "slow"].map((p) => ({
  label: p,
  value: p,
}));

const selected = computed(() => store.media.find((m) => m.id === mediaId.value) ?? null);

const aspect: AspectMode = { mode: "keep" } as AspectMode;

function currentAspect(): AspectMode {
  return { mode: aspectKind.value } as AspectMode;
}

const aspectHint = computed(() => {
  switch (aspectKind.value) {
    case "keep":
      return "保持原始画幅。不烧字幕时走流拷贝，最快且无损。";
    case "verticalCrop":
      return "等比放大后居中裁切为 1080×1920。画面填满，但左右会被切掉。";
    default:
      return "转为 1080×1920，原画面居中，上下用模糊背景填充。适合横屏素材直接发竖屏平台。";
  }
});

async function pickOutput() {
  const stem = selected.value?.name.replace(/\.[^.]+$/, "") ?? "output";
  const target = await save({
    title: "导出成片",
    defaultPath: `${stem}_out.mp4`,
    filters: [{ name: "MP4 视频", extensions: ["mp4"] }],
  });
  if (typeof target === "string") outPath.value = target;
}

async function refreshCueCount() {
  if (mediaId.value === null) {
    cueCount.value = 0;
    return;
  }
  try {
    cueCount.value = (await api.listCues(mediaId.value)).length;
  } catch {
    cueCount.value = 0;
  }
}

async function run() {
  if (mediaId.value === null) {
    message.warning("请先选择素材");
    return;
  }
  if (!outPath.value.trim()) {
    message.warning("请先选择导出路径");
    return;
  }
  if (burn.value && cueCount.value === 0) {
    message.warning("这个素材还没有字幕，无法烧进画面");
    return;
  }

  const startMs = Math.max(0, Math.round((rangeStart.value ?? 0) * 1000));
  const endMs = Math.max(0, Math.round((rangeEnd.value ?? 0) * 1000));
  if (endMs > 0 && endMs <= startMs) {
    message.warning("结束时间必须大于开始时间");
    return;
  }

  busy.value = true;
  result.value = null;
  try {
    result.value = await api.exportVideo({
      mediaId: mediaId.value,
      outPath: outPath.value.trim(),
      burnSubtitles: burn.value,
      aspect: currentAspect(),
      startMs,
      endMs,
      crf: crf.value,
      preset: preset.value,
    });
    message.success("导出完成");
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

watch(mediaId, () => {
  result.value = null;
  void refreshCueCount();
});

onMounted(async () => {
  if (store.media.length > 0) mediaId.value = store.media[0].id;
  await refreshCueCount();
  void aspect;
});
</script>

<template>
  <div v-if="store.media.length === 0" class="dd-card">
    <div class="dd-card-title">没有可导出的素材</div>
    <div class="dd-card-desc">先到「素材库」导入视频。</div>
  </div>

  <template v-else>
    <div class="dd-card">
      <div class="dd-card-title">导出成片</div>
      <div class="dd-card-desc">
        可以烧字幕、转竖屏、只导出一段。选择「保留原画幅」且不烧字幕时会走流拷贝，不重新编码。
      </div>

      <div class="dd-row" style="margin-bottom: 10px">
        <span class="dd-muted" style="width: 64px">素材</span>
        <n-select v-model:value="mediaId" :options="mediaOptions" style="width: 380px" />
        <n-tag size="small" :bordered="false">字幕 {{ cueCount }} 条</n-tag>
      </div>

      <div class="dd-row" style="margin-bottom: 10px">
        <span class="dd-muted" style="width: 64px">输出</span>
        <n-input v-model:value="outPath" size="small" style="width: 460px" placeholder="导出文件路径" />
        <n-button size="small" @click="pickOutput">另存为</n-button>
      </div>

      <div class="dd-row" style="margin-bottom: 10px">
        <span class="dd-muted" style="width: 64px">画幅</span>
        <n-select
          v-model:value="aspectKind"
          size="small"
          style="width: 260px"
          :options="[
            { label: '保持原画幅', value: 'keep' },
            { label: '竖屏 9:16（模糊背景）', value: 'verticalBlur' },
            { label: '竖屏 9:16（裁切填充）', value: 'verticalCrop' },
          ]"
        />
        <span class="dd-muted">{{ aspectHint }}</span>
      </div>

      <div class="dd-row" style="margin-bottom: 10px">
        <span class="dd-muted" style="width: 64px">选项</span>
        <n-switch v-model:value="burn" size="small" />
        <span class="dd-muted">烧字幕进画面</span>
        <span class="dd-muted" style="margin-left: 16px">CRF</span>
        <n-input-number v-model:value="crf" size="small" :min="0" :max="51" style="width: 110px" />
        <span class="dd-muted">预设</span>
        <n-select v-model:value="preset" size="small" :options="presetOptions" style="width: 140px" />
      </div>

      <div class="dd-row" style="margin-bottom: 14px">
        <span class="dd-muted" style="width: 64px">区间</span>
        <n-input-number v-model:value="rangeStart" size="small" :min="0" placeholder="开始(秒)" style="width: 140px" />
        <span class="dd-muted">→</span>
        <n-input-number v-model:value="rangeEnd" size="small" :min="0" placeholder="结束(秒)" style="width: 140px" />
        <span class="dd-muted">留空则导出整片；填了区间时字幕会自动裁到该区间</span>
      </div>

      <n-space>
        <n-button size="small" type="primary" :loading="busy" @click="run">开始导出</n-button>
      </n-space>
    </div>

    <div v-if="result" class="dd-card">
      <div class="dd-card-title">导出结果</div>
      <table class="dd-table">
        <tbody>
          <tr>
            <th style="width: 110px">文件</th>
            <td class="dd-wrap dd-mono">{{ result.outPath }}</td>
          </tr>
          <tr>
            <th>体积</th>
            <td>{{ formatSize(result.sizeBytes) }}</td>
          </tr>
          <tr>
            <th>滤镜</th>
            <td class="dd-wrap dd-mono">{{ result.filter }}</td>
          </tr>
        </tbody>
      </table>
      <n-alert v-if="result.log" type="default" style="margin-top: 12px" title="ffmpeg 输出（截断）">
        <pre class="dd-mono dd-wrap" style="margin: 0; white-space: pre-wrap">{{ result.log }}</pre>
      </n-alert>
    </div>
  </template>
</template>
