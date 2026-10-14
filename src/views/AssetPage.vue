<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
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
import { open } from "@tauri-apps/plugin-dialog";
import { api } from "@/api";
import { useAppStore } from "@/stores/app";
import type {
  Asset,
  AssetApplyReport,
  AssetImportReport,
  AssetKindOption,
  AssetSummary,
  DiscoveredAsset,
} from "@/types";
import { formatDuration } from "@/utils/format";

const store = useAppStore();
const message = useMessage();

const kinds = ref<AssetKindOption[]>([]);
const assets = ref<Asset[]>([]);
const summary = ref<AssetSummary | null>(null);
const filterKind = ref<string | null>(null);
const busy = ref(false);

// 导入
const discovered = ref<DiscoveredAsset[]>([]);
const scanDir = ref("");
const importCategory = ref("");
const skipped = ref<string[]>([]);

// 应用
const targetMedia = ref<number | null>(null);
const audioAssetId = ref<number | null>(null);
const lutAssetId = ref<number | null>(null);
const bgmMode = ref("mix");
const bgmVolume = ref(0.35);
const originalVolume = ref(1);
const loopBgm = ref(true);
const lutIntensity = ref(1);
const applyReport = ref<AssetApplyReport | null>(null);

const audioAssets = computed(() => assets.value.filter((a) => a.kind === "audio"));
const lutAssets = computed(() => assets.value.filter((a) => a.kind === "lut"));

const assetOptions = (list: Asset[]) =>
  list.map((a) => ({
    label: `${a.name}${lutSizeText(a)}`,
    value: a.id,
  }));

const mediaOptions = computed(() =>
  store.media.map((m) => ({
    label: `${m.name}（${formatDuration(m.durationMs)}）`,
    value: m.id,
  })),
);

function lutSizeText(a: Asset): string {
  if (a.kind !== "lut" || !a.meta) return "";
  try {
    const info = JSON.parse(a.meta) as { size?: number; title?: string };
    return info.size ? `（${info.size}³）` : "";
  } catch {
    return "";
  }
}

function kindLabel(kind: string): string {
  return kinds.value.find((k) => k.key === kind)?.label ?? kind;
}

function sizeText(bytes: number): string {
  if (bytes <= 0) return "—";
  const mb = bytes / 1024 / 1024;
  return mb >= 1 ? `${mb.toFixed(1)} MB` : `${(bytes / 1024).toFixed(0)} KB`;
}

async function refresh() {
  try {
    const [list, sum] = await Promise.all([
      api.listAssets(filterKind.value ?? undefined),
      api.assetSummary(),
    ]);
    assets.value = list;
    summary.value = sum;
  } catch (e) {
    message.error((e as Error).message);
  }
}

async function pickFiles() {
  const picked = await open({
    multiple: true,
    title: "选择要导入的素材（音频或 .cube）",
    filters: [
      { name: "素材", extensions: ["mp3", "wav", "m4a", "aac", "flac", "ogg", "cube"] },
    ],
  });
  if (!picked) return;
  const paths = Array.isArray(picked) ? picked : [picked];
  await doImport(paths);
}

async function pickScanDir() {
  const picked = await open({ directory: true, multiple: false, title: "选择素材目录" });
  if (typeof picked !== "string") return;
  scanDir.value = picked;
  try {
    discovered.value = await api.scanAssetDir(picked);
    if (discovered.value.length === 0) {
      message.warning("这个目录里没有可导入的素材（只认音频与 .cube）");
    }
  } catch (e) {
    message.error((e as Error).message);
  }
}

async function importDiscovered() {
  const paths = discovered.value.map((d) => d.path);
  if (paths.length === 0) {
    message.warning("先扫描一个目录");
    return;
  }
  await doImport(paths);
  discovered.value = [];
}

