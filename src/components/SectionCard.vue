<script setup lang="ts">
import { NButton, NSpace, NTag } from "naive-ui";

defineProps<{
  title: string;
  desc?: string;
  /** 诚实标注：未接入的能力在这里写明，而不是假装成功 */
  pending?: string;
}>();

defineEmits<{ (e: "action"): void }>();
</script>

<template>
  <div class="dd-card">
    <div class="dd-row" style="justify-content: space-between">
      <div>
        <div class="dd-card-title">{{ title }}</div>
        <div v-if="desc" class="dd-card-desc">{{ desc }}</div>
      </div>
      <n-space align="center" :size="8">
        <n-tag v-if="pending" size="small" type="warning" :bordered="false">
          {{ pending }}
        </n-tag>
        <slot name="actions">
          <n-button v-if="$slots.default" size="small" @click="$emit('action')">
            <slot />
          </n-button>
        </slot>
      </n-space>
    </div>
  </div>
</template>
