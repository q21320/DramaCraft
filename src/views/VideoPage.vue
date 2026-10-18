<script setup lang="ts">
/**
 * 视频：番茄在线短剧浏览。
 *
 * 一级分类只有两个：**最新上线**（分类接口，支持体裁/主题/设定等筛选）
 * 和 **新剧货架**（推荐流）。两个列表都支持触底自动加载。
 *
 * 封面由 Rust 侧把 HEIC 转成 JPEG 落盘缓存；列表只读、匿名。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";
import { useRouter } from "vue-router";
import { NAlert, NButton, NModal, NPopover, NSpin } from "naive-ui";
import { convertFileSrc } from "@tauri-apps/api/core";
import { api } from "@/api";
import { useDownloadStore } from "@/stores/download";
import type {
  FanqieCategoryPage,
  FanqieSelectItems,
  FanqieSelectorRow,
  OnlineDrama,
} from "@/types";

type OnlineTab = "fresh" | "shelf";

// ---------------- 在线短剧（番茄接口） ----------------

const PAGE_SIZE = 20;

const router = useRouter();

const onlineTab = ref<OnlineTab>("fresh");
const freshItems = ref<OnlineDrama[]>([]);
const shelfItems = ref<OnlineDrama[]>([]);
const freshLoading = ref(false);
const shelfLoading = ref(false);
const freshMoreLoading = ref(false);
const shelfMoreLoading = ref(false);
const freshHasMore = ref(false);
const shelfHasMore = ref(false);
const onlineError = ref("");
const detail = ref<OnlineDrama | null>(null);
/** 封面地址 → 可直接放进 img 的 src（HEIC 已转成本地 JPEG） */
const coverCache = ref<Record<string, string>>({});
const coverPending = new Set<string>();

/**
 * 翻页游标。接口偶尔会在下一页重复上一页的条目，单页去重后可能只新增几条，
 * 所以把多取到的条目先放进 buffer，下一轮先消费 buffer —— 每次自动续页
 * 都能稳定补满一页。
 */
interface Pager {
  offset: number;
  sessionId: string;
  hasMore: boolean;
  buffer: OnlineDrama[];
  seen: Set<string>;
}

function newPager(): Pager {
  // hasMore 先给 true，否则第一页还没请求就被当成「到底了」
  return { offset: 0, sessionId: "", hasMore: true, buffer: [], seen: new Set() };
}

function dramaKey(v: OnlineDrama): string {
  return v.vid || `${v.seriesId}|${v.title}`;
}

type PageFetcher = (offset: number, sessionId: string) => Promise<FanqieCategoryPage>;

let freshPager = newPager();
let shelfPager = newPager();
/** 分类筛选切换很快时，旧请求可能晚回来；用代际号丢弃过期结果。 */
let freshGeneration = 0;

const currentOnline = computed(() =>
  onlineTab.value === "fresh" ? freshItems.value : shelfItems.value,
);
const currentOnlineLoading = computed(() =>
  onlineTab.value === "fresh" ? freshLoading.value : shelfLoading.value,
);
const currentHasMore = computed(() =>
  onlineTab.value === "fresh" ? freshHasMore.value : shelfHasMore.value,
);
const currentMoreLoading = computed(() =>
  onlineTab.value === "fresh" ? freshMoreLoading.value : shelfMoreLoading.value,
);

// ---------------- 二级分类（分类接口的 select_items） ----------------

const selectorRows = ref<FanqieSelectorRow[]>([]);
const selectorLoading = ref(false);
const filterError = ref("");
const filterValues = reactive<Record<string, string | null>>({
  genre: null,
  sort: "online_time",
  category_dim_theme: null,
  category_dim_role: null,
  category_dim_epoch: null,
  gender: null,
  online_time: null,
  creation_status: null,
});

const FILTER_LABELS: Record<string, string> = {
  genre: "体裁",
  category_dim_theme: "主题",
  category_dim_role: "设定",
  category_dim_epoch: "背景",
  sort: "排序",
  gender: "受众",
  online_time: "上新时间",
  creation_status: "状态",
};

/** 把界面上的单选值拼成分类接口的 select_items；空数组表示「全部」。 */
function buildSelectItems(): FanqieSelectItems {
  const pick = (key: string) => {
    const value = filterValues[key];
    return value ? [value] : [];
  };
  const items: FanqieSelectItems = {
    genre: pick("genre"),
    sort: pick("sort"),
    category_dim_theme: pick("category_dim_theme"),
    category_dim_role: pick("category_dim_role"),
    category_dim_epoch: pick("category_dim_epoch"),
    gender: pick("gender"),
    online_time: pick("online_time"),
    creation_status: pick("creation_status"),
  };
  if (items.sort.length === 0) items.sort = ["online_time"];
  return items;
}

