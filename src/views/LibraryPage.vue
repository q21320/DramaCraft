<script setup lang="ts">
import { ref } from "vue";
import { NButton, NPopconfirm, NSpace, useMessage } from "naive-ui";
import { open } from "@tauri-apps/plugin-dialog";
import { useRouter } from "vue-router";
import { useAppStore } from "@/stores/app";
import { formatDuration, formatSize } from "@/utils/format";

const store = useAppStore();
const router = useRouter();
const message = useMessage();
const busy = ref(false);

const VIDEO_EXT = ["mp4", "mkv", "mov", "avi", "ts", "flv", "wmv", "m4v", "webm", "mpg", "mpeg"];

async function add() {
  if (store.currentProjectId === null) {
    message.warning("请先在「项目」里创建或选择一个项目");
    return;
  }
  const picked = await open({
    multiple: true,
    title: "选择视频素材",
    filters: [{ name: "视频文件", extensions: VIDEO_EXT }],
  });
  const list = typeof picked === "string" ? [picked] : Array.isArray(picked) ? picked : [];
  if (list.length === 0) return;

  busy.value = true;
  let ok = 0;
  const failures: string[] = [];
  for (const path of list) {
    try {
      await store.addMedia(path);
      ok += 1;
    } catch (e) {
      failures.push(`${path}：${(e as Error).message}`);
    }
  }
  busy.value = false;

  if (ok > 0) message.success(`已导入 ${ok} 个素材`);
  if (failures.length > 0) {
    message.error(`${failures.length} 个导入失败，第一个原因：${failures[0]}`);
  }
}

async function remove(id: number) {
  try {
    await store.removeMedia(id);
    message.success("已移除");
  } catch (e) {
    message.error((e as Error).message);
  }
}

function toSlice(id: number) {
  void router.push({ path: "/slice", query: { mediaId: String(id) } });
}
</script>

<template>
  <div v-if="store.currentProject" class="dd-card">
    <div class="dd-row" style="justify-content: space-between">
      <div>
        <div class="dd-card-title">素材库 · {{ store.currentProject.name }}</div>
        <div class="dd-card-desc">
          导入后会自动调用 ffprobe 探测时长与分辨率；探测失败会在提示里说明原因。
        </div>
      </div>
      <n-button size="small" type="primary" :loading="busy" @click="add">
        导入素材
      </n-button>
    </div>

    <div v-if="store.media.length === 0" class="dd-empty">
      还没有素材。点右上角「导入素材」选择视频文件。
    </div>

    <div v-else class="dd-scroll">
      <table class="dd-table">
        <thead>
          <tr>
            <th>文件</th>
            <th>时长</th>
            <th>分辨率</th>
            <th>音轨</th>
            <th>体积</th>
            <th>容器</th>
            <th style="width: 160px">操作</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="m in store.media" :key="m.id">
            <td class="dd-wrap">
              <div>{{ m.name }}</div>
              <div class="dd-mono dd-muted">{{ m.path }}</div>
            </td>
            <td>{{ formatDuration(m.durationMs) }}</td>
            <td>{{ m.width && m.height ? `${m.width}×${m.height}` : "—" }}</td>
            <td>{{ m.hasAudio ? "有" : "无" }}</td>
            <td>{{ formatSize(m.sizeBytes) }}</td>
            <td class="dd-muted">{{ m.formatName || "—" }}</td>
            <td>
              <n-space :size="6">
                <n-button size="tiny" @click="toSlice(m.id)">切片</n-button>
                <n-popconfirm @positive-click="remove(m.id)">
                  <template #trigger>
                    <n-button size="tiny" quaternary type="error">移除</n-button>
                  </template>
                  只从项目里移除记录，不会删除磁盘上的原文件。
                </n-popconfirm>
              </n-space>
            </td>
          </tr>
        </tbody>
      </table>
    </div>
  </div>

  <div v-else class="dd-card">
    <div class="dd-card-title">还没有选择项目</div>
    <div class="dd-card-desc">素材是挂在项目下的，先创建或选择一个项目。</div>
    <n-button size="small" @click="router.push('/')">去项目页</n-button>
  </div>
</template>
