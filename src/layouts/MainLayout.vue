<script setup lang="ts">
import { computed, h } from "vue";
import { RouterLink, useRoute, useRouter } from "vue-router";
import {
  NAlert,
  NLayout,
  NLayoutContent,
  NLayoutHeader,
  NLayoutSider,
  NMenu,
  NSpace,
  NTag,
  NText,
  type MenuOption,
} from "naive-ui";
import { navByPath, navGroups, primaryNav, videoNav } from "./nav";
import { useAppStore } from "@/stores/app";
import DownloadPanel from "@/components/DownloadPanel.vue";

const route = useRoute();
const router = useRouter();
const store = useAppStore();

const current = computed(() => navByPath(route.path) ?? primaryNav);

/**
 * 菜单：流水线单独摆最上面，其余按用途分组、默认收起。
 *
 * 用户说过「看着好乱，能不能只保留流水线」—— 那就让第一眼只有一个动作。
 * 分组用 `children`（可折叠），而不是 `type: 'group'`（常驻展开）。
 */
const menuOptions = computed<MenuOption[]>(() => [
  {
    key: videoNav.path,
    label: () => h(RouterLink, { to: videoNav.path }, { default: () => videoNav.label }),
  },
  {
    key: primaryNav.path,
    label: () => h(RouterLink, { to: primaryNav.path }, { default: () => primaryNav.label }),
  },
  ...navGroups.map((group) => ({
    key: `group:${group.key}`,
    label: group.title,
    children: group.items.map((item) => ({
      key: item.path,
      label: () => h(RouterLink, { to: item.path }, { default: () => item.label }),
    })),
  })),
]);

/** 只展开「当前页所在」的那一组，其余保持收起 */
const expandedKeys = computed(() => {
  const group = navGroups.find((g) => g.items.some((i) => i.path === route.path));
  return group ? [`group:${group.key}`] : [];
});

const toolWarning = computed(() => {
  if (store.loading) return "";
  const missing = store.toolReports.filter((r) => !r.available).map((r) => r.name);
  if (missing.length === 0) return "";
  return `未就绪的外部工具：${missing.join("、")}。切片功能需要 ffmpeg 与 ffprobe。`;
});
</script>

<template>
  <n-layout has-sider class="dd-shell">
    <n-layout-sider bordered :width="188" class="dd-sider">
      <div class="dd-brand">
        <div class="dd-brand-title">DramaDist</div>
        <div class="dd-brand-sub">短剧切片分发工作台</div>
      </div>
      <n-menu
        :value="current.path"
        :options="menuOptions"
        :default-expanded-keys="expandedKeys"
        :accordion="true"
      />
    </n-layout-sider>

    <n-layout content-class="dd-main">
      <n-layout-header bordered class="dd-header">
        <div class="dd-header-left">
          <div class="dd-title">{{ current.label }}</div>
          <div class="dd-subtitle">{{ current.subtitle }}</div>
        </div>
        <div class="dd-header-actions">
          <n-space v-if="!route.path.startsWith(`${videoNav.path}/`) && route.path !== videoNav.path" align="center" :size="12">
            <n-tag v-if="store.info" size="small" :bordered="false">
              v{{ store.info.version }}
            </n-tag>
          </n-space>
        </div>
      </n-layout-header>

      <n-layout-content class="dd-content" content-class="dd-content-scroll">
        <n-alert
          v-if="toolWarning"
          type="warning"
          :show-icon="true"
          class="dd-alert"
          title="外部工具未就绪"
        >
          {{ toolWarning }}
          <n-text depth="3" style="cursor: pointer; margin-left: 8px" @click="void router.push('/settings')">
            去设置 →
          </n-text>
        </n-alert>
        <n-alert v-if="store.error" type="error" class="dd-alert" title="启动异常">
          {{ store.error }}
        </n-alert>

        <!--
          视频页缓存起来：列表有几十条封面，每次进来重拉既慢又浪费。
          其他页不缓存 —— 它们的任务状态更适合进来就看最新。
          名字靠 VideoPage.vue 里的 defineOptions({ name: "VideoPage" }) 固定。
        -->
        <router-view v-slot="{ Component }">
          <keep-alive :include="['VideoPage']">
            <component :is="Component" />
          </keep-alive>
        </router-view>
      </n-layout-content>
    </n-layout>

    <!-- 下载进度浮层：全局挂在这里，切页面不丢；下载处理完自己收起 -->
    <DownloadPanel />
  </n-layout>
