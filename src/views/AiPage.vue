/**
 * AI 服务配置页。
 *
 * 单独一页的理由：Key、Base URL、模型、用途开关散在各个功能页里，
 * 用户根本不知道自己配了哪些、哪些在用。原软件把「火山 API」做成独立一页，
 * 那个做法是对的。
 *
 * 两条界面上的硬规矩：
 * 1. **Key 输入框永远不回显真 Key**。占位符显示打码值，留空 = 不改动。
 * 2. **用途开关默认全关**。用 AI 会产生费用、产出还带标识义务，
 *    得用户自己点头。
 */
<script setup lang="ts">
import { onMounted, ref } from "vue";
import {
  NAlert,
  NButton,
  NCheckbox,
  NInput,
  NSpace,
  NTag,
  useMessage,
} from "naive-ui";
import { api } from "@/api";
import type { AiConfigView, AiProvider, AiUsage } from "@/types";

const message = useMessage();

const loading = ref(false);
const saving = ref(false);
const testing = ref<string | null>(null);
/** 每个服务商的测试结果 */
const testResults = ref<Record<string, { ok: boolean; message: string }>>({});

/** 界面上正在编辑的副本 */
const providers = ref<AiProvider[]>([]);
const active = ref("");
const usage = ref<AiUsage>({
  coverGeneration: false,
  titleRewrite: false,
  subtitleCloud: false,
});
const warnings = ref<string[]>([]);

/** 每个服务商对应的 key 输入（空 = 不改动） */
const keyDrafts = ref<Record<string, string>>({});

const usageItems: { key: keyof AiUsage; label: string; hint: string }[] = [
  {
    key: "coverGeneration",
    label: "封面生成",
    hint: "用图生图重做封面：擦掉原封面上的旧剧名、写上当前剧名",
  },
  {
    key: "titleRewrite",
    label: "剧名 / 简介改写",
    hint: "用语言模型重写剧名与简介",
  },
  {
    key: "subtitleCloud",
    label: "云端字幕",
    hint: "把语音识别交给云端（本地 ASR 已内置，一般不需要）",
  },
];

function applyConfig(cfg: AiConfigView) {
  providers.value = cfg.providers.map((p) => ({
    id: p.id,
    label: p.label,
    enabled: p.enabled,
    baseUrl: p.baseUrl,
    textModel: p.textModel,
    imageModel: p.imageModel,
    apiKey: "",
    hasKey: p.hasKey,
    keyHint: p.keyHint,
  }));
  active.value = cfg.active;
  usage.value = { ...cfg.usage };
  warnings.value = cfg.warnings;
  keyDrafts.value = {};
}

async function load() {
  loading.value = true;
  try {
    applyConfig(await api.aiConfig());
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    loading.value = false;
  }
}

async function save() {
  saving.value = true;
  try {
    const payload = providers.value.map((p) => ({
      id: p.id,
      label: p.label,
      enabled: p.enabled,
      baseUrl: p.baseUrl,
      textModel: p.textModel,
      imageModel: p.imageModel,
      // 空串 = 后端保持原 Key 不变
      apiKey: keyDrafts.value[p.id] ?? "",
    }));
    applyConfig(await api.saveAiConfig(payload, active.value, usage.value));
    message.success("已保存");
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    saving.value = false;
  }
}

async function test(id: string) {
  testing.value = id;
  try {
    const r = await api.aiTest(id);
    testResults.value[id] = { ok: r.ok, message: `${r.message}（${r.elapsedMs} ms）` };
    if (r.ok) message.success(`「${id}」连接正常`);
    else message.warning(r.message);
  } catch (e) {
    testResults.value[id] = { ok: false, message: (e as Error).message };
  } finally {
    testing.value = null;
  }
}

async function resetPreset(id: string) {
  try {
    const p = await api.aiResetPreset(id);
    const i = providers.value.findIndex((x) => x.id === id);
    if (i >= 0) {
      providers.value[i] = {
        ...providers.value[i],
        baseUrl: p.baseUrl,
        textModel: p.textModel,
        imageModel: p.imageModel,
      };
    }
    message.success("已恢复内置预设");
  } catch (e) {
    message.error((e as Error).message);
  }
}

onMounted(load);
</script>

