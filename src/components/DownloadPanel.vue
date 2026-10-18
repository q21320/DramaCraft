<script setup lang="ts">
import { computed } from "vue";
import { useDownloadStore } from "@/stores/download";

/**
 * 批量下载进度浮层。
 *
 * 挂在 MainLayout 而不是视频页：下载是全局任务，切到任何页面都该看得见。
 * 整批成功后面板自己收起（用户要的「处理完就隐藏」）；
 * 有失败或错误时保留，得让用户看到原因再手动关。
 */
const dl = useDownloadStore();
const percent = computed(() => dl.percent);
</script>

<template>
  <div v-if="dl.active" class="dd-dl">
    <div class="dd-dl-head">
      <b>下载到默认下载目录</b>
      <span class="dd-muted">
        {{ Math.min(dl.seriesIndex + 1, dl.seriesTotal) }} / {{ dl.seriesTotal }} 部
      </span>
      <button v-if="!dl.running" class="dd-dl-close" title="收起" @click="dl.dismiss()">
        ×
      </button>
    </div>
    <div class="dd-dl-bar"><i :style="{ width: percent + '%' }" /></div>
    <div class="dd-dl-line">
      《{{ dl.title || "…" }}》
      <span v-if="dl.episodes > 0" class="dd-muted">
        已处理 {{ dl.episode }} / {{ dl.episodes }} 集
      </span>
    </div>
    <div class="dd-dl-line dd-muted dd-wrap">{{ dl.message }}</div>
    <div class="dd-dl-line dd-muted">
      已完成 {{ dl.done }} 集
      <template v-if="dl.failed > 0"> · 失败 {{ dl.failed }}</template>
    </div>
    <div v-if="dl.root" class="dd-dl-line dd-muted dd-wrap" style="font-size: 11px">
      保存到：{{ dl.root }}（每部剧自己的「二创」在剧目录里）
    </div>
    <div v-if="dl.error" class="dd-dl-error dd-wrap">{{ dl.error }}</div>
    <div class="dd-muted" style="font-size: 11px; margin-top: 4px">
      {{
        dl.running
          ? "下载在后台继续，切到别的页面不会中断"
          : dl.failed > 0
            ? "有失败集时不会自动开跑；再点一次「下载」只补缺的集"
            : "下载完会自动跳到整剧处理并开始"
      }}
    </div>
  </div>
</template>

<style scoped>
.dd-wrap {
  white-space: normal;
  word-break: break-all;
}
.dd-dl {
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
.dd-dl-head {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 8px;
  font-size: 13px;
}
.dd-dl-head b {
  flex: none;
}
.dd-dl-head .dd-muted {
  margin-left: auto;
}
.dd-dl-close {
  flex: none;
  border: 0;
  background: transparent;
  color: inherit;
  font-size: 16px;
  line-height: 1;
  cursor: pointer;
  opacity: 0.55;
}
.dd-dl-close:hover {
  opacity: 1;
}
.dd-dl-bar {
  height: 6px;
  margin-bottom: 8px;
  border-radius: 999px;
  background: rgba(128, 128, 128, 0.18);
  overflow: hidden;
}
.dd-dl-bar i {
  display: block;
  height: 100%;
  background: #18a058;
  transition: width 0.3s ease;
}
.dd-dl-line {
  line-height: 1.7;
}
.dd-dl-error {
  margin-top: 4px;
  color: #d03050;
  line-height: 1.6;
}
</style>
