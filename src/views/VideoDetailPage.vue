<script setup lang="ts">
/**
 * 视频详情：一部短剧的分集列表 + 可播放的播放器。
 *
 * 播放链路：
 * 1. App 详情接口 `/novel/player/multi_video_detail/preload/v1` 拿整部剧的分集；
 * 2. 主链路 `fanqie_prepare_video`：下载 App 的 CENC-AES-CTR 加密流，
 *    用 `spade_a` 派生的 key 交给 ffmpeg 解密并转成 H.264，缓存在本地；
 *    前端用 convertFileSrc 指向这份文件，全部集数都能看。
 * 3. 官网明文 MP4 只作为兜底 —— 它只公开前 50 集，而且部分文件本身就 404。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { NAlert, NButton, NSpin } from "naive-ui";
import { convertFileSrc } from "@tauri-apps/api/core";
import { api } from "@/api";
import type { FanqieEpisode, FanqiePlayInfo, FanqieSeriesDetail, FanqieVideoInfo } from "@/types";

const route = useRoute();
const router = useRouter();

const seriesId = String(route.params.seriesId ?? "");
const initialVid = String(route.params.vid ?? "");

const detail = ref<FanqieSeriesDetail | null>(null);
const cover = ref("");
const loading = ref(false);
const error = ref("");

const currentVid = ref("");
const playInfo = ref<FanqiePlayInfo | null>(null);
const videoInfo = ref<FanqieVideoInfo | null>(null);
const playLoading = ref(false);
const playError = ref("");
const autoNext = ref(true);

const videoEl = ref<HTMLVideoElement | null>(null);
/** 快速切集时旧请求可能晚回来，用代际号丢弃过期结果。 */
let playGeneration = 0;

const episodes = computed<FanqieEpisode[]>(() => detail.value?.episodes ?? []);
const currentIndex = computed(() => episodes.value.findIndex((e) => e.vid === currentVid.value));
const currentEpisode = computed(() => episodes.value[currentIndex.value] ?? null);
const hasPrev = computed(() => currentIndex.value > 0);
const hasNext = computed(
  () => currentIndex.value >= 0 && currentIndex.value < episodes.value.length - 1,
);
const sortedQualities = computed(() =>
  [...(videoInfo.value?.qualities ?? [])].sort((a, b) => parseInt(b) - parseInt(a)),
);

function formatClock(sec: number): string {
  if (!Number.isFinite(sec) || sec <= 0) return "";
  const total = Math.round(sec);
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const mm = h > 0 ? String(m).padStart(2, "0") : String(m);
  return h > 0 ? `${h}:${mm}:${String(s).padStart(2, "0")}` : `${mm}:${String(s).padStart(2, "0")}`;
}

/** HEIC 封面交给 Rust 侧转 JPEG，其余地址直接用。 */
async function loadCover(raw: string) {
  cover.value = "";
  if (!raw) return;
  try {
    if (!/\.heic(\?|$)/i.test(raw)) {
      cover.value = raw;
      return;
    }
    const path = await api.fanqieCover(raw);
    cover.value = path.startsWith("http") ? path : convertFileSrc(path);
  } catch {
    // 封面失败不影响播放，播放器留黑底即可
  }
}

function scrollPageTop() {
  document.querySelector(".dd-content")?.scrollTo({ top: 0, behavior: "auto" });
}

async function load() {
  if (!seriesId) {
    error.value = "路由里缺少剧集 ID";
    return;
  }
  loading.value = true;
  error.value = "";
  detail.value = null;
  try {
    const data = await api.fanqieSeriesDetail(seriesId);
    detail.value = data;
    void loadCover(data.cover);
    const wanted = episodes.value.find((e) => e.vid === initialVid);
    const first = wanted ?? episodes.value[0];
    if (!first) {
      error.value = "这部剧没有可播放的分集";
      return;
    }
    await selectEpisode(first, false);
  } catch (e) {
    error.value = `拉取剧集详情失败：${(e as Error).message ?? e}`;
  } finally {
    loading.value = false;
  }
}

