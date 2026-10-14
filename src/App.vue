<script setup lang="ts">
import { onMounted } from "vue";
import {
  NConfigProvider,
  NDialogProvider,
  NMessageProvider,
  dateZhCN,
  zhCN,
} from "naive-ui";
import MainLayout from "@/layouts/MainLayout.vue";
import { useAppStore } from "@/stores/app";

const store = useAppStore();

onMounted(() => {
  void store.bootstrap();
});
</script>

<template>
  <n-config-provider :locale="zhCN" :date-locale="dateZhCN">
    <n-message-provider>
      <n-dialog-provider>
        <MainLayout />
      </n-dialog-provider>
    </n-message-provider>
  </n-config-provider>
</template>
