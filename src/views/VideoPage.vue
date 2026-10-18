<script setup lang="ts">
/**
 * 视频：把项目里的所有视频摊开看。
 *
 * 上面两个 tab —— **最新**（按时间倒序，一屏看完所有片子）和 **分类**
 * （先选类型，再看这一类）。下面一律是视频卡片网格，点开直接播。
 *
 * 数据全部来自项目库，不扫盘：
 *
 *   源视频     media.path
 *   去重成片   media.processed_path（空串表示还没去重过）
 *   切片       segment.out_path（只算 status == "ok" 的）
 *
 * 缩略图不生成额外的图 —— 直接用 `<video preload="metadata">` 让浏览器
 * 自己解第一帧。省一套抽帧缓存，代价是卡片多的时候首屏慢一点。
 */
import { computed, onMounted, ref } from "vue";
import { NButton, NModal, NSpin, NTag, useMessage } from "naive-ui";
import { convertFileSrc } from "@tauri-apps/api/core";
import { api } from "@/api";
import type { Segment } from "@/types";
import { formatDuration } from "@/utils/format";
import SectionCard from "@/components/SectionCard.vue";

type Kind = "source" | "dedup" | "slice";

interface Item {
  key: string;
  kind: Kind;
  path: string;
  title: string;
  drama: string;
  durationMs: number;
  width: number;
  height: number;
  createdAt: string;
}

const message = useMessage();

const tab = ref<"latest" | "category">("latest");
const cat = ref<Kind | "all">("all");
const items = ref<Item[]>([]);
const loading = ref(true);
const playing = ref<Item | null>(null);

const CATS: { key: Kind | "all"; label: string; hint: string }[] = [
  { key: "all", label: "全部", hint: "所有视频" },
  { key: "source", label: "源视频", hint: "导入进来的原片" },
  { key: "dedup", label: "去重成片", hint: "整集去重后的可直接发布版本" },
  { key: "slice", label: "切片", hint: "按秒数切出来的短片段" },
];

const KIND_LABEL: Record<Kind, string> = {
  source: "源视频",
  dedup: "去重成片",
  slice: "切片",
};

function baseName(p: string): string {
  const parts = p.split(/[\\/]/);
  return parts[parts.length - 1] || p;
}

function fileUrl(path: string): string {
  return convertFileSrc(path);
}

/** 按当前 tab / 分类算出来要显示的列表。 */
const shown = computed(() => {
  const list =
    tab.value === "category" && cat.value !== "all"
      ? items.value.filter((v) => v.kind === cat.value)
      : items.value;
  // 新的在前。同一天的按标题排，保证顺序稳定（不然每次渲染都可能变）。
  return [...list].sort((a, b) => {
    if (a.createdAt !== b.createdAt) return a.createdAt < b.createdAt ? 1 : -1;
    return a.title.localeCompare(b.title, "zh");
  });
});

/** 每个分类有多少个，给分类按钮上的角标用。 */
const counts = computed(() => {
  const m: Record<string, number> = { all: items.value.length };
  for (const v of items.value) m[v.kind] = (m[v.kind] ?? 0) + 1;
  return m;
});

async function load() {
  loading.value = true;
  try {
    const out: Item[] = [];
    const projects = await api.listProjects();
    for (const p of projects) {
      const media = await api.listMedia(p.id);
      for (const m of media) {
        out.push({
          key: `s${m.id}`,
          kind: "source",
          path: m.path,
          title: m.name,
          drama: p.name,
          durationMs: m.durationMs,
          width: m.width,
          height: m.height,
          createdAt: p.createdAt,
        });
        // 去重产物和源片同 id，但文件不同 —— key 加前缀区分
        if (m.processedPath) {
          out.push({
            key: `d${m.id}`,
            kind: "dedup",
            path: m.processedPath,
            title: baseName(m.processedPath),
            drama: p.name,
            durationMs: m.durationMs,
            width: m.width,
            height: m.height,
            createdAt: p.createdAt,
          });
        }
        let segs: Segment[] = [];
        try {
          segs = await api.listSegments(m.id);
        } catch {
          // 某集没切过不影响别的
          segs = [];
        }
        for (const s of segs) {
          if (s.status !== "ok" || !s.outPath) continue;
          out.push({
            key: `g${s.id}`,
            kind: "slice",
            path: s.outPath,
            title: baseName(s.outPath),
            drama: p.name,
            durationMs: s.endMs - s.startMs,
            width: m.width,
            height: m.height,
            createdAt: p.createdAt,
          });
        }
      }
    }
    items.value = out;
  } catch (e) {
    message.error(`读取视频列表失败：${(e as Error).message}`);
  } finally {
    loading.value = false;
  }
}

async function reveal(path: string) {
  try {
    await api.revealInExplorer(path);
  } catch (e) {
    message.error((e as Error).message);
  }
}

onMounted(load);
</script>