async function selectEpisode(ep: FanqieEpisode, syncRoute = true) {
  currentVid.value = ep.vid;
  if (syncRoute) {
    // replace：返回键要回列表，而不是一集一集往回退
    void router.replace({ name: "videoDetail", params: { seriesId, vid: ep.vid } });
  }

  const gen = ++playGeneration;
  playLoading.value = true;
  playError.value = "";
  playInfo.value = null;
  videoInfo.value = null;

  // 两个请求并行：播放地址先回来就先起播，`video_model` 只影响信息区
  const infoTask = api.fanqieVideoInfo(ep.vid).catch(() => null);
  try {
    // 主链路：App 流下载 → 解密 → 转 H.264（覆盖全部集数）；
    // 解不开时才退回官网明文 MP4（只覆盖前 50 集左右）。
    let play: FanqiePlayInfo;
    try {
      const prepared = await api.fanqiePrepareVideo(ep.vid);
      if (gen !== playGeneration) return;
      play = {
        url: convertFileSrc(prepared.path),
        quality: prepared.quality || "本地缓存",
        source: prepared.source === "cache" ? "cache" : "app",
      };
    } catch (appError) {
      if (gen !== playGeneration) return;
      try {
        play = await api.fanqiePlayUrl(seriesId, ep.vid);
      } catch (webError) {
        throw new Error(
          `解密失败（${(appError as Error)?.message ?? appError}）；` +
            `官网兜底也失败（${(webError as Error)?.message ?? webError}）`,
        );
      }
    }
    if (gen !== playGeneration) return;
    playError.value = "";
    playInfo.value = play;
    await nextTick();
    if (gen !== playGeneration) return;
    const el = videoEl.value;
    if (el) {
      // 换集后必须重新 load，否则 WebView 可能继续用上一条流
      el.load();
      void el.play().catch(() => {
        // 自动播放被拦时不打扰用户，原生控件可以手动点播
      });
    }
  } catch (e) {
    if (gen === playGeneration) {
      playError.value = `拿不到播放地址：${(e as Error)?.message ?? e}`;
    }
  } finally {
    if (gen === playGeneration) playLoading.value = false;
  }

  const info = await infoTask;
  if (gen === playGeneration && info) videoInfo.value = info;
}

function retryPlay() {
  if (currentEpisode.value) void selectEpisode(currentEpisode.value, false);
}

function goPrev() {
  if (!hasPrev.value) return;
  void selectEpisode(episodes.value[currentIndex.value - 1]);
}

function goNext() {
  if (!hasNext.value) return;
  void selectEpisode(episodes.value[currentIndex.value + 1]);
}

function onEnded() {
  if (autoNext.value && hasNext.value) goNext();
}

function onMediaError() {
  if (!playInfo.value) return;
  // 1 = MEDIA_ERR_ABORTED：换集时取消上一次加载，不算失败
  if (videoEl.value?.error?.code === 1) return;
  playError.value = "视频加载失败：本地文件可能被清理，点「重试」重新解密。";
}

function goBack() {
  if (window.history.length > 1) router.back();
  else void router.push({ name: "video" });
}

watch(currentVid, () => {
  void nextTick(() => {
    const el = document.querySelector(".vd-ep.on");
    el?.scrollIntoView({ block: "nearest" });
  });
});

onMounted(() => {
  scrollPageTop();
  void load();
});

onBeforeUnmount(() => {
  // 离开页面时停掉网络请求，别让视频在后台继续拉流
  videoEl.value?.pause();
});
</script>

