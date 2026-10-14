<script setup lang="ts">
import { NAlert, NTag } from "naive-ui";
import { useAppStore } from "@/stores/app";

const store = useAppStore();
</script>

<template>
  <div class="dd-card">
    <div class="dd-row" style="justify-content: space-between">
      <div>
        <div class="dd-card-title">ASR 字幕轴</div>
        <div class="dd-card-desc">
          计划用 faster-whisper（MIT）在独立 Python 子进程里跑，产出 SRT 与逐条时间轴。
          走子进程是为了模型崩溃时不拖死界面。
        </div>
      </div>
      <n-tag size="small" type="warning" :bordered="false">v0.2 接入</n-tag>
    </div>

    <n-alert
      :type="store.asr?.available ? 'success' : 'default'"
      :title="store.asr?.available ? '运行时就绪' : '运行时未就绪'"
      :show-icon="true"
    >
      {{ store.asr?.detail || "尚未检测" }}
    </n-alert>

    <div style="margin-top: 14px">
      <div class="dd-card-title">这一版为什么先不做</div>
      <ul class="dd-muted" style="margin: 8px 0 0; padding-left: 18px">
        <li>字幕要等切片稳定之后再对齐，否则时间轴会随切点反复变化；</li>
        <li>Whisper 模型体积不小，需要先确定模型存放位置与下载策略；</li>
        <li>接口与子进程协议已经在 <span class="dd-mono">src-tauri/src/asr/mod.rs</span> 里定好，接入时不需要改动上层。</li>
      </ul>
    </div>
  </div>
</template>
