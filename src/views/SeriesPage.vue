<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { NAlert, NButton, NInput, NInputNumber, NSelect, NSpace, NTag, useMessage } from "naive-ui";
import { open } from "@tauri-apps/plugin-dialog";
import { api } from "@/api";
import { useAppStore } from "@/stores/app";
import type { EpisodeDetection, ResplitReport, SeriesGroup } from "@/types";
import { formatDuration, formatTimecode } from "@/utils/format";

const store = useAppStore();
const message = useMessage();

// 剧目重组
const scanDir = ref("");
const groups = ref<SeriesGroup[]>([]);
const scanning = ref(false);
const clustering = ref(false);
const clustered = ref(false);

// 集数重切
const mediaId = ref<number | null>(null);
const sampleFps = ref(2);
const introSeconds = ref(3);
const tolerance = ref(6);
const detection = ref<EpisodeDetection | null>(null);
const detecting = ref(false);
const outDir = ref("");
const resplitting = ref(false);
const resplitReport = ref<ResplitReport | null>(null);

const mediaOptions = computed(() =>
  store.media.map((m) => ({
    label: `${m.name}（${formatDuration(m.durationMs)}）`,
    value: m.id,
  })),
);

const totalFiles = computed(() =>
  groups.value.reduce((sum, g) => sum + g.episodes.length, 0),
);

const allPaths = computed(() => groups.value.flatMap((g) => g.episodes.map((e) => e.path)));

const canDetect = computed(() => mediaId.value !== null);

async function pickScanDir() {
  const picked = await open({ directory: true, multiple: false, title: "选择素材目录" });
  if (typeof picked === "string") {
    scanDir.value = picked;
    await scan();
  }
}

async function scan() {
  if (!scanDir.value.trim()) {
    message.warning("请先选择要扫描的目录");
    return;
  }
  scanning.value = true;
  clustered.value = false;
  try {
    groups.value = await api.scanSeriesDir(scanDir.value.trim());
    if (groups.value.length === 0) {
      message.warning("这个目录下没找到视频文件");
    } else {
      message.success(`解析出 ${groups.value.length} 个剧目，共 ${totalFiles.value} 个文件`);
    }
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    scanning.value = false;
  }
}

/** 文件名实在没规律时，用片头指纹重新聚类 */
async function clusterByIntro() {
  if (allPaths.value.length === 0) {
    message.warning("先扫描一个目录");
    return;
  }
  clustering.value = true;
  try {
    groups.value = await api.clusterSeriesByIntro(allPaths.value, 3, 10);
    clustered.value = true;
    message.success(`按片头聚类出 ${groups.value.length} 组`);
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    clustering.value = false;
  }
}

async function pickOutDir() {
  const picked = await open({ directory: true, multiple: false, title: "选择输出目录" });
  if (typeof picked === "string") outDir.value = picked;
}

async function detect() {
  if (mediaId.value === null) {
    message.warning("请先选择素材");
    return;
  }
  detecting.value = true;
  detection.value = null;
  resplitReport.value = null;
  try {
    detection.value = await api.detectEpisodes(
      mediaId.value,
      sampleFps.value,
      introSeconds.value,
      tolerance.value,
    );
    message.success(`检测到 ${detection.value.boundaries.length} 集`);
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    detecting.value = false;
  }
}

async function resplit() {
  if (mediaId.value === null || !detection.value) {
    message.warning("请先检测集边界");
    return;
  }
  if (!outDir.value.trim()) {
    message.warning("请先选择输出目录");
    return;
  }
  resplitting.value = true;
  resplitReport.value = null;
  try {
    resplitReport.value = await api.resplitEpisodes(
      mediaId.value,
      detection.value.boundaries,
      outDir.value.trim(),
    );
    message.success(`已切出 ${resplitReport.value.episodeCount} 集`);
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    resplitting.value = false;
  }
}

onMounted(() => {
  if (store.media.length > 0) mediaId.value = store.media[0].id;
});
</script>