<template>
  <div class="vd-page">
    <div class="vd-top">
      <button class="vd-back" @click="goBack">← 返回列表</button>
      <div class="vd-top-title">{{ detail?.title || "视频详情" }}</div>
      <div class="vd-top-meta dd-muted">
        <span v-if="episodes.length">{{ detail?.episodeCount || episodes.length }} 集</span>
        <span v-if="playInfo">{{ playInfo.quality }}</span>
        <span v-if="playInfo?.source === 'app'">本地解密</span>
        <span v-else-if="playInfo?.source === 'cache'">本地缓存</span>
        <span v-else-if="playInfo?.source?.startsWith('web')">官网兜底</span>
        <span v-if="detail?.source === 'app'">App 详情</span>
        <span v-else-if="detail?.source === 'web'">官网详情</span>
      </div>
      <n-button size="small" style="margin-left: auto" :loading="loading" @click="load">
        重新加载
      </n-button>
    </div>

    <n-alert v-if="error" type="error" title="加载失败" style="margin-bottom: 14px">
      {{ error }}
    </n-alert>

    <div v-if="loading && !detail" class="dd-empty">
      <n-spin size="small" /> 正在拉取剧集…
    </div>

    <div v-else-if="detail" class="vd-body">
      <!-- 播放器列 -->
      <div class="vd-player-col">
        <div class="vd-player">
          <video
            ref="videoEl"
            :src="playInfo?.url || undefined"
            :poster="cover || undefined"
            controls
            playsinline
            preload="metadata"
            @ended="onEnded"
            @error="onMediaError"
          />
          <div v-if="playLoading" class="vd-overlay">
            <n-spin size="small" />
            <span>正在下载并解密视频…（首次需要转码，后面会命中缓存）</span>
          </div>
          <div v-else-if="playError" class="vd-overlay vd-overlay-error">
            <div>{{ playError }}</div>
            <n-button size="small" @click="retryPlay">重试</n-button>
          </div>
          <div v-else-if="!playInfo" class="vd-overlay">暂无可播放地址</div>
        </div>

        <div class="vd-controls">
          <n-button size="small" :disabled="!hasPrev || playLoading" @click="goPrev">
            上一集
          </n-button>
          <n-button size="small" :disabled="!hasNext || playLoading" @click="goNext">
            下一集
          </n-button>
          <span class="vd-now">
            {{ currentEpisode ? `第 ${currentEpisode.index} 集` : "" }}
            <span v-if="currentEpisode?.durationSec" class="dd-muted">
              · {{ formatClock(currentEpisode.durationSec) }}
            </span>
          </span>
          <label class="vd-auto dd-muted">
            <input v-model="autoNext" type="checkbox" />
            播完自动下一集
          </label>
        </div>

        <div class="vd-note dd-muted">
          播放的是 App 流：本地下载后按 CENC-AES-CTR 解密，再转成 H.264 缓存，
          不受官网 50 集限制；官网明文流仅在解密失败时兜底。
        </div>
      </div>

      <!-- 剧集信息列 -->
      <div class="vd-info-col">
        <div class="vd-head">
          <div class="vd-cover">
            <img v-if="cover" :src="cover" alt="" />
            <div v-else class="vd-cover-fallback">{{ (detail.title || "剧").slice(0, 1) }}</div>
          </div>
          <div class="vd-head-main">
            <div class="vd-title">{{ detail.title || "未命名短剧" }}</div>
            <div class="vd-meta dd-muted">
              <span>共 {{ detail.episodeCount || episodes.length }} 集</span>
              <span v-if="detail.onlineTime">上架 {{ detail.onlineTime }}</span>
              <span v-if="videoInfo?.durationSec">单集 {{ formatClock(videoInfo.durationSec) }}</span>
              <span v-if="playInfo">当前 {{ playInfo.quality }}</span>
              <span v-if="videoInfo?.encrypted">平台流已加密（已本地解密）</span>
            </div>
            <div v-if="sortedQualities.length" class="vd-chips">
              <span class="vd-chip-label dd-muted">平台清晰度</span>
              <span v-for="q in sortedQualities" :key="q" class="vd-chip">{{ q }}</span>
            </div>
            <div class="vd-id dd-mono dd-muted">
              SID {{ detail.seriesId }}
              <template v-if="currentVid">/ VID {{ currentVid }}</template>
            </div>
          </div>
        </div>

        <p class="vd-intro">{{ detail.intro || "接口没给简介" }}</p>

        <div class="vd-ep-head">
          <span class="vd-ep-title">选集</span>
          <span class="dd-muted" style="font-size: 12px">
            当前第 {{ currentEpisode?.index ?? "—" }} 集
          </span>
        </div>
        <div class="vd-eps dd-scroll">
          <button
            v-for="ep in episodes"
            :key="ep.vid"
            class="vd-ep"
            :class="{ on: ep.vid === currentVid }"
            :title="ep.title"
            @click="selectEpisode(ep)"
          >
            {{ ep.index }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<style scoped>
.vd-top {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-bottom: 14px;
}
.vd-back {
  border: 1px solid rgba(128, 128, 128, 0.28);
  background: transparent;
  color: inherit;
  border-radius: 8px;
  padding: 4px 12px;
  font-size: 13px;
  cursor: pointer;
}
.vd-back:hover {
  border-color: #18a058;
  color: #18a058;
}
.vd-top-title {
  font-size: 15px;
  font-weight: 600;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  max-width: 46vw;
}
.vd-top-meta {
  display: flex;
  gap: 10px;
  font-size: 12px;
}

.vd-body {
  display: flex;
  align-items: flex-start;
  gap: 22px;
}

/* ---------- 播放器 ---------- */
.vd-player-col {
  --vd-player-h: min(60vh, 600px);
  flex: none;
  width: calc(var(--vd-player-h) * 9 / 16);
  max-width: 46%;
}
.vd-player {
  position: relative;
  height: var(--vd-player-h);
  background: #000;
  border-radius: 10px;
  overflow: hidden;
}
.vd-player video {
  width: 100%;
  height: 100%;
  object-fit: contain;
  display: block;
  background: #000;
}
.vd-overlay {
  position: absolute;
  inset: 0;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  padding: 20px;
  text-align: center;
  color: #fff;
  font-size: 13px;
  background: rgba(0, 0, 0, 0.62);
}
.vd-overlay-error {
  color: #ffb4a2;
}
.vd-controls {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 10px;
  flex-wrap: wrap;
}
.vd-now {
  font-size: 13px;
  font-weight: 600;
  margin-left: 4px;
}
.vd-auto {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  font-size: 12px;
  margin-left: auto;
  cursor: pointer;
}
.vd-note {
  font-size: 12px;
  margin-top: 8px;
  line-height: 1.6;
}

/* ---------- 剧集信息 ---------- */
.vd-info-col {
  flex: 1;
  min-width: 0;
}
.vd-head {
  display: flex;
  gap: 16px;
  align-items: flex-start;
}
.vd-cover {
  width: 132px;
  flex: none;
  aspect-ratio: 3 / 4;
  border-radius: 8px;
  overflow: hidden;
  background: rgba(128, 128, 128, 0.12);
}
.vd-cover img {
  width: 100%;
  height: 100%;
  object-fit: cover;
  display: block;
}
.vd-cover-fallback {
  width: 100%;
  height: 100%;
  display: flex;
  align-items: center;
  justify-content: center;
  font-size: 34px;
  font-weight: 600;
  color: rgba(128, 128, 128, 0.55);
  background: linear-gradient(135deg, rgba(128, 128, 128, 0.1), rgba(128, 128, 128, 0.22));
}
.vd-head-main {
  flex: 1;
  min-width: 0;
}
.vd-title {
  font-size: 18px;
  font-weight: 600;
  line-height: 1.4;
}
.vd-meta {
  display: flex;
  flex-wrap: wrap;
  gap: 10px;
  font-size: 12px;
  margin-top: 4px;
}
.vd-chips {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 6px;
  margin-top: 8px;
}
.vd-chip-label {
  font-size: 12px;
}
.vd-chip {
  font-size: 11px;
  line-height: 1;
  padding: 4px 8px;
  border-radius: 999px;
  background: rgba(128, 128, 128, 0.14);
}
.vd-id {
  margin-top: 8px;
  font-size: 11px;
  word-break: break-all;
}
.vd-intro {
  font-size: 13px;
  line-height: 1.75;
  margin: 14px 0 0;
  white-space: pre-wrap;
  color: rgba(128, 128, 128, 0.95);
}
.vd-ep-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  margin: 16px 0 8px;
}
.vd-ep-title {
  font-size: 14px;
  font-weight: 600;
}
.vd-eps {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(44px, 1fr));
  gap: 8px;
  max-height: 280px;
  padding: 10px;
}
.vd-ep {
  border: 1px solid rgba(128, 128, 128, 0.24);
  background: transparent;
  color: inherit;
  border-radius: 6px;
  padding: 5px 0;
  font-size: 13px;
  cursor: pointer;
  transition: border-color 0.15s, color 0.15s, background 0.15s;
}
.vd-ep:hover {
  border-color: #18a058;
}
.vd-ep.on {
  border-color: #18a058;
  color: #18a058;
  font-weight: 600;
  background: rgba(24, 160, 88, 0.08);
}

@media (max-width: 1180px) {
  .vd-body {
    flex-direction: column;
  }
  .vd-player-col {
    width: 100%;
    max-width: 100%;
    --vd-player-h: min(56vh, 520px);
  }
  .vd-player {
    width: calc(var(--vd-player-h) * 9 / 16);
    max-width: 100%;
    margin: 0 auto;
  }
}
</style>