/** 拉筛选面板；面板 ID 随体裁变化，切体裁后必须重拉。 */
async function loadSelectorRows() {
  selectorLoading.value = true;
  filterError.value = "";
  try {
    const rows = await api.fanqieSelectorRows(filterValues.genre ?? "");
    selectorRows.value = rows;
    const valid = new Set(rows.map((row) => row.key));
    // 新面板里没有的键、或已失效的选项，清掉，避免带着旧 ID 去请求
    for (const key of Object.keys(filterValues)) {
      if (!valid.has(key)) {
        filterValues[key] = key === "sort" ? "online_time" : null;
        continue;
      }
      const row = rows.find((r) => r.key === key);
      const value = filterValues[key];
      if (value && row && !row.items.some((item) => item.id === value)) {
        filterValues[key] = key === "sort" ? "online_time" : null;
      }
    }
    if (!valid.has("sort")) filterValues.sort = "online_time";
  } catch (e) {
    filterError.value = `筛选面板拉取失败：${(e as Error).message}`;
  } finally {
    selectorLoading.value = false;
  }
}

const openFilter = ref<string | null>(null);

/** 胶囊上显示当前选中的名字；没选就是「全部」。 */
function filterValueName(row: FanqieSelectorRow): string {
  const value = filterValues[row.key];
  if (!value) return "全部";
  return row.items.find((item) => item.id === value)?.name ?? "全部";
}

function setFilterOpen(key: string, show: boolean) {
  openFilter.value = show ? key : null;
}

/** 换分类 / 换筛选后滚回顶部，否则第一排会缩在吸顶条下面。 */
function scrollListTop(smooth = true) {
  document
    .querySelector(".dd-content")
    ?.scrollTo({ top: 0, behavior: smooth ? "smooth" : "auto" });
}

/** 选定某个二级分类：关弹层、清掉依赖旧面板的选项，再按新条件拉第一页。 */
async function pickFilter(key: string, value: string | null) {
  openFilter.value = null;
  if (filterValues[key] === value) return;
  filterValues[key] = value;
  scrollListTop();
  if (key === "genre") {
    for (const k of [
      "category_dim_theme",
      "category_dim_role",
      "category_dim_epoch",
      "creation_status",
    ]) {
      filterValues[k] = null;
    }
    await loadSelectorRows();
  }
  if (onlineTab.value === "fresh") await loadFresh();
}

function switchOnline(next: OnlineTab) {
  onlineTab.value = next;
  onlineError.value = "";
  scrollListTop();
  if (next === "fresh" && freshItems.value.length === 0 && !freshLoading.value) void loadFresh();
  if (next === "fresh" && selectorRows.value.length === 0 && !selectorLoading.value) {
    void loadSelectorRows();
  }
  if (next === "shelf" && shelfItems.value.length === 0 && !shelfLoading.value) void loadShelf();
}

async function refreshOnline() {
  onlineError.value = "";
  if (onlineTab.value === "fresh") {
    await loadSelectorRows();
    await loadFresh();
  } else {
    await loadShelf();
  }
}

/**
 * 取够 need 条没显示过的剧：先消费 buffer，再按游标继续请求。
 * 取到的条目直接进 out，调用方即使中途遇到网络错误也能保住已拿到的部分。
 */
async function fillPage(
  pager: Pager,
  need: number,
  fetchPage: PageFetcher,
  out: OnlineDrama[],
): Promise<void> {
  while (out.length < need && pager.buffer.length > 0) {
    out.push(pager.buffer.shift()!);
  }
  let guard = 0;
  // 货架是推荐流：服务端一直说 has_more，但后面可能连续几页都是旧内容。
  // 连续 3 页没有新条目就当作本轮到底，避免滑到底后列表一动不动。
  let emptyStreak = 0;
  // 接口如果一直返回重复内容，最多连拉 6 页，避免死循环
  while (out.length < need && pager.hasMore && guard < 6) {
    const page = await fetchPage(pager.offset, pager.sessionId);
    pager.offset = page.nextOffset;
    pager.sessionId = page.sessionId;
    pager.hasMore = page.hasMore;
    guard++;
    const before = pager.seen.size;
    for (const v of page.items) {
      const key = dramaKey(v);
      if (pager.seen.has(key)) continue;
      pager.seen.add(key);
      pager.buffer.push(v);
    }
    if (pager.seen.size === before) {
      emptyStreak++;
      if (emptyStreak >= 3) {
        pager.hasMore = false;
        break;
      }
    } else {
      emptyStreak = 0;
    }
    while (out.length < need && pager.buffer.length > 0) {
      out.push(pager.buffer.shift()!);
    }
  }
}

