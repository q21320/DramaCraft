<script setup lang="ts">
import { computed, onMounted } from "vue";
import { useRouter } from "vue-router";
import { NButton } from "naive-ui";
import { useDownloadStore } from "@/stores/download";

/**
 * 批量下载进度浮层。
 *
 * 挂在 MainLayout 而不是视频页：下载是全局任务，切到任何页面都该看得见。
 * 进度本身住在 Rust 那边 —— 页面刷新（组件重建）时 `restore()` 读回来，
 * 正在跑或者上次被关掉都还能接着看。
 * 整批成功后面板自己收起（用户要的「处理完就隐藏」）；
 * 有失败或错误时保留，得让用户看到原因再手动关。
 */
const dl = useDownloadStore();
const percent = computed(() => dl.percent);
const router = useRouter();

/**
 * 刷新过页面的那批下载不会自动跳走了（`onDone` 只活在发起它的那次页面里）。
 * 给个入口把刚落盘的剧送去整剧处理，和原来自动跳转的参数一致。
 */
function goPipeline() {
  if (dl.running || dl.landed.length === 0) return;
  void router.push({
    name: "pipeline",
    query: { root: dl.root, autostart: "1", only: JSON.stringify(dl.landed) },
  });
  dl.dismiss();
}

// 刷新/重开应用后第一次挂载：把后端那份状态检回来
onMounted(() => void dl.restore());
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
    <!-- 并发下载时 message 只留最后一条，像只有一集在跑；
         这里把并行的那几集都摆出来。 -->
    <div v-if="dl.running && dl.inflight.length > 0" class="dd-dl-line dd-muted dd-wrap">
      正在下载 {{ dl.inflight.length }} 集：{{ dl.inflight.join("、") }}
    </div>
    <div class="dd-dl-line dd-muted dd-wrap">{{ dl.message }}</div>
    <div class="dd-dl-line dd-muted">
      已完成 {{ dl.done }} 集
      <template v-if="dl.failed > 0"> · 失败 {{ dl.failed }} 项</template>
    </div>
    <div v-if="dl.root" class="dd-dl-line dd-muted dd-wrap" style="font-size: 11px">
      保存到：{{ dl.root }}（每部剧的产出会生成在自己的剧目录里）
    </div>
    <div v-if="dl.error" class="dd-dl-error dd-wrap">{{ dl.error }}</div>
    <n-button
      v-if="!dl.running && dl.failed === 0 && dl.landed.length > 0"
      size="tiny"
      type="primary"
      style="margin-top: 8px"
      @click="goPipeline"
    >
      去整剧处理（{{ dl.landed.length }} 部）
    </n-button>
    <div class="dd-muted" style="font-size: 11px; margin-top: 4px">
      {{
        dl.running
          ? "下载在后台继续，刷新页面或切走都不会中断"
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
