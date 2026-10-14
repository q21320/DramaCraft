import { createRouter, createWebHashHistory, type RouteRecordRaw } from "vue-router";

const routes: RouteRecordRaw[] = [
  { path: "/", name: "home", component: () => import("@/views/HomePage.vue") },
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
  { path: "/:pathMatch(.*)*", redirect: "/" },
];

export default createRouter({
  history: createWebHashHistory(),
  routes,
});
