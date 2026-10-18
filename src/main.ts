import { createApp } from "vue";
import { createPinia } from "pinia";
import App from "./App.vue";
import router from "./router";
import { installShellGuards } from "./utils/shell";
import "./styles/base.css";

// 挂载之前先把「浏览器」那一套收起来（右键菜单、F5、缩放、拖文件跳转）
installShellGuards();

// naive-ui 组件在各组件内按需引入，便于 tree-shaking 与类型检查
createApp(App).use(createPinia()).use(router).mount("#app");
