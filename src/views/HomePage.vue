<script setup lang="ts">
import { ref } from "vue";
import { NButton, NInput, NPopconfirm, NSpace, useMessage } from "naive-ui";
import { open } from "@tauri-apps/plugin-dialog";
import { useRouter } from "vue-router";
import { useAppStore } from "@/stores/app";

const store = useAppStore();
const router = useRouter();
const message = useMessage();

const name = ref("");
const rootDir = ref("");
const busy = ref(false);

async function pickRoot() {
  const picked = await open({ directory: true, multiple: false, title: "选择项目目录" });
  if (typeof picked === "string") rootDir.value = picked;
}

async function create() {
  const trimmed = name.value.trim();
  if (!trimmed) {
    message.warning("请先填写项目名");
    return;
  }
  busy.value = true;
  try {
    await store.createProject(trimmed, rootDir.value.trim());
    name.value = "";
    rootDir.value = "";
    message.success("项目已创建");
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

async function remove(id: number) {
  try {
    await store.deleteProject(id);
    message.success("已删除");
  } catch (e) {
    message.error((e as Error).message);
  }
}

async function enter(id: number) {
  await store.selectProject(id);
  void router.push("/library");
}
</script>

<template>
  <div class="dd-card">
    <div class="dd-card-title">新建项目</div>
    <div class="dd-card-desc">
      一个项目对应一部短剧。素材、切片结果都挂在这个项目下，互不干扰。
    </div>
    <div class="dd-row">
      <n-input
        v-model:value="name"
        placeholder="项目名，例如：都市爽剧 A"
        style="width: 260px"
        @keyup.enter="create"
      />
      <n-input
        v-model:value="rootDir"
        placeholder="项目目录（可选，用于存放输出）"
        style="width: 380px"
      />
      <n-button size="small" @click="pickRoot">选择目录</n-button>
      <n-button size="small" type="primary" :loading="busy" @click="create">
        创建
      </n-button>
    </div>
  </div>

  <div class="dd-card">
    <div class="dd-card-title">项目列表</div>
    <div class="dd-card-desc">
      共 {{ store.projects.length }} 个项目
    </div>

    <div v-if="store.projects.length === 0" class="dd-empty">
      还没有项目。先在上面创建一个，再去「素材库」导入长剧素材。
    </div>

    <table v-else class="dd-table">
      <thead>
        <tr>
          <th>名称</th>
          <th>目录</th>
          <th>创建时间</th>
          <th style="width: 180px">操作</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="p in store.projects" :key="p.id">
          <td>{{ p.name }}</td>
          <td class="dd-wrap dd-mono">{{ p.rootDir || "—" }}</td>
          <td class="dd-muted">{{ p.createdAt }}</td>
          <td>
            <n-space :size="6">
              <n-button size="tiny" @click="enter(p.id)">进入</n-button>
              <n-popconfirm @positive-click="remove(p.id)">
                <template #trigger>
                  <n-button size="tiny" type="error" quaternary>删除</n-button>
                </template>
                删除项目会同时删除它的素材与切片记录，确认吗？
              </n-popconfirm>
            </n-space>
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</template>
