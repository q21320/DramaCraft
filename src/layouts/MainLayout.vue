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
  NSelect,
  NSpace,
  NTag,
  NText,
  type MenuOption,
} from "naive-ui";
import { navByPath, navGroups, primaryNav } from "./nav";
import { useAppStore } from "@/stores/app";

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

const projectOptions = computed(() =>
  store.projects.map((p) => ({ label: p.name, value: p.id })),
);

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

    <n-layout>
      <n-layout-header bordered class="dd-header">
        <div class="dd-header-left">
          <div class="dd-title">{{ current.label }}</div>
          <div class="dd-subtitle">{{ current.subtitle }}</div>
        </div>
        <n-space align="center" :size="12">
          <n-tag v-if="store.info" size="small" :bordered="false">
            v{{ store.info.version }}
          </n-tag>
          <n-select
            v-if="store.projects.length > 0"
            :value="store.currentProjectId"
            :options="projectOptions"
            size="small"
            style="width: 200px"
            placeholder="选择项目"
            @update:value="(v: number) => store.selectProject(v)"
          />
        </n-space>
      </n-layout-header>

      <n-layout-content class="dd-content">
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

        <router-view />
      </n-layout-content>
    </n-layout>
  </n-layout>
</template>

<style scoped>
.dd-shell {
  height: 100vh;
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
.dd-title {
  font-size: 16px;
  font-weight: 600;
}
.dd-subtitle {
  font-size: 12px;
  opacity: 0.62;
  margin-top: 2px;
}
.dd-content {
  padding: 18px 20px 40px;
  height: calc(100vh - 66px);
  overflow: auto;
  box-sizing: border-box;
}
.dd-alert {
  margin-bottom: 14px;
}
</style>
