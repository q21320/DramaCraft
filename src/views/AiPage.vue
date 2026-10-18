/**
 * AI 服务配置页。
 *
 * 单独一页的理由：Key、Base URL、模型散在各个功能页里，
 * 用户根本不知道自己配了哪些、哪些在用。原软件把「火山 API」做成独立一页，
 * 那个做法是对的。
 *
 * 界面上的几条硬规矩：
 * 1. **Key 输入框永远不回显真 Key**。占位符显示打码值，留空 = 不改动。
 * 2. **默认选中火山方舟**。它是内置预设里唯一填个 Key 就能跑的。
 * 3. **配好就能用**。不再设「用途开关」：服务商启用 + 有 Key + 有 Base URL，
 *    需要 AI 的功能就直接走 AI；没配就退回本地处理。
 * 4. **云端字幕的配置也在这页** —— 它本质就是「另一个外部服务」，
 *    放设置页会让人找不到。
 * 5. **试改剧名 / 简介默认收起**：它是偶发的验证动作，不该天天占版面。
 */
<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import {
  NAlert,
  NButton,
  NCheckbox,
  NInput,
  NSelect,
  NSpace,
  NTag,
  useMessage,
} from "naive-ui";
import { open } from "@tauri-apps/plugin-dialog";
import { api } from "@/api";
import { useAppStore } from "@/stores/app";
import type { AiConfigView, AiModelInfo, AiProvider, CloudTestResult, RewriteOutcome } from "@/types";

const store = useAppStore();
const message = useMessage();

const loading = ref(false);
const saving = ref(false);
const testing = ref<string | null>(null);
/** 每个服务商的测试结果 */
const testResults = ref<Record<string, { ok: boolean; message: string }>>({});

/** 界面上正在编辑的副本 */
const providers = ref<AiProvider[]>([]);
const active = ref("");
const warnings = ref<string[]>([]);

/** 当前选中的服务商（只有它的表单会展开，避免三份表单平铺） */
const activeProvider = computed<AiProvider | null>(
  () => providers.value.find((p) => p.id === active.value) ?? null,
);

/**
 * 模板用的表单对象：永远是对象，空选时指向占位。
 * 不直接用 `activeProvider` —— 模板里的 `v-if` 收窄对 computed 不生效，
 * 会在 v-model 与索引表达式上报「可能为 null」。
 */
const EMPTY_PROVIDER: AiProvider = {
  id: "",
  label: "",
  enabled: false,
  baseUrl: "",
  textModel: "",
  imageModel: "",
  apiKey: "",
  hasKey: false,
  keyHint: "",
};
const form = computed<AiProvider>(() => activeProvider.value ?? EMPTY_PROVIDER);

/** 当前服务商的测试结果（同样避免模板里出现 null） */
const testResult = computed(
  () => testResults.value[active.value] ?? { ok: false, message: "" },
);
const hasTestResult = computed(() => Boolean(testResults.value[active.value]));

/** 每个服务商对应的 key 输入（空 = 不改动） */
const keyDrafts = ref<Record<string, string>>({});

/** 当前服务商输入框里刚填、还没保存的 Key */
const keyDraft = computed(() => (keyDrafts.value[form.value.id] ?? "").trim());
/** 能不能测/列模型：库里存过 Key，或者输入框里刚填了一个 */
const canUseKey = computed(() => Boolean(form.value.hasKey) || keyDraft.value !== "");

// ---- 云端字幕 ----
// apiKey 留空表示「不改动已保存的 Key」——
// 前端永远拿不到原 Key，只能看到打码后的提示。
const cloudForm = ref({ baseUrl: "", apiKey: "", model: "whisper-1", language: "zh" });
const cloudSaving = ref(false);
const cloudTesting = ref(false);
const cloudResult = ref<CloudTestResult | null>(null);
const cloudLanguages = [
  { label: "中文", value: "zh" },
  { label: "英文", value: "en" },
  { label: "自动", value: "auto" },
];

