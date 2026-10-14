<script setup lang="ts">
import { computed, onMounted, ref, watch } from "vue";
import { NAlert, NButton, NInput, NSelect, NSpace, NTag, useMessage } from "naive-ui";
import { open } from "@tauri-apps/plugin-dialog";
import { api } from "@/api";
import { useAppStore } from "@/stores/app";
import type { PlatformOption, PublishAccount, PublishPackageReport, PublishPlan } from "@/types";
import { formatDuration } from "@/utils/format";

const store = useAppStore();
const message = useMessage();

const mediaId = ref<number | null>(null);
const platforms = ref<PlatformOption[]>([]);
const platform = ref("douyin");
const account = ref("默认账号");
const plans = ref<PublishPlan[]>([]);
const accounts = ref<PublishAccount[]>([]);
const outRoot = ref("");
const busy = ref(false);
const report = ref<PublishPackageReport | null>(null);
const segmentCount = ref(0);

const form = ref<PublishPlan>({
  id: 0,
  mediaId: 0,
  platform: "douyin",
  account: "默认账号",
  title: "",
  description: "",
  tags: "",
  coverPath: "",
  status: "draft",
  note: "",
  updatedAt: "",
});

/** 当前平台下已经用过的账号名，方便复用而不是每次手打 */
const accountOptions = computed(() => {
  const names = new Set<string>();
  names.add("默认账号");
  for (const p of plans.value) {
    if (p.platform === platform.value && p.account) names.add(p.account);
  }
  for (const a of accounts.value) {
    if (a.platform === platform.value && a.account) names.add(a.account);
  }
  return [...names].map((n) => ({ label: n, value: n }));
});

const mediaOptions = computed(() =>
  store.media.map((m) => ({
    label: `${m.name}（${formatDuration(m.durationMs)}）`,
    value: m.id,
  })),
);

const platformOptions = computed(() =>
  platforms.value.map((p) => ({
    label: p.hasPublicApi ? `${p.label}` : `${p.label}（无公开接口）`,
    value: p.key,
  })),
);

const currentPlatform = computed(
  () => platforms.value.find((p) => p.key === platform.value) ?? null,
);

const statusLabels: Record<string, string> = {
  draft: "草稿",
  ready: "待发布",
  published: "已发布",
  failed: "失败",
};

const statusTypes: Record<string, "default" | "info" | "success" | "error"> = {
  draft: "default",
  ready: "info",
  published: "success",
  failed: "error",
};

const selected = computed(() => store.media.find((m) => m.id === mediaId.value) ?? null);

function blankPlan(): PublishPlan {
  return {
    id: 0,
    mediaId: mediaId.value ?? 0,
    platform: platform.value,
    account: account.value,
    title: selected.value?.name.replace(/\.[^.]+$/, "") ?? "",
    description: "",
    tags: "",
    coverPath: "",
    status: "draft",
    note: "",
    updatedAt: "",
  };
}

async function refreshSegments() {
  if (mediaId.value === null) {
    segmentCount.value = 0;
    return;
  }
  try {
    const segments = await api.listSegments(mediaId.value);
    segmentCount.value = segments.filter((s) => s.status === "ok").length;
  } catch {
    segmentCount.value = 0;
  }
}

async function loadPlan() {
  if (mediaId.value === null) return;
  try {
    plans.value = await api.listPublishPlans(mediaId.value);
    const found = plans.value.find(
      (p) => p.platform === platform.value && p.account === account.value,
    );
    form.value = found ? { ...found } : blankPlan();
  } catch (e) {
    message.error((e as Error).message);
  }
}

async function loadAccounts() {
  try {
    accounts.value = await api.listPublishAccounts();
  } catch {
    accounts.value = [];
  }
}

async function pickCover() {
  const picked = await open({
    multiple: false,
    title: "选择封面图",
    filters: [{ name: "图片", extensions: ["jpg", "jpeg", "png", "webp"] }],
  });
  if (typeof picked === "string") form.value.coverPath = picked;
}