/** 拉「最新上线」；reset=false 表示接着当前游标翻页。 */
async function loadFresh(reset = true) {
  const generation = ++freshGeneration;
  if (reset) freshLoading.value = true;
  else freshMoreLoading.value = true;
  const previous = freshPager;
  const pager = reset ? newPager() : freshPager;
  if (reset) freshPager = pager;
  const selectItems = buildSelectItems();
  const taken: OnlineDrama[] = [];
  let ok = false;
  try {
    await fillPage(
      pager,
      PAGE_SIZE,
      (offset, sessionId) => api.fanqieCategory(offset, PAGE_SIZE, sessionId, selectItems),
      taken,
    );
    if (generation !== freshGeneration) return;
    freshItems.value = reset ? taken : [...freshItems.value, ...taken];
    freshHasMore.value = pager.hasMore || pager.buffer.length > 0;
    if (taken.length > 0) void ensureCovers(taken);
    ok = true;
  } catch (e) {
    if (generation !== freshGeneration) return;
    // 第一页就失败：保留原列表和原游标，用户可以直接重试
    if (reset && taken.length === 0) freshPager = previous;
    if (taken.length > 0) {
      freshItems.value = reset ? taken : [...freshItems.value, ...taken];
      freshHasMore.value = pager.hasMore || pager.buffer.length > 0;
      void ensureCovers(taken);
    }
    onlineError.value = `拉取「最新上线」失败：${(e as Error).message}`;
  } finally {
    if (generation === freshGeneration) {
      freshLoading.value = false;
      freshMoreLoading.value = false;
      // 只在成功后续挂：失败时不自动重试，避免网络挂了以后无限打接口
      if (ok) void nextTick(observeSentinel);
    }
  }
}

/** 拉「新剧货架」；reset=false 表示接着当前游标翻页。 */
async function loadShelf(reset = true) {
  if (reset) shelfLoading.value = true;
  else shelfMoreLoading.value = true;
  const previous = shelfPager;
  const pager = reset ? newPager() : shelfPager;
  if (reset) shelfPager = pager;
  const taken: OnlineDrama[] = [];
  let ok = false;
  try {
    await fillPage(
      pager,
      PAGE_SIZE,
      (offset, sessionId) => api.fanqieNewShelf(offset, sessionId),
      taken,
    );
    shelfItems.value = reset ? taken : [...shelfItems.value, ...taken];
    shelfHasMore.value = pager.hasMore || pager.buffer.length > 0;
    if (taken.length > 0) void ensureCovers(taken);
    ok = true;
  } catch (e) {
    if (reset && taken.length === 0) shelfPager = previous;
    if (taken.length > 0) {
      shelfItems.value = reset ? taken : [...shelfItems.value, ...taken];
      shelfHasMore.value = pager.hasMore || pager.buffer.length > 0;
      void ensureCovers(taken);
    }
    onlineError.value = `拉取「新剧货架」失败：${(e as Error).message}`;
  } finally {
    shelfLoading.value = false;
    shelfMoreLoading.value = false;
    if (ok) void nextTick(observeSentinel);
  }
}

function loadMore() {
  if (onlineTab.value === "fresh") void loadFresh(false);
  else void loadShelf(false);
}

// ---------------- 无限滚动：滑到底自动加载，不再点按钮 ----------------

const loadMoreSentinel = ref<HTMLElement | null>(null);
let moreObserver: IntersectionObserver | null = null;

function maybeLoadMore() {
  if (currentOnlineLoading.value || currentMoreLoading.value) return;
  if (!currentHasMore.value) return;
  loadMore();
}

/** 观察列表底部的哨兵；切一级分类或每次加载完后都要重新挂一次。 */
function observeSentinel() {
  const el = loadMoreSentinel.value;
  moreObserver?.disconnect();
  moreObserver = null;
  if (!el) return;
  // 实际滚动的是 MainLayout 的 .dd-content，用它做 root 才能正确触发
  const root = (el.closest(".dd-content") as HTMLElement | null) ?? null;
  moreObserver = new IntersectionObserver(
    (entries) => {
      if (entries.some((e) => e.isIntersecting)) maybeLoadMore();
    },
    { root, rootMargin: "240px 0px" },
  );
  moreObserver.observe(el);
}

watch(loadMoreSentinel, observeSentinel, { flush: "post" });
watch(onlineTab, () => void nextTick(observeSentinel), { flush: "post" });
onBeforeUnmount(() => moreObserver?.disconnect());

/**
 * 封面按需转换成能显示的格式。
 *
 * 分类接口给的是 HEIC，WebView 不认；转换在 Rust 侧做并落盘缓存，
 * 同一张图第二次进来直接命中。这里最多同时转 4 张，避免一次拉起
 * 十几个 ffmpeg 进程。
 */