// ---- 试改剧名/简介 ----
const rwTitle = ref("");
const rwSynopsis = ref("");
const rwOutDir = ref("");
const rwLoading = ref(false);
const rwResult = ref<RewriteOutcome | null>(null);

async function pickOutDir() {
  const p = await open({ directory: true, multiple: false, title: "选择写入目录" });
  if (typeof p === "string") rwOutDir.value = p;
}

async function doRewrite() {
  if (!rwSynopsis.value.trim()) {
    message.warning("先填原简介 —— 改写要有素材");
    return;
  }
  rwLoading.value = true;
  try {
    rwResult.value = await api.aiRewrite(
      rwTitle.value,
      rwSynopsis.value,
      rwOutDir.value.trim() || null,
    );
    message.success(`新剧名：${rwResult.value.newTitle}`);
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    rwLoading.value = false;
  }
}

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

/** 选一张服务商卡片：设为当前使用，并顺手勾上「启用」。 */
function pickProvider(p: AiProvider) {
  active.value = p.id;
  p.enabled = true;
}

async function save() {
  saving.value = true;
  try {
    const payload = providers.value.map((p) => ({
      id: p.id,
      label: p.label,
      // 当前使用的服务商必须启用，否则会出现「选了却用不了」的怪状态
      enabled: p.id === active.value ? true : p.enabled,
      baseUrl: p.baseUrl,
      textModel: p.textModel,
      imageModel: p.imageModel,
      // 空串 = 后端保持原 Key 不变
      apiKey: keyDrafts.value[p.id] ?? "",
    }));
    applyConfig(await api.saveAiConfig(payload, active.value));
    message.success("已保存");
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    saving.value = false;
  }
}

