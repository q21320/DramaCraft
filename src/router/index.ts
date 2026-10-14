import { createRouter, createWebHashHistory, type RouteRecordRaw } from "vue-router";

/**
 * 路由。
 *
 * 注意默认页是**流水线**而不是「项目」—— 打开应用第一眼应当落在
 * 每天真正要用的那个动作上。项目列表挪到「素材与证明」分组里。
 */
const routes: RouteRecordRaw[] = [
  { path: "/", redirect: "/pipeline" },
  { path: "/pipeline", name: "pipeline", component: () => import("@/views/PipelinePage.vue") },
  { path: "/projects", name: "home", component: () => import("@/views/HomePage.vue") },
  { path: "/library", name: "library", component: () => import("@/views/LibraryPage.vue") },
  { path: "/slice", name: "slice", component: () => import("@/views/SlicePage.vue") },
  { path: "/subtitle", name: "subtitle", component: () => import("@/views/SubtitlePage.vue") },
  { path: "/export", name: "export", component: () => import("@/views/ExportPage.vue") },
  { path: "/draft", name: "draft", component: () => import("@/views/DraftPage.vue") },
  { path: "/transform", name: "transform", component: () => import("@/views/TransformPage.vue") },
  { path: "/publish", name: "publish", component: () => import("@/views/PublishPage.vue") },
  { path: "/series", name: "series", component: () => import("@/views/SeriesPage.vue") },
  { path: "/queue", name: "queue", component: () => import("@/views/QueuePage.vue") },
  { path: "/proof", name: "proof", component: () => import("@/views/ProofPage.vue") },
  { path: "/asset", name: "asset", component: () => import("@/views/AssetPage.vue") },
  { path: "/settings", name: "settings", component: () => import("@/views/SettingsPage.vue") },
  { path: "/:pathMatch(.*)*", redirect: "/pipeline" },
];

export default createRouter({
  history: createWebHashHistory(),
  routes,
});