async function ensureCovers(list: OnlineDrama[]) {
  const queue = list.filter(
    (v) => v.cover && !coverCache.value[v.cover] && !coverPending.has(v.cover),
  );
  const worker = async () => {
    for (let item = queue.shift(); item; item = queue.shift()) {
      const raw = item.cover;
      coverPending.add(raw);
      try {
        if (!/\.heic(\?|$)/i.test(raw)) {
          coverCache.value[raw] = raw;
          continue;
        }
        const path = await api.fanqieCover(raw);
        coverCache.value[raw] = path.startsWith("http") ? path : convertFileSrc(path);
      } catch {
        // 单张封面失败不影响列表，卡片显示占位块
      } finally {
        coverPending.delete(raw);
      }
    }
  };
  await Promise.all(Array.from({ length: Math.min(4, queue.length) }, worker));
}

function coverSrc(item: OnlineDrama): string {
  return coverCache.value[item.cover] ?? "";
}

function formatCount(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "";
  if (n >= 100_000_000) return `${(n / 100_000_000).toFixed(1)} 亿`;
  if (n >= 10_000) return `${(n / 10_000).toFixed(1)} 万`;
  return String(n);
}

// ---------------- 已下载标记 ----------------

/**
 * 剧名 → 下载目录里已有的集数。
 *
 * 列表是分批加载的，只查还没查过的标题 —— 翻页不会把整屏重扫一遍。
 * 标上之后用户一眼能看出哪些不用再点，不用等勾上才发现全是「已存在」。
 */
const downloaded = ref<Record<string, number>>({});
const checkingDownloaded = new Set<string>();

/** 列表项的匹配键：有 seriesId 用 ID，没有才退回标题。 */
function downloadKey(v: { seriesId?: string; title: string }): string {
  return v.seriesId || v.title;
}

async function refreshDownloaded(force = false) {
  const entries = [
    ...new Map(
      [...freshItems.value, ...shelfItems.value]
        .filter((v) => v.title)
        .map(
          (v) =>
            [
              downloadKey(v),
              { seriesId: v.seriesId || "", title: v.title },
            ] as const,
        ),
    ).values(),
  ].filter((e) => {
    const key = e.seriesId || e.title;
    return (force || !(key in downloaded.value)) && !checkingDownloaded.has(key);
  });
  if (entries.length === 0) return;
  const keys = entries.map((e) => e.seriesId || e.title);
  keys.forEach((k) => checkingDownloaded.add(k));
  try {
    const rows = await api.fanqieDownloaded(entries);
    const next = { ...downloaded.value };
    // 没返回的也记成 0：否则每次翻页都会重新查一遍同样没下过的标题
    keys.forEach((k) => {
      next[k] = 0;
    });
    rows.forEach((r) => {
      next[r.seriesId || r.title] = r.episodes;
    });
    downloaded.value = next;
  } catch {
    // 查不到就不显示标记，不影响列表
  } finally {
    keys.forEach((k) => checkingDownloaded.delete(k));
  }
}

watch([freshItems, shelfItems], () => void refreshDownloaded(), { flush: "post" });

// ---------------- 多选下载：落进用户指定的下载目录，然后直接开流水线 ----------------

/** 多选模式：卡片点击变成勾选，不再进详情。 */
const selecting = ref(false);
/** 选中的 seriesId；切 tab / 翻页都不丢。 */
const selectedSeries = ref<Set<string>>(new Set());
const selectedCount = computed(() => selectedSeries.value.size);

/**
 * 下载状态放在全局 store：切页面再回来，进度和下载流程都还在。
 * 以前这些是组件内的 ref，页面一卸载就全没了。
 */
const dl = useDownloadStore();
const downloading = computed(() => dl.running);
const downloadError = computed(() => dl.error);
const dlPercent = computed(() => dl.percent);

function isSelected(v: OnlineDrama): boolean {
  return !!v.seriesId && selectedSeries.value.has(v.seriesId);
}

function toggleSelect(v: OnlineDrama) {
  if (!v.seriesId) return;
  const next = new Set(selectedSeries.value);
  if (next.has(v.seriesId)) next.delete(v.seriesId);
  else next.add(v.seriesId);
  selectedSeries.value = next;
}

function toggleSelecting() {
  selecting.value = !selecting.value;
  if (!selecting.value) selectedSeries.value = new Set();
}

function clearSelection() {
  selectedSeries.value = new Set();
}

/** 两个 tab 可能都有同一部剧，按 seriesId 去重。 */
function selectedDramas(): OnlineDrama[] {
  const seen = new Set<string>();
  const out: OnlineDrama[] = [];
  for (const v of [...freshItems.value, ...shelfItems.value]) {
    if (!v.seriesId || !selectedSeries.value.has(v.seriesId)) continue;
    if (seen.has(v.seriesId)) continue;
    seen.add(v.seriesId);
    out.push(v);
  }
  return out;
}

/**
 * 下载选中的剧：逐部拉分集、逐集解密，落到 `<下载目录>/<剧名>/`。
 *
 * 流程本体在 `stores/download.ts` —— 这里只负责把勾选的剧交过去。
 * 这样用户切页面（组件卸载）下载也不会断，回来进度还在。
 */