async function doImport(paths: string[]) {
  if (paths.length === 0) {
    message.warning("没有要导入的文件");
    return;
  }
  busy.value = true;
  skipped.value = [];
  try {
    const report: AssetImportReport = await api.importAssets(paths, importCategory.value);
    skipped.value = report.skipped;
    summary.value = report.summary;
    await refresh();
    if (report.imported === 0) {
      message.warning("没有导入任何文件，看看下面的原因");
    } else {
      message.success(`已导入 ${report.imported} 个素材`);
    }
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

async function saveMeta(a: Asset) {
  try {
    await api.updateAsset(a.id, a.name, a.category, a.tags, a.note);
    await refresh();
    message.success("已保存");
  } catch (e) {
    message.error((e as Error).message);
  }
}

async function remove(a: Asset) {
  try {
    await api.deleteAsset(a.id);
    await refresh();
    message.success(`已从库里移除「${a.name}」（磁盘上的文件没动）`);
  } catch (e) {
    message.error((e as Error).message);
  }
}

async function runApplyBgm() {
  if (targetMedia.value === null || audioAssetId.value === null) {
    message.warning("请先选择素材和音频");
    return;
  }
  busy.value = true;
  applyReport.value = null;
  try {
    applyReport.value = await api.applyBgm(
      targetMedia.value,
      audioAssetId.value,
      bgmMode.value,
      bgmVolume.value,
      originalVolume.value,
      loopBgm.value,
    );
    message.success("已配好 BGM");
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

async function runApplyLut() {
  if (targetMedia.value === null || lutAssetId.value === null) {
    message.warning("请先选择素材和 LUT");
    return;
  }
  busy.value = true;
  applyReport.value = null;
  try {
    applyReport.value = await api.applyLut(
      targetMedia.value,
      lutAssetId.value,
      lutIntensity.value,
    );
    message.success("已套用 LUT");
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

onMounted(async () => {
  try {
    kinds.value = await api.assetKinds();
  } catch (e) {
    message.error((e as Error).message);
  }
  if (store.media.length > 0) targetMedia.value = store.media[0].id;
  await refresh();
});
</script>

<template>
  <div class="dd-card">
    <div class="dd-card-desc">
      <n-alert type="info" title="这里不内置任何素材">
        原软件打包了一整套音效和面板素材，那些资产的授权状况不明。所以这个库是「空的开始」：
        你自己的音效、BGM、LUT 导进来就能用，版权始终在你手上。
        从库里移除记录不会删磁盘上的文件。
      </n-alert>
    </div>

    <div class="dd-row" style="margin: 12px 0 10px">
      <span class="dd-muted" style="width: 64px">导入</span>
      <n-button size="small" type="primary" :loading="busy" @click="pickFiles">
        选择文件
      </n-button>
      <span class="dd-muted">或扫描一个目录：</span>
      <n-input v-model:value="scanDir" size="small" style="width: 320px" placeholder="素材目录" />
      <n-button size="small" @click="pickScanDir">扫描</n-button>
      <n-button
        size="small"
        :disabled="discovered.length === 0"
        :loading="busy"
        @click="importDiscovered"
      >
        导入扫描到的 {{ discovered.length }} 个
      </n-button>
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 64px">分类</span>
      <n-input
        v-model:value="importCategory"
        size="small"
        style="width: 240px"
        placeholder="例如：悬疑 / 提示音（可留空）"
      />
      <span class="dd-muted">支持 MP3 / WAV / M4A / AAC / FLAC / OGG 与 .cube</span>
    </div>

    <div v-if="discovered.length > 0" class="dd-scroll" style="max-height: 200px; margin-bottom: 12px">
      <table class="dd-table">
        <thead>
          <tr>
            <th style="width: 240px">文件</th>
            <th style="width: 90px">类型</th>
            <th style="width: 90px">大小</th>
            <th>状态</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="d in discovered" :key="d.path">
            <td class="dd-wrap">{{ d.fileName }}</td>
            <td>{{ kindLabel(d.kind) }}</td>
            <td>{{ sizeText(d.sizeBytes) }}</td>
            <td>
              <n-tag v-if="d.problem" size="tiny" type="error" :bordered="false">有问题</n-tag>
              <n-tag v-else size="tiny" type="success" :bordered="false">可导入</n-tag>
              <span v-if="d.problem" class="dd-muted dd-wrap"> {{ d.problem }}</span>
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <n-alert v-if="skipped.length > 0" type="warning" title="这些文件没有导入">
      <ul style="margin: 0; padding-left: 18px">
        <li v-for="(s, i) in skipped" :key="i" class="dd-wrap">{{ s }}</li>
      </ul>
    </n-alert>
  </div>

  <div class="dd-card">
    <div class="dd-row" style="justify-content: space-between; margin-bottom: 10px">
      <div>
        <div class="dd-card-title">素材库</div>
        <div class="dd-card-desc">点名称即可直接改，改完点「存」。</div>
      </div>
      <n-space :size="8">
        <n-tag v-if="summary" size="small" :bordered="false">音频 {{ summary.audio }}</n-tag>
        <n-tag v-if="summary" size="small" :bordered="false">LUT {{ summary.lut }}</n-tag>
        <n-tag v-if="summary" size="small" :bordered="false">
          合计 {{ sizeText(summary.totalBytes) }}
        </n-tag>
        <n-select
          v-model:value="filterKind"
          size="small"
          style="width: 140px"
          clearable
          placeholder="全部类型"
          :options="kinds.map((k) => ({ label: k.label, value: k.key }))"
          @update:value="refresh"
        />
      </n-space>
    </div>

    <div v-if="assets.length === 0" class="dd-empty">
      库里还没有素材。上面导入你自己的音效 / BGM / LUT。
    </div>

    <div v-else class="dd-scroll" style="max-height: 420px">
      <table class="dd-table">
        <thead>
          <tr>
            <th style="width: 160px">名称</th>
            <th style="width: 90px">类型</th>
            <th style="width: 120px">分类</th>
            <th style="width: 90px">时长</th>
            <th style="width: 90px">大小</th>
            <th>路径</th>
            <th style="width: 60px"></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="a in assets" :key="a.id">
            <td><n-input v-model:value="a.name" size="tiny" /></td>
            <td>
              {{ kindLabel(a.kind) }}
              <span class="dd-muted">{{ lutSizeText(a) }}</span>
            </td>
            <td><n-input v-model:value="a.category" size="tiny" /></td>
            <td class="dd-muted">
              {{ a.kind === "audio" && a.durationMs > 0 ? formatDuration(a.durationMs) : "—" }}
            </td>
            <td class="dd-muted">{{ sizeText(a.sizeBytes) }}</td>
            <td class="dd-wrap dd-mono dd-muted">{{ a.path }}</td>
            <td>
              <n-button size="tiny" quaternary @click="saveMeta(a)">存</n-button>
              <n-button size="tiny" quaternary @click="remove(a)">删</n-button>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>

  <div class="dd-card">
    <div class="dd-card-title">把素材用起来</div>
    <div class="dd-card-desc">
      结果另存为「原名_bgm.mp4」「原名_lut.mp4」，不动原文件。
    </div>

    <div class="dd-row" style="margin-bottom: 12px">
      <span class="dd-muted" style="width: 64px">素材</span>
      <n-select
        v-model:value="targetMedia"
        :options="mediaOptions"
        style="width: 380px"
        placeholder="从素材库里选一条"
      />
    </div>

    <div class="dd-card-title" style="font-size: 13px">配 BGM / 音效</div>
    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 64px">音频</span>
      <n-select
        v-model:value="audioAssetId"
        :options="assetOptions(audioAssets)"
        style="width: 260px"
        placeholder="库里的音频素材"
      />
      <n-select
        v-model:value="bgmMode"
        size="small"
        style="width: 170px"
        :options="[
          { label: '与原声混合', value: 'mix' },
          { label: '替换原声', value: 'replace' },
        ]"
      />
      <span class="dd-muted">BGM 音量</span>
      <n-input-number v-model:value="bgmVolume" size="small" :min="0" :max="4" :step="0.05" style="width: 100px" />
      <span class="dd-muted">原声音量</span>
      <n-input-number v-model:value="originalVolume" size="small" :min="0" :max="4" :step="0.05" style="width: 100px" />
      <n-checkbox v-model:checked="loopBgm">循环铺满</n-checkbox>
      <n-button size="small" type="primary" :loading="busy" @click="runApplyBgm">配 BGM</n-button>
    </div>

    <div class="dd-card-title" style="font-size: 13px">套 LUT 调色</div>
    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 64px">LUT</span>
      <n-select
        v-model:value="lutAssetId"
        :options="assetOptions(lutAssets)"
        style="width: 260px"
        placeholder="库里的 .cube 素材"
      />
      <span class="dd-muted">强度</span>
      <n-input-number
        v-model:value="lutIntensity"
        size="small"
        :min="0"
        :max="1"
        :step="0.05"
        style="width: 100px"
      />
      <span class="dd-muted">小于 1 时与原画面混合</span>
      <n-button size="small" type="primary" :loading="busy" @click="runApplyLut">套用 LUT</n-button>
    </div>

    <n-alert v-if="applyReport" type="success" title="完成">
      <div class="dd-wrap">输出：<span class="dd-mono">{{ applyReport.outPath }}</span></div>
      <div class="dd-muted">{{ sizeText(applyReport.sizeBytes) }}</div>
      <ul v-if="applyReport.warnings.length > 0" style="margin: 6px 0 0; padding-left: 18px">
        <li v-for="(w, i) in applyReport.warnings" :key="i" class="dd-wrap">{{ w }}</li>
      </ul>
    </n-alert>
  </div>
</template>