async function test(p: AiProvider) {
  testing.value = p.id;
  try {
    // 把界面上刚填的 Base URL / Key 一起送过去 —— 用户填完就点「测试连接」
    // 是常态，不该要求他先保存；Key 为空时后端仍用已存的那个。
    const typedKey = (keyDrafts.value[p.id] ?? "").trim();
    const r = await api.aiTest(p.id, p.baseUrl, typedKey);
    const note = typedKey ? " · 用的是还没保存的 Key" : "";
    testResults.value[p.id] = {
      ok: r.ok,
      message: `${r.message}（${r.elapsedMs} ms）${note}`,
    };
    if (r.ok) message.success(`「${p.id}」连接正常`);
    else message.warning(r.message);
  } catch (e) {
    testResults.value[p.id] = { ok: false, message: (e as Error).message };
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

/**
 * 拉一次账号里的模型列表。
 *
 * 为什么值得做：用户填的模型名常常是错的 ——
 * 比如写 `Doubao-Seedream-5.0-flash`，而真名是 `doubao-seedream-5-0-260128`；
 * 或者名字对了但账号没开通（调用时返 `ModelNotOpen`）。
 * 与其让他一次次试错，不如把列表摆出来。
 */
const models = ref<Record<string, AiModelInfo[]>>({});
const loadingModels = ref<string | null>(null);
/** 当前服务商的模型列表（"> 0" 才展示；避免模板里出现 null） */
const modelList = computed<AiModelInfo[]>(() => models.value[active.value] ?? []);
/** 刚点过哪个模型（给一点反馈） */
const pickedModel = ref("");

async function listModels(p: AiProvider) {
  loadingModels.value = p.id;
  try {
    // 同理：测/列都用界面上的值，不要求先保存
    const list = await api.aiListModels(p.id, p.baseUrl, (keyDrafts.value[p.id] ?? "").trim());
    models.value = { ...models.value, [p.id]: list };
    if (list.length === 0) {
      message.warning("账号里一个模型都没有 —— 先去火山方舟控制台开通");
    } else {
      message.success(`拿到 ${list.length} 个模型`);
    }
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    loadingModels.value = null;
  }
}

/** 点列表里的模型 → 填进输入框。填哪个由「视觉 / 语言」决定。 */
function useModel(p: AiProvider, modelId: string, kind: "text" | "image") {
  const i = providers.value.findIndex((x) => x.id === p.id);
  if (i < 0) return;
  providers.value[i] = {
    ...providers.value[i],
    ...(kind === "text" ? { textModel: modelId } : { imageModel: modelId }),
  };
  pickedModel.value = modelId;
  message.success(`已填入${kind === "text" ? "语言" : "生图"}模型：${modelId}（记得点保存）`);
}

// ---- 云端字幕 ----

function syncCloudForm() {
  const cfg = store.cloud;
  cloudForm.value = {
    baseUrl: cfg?.baseUrl ?? "",
    apiKey: "",
    model: cfg?.model ?? "whisper-1",
    language: cfg?.language ?? "zh",
  };
}

async function saveCloud() {
  cloudSaving.value = true;
  cloudResult.value = null;
  try {
    // 留空 → 传 null，表示保留原 Key
    const apiKey = cloudForm.value.apiKey.trim();
    store.cloud = await api.saveCloudConfig(
      cloudForm.value.baseUrl,
      apiKey === "" ? null : apiKey,
      cloudForm.value.model,
      cloudForm.value.language,
    );
    syncCloudForm();
    message.success("云端字幕配置已保存");
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    cloudSaving.value = false;
  }
}

async function testCloud() {
  cloudTesting.value = true;
  cloudResult.value = null;
  try {
    cloudResult.value = await api.cloudTest();
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    cloudTesting.value = false;
  }
}

onMounted(() => {
  void load();
  // 直接落地在 /ai 时 bootstrap 可能还没回来，先补拉一次云端配置
  void (async () => {
    if (!store.cloud) {
      try {
        await store.reloadCloud();
      } catch {
        // 读不出来当没配过
      }
    }
    syncCloudForm();
  })();
});
</script>

<template>
  <div class="dd-card ai-head">
    <div>
      <div class="ai-title">AI 服务</div>
      <div class="dd-muted ai-sub">
        所有外部服务集中在这里：选服务商 → 填 Key，配好就能用。
        <b>Key 只存在本机</b>，界面上只显示打码后的样子。
      </div>
    </div>
    <n-space :size="8">
      <n-button size="small" :loading="loading" @click="load">重新载入</n-button>
      <n-button size="small" type="primary" :loading="saving || loading" @click="save">
        保存配置
      </n-button>
    </n-space>
  </div>

  <n-alert v-if="warnings.length > 0" type="warning" title="配置还差一点" style="margin-bottom: 14px">
    <ul style="margin: 0; padding-left: 18px">
      <li v-for="(w, i) in warnings" :key="i" class="dd-wrap">{{ w }}</li>
    </ul>
  </n-alert>

  <div class="ai-grid">
    <!-- 左列：服务商 + 表单 -->
    <div class="dd-card ai-card">
      <div class="ai-card-head">
        <span class="ai-card-title">服务商</span>
        <span class="dd-muted">点一张卡片切换「当前使用」</span>
      </div>

      <div class="ai-providers">
        <button
          v-for="p in providers"
          :key="p.id"
          type="button"
          class="ai-provider"
          :class="{ on: active === p.id }"
          @click="pickProvider(p)"
        >
          <div class="ai-provider-name">
            <span>{{ p.label }}</span>
            <n-tag v-if="active === p.id" size="tiny" type="primary" :bordered="false">
              当前使用
            </n-tag>
          </div>
          <div class="ai-provider-meta">
            <n-tag v-if="p.hasKey" size="tiny" type="success" :bordered="false">已填 Key</n-tag>
            <n-tag v-else size="tiny" :bordered="false">未填 Key</n-tag>
            <span v-if="!p.enabled" class="dd-muted">未启用</span>
          </div>
        </button>
      </div>

      <div v-if="activeProvider" class="ai-form">
        <div class="ai-form-head">
          <n-checkbox v-model:checked="form.enabled">启用这个服务商</n-checkbox>
          <n-space :size="8">
            <n-button
              size="tiny"
              :loading="testing === form.id"
              @click="test(form)"
            >
              测试连接
            </n-button>
            <n-button
              size="tiny"
              :loading="loadingModels === form.id"
              :disabled="!canUseKey"
              @click="listModels(form)"
            >
              可用模型
            </n-button>
            <n-button size="tiny" quaternary @click="resetPreset(form.id)">
              恢复预设
            </n-button>
          </n-space>
        </div>

        <div class="dd-field">
          <span class="dd-label">API Key</span>
          <n-input
            :value="keyDrafts[form.id] ?? ''"
            size="small"
            type="password"
            show-password-on="click"
            :placeholder="form.hasKey ? form.keyHint + '（留空 = 不改动）' : '还没填'"
            @update:value="(v: string) => (keyDrafts[form.id] = v)"
          />
        </div>
        <div class="dd-field">
          <span class="dd-label">Base URL</span>
          <n-input v-model:value="form.baseUrl" size="small" placeholder="https://..." />
        </div>
        <div class="dd-field">
          <span class="dd-label">语言模型</span>
          <n-input
            v-model:value="form.textModel"
            size="small"
            placeholder="改写剧名 / 简介用"
          />
        </div>
        <div class="dd-field">
          <span class="dd-label">生图模型</span>
          <n-input
            v-model:value="form.imageModel"
            size="small"
            placeholder="封面生成用"
          />
        </div>

        <div v-if="hasTestResult" class="dd-test" :class="testResult.ok ? 'ok' : 'bad'">
          {{ testResult.message }}
        </div>

        <!-- 模型列表：点了就填进对应的输入框 -->
        <div v-if="modelList.length > 0" class="dd-models">
          <div class="dd-muted" style="margin-bottom: 6px">
            账号能看到的 {{ modelList.length }} 个模型（
            <b>目录里有 ≠ 开通了</b> —— 没开通的调用时会报 ModelNotOpen）。
            左键填「语言模型」，右键填「生图模型」：
          </div>
          <div class="dd-model-list">
            <n-tag
              v-for="m in modelList"
              :key="m.id"
              size="tiny"
              :type="pickedModel === m.id ? 'primary' : m.available ? 'success' : 'warning'"
              :bordered="false"
              style="cursor: pointer; margin: 0 6px 6px 0"
              @click="useModel(form, m.id, 'text')"
              @contextmenu.prevent="useModel(form, m.id, 'image')"
            >
              {{ m.id }}<span v-if="m.state">{{ " · " + m.state }}</span>
            </n-tag>
          </div>
        </div>

        <div class="ai-form-foot">
          「测试连接」只发一次廉价探测（列模型），不会调用生图接口，不产生费用。
        </div>
      </div>
      <div v-else class="ai-empty">
        没选服务商 —— 需要 AI 的功能会退回本地处理。点上面任意一张卡片即可选中。
      </div>
    </div>

    <!-- 右列：云端字幕 -->
    <div class="ai-side">
      <div class="dd-card ai-card">
        <div class="ai-card-head">
          <span class="ai-card-title">云端字幕</span>
          <n-tag
            size="tiny"
            :type="store.cloud?.hasKey ? 'success' : 'default'"
            :bordered="false"
          >
            {{ store.cloud?.hasKey ? "已配置" : "未配置" }}
          </n-tag>
        </div>
        <div class="dd-card-desc" style="margin-bottom: 8px">
          本地 whisper 慢但免费；云端按量计费、几十秒出结果。
          按「OpenAI 兼容」的接口形状实现，填任何一家都行。
        </div>

        <div class="dd-field">
          <span class="dd-label">Base URL</span>
          <n-input
            v-model:value="cloudForm.baseUrl"
            size="small"
            placeholder="https://api.openai.com/v1"
          />
        </div>
        <div class="dd-field">
          <span class="dd-label">API Key</span>
          <n-input
            v-model:value="cloudForm.apiKey"
            type="password"
            show-password-on="click"
            size="small"
            :placeholder="store.cloud?.hasKey ? `已保存（${store.cloud.keyHint}），留空表示不改动` : '粘贴你的 Key'"
          />
        </div>
        <div class="dd-field">
          <span class="dd-label">模型</span>
          <n-input v-model:value="cloudForm.model" size="small" placeholder="whisper-1" />
          <n-select
            v-model:value="cloudForm.language"
            size="small"
            style="width: 96px; flex: none"
            :options="cloudLanguages"
          />
        </div>

        <n-space style="margin-top: 10px">
          <n-button size="tiny" type="primary" :loading="cloudSaving" @click="saveCloud">
            保存
          </n-button>
          <n-button
            size="tiny"
            :loading="cloudTesting"
            :disabled="!store.cloud?.hasKey"
            @click="testCloud"
          >
            测试连接
          </n-button>
        </n-space>

        <n-alert
          v-if="cloudResult"
          :type="cloudResult.ok ? 'success' : 'error'"
          style="margin-top: 10px"
          :title="cloudResult.ok ? '连接正常' : '连接失败'"
        >
          <div class="dd-wrap">{{ cloudResult.detail }}</div>
          <div class="dd-mono dd-muted" style="margin-top: 6px">{{ cloudResult.endpoint }}</div>
        </n-alert>

        <n-alert
          v-if="store.cloud && store.cloud.warnings.length > 0"
          type="warning"
          title="需要注意"
          style="margin-top: 10px"
        >
          <ul style="margin: 0; padding-left: 18px">
            <li v-for="(w, i) in store.cloud.warnings" :key="i" class="dd-wrap">{{ w }}</li>
          </ul>
        </n-alert>
      </div>
    </div>
  </div>

  <!--
    试改是偶发动作（配好 Key 后验证一次），默认收起。
    用原生 details：不引组件、默认合上、点击就展开。
  -->
  <details class="dd-card ai-adv">
    <summary>
      <span class="ai-card-title">试改剧名 / 简介</span>
      <span class="dd-muted">配好 Key 后可以试跑一次（会调用两次语言模型，产生费用）</span>
    </summary>
    <div class="ai-adv-body">
      <div class="dd-card-desc">
        先有原剧名和原简介，AI 重写简介，再<b>按新简介</b>起新剧名。
        顺序不能反 —— 反过来的话剧名是根据旧简介起的，两者对不上。
        结果可以只看看，也可以写进某个目录（会在那里生成
        <code>剧名.txt</code> / <code>详细简介.txt</code>，并追加一行台账）。
      </div>

      <div class="dd-field">
        <span class="dd-label">原剧名</span>
        <n-input v-model:value="rwTitle" size="small" placeholder="从源目录扫出来的剧名" />
      </div>
      <div class="dd-field" style="align-items: flex-start">
        <span class="dd-label" style="padding-top: 5px">原简介</span>
        <n-input
          v-model:value="rwSynopsis"
          type="textarea"
          size="small"
          :rows="4"
          placeholder="贴进 intro.txt 里的内容"
        />
      </div>
      <div class="dd-field">
        <span class="dd-label">写入目录</span>
        <n-input v-model:value="rwOutDir" size="small" placeholder="留空则只在界面上看，不落盘" />
        <n-button size="small" @click="pickOutDir">选择</n-button>
      </div>

      <n-space style="margin-top: 10px">
        <n-button type="primary" size="small" :loading="rwLoading" @click="doRewrite">改写</n-button>
      </n-space>

      <div v-if="rwResult" class="dd-rw">
        <div class="dd-rw-row">
          <span class="dd-muted">新剧名</span>
          <b>{{ rwResult.newTitle }}</b>
        </div>
        <div class="dd-rw-row" style="align-items: flex-start">
          <span class="dd-muted">新简介</span>
          <span class="dd-wrap">{{ rwResult.newSynopsis }}</span>
        </div>
        <div v-if="rwResult.saved.length > 0" class="dd-rw-row" style="align-items: flex-start">
          <span class="dd-muted">已写入</span>
          <span class="dd-wrap" style="font-size: 12px">{{ rwResult.saved.join("、") }}</span>
        </div>
      </div>
    </div>
  </details>

  <div class="dd-card">
    <div class="dd-card-title">说明</div>
    <div class="dd-card-desc">
      <p style="margin: 0 0 6px">
        <b>封面生成走哪条路？</b>
        配好服务商 + 填了生图模型，就走 <b>AI 图生图</b>
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
.ai-head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
  flex-wrap: wrap;
}
.ai-title {
  font-size: 16px;
  font-weight: 600;
  margin-bottom: 4px;
}
.ai-sub {
  font-size: 13px;
  line-height: 1.6;
  max-width: 720px;
}

.ai-grid {
  display: grid;
  grid-template-columns: minmax(0, 1.2fr) minmax(320px, 0.9fr);
  gap: var(--dd-gap, 14px);
  align-items: start;
}
.ai-side {
  display: flex;
  flex-direction: column;
  gap: var(--dd-gap, 14px);
}
.ai-grid .ai-card,
.ai-side .ai-card {
  margin-bottom: 0;
}

.ai-card-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  flex-wrap: wrap;
  margin-bottom: 10px;
}
.ai-card-title {
  font-size: 14px;
  font-weight: 600;
}