<template>
  <div class="dd-card">
    <div class="dd-card-title">AI 服务</div>
    <div class="dd-card-desc">
      所有外部服务的配置集中在这里：API Key、接口地址、模型名、以及哪些功能走 AI。
      <b>Key 只存在本机</b>，界面上只显示打码后的样子。
    </div>

    <n-alert v-if="warnings.length > 0" type="warning" title="配置还差一点" style="margin-bottom: 14px">
      <ul style="margin: 0; padding-left: 18px">
        <li v-for="(w, i) in warnings" :key="i" class="dd-wrap">{{ w }}</li>
      </ul>
    </n-alert>

    <div class="dd-card-title" style="font-size: 13px; margin-bottom: 6px">当前使用</div>
    <n-space :size="8" style="margin-bottom: 4px; flex-wrap: wrap">
      <n-button
        v-for="p in providers"
        :key="p.id"
        size="small"
        :type="active === p.id ? 'primary' : 'default'"
        @click="active = active === p.id ? '' : p.id"
      >
        {{ p.label }}
      </n-button>
      <span class="dd-muted" style="align-self: center">
        {{ active ? "" : "（没选服务商 —— 需要 AI 的功能会退回本地处理）" }}
      </span>
    </n-space>

    <div class="dd-card-title" style="font-size: 13px; margin: 16px 0 6px">用途开关</div>
    <div class="dd-card-desc" style="margin-bottom: 8px">
      <b>默认全关。</b>用 AI 会产生费用，产出的内容还属于 AI 生成内容（有标识义务）——
      所以要不要用，由你决定。配了 Key 不等于要用。
    </div>
    <div v-for="item in usageItems" :key="item.key" style="margin-bottom: 8px">
      <n-checkbox
        :checked="usage[item.key]"
        @update:checked="(v: boolean) => (usage[item.key] = v)"
      >
        {{ item.label }}
      </n-checkbox>
      <div class="dd-muted" style="margin-left: 22px">{{ item.hint }}</div>
    </div>

    <div class="dd-card-title" style="font-size: 13px; margin: 18px 0 8px">服务商</div>

    <n-space vertical :size="10">
      <div v-for="p in providers" :key="p.id" class="dd-provider">
        <div class="dd-row" style="justify-content: space-between; align-items: center">
          <n-space :size="8" align="center">
            <n-checkbox v-model:checked="p.enabled">启用</n-checkbox>
            <b>{{ p.label }}</b>
            <n-tag v-if="active === p.id" size="tiny" type="primary" :bordered="false">当前使用</n-tag>
            <n-tag v-if="p.hasKey" size="tiny" type="success" :bordered="false">已填 Key</n-tag>
          </n-space>
          <n-space :size="8">
            <n-button size="tiny" :loading="testing === p.id" @click="test(p.id)">测试连接</n-button>
            <n-button size="tiny" quaternary @click="resetPreset(p.id)">恢复预设</n-button>
          </n-space>
        </div>

        <div v-if="testResults[p.id]" class="dd-test" :class="testResults[p.id].ok ? 'ok' : 'bad'">
          {{ testResults[p.id].message }}
        </div>

        <div class="dd-field">
          <span class="dd-label">API Key</span>
          <n-input
            :value="keyDrafts[p.id] ?? ''"
            size="small"
            type="password"
            show-password-on="click"
            :placeholder="p.hasKey ? p.keyHint + '（留空 = 不改动）' : '还没填'"
            @update:value="(v: string) => (keyDrafts[p.id] = v)"
          />
        </div>
        <div class="dd-field">
          <span class="dd-label">Base URL</span>
          <n-input v-model:value="p.baseUrl" size="small" placeholder="https://..." />
        </div>
        <div class="dd-field">
          <span class="dd-label">语言模型</span>
          <n-input v-model:value="p.textModel" size="small" placeholder="改写剧名/简介用" />
        </div>
        <div class="dd-field">
          <span class="dd-label">生图模型</span>
          <n-input v-model:value="p.imageModel" size="small" placeholder="封面生成用" />
        </div>
      </div>
    </n-space>

    <n-space style="margin-top: 16px">
      <n-button type="primary" size="small" :loading="saving || loading" @click="save">
        保存配置
      </n-button>
      <n-button size="small" :loading="loading" @click="load">重新载入</n-button>
      <span class="dd-muted" style="align-self: center">
        「测试连接」只发一次廉价探测，不会调用生图接口，不产生费用
      </span>
    </n-space>
  </div>

  <div class="dd-card">
    <div class="dd-card-title">说明</div>
    <div class="dd-card-desc">
      <p style="margin: 0 0 6px">
        <b>封面生成走哪条路？</b>
        配好服务商 + 填了生图模型 + 打开上面「封面生成」开关，就走 <b>AI 图生图</b>
        （能真擦掉原封面上的旧剧名）；否则走 <b>本地合成</b>——用标题条把旧字盖住再写新剧名。
        本地合成离线可用、不产生 AI 内容，代价是盖住的那块画面损失一点。
      </p>
      <p style="margin: 0">
        <b>AI 生成的封面要标识。</b>
        按《人工智能生成合成内容标识办法》，AI 生成 / 合成的内容应当显著标识 ——
        可以用流水线页的「片头 AI 生成内容标识」。
      </p>
    </div>
  </div>
</template>

<style scoped>
.dd-provider {
  border: 1px solid rgba(128, 128, 128, 0.22);
  border-radius: 6px;
  padding: 10px 12px;
}
.dd-field {
  display: flex;
  align-items: center;
  gap: 10px;
  margin-top: 7px;
}
.dd-label {
  width: 76px;
  flex: none;
  font-size: 12px;
  opacity: 0.72;
}
.dd-test {
  margin-top: 7px;
  font-size: 12px;
  padding: 4px 8px;
  border-radius: 4px;
}
.dd-test.ok {
  background: rgba(24, 160, 88, 0.12);
  color: #18a058;
}
.dd-test.bad {
  background: rgba(208, 48, 80, 0.12);
  color: #d03050;
}
</style>