<template>
  <div>
    <!-- 两个 tab -->
    <div class="v-tabs">
      <button class="v-tab" :class="{ on: tab === 'latest' }" @click="tab = 'latest'">最新</button>
      <button class="v-tab" :class="{ on: tab === 'category' }" @click="tab = 'category'">分类</button>
      <span class="dd-muted" style="margin-left: auto; font-size: 12px">
        共 {{ items.length }} 个视频
      </span>
      <n-button size="small" :loading="loading" @click="load">刷新</n-button>
    </div>

    <!-- 分类 -->
    <div v-if="tab === 'category'" class="v-cats">
      <button
        v-for="c in CATS"
        :key="c.key"
        class="v-cat"
        :class="{ on: cat === c.key }"
        :title="c.hint"
        @click="cat = c.key"
      >
        {{ c.label }}
        <span class="v-count">{{ counts[c.key] ?? 0 }}</span>
      </button>
    </div>

    <SectionCard
      :title="tab === 'latest' ? '最新' : CATS.find((c) => c.key === cat)?.label ?? '分类'"
      :desc="
        tab === 'latest'
          ? '所有视频按时间倒序，点卡片直接播'
          : CATS.find((c) => c.key === cat)?.hint ?? '按类型挑'
      "
    >
      <template #actions>
        <n-tag size="small" :bordered="false">{{ shown.length }} 个</n-tag>
      </template>
    </SectionCard>

    <div v-if="loading" class="dd-empty"><n-spin size="small" /> 正在读取…</div>
    <div v-else-if="shown.length === 0" class="dd-empty">
      这个分类下还没有视频 —— 先在「流水线」或「长剧素材」里导入并处理
    </div>

    <div v-else class="v-grid">
      <div v-for="v in shown" :key="v.key" class="v-card" @click="playing = v">
        <div class="v-thumb">
          <video :src="fileUrl(v.path)" preload="metadata" muted playsinline />
          <span class="v-kind">{{ KIND_LABEL[v.kind] }}</span>
          <span class="v-dur">{{ formatDuration(v.durationMs) }}</span>
        </div>
        <div class="v-title" :title="v.title">{{ v.title }}</div>
        <div class="v-meta dd-muted">
          {{ v.drama }}
          <template v-if="v.width && v.height"> · {{ v.width }}×{{ v.height }}</template>
        </div>
        <div class="v-actions">
          <n-button text size="tiny" @click.stop="reveal(v.path)">打开文件夹</n-button>
        </div>
      </div>
    </div>

    <!-- 播放 -->
    <n-modal
      :show="playing !== null"
      preset="card"
      style="width: 820px"
      :title="playing?.title ?? ''"
      @close="playing = null"
    >
      <video
        v-if="playing"
        :src="fileUrl(playing.path)"
        controls
        autoplay
        style="width: 100%; max-height: 64vh; background: #000"
      />
      <template #footer>
        <div class="dd-row" style="justify-content: space-between">
          <span class="dd-muted dd-mono">{{ playing?.path }}</span>
          <n-button size="small" @click="playing && reveal(playing.path)">打开文件夹</n-button>
        </div>
      </template>
    </n-modal>
  </div>
</template>

<style scoped>
/* tab：不要用组件库的，两行 CSS 就够，而且和整站的克制风格一致 */
.v-tabs {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 14px;
}
.v-tab {
  border: 1px solid rgba(128, 128, 128, 0.28);
  background: transparent;
  color: inherit;
  border-radius: 8px;
  padding: 5px 16px;
  font-size: 13px;
  cursor: pointer;
}
.v-tab.on {
  border-color: #18a058;
  color: #18a058;
  font-weight: 600;
}

.v-cats {
  display: flex;
  flex-wrap: wrap;
  gap: 8px;
  margin-bottom: 14px;
}
.v-cat {
  border: 1px solid rgba(128, 128, 128, 0.22);
  background: transparent;
  color: inherit;
  border-radius: 999px;
  padding: 4px 14px;
  font-size: 13px;
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.v-cat.on {
  border-color: #18a058;
  color: #18a058;
  font-weight: 600;
}
.v-count {
  font-size: 11px;
  opacity: 0.65;
}

/* 卡片网格：自适应列宽，窄窗口自动减列 */
.v-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(184px, 1fr));
  gap: 14px;
}
.v-card {
  border: 1px solid rgba(128, 128, 128, 0.22);
  border-radius: 8px;
  padding: 8px;
  cursor: pointer;
  transition: border-color 0.15s;
}
.v-card:hover {
  border-color: #18a058;
}
.v-thumb {
  position: relative;
  width: 100%;
  aspect-ratio: 9 / 16;
  background: #000;
  border-radius: 6px;
  overflow: hidden;
}
.v-thumb video {
  width: 100%;
  height: 100%;
  object-fit: contain;
  display: block;
}
.v-kind,
.v-dur {
  position: absolute;
  bottom: 6px;
  font-size: 11px;
  line-height: 1;
  padding: 3px 6px;
  border-radius: 4px;
  background: rgba(0, 0, 0, 0.62);
  color: #fff;
}
.v-kind {
  left: 6px;
}
.v-dur {
  right: 6px;
  font-variant-numeric: tabular-nums;
}
.v-title {
  margin-top: 8px;
  font-size: 13px;
  font-weight: 500;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.v-meta {
  font-size: 12px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.v-actions {
  margin-top: 2px;
}
</style>