async function pickOutRoot() {
  const picked = await open({
    directory: true,
    multiple: false,
    title: "选择发布包输出目录",
  });
  if (typeof picked === "string") outRoot.value = picked;
}

async function save() {
  if (mediaId.value === null) {
    message.warning("请先选择素材");
    return;
  }
  if (!form.value.title.trim()) {
    message.warning("标题不能为空");
    return;
  }
  busy.value = true;
  try {
    form.value.platform = platform.value;
    form.value.account = account.value;
    form.value.mediaId = mediaId.value;
    const saved = await api.savePublishPlan({ ...form.value });
    form.value = { ...saved };
    account.value = saved.account;
    plans.value = await api.listPublishPlans(mediaId.value);
    await loadAccounts();
    message.success("已保存");
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

async function mark(status: string) {
  if (mediaId.value === null) return;
  busy.value = true;
  try {
    const updated = await api.markPublishStatus(
      mediaId.value,
      platform.value,
      account.value,
      status,
      form.value.note,
    );
    form.value = { ...updated };
    plans.value = await api.listPublishPlans(mediaId.value);
    await loadAccounts();
    message.success(`已标记为「${statusLabels[status]}」`);
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

async function exportPackage() {
  if (mediaId.value === null) {
    message.warning("请先选择素材");
    return;
  }
  if (!outRoot.value.trim()) {
    message.warning("请先选择发布包输出目录");
    return;
  }
  if (segmentCount.value === 0) {
    message.warning("这个素材还没有成功的切片");
    return;
  }
  busy.value = true;
  report.value = null;
  try {
    report.value = await api.exportPublishPackage(
      mediaId.value,
      platform.value,
      account.value,
      outRoot.value.trim(),
    );
    if (report.value.warnings.length > 0) {
      message.warning(`已生成，有 ${report.value.warnings.length} 条提示`);
    } else {
      message.success("发布包已生成");
    }
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

watch(mediaId, async () => {
  report.value = null;
  await refreshSegments();
  await loadPlan();
});

watch(platform, () => {
  report.value = null;
  void loadPlan();
});

watch(account, () => {
  report.value = null;
  void loadPlan();
});

onMounted(async () => {
  try {
    platforms.value = await api.listPlatforms();
  } catch (e) {
    message.error((e as Error).message);
  }
  if (store.media.length > 0) mediaId.value = store.media[0].id;
  await refreshSegments();
  await loadAccounts();
  await loadPlan();
});
</script>

<template>
  <div v-if="store.media.length === 0" class="dd-card">
    <div class="dd-card-title">没有可发布的素材</div>
    <div class="dd-card-desc">先到「素材库」导入视频并完成切片。</div>
  </div>

  <template v-else>
    <div class="dd-card">
      <div class="dd-row" style="justify-content: space-between">
        <div>
          <div class="dd-card-title">发布设置</div>
          <div class="dd-card-desc">
            这些内容只存在本地，用于生成发布包和记账；DramaDist 不会替你上传。
          </div>
        </div>
        <n-space :size="8">
          <n-tag size="small" :bordered="false">
            可用切片 {{ segmentCount }}
          </n-tag>
          <n-tag size="small" :type="statusTypes[form.status] ?? 'default'" :bordered="false">
            {{ statusLabels[form.status] ?? form.status }}
          </n-tag>
        </n-space>
      </div>

      <div class="dd-row" style="margin-bottom: 10px">
        <span class="dd-muted" style="width: 64px">素材</span>
        <n-select v-model:value="mediaId" :options="mediaOptions" style="width: 320px" />
        <span class="dd-muted" style="margin-left: 12px">平台</span>
        <n-select v-model:value="platform" :options="platformOptions" style="width: 200px" />
        <span class="dd-muted" style="margin-left: 12px">账号</span>
        <n-select
          v-model:value="account"
          :options="accountOptions"
          filterable
          tag
          style="width: 180px"
          placeholder="账号名"
        />
      </div>

      <div class="dd-row" style="margin-bottom: 10px">
        <span class="dd-muted" style="width: 64px">标题</span>
        <n-input
          v-model:value="form.title"
          size="small"
          style="width: 520px"
          placeholder="发布用的标题；导出时会自动给每个片段加（序号/总数）"
        />
      </div>

      <div class="dd-row" style="margin-bottom: 10px; align-items: flex-start">
        <span class="dd-muted" style="width: 64px; line-height: 28px">简介</span>
        <n-input
          v-model:value="form.description"
          type="textarea"
          size="small"
          style="width: 520px"
          :autosize="{ minRows: 2, maxRows: 4 }"
          placeholder="发布用的简介"
        />
      </div>

      <div class="dd-row" style="margin-bottom: 10px">
        <span class="dd-muted" style="width: 64px">标签</span>
        <n-input
          v-model:value="form.tags"
          size="small"
          style="width: 380px"
          placeholder="逗号或空格分隔，例如：短剧, 推荐, 逆袭"
        />
        <span class="dd-muted"># 会自动去掉并去重</span>
      </div>

      <div class="dd-row" style="margin-bottom: 10px">
        <span class="dd-muted" style="width: 64px">封面</span>
        <n-input
          v-model:value="form.coverPath"
          size="small"
          style="width: 460px"
          placeholder="留空则导出时从视频抽一帧"
        />
        <n-button size="small" @click="pickCover">选择</n-button>
      </div>

      <div class="dd-row" style="margin-bottom: 14px">
        <span class="dd-muted" style="width: 64px">备注</span>
        <n-input
          v-model:value="form.note"
          size="small"
          style="width: 460px"
          placeholder="记账用，例如：已投 3 条，播放 12 万"
        />
      </div>

      <n-space>
        <n-button size="small" type="primary" :loading="busy" @click="save">保存设置</n-button>
        <n-button size="small" :loading="busy" @click="mark('ready')">标记待发布</n-button>
        <n-button size="small" :loading="busy" @click="mark('published')">标记已发布</n-button>
        <n-button size="small" :loading="busy" @click="mark('failed')">标记失败</n-button>
      </n-space>
    </div>

    <div class="dd-card">
      <div class="dd-card-title">导出发布包</div>
      <div class="dd-card-desc">
        把切片、字幕、封面、标题、简介、标签整理成一个可以直接上传的目录。**只在本地写文件。**
      </div>

      <div class="dd-row" style="margin-bottom: 12px">
        <span class="dd-muted" style="width: 64px">输出到</span>
        <n-input v-model:value="outRoot" size="small" style="width: 460px" placeholder="发布包输出目录" />
        <n-button size="small" @click="pickOutRoot">选择</n-button>
        <n-button
          size="small"
          type="primary"
          :loading="busy"
          :disabled="segmentCount === 0"
          @click="exportPackage"
        >
          生成发布包
        </n-button>
      </div>

      <div class="dd-muted" style="font-size: 12px">
        目录结构：<span class="dd-mono">&lt;输出目录&gt;/&lt;平台&gt;/&lt;素材名&gt;/</span>
        ，内含
        <span class="dd-mono">clips/</span>、
        <span class="dd-mono">subtitles/</span>、
        <span class="dd-mono">cover.jpg</span>、
        <span class="dd-mono">metadata.json</span>、
        <span class="dd-mono">README.txt</span>
      </div>
    </div>

    <div v-if="report" class="dd-card">
      <div class="dd-card-title">发布包已生成</div>
      <table class="dd-table">
        <tbody>
          <tr>
            <th style="width: 110px">目录</th>
            <td class="dd-wrap dd-mono">{{ report.outDir }}</td>
          </tr>
          <tr>
            <th>内容</th>
            <td>
              {{ report.clipCount }} 个片段 · {{ report.subtitleCount }} 份字幕 ·
              封面 {{ report.coverPath ? "已生成" : "未生成" }}
            </td>
          </tr>
          <tr>
            <th>说明</th>
            <td class="dd-wrap dd-mono">{{ report.readmePath }}</td>
          </tr>
        </tbody>
      </table>
      <n-alert
        v-if="report.warnings.length > 0"
        type="warning"
        style="margin-top: 12px"
        title="提示"
      >
        <ul style="margin: 0; padding-left: 18px">
          <li v-for="(w, i) in report.warnings" :key="i">{{ w }}</li>
        </ul>
      </n-alert>
    </div>

    <div v-if="plans.length > 0" class="dd-card">
      <div class="dd-card-title">本素材的发布台账</div>
      <table class="dd-table">
        <thead>
          <tr>
            <th>平台</th>
            <th>标题</th>
            <th style="width: 90px">状态</th>
            <th>备注</th>
            <th style="width: 150px">更新时间</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="p in plans" :key="p.id">
            <td>
              {{ platforms.find((x) => x.key === p.platform)?.label ?? p.platform }}
            </td>
            <td class="dd-wrap">{{ p.title }}</td>
            <td>
              <n-tag size="tiny" :type="statusTypes[p.status] ?? 'default'" :bordered="false">
                {{ statusLabels[p.status] ?? p.status }}
              </n-tag>
            </td>
            <td class="dd-wrap dd-muted">{{ p.note || "—" }}</td>
            <td class="dd-muted">{{ p.updatedAt }}</td>
          </tr>
        </tbody>
      </table>
    </div>

    <div v-if="accounts.length > 0" class="dd-card">
      <div class="dd-card-title">账号台账总览</div>
      <div class="dd-card-desc">
        每个「平台 × 账号」的开发现状。切换上面的账号，下面的设置与台账会跟着切。
      </div>
      <table class="dd-table">
        <thead>
          <tr>
            <th style="width: 130px">平台</th>
            <th style="width: 160px">账号</th>
            <th style="width: 70px">合计</th>
            <th style="width: 70px">草稿</th>
            <th style="width: 80px">待发布</th>
            <th style="width: 80px">已发布</th>
            <th style="width: 70px">失败</th>
            <th>最近更新</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="a in accounts" :key="`${a.platform}-${a.account}`">
            <td>{{ a.platformLabel }}</td>
            <td
              style="cursor: pointer"
              @click="
                platform = a.platform;
                account = a.account;
              "
            >
              <n-tag size="tiny" :bordered="false">{{ a.account }}</n-tag>
            </td>
            <td>{{ a.total }}</td>
            <td class="dd-muted">{{ a.draft }}</td>
            <td class="dd-muted">{{ a.ready }}</td>
            <td>{{ a.published }}</td>
            <td :style="a.failed > 0 ? 'color: #d03050' : ''">{{ a.failed }}</td>
            <td class="dd-muted">{{ a.lastUpdated }}</td>
          </tr>
        </tbody>
      </table>
    </div>

    <n-alert v-if="currentPlatform" type="info" :title="`关于${currentPlatform.label}`">
      {{ currentPlatform.note }}。
      <template v-if="!currentPlatform.hasPublicApi">
        因此本工具不提供自动上传，只能帮你把发布包整理好，由你人工上传。
      </template>
      <template v-else>
        拿到资质后可以在这里接入官方接口；在那之前，发布包 + 人工上传同样可用。
      </template>
    </n-alert>

    <n-alert type="warning" title="使用前提" style="margin-top: 12px">
      请确认你对分发的内容拥有相应权利（例如已获得短剧方的切片推广授权）。
      平台规则与著作权责任由使用者承担。
    </n-alert>
  </template>
</template>
