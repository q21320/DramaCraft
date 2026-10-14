import { createApp } from "vue";
import { createPinia } from "pinia";
import App from "./App.vue";
import router from "./router";
import "./styles/base.css";

// naive-ui 组件在各组件内按需引入，便于 tree-shaking 与类型检查
createApp(App).use(createPinia()).use(router).mount("#app");