async function downloadSelected() {
  const list = selectedDramas().map((v) => ({
    seriesId: v.seriesId!,
    title: v.title,
    episodeCount: v.episodeCount,
  }));
  await dl.run(list, (root, titles) => {
    // 把刚下完的剧名一并带去：下载根里往往还躺着以前下的剧，
    // 不带的话流水线会把整个库重新排一遍。
    void router.push({
      name: "pipeline",
      query: { root, autostart: "1", only: JSON.stringify(titles) },
    });
  });
  // 刚落盘的剧立刻标上「已下载」（若已跳走，回来重新挂载时会重新查）
  void refreshDownloaded(true);
}

/** 点卡片进详情页；多选模式下改成勾选。没有 seriesId 的条目退回首屏弹窗。 */
function openDrama(v: OnlineDrama) {
  if (selecting.value) {
    toggleSelect(v);
    return;
  }
  if (!v.seriesId) {
    detail.value = v;
    return;
  }
  const params: Record<string, string> = { seriesId: v.seriesId };
  if (v.vid) params.vid = v.vid;
  void router.push({ name: "videoDetail", params });
}

/** 接口的 duration 是秒，不是毫秒。 */
function formatSeconds(sec: number): string {
  if (!Number.isFinite(sec) || sec <= 0) return "—";
  const h = Math.floor(sec / 3600);
  const m = Math.floor((sec % 3600) / 60);
  return h > 0 ? `${h} 小时 ${m} 分` : `${m} 分`;
}

onMounted(() => {
  // 从别的页面回来时内容区可能还停在上次的滚动位置
  scrollListTop(false);
  // 面板和第一页并行拉；面板失败不影响默认列表
  void loadSelectorRows();
  void loadFresh();
  // 下载进度由 download store 自己监听 —— 页面卸载不会把监听带走
});
</script>