<template>
  <div class="dd-card">
    <div class="dd-row" style="justify-content: space-between">
      <div>
        <div class="dd-card-title">剧目重组</div>
        <div class="dd-card-desc">
          扫描一个目录，按文件名里的「第N集 / EP05 / S01E07」这类标记，把素材分回各自剧目。
        </div>
      </div>
      <n-space :size="8">
        <n-tag v-if="groups.length > 0" size="small" :bordered="false">
          {{ groups.length }} 个剧目 / {{ totalFiles }} 个文件
        </n-tag>
        <n-tag v-if="clustered" size="small" type="info" :bordered="false">按片头聚类</n-tag>
      </n-space>
    </div>

    <div class="dd-row" style="margin-bottom: 12px">
      <span class="dd-muted" style="width: 64px">目录</span>
      <n-input v-model:value="scanDir" size="small" style="width: 440px" placeholder="素材所在目录" />
      <n-button size="small" @click="pickScanDir">选择并扫描</n-button>
      <n-button size="small" :loading="scanning" @click="scan">重新扫描</n-button>
    </div>

    <div v-if="groups.length > 0" class="dd-row" style="margin-bottom: 12px">
      <n-button size="small" :loading="clustering" @click="clusterByIntro">
        文件名没规律？按片头重新聚类
      </n-button>
      <span class="dd-muted">同一部剧的片头画面是一样的，用画面指纹把它们找回来</span>
    </div>

    <div v-if="groups.length === 0" class="dd-empty">
      还没有扫描结果。选一个目录试试。
    </div>

    <div v-else class="dd-scroll" style="max-height: 340px">
      <table class="dd-table">
        <thead>
          <tr>
            <th style="width: 240px">剧目</th>
            <th style="width: 70px">集数</th>
            <th>文件</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="g in groups" :key="g.series">
            <td>{{ g.series }}</td>
            <td>{{ g.episodes.length }}</td>
            <td class="dd-wrap">
              <div v-for="e in g.episodes" :key="e.path" class="dd-mono">
                <span v-if="e.episode !== null" class="dd-muted">第{{ e.episode }}集 · </span>
                <span v-else class="dd-muted">无集号 · </span>
                {{ e.fileName }}
              </div>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>

  <div class="dd-card">
    <div class="dd-card-title">集数重切</div>
    <div class="dd-card-desc">
      一整条长视频里连播好几集时，用「片头」当路标：把开头一段当模板，在全片找它每次出现的位置，
      那就是每集的起点。全程不联网，只用画面指纹。
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 64px">素材</span>
      <n-select
        v-if="store.media.length > 0"
        v-model:value="mediaId"
        :options="mediaOptions"
        style="width: 400px"
      />
      <span v-else class="dd-muted">先到「素材库」导入一条长视频</span>
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 64px">采样</span>
      <n-input-number v-model:value="sampleFps" size="small" :min="0.2" :max="10" :step="0.5" style="width: 110px" />
      <span class="dd-muted">帧/秒</span>
      <span class="dd-muted" style="margin-left: 14px">片头时长</span>
      <n-input-number v-model:value="introSeconds" size="small" :min="0.5" :max="60" :step="0.5" style="width: 110px" />
      <span class="dd-muted">秒</span>
      <span class="dd-muted" style="margin-left: 14px">容忍度</span>
      <n-input-number v-model:value="tolerance" size="small" :min="0" :max="24" style="width: 90px" />
      <span class="dd-muted">位（越大越宽松）</span>
    </div>

    <n-space style="margin-bottom: 12px">
      <n-button size="small" type="primary" :loading="detecting" :disabled="!canDetect" @click="detect">
        检测集边界
      </n-button>
    </n-space>

    <div v-if="detection">
      <div class="dd-row" style="margin-bottom: 10px">
        <n-tag size="small" :type="detection.boundaries.length > 1 ? 'success' : 'warning'" :bordered="false">
          检测到 {{ detection.boundaries.length }} 集
        </n-tag>
        <span class="dd-muted">
          采样 {{ detection.sampledFrames }} 帧，片头模板 {{ detection.templateFrames }} 帧，
          原片 {{ formatDuration(detection.totalDurationMs) }}
        </span>
      </div>

      <div class="dd-scroll" style="max-height: 260px; margin-bottom: 12px">
        <table class="dd-table">
          <thead>
            <tr>
              <th style="width: 60px">集</th>
              <th>起点</th>
              <th>终点</th>
              <th style="width: 90px">时长</th>
              <th style="width: 80px">置信度</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="b in detection.boundaries" :key="b.indexNo">
              <td>{{ b.indexNo }}</td>
              <td class="dd-mono">{{ formatTimecode(b.startMs) }}</td>
              <td class="dd-mono">{{ formatTimecode(b.endMs) }}</td>
              <td>{{ formatDuration(b.durationMs) }}</td>
              <td>
                <n-tag size="tiny" :type="b.confidence === 'high' ? 'success' : 'warning'" :bordered="false">
                  {{ b.confidence === "high" ? "高" : "低" }}
                </n-tag>
              </td>
            </tr>
          </tbody>
        </table>
      </div>

      <div class="dd-row" style="margin-bottom: 12px">
        <span class="dd-muted" style="width: 64px">输出到</span>
        <n-input v-model:value="outDir" size="small" style="width: 400px" placeholder="每集一个文件的输出目录" />
        <n-button size="small" @click="pickOutDir">选择</n-button>
        <n-button size="small" type="primary" :loading="resplitting" @click="resplit">
          按边界切分
        </n-button>
      </div>

      <n-alert v-if="detection.warnings.length > 0" type="info" title="说明">
        <ul style="margin: 0; padding-left: 18px">
          <li v-for="(w, i) in detection.warnings" :key="i" class="dd-wrap">{{ w }}</li>
        </ul>
      </n-alert>
    </div>

    <div v-if="resplitReport" class="dd-card" style="margin: 12px 0 0">
      <div class="dd-card-title">切分完成</div>
      <div class="dd-card-desc">
        共 {{ resplitReport.episodeCount }} 集，总时长 {{ formatDuration(resplitReport.totalDurationMs) }}，
        输出到 <span class="dd-mono">{{ resplitReport.outDir }}</span>
      </div>
      <div class="dd-scroll" style="max-height: 200px">
        <table class="dd-table">
          <tbody>
            <tr v-for="f in resplitReport.files" :key="f">
              <td class="dd-wrap dd-mono">{{ f }}</td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>
  </div>
</template>