</template>

<style scoped>
/*
 * 整页只留一条滚动条。
 *
 * naive-ui 的每个 n-layout 都自带一个 `.n-layout-scroll-container`，它的
 * `overflow-x: hidden` 会让另一轴按 CSS 规则算成 `auto` —— 外层、右半边、内容区
 * 三层叠在一起，窗口一变窄就是两三条并排的滚动条。这里把 naive 那几层全锁死，
 * 只让 `.dd-content` 自己滚。
 */
.dd-shell {
  height: 100vh;
  overflow: hidden;
}
.dd-shell :deep(.n-layout-scroll-container) {
  overflow: hidden;
}
/*
 * header + content 纵向排开，content 吃掉剩余高度。
 * 原来的 `calc(100vh - 66px)` 写死了 header 高度，实际量出来是 74px：
 * 差的那 8px 让每一页都恒定多出一点可滚距离，也就是那条多余的主滚动条。
 *
 * `dd-main` / `dd-content-scroll` 是 n-layout 的 `content-class`，
 * 直接挂在它自己的滚动容器上 —— 用 :deep 拼层级选择器太脆，换版面就失效。
 */
:deep(.dd-main) {
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
.dd-content {
  flex: 1;
  min-height: 0;
  height: auto;
  /* 纵向自己滚；横向裁掉，避免内容区那层改成 visible 后冒出一条横向滚动条 */
  overflow-x: hidden;
  overflow-y: auto;
  /*
   * 注意：滚动容器自己不能有 padding。
   * sticky 的 top:0 是相对滚动容器的内边距内侧算的 —— 只要这里有
   * padding-top，吸顶条顶到位后上方就会漏出一条（视频页实测漏 18px，
   * 卡片从缝里穿过去）。padding 挪到里层 `.dd-content-scroll`，
   * 跟着内容一起滚，静止时视觉完全一致。
   */
}
/*
 * 内容区那层必须回到 `visible`。
 *
 * 上面的 `overflow: hidden` 是给外层布局容器去滚动条用的，但它对 sticky 是致命的：
 * CSS 里 sticky 找的是「最近的滚动祖先」，`overflow: hidden` 也算一个 —— 而它自己
 * 永远不滚，于是视频页那条吸顶的分类筛选条就成了普通元素，跟着内容滚走。
 * 这层高度是 auto，本来就不需要裁剪，滚动统一交给外层 `.dd-content`。
 * 选择器要带上 `.dd-shell`，否则 (0,2,0) 压不过上面那条 (0,3,0)。
 */
.dd-shell :deep(.dd-content-scroll) {
  height: auto;
  overflow: visible;
  padding: 18px 20px 40px;
}
.dd-sider {
  display: flex;
  flex-direction: column;
}
.dd-brand {
  padding: 18px 18px 12px;
}
.dd-brand-title {
  font-size: 17px;
  font-weight: 600;
  letter-spacing: 0.3px;
}
.dd-brand-sub {
  font-size: 12px;
  opacity: 0.6;
  margin-top: 2px;
}
.dd-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 12px 20px;
}
.dd-header-actions {
  display: flex;
  align-items: center;
  gap: 12px;
}
.dd-title {
  font-size: 16px;
  font-weight: 600;
}
.dd-subtitle {
  font-size: 12px;
  opacity: 0.62;
  margin-top: 2px;
}
.dd-alert {
  margin-bottom: 14px;
}
</style>