<template>
  <div>
    <!-- 一级分类：最新上线 / 新剧货架；二级筛选跟分类接口走 -->
    <div class="v-sticky">
      <div class="v-tabs">
        <button
          class="v-tab"
          :class="{ on: onlineTab === 'fresh' }"
          @click="switchOnline('fresh')"
        >
          最新上线
        </button>
        <button
          class="v-tab"
          :class="{ on: onlineTab === 'shelf' }"
          @click="switchOnline('shelf')"
        >
          新剧货架
        </button>
        <span
          v-if="!selecting"
          class="dd-muted"
          style="margin-left: auto; font-size: 12px"
        >
          {{
            onlineTab === "fresh"
              ? "番茄短剧 · 可按体裁 / 主题 / 设定等筛选"
              : "番茄短剧 · 新剧推荐流，翻到后面新内容会变少"
          }}
        </span>
        <span v-else style="margin-left: auto"></span>
        <template v-if="selecting">
          <span class="dd-muted" style="font-size: 12px">已选 {{ selectedCount }} 部</span>
          <n-button size="small" quaternary @click="clearSelection">清空</n-button>
          <n-button
            size="small"
            type="primary"
            :loading="downloading"
            :disabled="selectedCount === 0"
            @click="downloadSelected"
          >
            下载 → 跑流水线
          </n-button>
        </template>
        <span class="dd-muted" style="font-size: 12px">已加载 {{ currentOnline.length }} 条</span>
        <n-button size="small" :type="selecting ? 'primary' : 'default'" @click="toggleSelecting">
          {{ selecting ? "退出多选" : "多选下载" }}
        </n-button>
        <n-button size="small" :loading="currentOnlineLoading" @click="refreshOnline">刷新</n-button>
      </div>

      <!-- 二级分类：只有「最新上线」接了分类接口的 select_items -->
      <div v-if="onlineTab === 'fresh'" class="o-filters">
        <n-popover
          v-for="row in selectorRows"
          :key="row.key"
          trigger="click"
          placement="bottom-start"
          :show-arrow="false"
          :show="openFilter === row.key"
          @update:show="setFilterOpen(row.key, $event)"
        >
          <template #trigger>
            <button class="o-chip" :class="{ on: !!filterValues[row.key] }">
              <span class="o-chip-label">{{ FILTER_LABELS[row.key] ?? row.label }}</span>
              <span>{{ filterValueName(row) }}</span>
              <span class="o-chip-caret">▾</span>
            </button>
          </template>
          <div class="o-menu">
            <button
              v-if="row.key !== 'sort'"
              class="o-menu-item"
              :class="{ sel: !filterValues[row.key] }"
              @click="pickFilter(row.key, null)"
            >
              全部
            </button>
            <button
              v-for="item in row.items"
              :key="item.id"
              class="o-menu-item"
              :class="{ sel: filterValues[row.key] === item.id }"
              @click="pickFilter(row.key, item.id)"
            >
              {{ item.name }}
            </button>
          </div>
        </n-popover>
        <span v-if="filterError" class="o-filter-error dd-muted">{{ filterError }}</span>
        <span v-else-if="selectorLoading && selectorRows.length === 0" class="o-filter-error dd-muted">
          正在加载筛选…
        </span>
      </div>
    </div>

    <!-- 在线短剧：番茄公开接口 -->
    <n-alert
      v-if="onlineError"
      type="error"
      style="margin-bottom: 12px"
      title="在线列表拉取失败"
    >
      {{ onlineError }}
    </n-alert>

    <div v-if="currentOnlineLoading && currentOnline.length === 0" class="dd-empty">
      <n-spin size="small" /> 正在拉取…
    </div>
    <div v-else-if="currentOnline.length === 0" class="dd-empty">
      没有拿到数据 —— 检查网络后点右上角「刷新」
    </div>

    <template v-else>
      <div class="o-grid">
        <div
          v-for="v in currentOnline"
          :key="v.vid || v.seriesId || v.title"
          class="o-card"
          :class="{ selecting, sel: isSelected(v) }"
          @click="openDrama(v)"
        >
          <div class="o-cover">
            <img v-if="coverSrc(v)" :src="coverSrc(v)" loading="lazy" alt="" />
            <div v-else class="o-cover-fallback">{{ (v.title || "剧").slice(0, 1) }}</div>
            <span
              v-if="selecting"
              class="o-check"
              :class="{ on: isSelected(v), off: !v.seriesId }"
              :title="v.seriesId ? (isSelected(v) ? '取消选中' : '选中下载') : '这条没有 seriesId，不能下载'"
            >
              <span v-if="isSelected(v)">✓</span>
            </span>
            <span v-if="v.isNew" class="o-badge">新剧</span>
            <span v-if="v.episodeCount > 0" class="o-ep">{{ v.episodeCount }} 集</span>
            <span
              v-if="downloaded[downloadKey(v)] > 0"
              class="o-dl-badge"
              :title="`下载目录里已有 ${downloaded[downloadKey(v)]} 集`"
            >
              {{
                v.episodeCount > 0 && downloaded[downloadKey(v)] >= v.episodeCount
                  ? "已下载"
                  : `已下 ${downloaded[downloadKey(v)]} 集`
              }}
            </span>
          </div>
          <div class="o-title" :title="v.title">{{ v.title }}</div>
          <div class="o-tags">
            <span v-for="t in v.tags.slice(0, 3)" :key="t" class="o-tag">{{ t }}</span>
          </div>
          <div class="o-meta dd-muted">
            <span v-if="v.heat">热度 {{ v.heat }}</span>
            <span v-else-if="v.playCount > 0">播放 {{ formatCount(v.playCount) }}</span>
            <span v-if="v.score">评分 {{ v.score }}</span>
          </div>
          <div class="o-desc">{{ v.desc || "接口没给简介" }}</div>
        </div>
      </div>

      <div ref="loadMoreSentinel" class="o-more">
        <span v-if="currentMoreLoading" class="o-more-hint dd-muted">
          <n-spin size="small" /> 正在加载…
        </span>
        <span v-else-if="currentHasMore" class="dd-muted">继续下滑，自动加载…</span>
        <span v-else class="dd-muted">已经到底了</span>
      </div>
    </template>

    <!-- 在线短剧详情 -->
    <n-modal
      :show="detail !== null"
      preset="card"
      style="width: 760px"
      :title="detail?.title ?? ''"
      @close="detail = null"
    >
      <div v-if="detail" class="o-detail">
        <div class="o-detail-cover">
          <img v-if="coverSrc(detail)" :src="coverSrc(detail)" alt="" />
          <div v-else class="o-cover-fallback">{{ (detail.title || "剧").slice(0, 1) }}</div>
        </div>
        <div class="o-detail-body">
          <div class="o-tags" style="margin-bottom: 8px">
            <span v-for="t in detail.tags" :key="t" class="o-tag">{{ t }}</span>
          </div>
          <p class="o-detail-desc">{{ detail.desc || "接口没给简介" }}</p>
          <table class="dd-table">
            <tbody>
              <tr v-if="detail.heat">
                <th>热度</th>
                <td>{{ detail.heat }}</td>
              </tr>
              <tr v-if="detail.score">
                <th>评分</th>
                <td>{{ detail.score }}</td>
              </tr>
              <tr v-if="detail.playCount > 0">
                <th>播放</th>
                <td>{{ formatCount(detail.playCount) }}</td>
              </tr>
              <tr v-if="detail.episodeCount > 0">
                <th>集数</th>
                <td>{{ detail.episodeCount }} 集</td>
              </tr>
              <tr v-if="downloaded[downloadKey(detail)] > 0">
                <th>已下载</th>
                <td>{{ downloaded[downloadKey(detail)] }} 集</td>
              </tr>
              <tr v-if="detail.durationSec > 0">
                <th>时长</th>
                <td>{{ formatSeconds(detail.durationSec) }}</td>
              </tr>
              <tr v-if="detail.copyright">
                <th>版权</th>
                <td class="dd-wrap">{{ detail.copyright }}</td>
              </tr>
              <tr>
                <th>视频 ID</th>
                <td class="dd-mono">{{ detail.vid || "—" }}</td>
              </tr>
              <tr>
                <th>系列 ID</th>
                <td class="dd-mono">{{ detail.seriesId || "—" }}</td>
              </tr>
            </tbody>
          </table>
        </div>
      </div>
    </n-modal>

    <!-- 批量下载进度：下载完自动跳流水线并开跑 -->
    <div v-if="dl.active" class="o-dl">
      <div class="o-dl-head">
        <b>下载到默认下载目录</b>
        <span class="dd-muted">
          {{ Math.min(dl.seriesIndex + 1, dl.seriesTotal) }} / {{ dl.seriesTotal }} 部
        </span>
        <button v-if="!dl.running" class="o-dl-close" title="收起" @click="dl.dismiss()">
          ×
        </button>
      </div>
      <div class="o-dl-bar"><i :style="{ width: dlPercent + '%' }" /></div>
      <div class="o-dl-line">
        《{{ dl.title || "…" }}》
        <span v-if="dl.episodes > 0" class="dd-muted">
          第 {{ dl.episode }} / {{ dl.episodes }} 集
        </span>
      </div>
      <div class="o-dl-line dd-muted dd-wrap">{{ dl.message }}</div>
      <div class="o-dl-line dd-muted">
        已完成 {{ dl.done }} 集
        <template v-if="dl.failed > 0"> · 失败 {{ dl.failed }}</template>
      </div>
      <div v-if="dl.root" class="o-dl-line dd-muted dd-wrap" style="font-size: 11px">
        保存到：{{ dl.root }}（每部剧自己的「二创」在剧目录里）
      </div>
      <div v-if="downloadError" class="o-dl-error dd-wrap">{{ downloadError }}</div>
      <div class="dd-muted" style="font-size: 11px; margin-top: 4px">
        {{
          dl.running
            ? "下载在后台继续，切到别的页面不会中断"
            : dl.failed > 0
              ? "有失败集时不会自动开跑；再点一次「下载」只补缺的集"
              : "下载完会自动跳到流水线并开始"
        }}
      </div>
    </div>
  </div>