/* 服务商选择：三张可点的卡片 */
.ai-providers {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
  gap: 8px;
  margin-bottom: 12px;
}
.ai-provider {
  text-align: left;
  padding: 9px 10px;
  border-radius: 8px;
  border: 1px solid rgba(128, 128, 128, 0.25);
  background: transparent;
  color: inherit;
  font: inherit;
  cursor: pointer;
  transition: border-color 0.15s, background 0.15s;
}
.ai-provider:hover {
  border-color: rgba(24, 160, 88, 0.55);
}
.ai-provider.on {
  border-color: #18a058;
  background: rgba(24, 160, 88, 0.08);
}
.ai-provider-name {
  display: flex;
  align-items: center;
  gap: 6px;
  flex-wrap: wrap;
  font-size: 13px;
  font-weight: 600;
  margin-bottom: 4px;
}
.ai-provider-meta {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 12px;
}

.ai-form {
  border-top: 1px solid rgba(128, 128, 128, 0.18);
  padding-top: 10px;
}
.ai-form-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  flex-wrap: wrap;
  margin-bottom: 4px;
}
.ai-form-foot {
  margin-top: 10px;
  font-size: 12px;
  color: var(--dd-muted);
}
.ai-empty {
  font-size: 13px;
  color: var(--dd-muted);
  padding: 10px 0 2px;
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

/* 用途开关已移除：配好服务商就直接走 AI */

.dd-models {
  margin: 8px 0 4px;
  padding: 8px 10px;
  border: 1px solid rgba(128, 128, 128, 0.18);
  border-radius: 6px;
}
.dd-model-list {
  max-height: 190px;
  overflow: auto;
  margin-bottom: 4px;
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

/* 试改：默认收起，点标题展开 */
.ai-adv > summary {
  display: flex;
  align-items: baseline;
  gap: 10px;
  flex-wrap: wrap;
  cursor: pointer;
  list-style: none;
}
.ai-adv > summary::-webkit-details-marker {
  display: none;
}
.ai-adv > summary::before {
  content: "▸";
  font-size: 12px;
  color: var(--dd-muted);
}
.ai-adv[open] > summary::before {
  content: "▾";
}
.ai-adv-body {
  margin-top: 12px;
  border-top: 1px solid rgba(128, 128, 128, 0.18);
  padding-top: 12px;
}

.dd-rw {
  margin-top: 12px;
  border-top: 1px solid rgba(128, 128, 128, 0.18);
  padding-top: 10px;
}
.dd-rw-row {
  display: flex;
  gap: 10px;
  margin-bottom: 6px;
}
.dd-rw-row .dd-muted {
  width: 52px;
  flex: none;
}

@media (max-width: 980px) {
  .ai-grid {
    grid-template-columns: 1fr;
  }
}
</style>
