<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { NAlert, NButton, NInput, NSelect, NSpace, NTag, useMessage } from "naive-ui";
import { open } from "@tauri-apps/plugin-dialog";
import { api } from "@/api";
import { useAppStore } from "@/stores/app";
import type { DraftReport } from "@/types";
import { formatDuration } from "@/utils/format";

const store = useAppStore();
const message = useMessage();

const mediaId = ref<number | null>(null);
const draftRoot = ref("");
const draftName = ref("");
const fps = ref(30);
const busy = ref(false);
const report = ref<DraftReport | null>(null);
const segmentCount = ref(0);
const cueCount = ref(0);

const mediaOptions = computed(() =>
  store.media.map((m) => ({
    label: `${m.name}（${formatDuration(m.durationMs)}）`,
    value: m.id,
  })),
);

const selected = computed(() => store.media.find((m) => m.id === mediaId.value) ?? null);

const ready = computed(() => segmentCount.value > 0);

async function refreshCounts() {
  if (mediaId.value === null) {
    segmentCount.value = 0;
    cueCount.value = 0;
    return;
  }
  try {
    const [segments, cues] = await Promise.all([
      api.listSegments(mediaId.value),
      api.listCues(mediaId.value),
    ]);
    segmentCount.value = segments.filter((s) => s.status === "ok").length;
    cueCount.value = cues.length;
  } catch {
    segmentCount.value = 0;
    cueCount.value = 0;
  }
}

async function pickRoot() {
  const picked = await open({
    directory: true,
    multiple: false,
    title: "选择剪映草稿根目录",
  });
  if (typeof picked === "string") draftRoot.value = picked;
}

function defaultName() {
  const base = selected.value?.name.replace(/\.[^.]+$/, "") ?? "切片";
  return `${base}_切片`;
}

async function run() {
  if (mediaId.value === null) {
    message.warning("请先选择素材");
    return;
  }
  if (!draftRoot.value.trim()) {
    message.warning("请先选择剪映草稿根目录");
    return;
  }
  if (segmentCount.value === 0) {
    message.warning("这个素材还没有成功的切片，先去「切片工作台」执行切片");
    return;
  }
  busy.value = true;
  report.value = null;
  try {
    report.value = await api.exportDraft(
      mediaId.value,
      draftRoot.value.trim(),
      draftName.value.trim() || defaultName(),
      fps.value,
    );
    if (report.value.warnings.length > 0) {
      message.warning(`已生成，但有 ${report.value.warnings.length} 处提示`);
    } else {
      message.success("草稿已生成，打开剪映即可看到");
    }
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

watch(mediaId, () => {
  report.value = null;
  draftName.value = defaultName();
  void refreshCounts();
});

onMounted(async () => {
  if (store.media.length > 0) mediaId.value = store.media[0].id;
  draftName.value = defaultName();
  await refreshCounts();
});
</script>

<template>
  <div v-if="store.media.length === 0" class="dd-card">
    <div class="dd-card-title">没有可导出的素材</div>
    <div class="dd-card-desc">先到「素材库」导入视频，再去「切片工作台」切片。</div>
  </div>

  <template v-else>
    <div class="dd-card">
      <div class="dd-card-title">导出剪映草稿</div>
      <div class="dd-card-desc">
        把该素材的切片按顺序拼成一条剪映时间轴，字幕按切片区间自动对齐。
        生成后直接打开剪映就能看到这份草稿，可以继续手工微调再导出。
      </div>

      <div class="dd-row" style="margin-bottom: 10px">
        <span class="dd-muted" style="width: 76px">素材</span>
        <n-select v-model:value="mediaId" :options="mediaOptions" style="width: 380px" />
        <n-tag size="small" :type="ready ? 'success' : 'warning'" :bordered="false">
          可用切片 {{ segmentCount }}
        </n-tag>
        <n-tag size="small" :bordered="false">字幕 {{ cueCount }} 条</n-tag>
      </div>

      <div class="dd-row" style="margin-bottom: 10px">
        <span class="dd-muted" style="width: 76px">草稿目录</span>
        <n-input
          v-model:value="draftRoot"
          size="small"
          style="width: 460px"
          placeholder="剪映「全局设置 → 草稿位置」指向的那个目录"
        />
        <n-button size="small" @click="pickRoot">选择</n-button>
      </div>

      <div class="dd-row" style="margin-bottom: 14px">
        <span class="dd-muted" style="width: 76px">草稿名</span>
        <n-input v-model:value="draftName" size="small" style="width: 300px" :placeholder="defaultName()" />
        <span class="dd-muted" style="margin-left: 12px">帧率</span>
        <n-select
          v-model:value="fps"
          size="small"
          style="width: 110px"
          :options="[
            { label: '30 fps', value: 30 },
            { label: '25 fps', value: 25 },
            { label: '60 fps', value: 60 },
          ]"
        />
      </div>

      <n-alert v-if="!ready" type="warning" style="margin-bottom: 12px" title="还没有可用的切片">
        先到「切片工作台」为这个素材生成切片，草稿才有内容可拼。
      </n-alert>

      <n-space>
        <n-button size="small" type="primary" :loading="busy" :disabled="!ready" @click="run">
          生成剪映草稿
        </n-button>
      </n-space>
    </div>

    <div v-if="report" class="dd-card">
      <div class="dd-card-title">生成结果</div>
      <table class="dd-table">
        <tbody>
          <tr>
            <th style="width: 110px">草稿目录</th>
            <td class="dd-wrap dd-mono">{{ report.draftPath }}</td>
          </tr>
          <tr>
            <th>草稿名</th>
            <td>{{ report.draftName }}</td>
          </tr>
          <tr>
            <th>内容</th>
            <td>
              {{ report.clipCount }} 个片段 · {{ report.cueCount }} 条字幕 ·
              总时长 {{ formatDuration(report.durationMs) }}
            </td>
          </tr>
        </tbody>
      </table>

      <n-alert
        v-if="report.warnings.length > 0"
        type="warning"
        style="margin-top: 12px"
        :title="`${report.warnings.length} 处提示`"
      >
        <ul style="margin: 0; padding-left: 18px">
          <li v-for="(w, i) in report.warnings.slice(0, 10)" :key="i">{{ w }}</li>
        </ul>
      </n-alert>

      <n-alert type="info" style="margin-top: 12px" title="如果剪映里没看到草稿">
        确认「草稿目录」填的是剪映当前的草稿位置；若剪映正在运行，重启一次剪映再查看。
      </n-alert>
    </div>

    <div class="dd-card">
      <div class="dd-card-title">前置条件</div>
      <div class="dd-card-desc">
        草稿生成依赖开源库 pyJianYingDraft（Apache-2.0），装在「设置」里指定的那个 Python 中：
      </div>
      <pre class="dd-mono" style="margin: 0; padding: 10px; background: rgba(128,128,128,0.08); border-radius: 6px">pip install pyJianYingDraft</pre>
      <div class="dd-muted" style="margin-top: 8px">
        没装的话，点「生成剪映草稿」会直接告诉你去装什么，不会静默失败。
      </div>
    </div>
  </template>
</template>