</template>

<style scoped>
/* tab：不要用组件库的，两行 CSS 就够，而且和整站的克制风格一致 */
/* 顶部筛选条吸顶：滚动列表时 tab / 分类始终可见 */
.v-sticky {
  position: sticky;
  top: 0;
  z-index: 20;
  margin: -18px -20px 0;
  padding: 18px 20px 14px;
  background: var(--n-color, #fff);
  border-bottom: 1px solid rgba(128, 128, 128, 0.12);
  box-shadow: 0 6px 10px -10px rgba(0, 0, 0, 0.35);
}
.v-tabs {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 10px;
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

/* 二级分类：胶囊按钮 + 弹层菜单，比一排下拉框紧凑，也不会撑出两行 */
.o-filters {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
}
.o-chip {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  border: 1px solid rgba(128, 128, 128, 0.28);
  background: transparent;
  color: inherit;
  border-radius: 999px;
  padding: 4px 12px;
  font-size: 12px;
  line-height: 1.5;
  cursor: pointer;
  transition: border-color 0.15s, color 0.15s, background 0.15s;
}
.o-chip:hover {
  border-color: #18a058;
}
.o-chip.on {
  border-color: #18a058;
  color: #18a058;
  background: rgba(24, 160, 88, 0.08);
  font-weight: 600;
}
.o-chip-label {
  opacity: 0.62;
}
.o-chip.on .o-chip-label {
  opacity: 0.85;
}
.o-chip-caret {
  font-size: 10px;
  opacity: 0.6;
}
.o-menu {
  min-width: 136px;
  max-height: 320px;
  overflow-y: auto;
  padding: 4px;
}
.o-menu-item {
  display: block;
  width: 100%;
  border: 0;
  background: transparent;
  color: inherit;
  text-align: left;
  padding: 6px 10px;
  border-radius: 6px;
  font-size: 13px;
  line-height: 1.6;
  white-space: nowrap;
  cursor: pointer;
}
.o-menu-item:hover {
  background: rgba(128, 128, 128, 0.12);
}
.o-menu-item.sel {
  color: #18a058;
  font-weight: 600;
  background: rgba(24, 160, 88, 0.08);
}
.o-filter-error {
  font-size: 12px;
}

/* ---------- 在线短剧 ---------- */
.o-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(184px, 1fr));
  gap: 14px;
  margin-top: 30px;
}
.o-card {
  border: 1px solid rgba(128, 128, 128, 0.22);
  border-radius: 8px;
  padding: 8px;
  cursor: pointer;
  transition: border-color 0.15s;
}
.o-card:hover {
  border-color: #18a058;
}
.o-card.sel {
  border-color: #18a058;
  box-shadow: 0 0 0 1px #18a058 inset;
}
.o-cover {
  position: relative;
  width: 100%;
  aspect-ratio: 3 / 4;
  background: rgba(128, 128, 128, 0.12);
  border-radius: 6px;
  overflow: hidden;
}
.o-cover img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}
.o-cover-fallback {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 40px;
  font-weight: 600;
  color: rgba(128, 128, 128, 0.55);
  background: linear-gradient(135deg, rgba(128, 128, 128, 0.1), rgba(128, 128, 128, 0.22));
}
.o-badge,
.o-ep {
  position: absolute;
  top: 6px;
  font-size: 11px;
  line-height: 1;
  padding: 3px 6px;
  border-radius: 4px;
  color: #fff;
}
.o-badge {
  left: 6px;
  background: #18a058;
}
.o-ep {
  right: 6px;
  background: rgba(0, 0, 0, 0.62);
}
/* 已下载：封面左下角，和「新剧」「集数」错开 */
.o-dl-badge {
  position: absolute;
  left: 6px;
  bottom: 6px;
  font-size: 11px;
  line-height: 1;
  padding: 3px 6px;
  border-radius: 4px;
  color: #fff;
  background: rgba(24, 160, 88, 0.9);
}
/* 多选模式的勾选圈；有「新剧」徽标时把它往下让一格 */
.o-check {
  position: absolute;
  top: 6px;
  left: 6px;
  z-index: 2;
  width: 22px;
  height: 22px;
  box-sizing: border-box;
  border: 2px solid rgba(255, 255, 255, 0.9);
  border-radius: 50%;
  background: rgba(0, 0, 0, 0.35);
  color: #fff;
  font-size: 13px;
  line-height: 18px;
  text-align: center;
}
.o-check.on {
  background: #18a058;
  border-color: #18a058;
}
.o-check.off {
  opacity: 0.35;
}
.o-card.selecting .o-badge {
  top: 34px;
}
.o-title {
  margin-top: 8px;
  font-size: 13px;
  font-weight: 600;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.o-tags {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
  margin-top: 2px;
  min-height: 16px;
}
.o-tag {
  font-size: 11px;
  line-height: 1;
  padding: 3px 6px;
  border-radius: 999px;
  background: rgba(128, 128, 128, 0.14);
}
.o-meta {
  display: flex;
  gap: 8px;
  font-size: 12px;
  margin-top: 2px;
}
.o-desc {
  margin-top: 4px;
  font-size: 12px;
  color: var(--dd-muted);
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}
.o-more {
  text-align: center;
  margin-top: 16px;
}
.o-more-hint {
  display: inline-flex;
  align-items: center;
  gap: 8px;
}
.o-detail {
  display: flex;
  gap: 18px;
  align-items: flex-start;
}
.o-detail-cover {
  width: 220px;
  flex: none;
  aspect-ratio: 3 / 4;
  border-radius: 8px;
  overflow: hidden;
  background: rgba(128, 128, 128, 0.12);
}
.o-detail-cover img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}
.o-detail-body {
  flex: 1;
  min-width: 0;
}
.o-detail-desc {
  font-size: 13px;
  line-height: 1.75;
  margin: 0 0 12px;
  white-space: pre-wrap;
}
.o-detail table th {
  width: 84px;
}

/* 批量下载进度浮层：固定在右下角，不挡列表 */
.o-dl {
  position: fixed;
  right: 24px;
  bottom: 24px;
  z-index: 60;
  width: 320px;
  box-sizing: border-box;
  padding: 12px 14px;
  border-radius: 10px;
  border: 1px solid rgba(128, 128, 128, 0.24);
  background: var(--n-color, #fff);
  box-shadow: 0 10px 28px rgba(0, 0, 0, 0.18);
  font-size: 12px;
}
.o-dl-head {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 8px;
  font-size: 13px;
}
.o-dl-head b {
  flex: none;
}
.o-dl-head .dd-muted {
  margin-left: auto;
}
.o-dl-close {
  flex: none;
  border: 0;
  background: transparent;
  color: inherit;
  font-size: 16px;
  line-height: 1;
  cursor: pointer;
  opacity: 0.55;
}
.o-dl-close:hover {
  opacity: 1;
}
.o-dl-bar {
  height: 6px;
  margin-bottom: 8px;
  border-radius: 999px;
  background: rgba(128, 128, 128, 0.18);
  overflow: hidden;
}
.o-dl-bar i {
  display: block;
  height: 100%;
  background: #18a058;
  transition: width 0.3s ease;
}
.o-dl-line {
  line-height: 1.7;
}
.o-dl-error {
  margin-top: 4px;
  color: #d03050;
  line-height: 1.6;
}
</style>
