<script setup lang="ts">
import { onMounted, ref } from "vue";
import { NAlert, NButton, NInput, NSelect, NSpace, NTag, useMessage } from "naive-ui";
import { open } from "@tauri-apps/plugin-dialog";
import { api } from "@/api";
import { useAppStore } from "@/stores/app";
import type { CloudTestResult, ToolPaths } from "@/types";

const store = useAppStore();
const message = useMessage();

const draft = ref<ToolPaths>({ ffmpeg: "", ffprobe: "", python: "" });
const saving = ref(false);
const detecting = ref(false);

// 云端字幕配置。apiKey 留空表示「不改动已保存的 Key」——
// 前端永远拿不到原 Key，只能看到打码后的提示。
const cloudForm = ref({ baseUrl: "", apiKey: "", model: "whisper-1", language: "zh" });
const cloudSaving = ref(false);
const cloudTesting = ref(false);
const cloudResult = ref<CloudTestResult | null>(null);

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
    cloudForm.value.apiKey = "";
    message.success("云端配置已保存");
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

async function pick(field: keyof ToolPaths) {
  const picked = await open({
    multiple: false,
    title: `选择 ${field} 可执行文件`,
    filters: [{ name: "可执行文件", extensions: ["exe", "cmd", "bat"] }],
  });
  if (typeof picked === "string") draft.value[field] = picked;
}

async function save() {
  saving.value = true;
  try {
    await store.saveTools({ ...draft.value });
    message.success("已保存并重新检测");
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    saving.value = false;
  }
}

async function redetect() {
  detecting.value = true;
  try {
    // 只重新探测当前生效的配置，不改动已保存的值
    store.toolReports = await api.detectTools();
    message.success("检测完成");
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    detecting.value = false;
  }
}

onMounted(() => {
  draft.value = { ...store.tools };
  syncCloudForm();
});
</script>

<template>
  <div class="dd-card">
    <div class="dd-card-title">外部工具</div>
    <div class="dd-card-desc">
      留空则自动在 PATH 与常见位置查找。保存后立即重新检测可用性。
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 70px">ffmpeg</span>
      <n-input v-model:value="draft.ffmpeg" size="small" style="width: 480px" placeholder="留空自动查找" />
      <n-button size="small" @click="pick('ffmpeg')">选择</n-button>
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 70px">ffprobe</span>
      <n-input v-model:value="draft.ffprobe" size="small" style="width: 480px" placeholder="留空自动查找" />
      <n-button size="small" @click="pick('ffprobe')">选择</n-button>
    </div>

    <div class="dd-row" style="margin-bottom: 14px">
      <span class="dd-muted" style="width: 70px">Python</span>
      <n-input v-model:value="draft.python" size="small" style="width: 480px" placeholder="用于 ASR，可留空" />
      <n-button size="small" @click="pick('python')">选择</n-button>
    </div>

    <n-space>
      <n-button size="small" type="primary" :loading="saving" @click="save">保存并检测</n-button>
      <n-button size="small" :loading="detecting" @click="redetect">重新检测</n-button>
    </n-space>
  </div>

  <div class="dd-card">
    <div class="dd-card-title">可用性</div>
    <div class="dd-card-desc">检测方式是实际执行一次 <span class="dd-mono">--version</span>，不只看文件是否存在。</div>

    <table class="dd-table">
      <thead>
        <tr>
          <th style="width: 90px">工具</th>
          <th style="width: 90px">状态</th>
          <th>路径</th>
          <th>版本 / 说明</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="r in store.toolReports" :key="r.name">
          <td>{{ r.name }}</td>
          <td>
            <n-tag size="tiny" :type="r.available ? 'success' : 'error'" :bordered="false">
              {{ r.available ? "可用" : "不可用" }}
            </n-tag>
          </td>
          <td class="dd-wrap dd-mono">{{ r.path || "—" }}</td>
          <td class="dd-wrap">{{ r.version || r.detail }}</td>
        </tr>
      </tbody>
    </table>
  </div>

  <div class="dd-card">
    <div class="dd-card-title">可选运行时</div>
    <div class="dd-card-desc">
      这两项都跑在上面那个 Python 里，各自需要单独的依赖包；没装不影响其他功能。
    </div>

    <table class="dd-table">
      <thead>
        <tr>
          <th style="width: 150px">能力</th>
          <th style="width: 90px">状态</th>
          <th>说明</th>
        </tr>
      </thead>
      <tbody>
        <tr>
          <th>语音转字幕（ASR）</th>
          <td>
            <n-tag size="tiny" :type="store.asr?.available ? 'success' : 'warning'" :bordered="false">
              {{ store.asr?.available ? "就绪" : "未就绪" }}
            </n-tag>
          </td>
          <td class="dd-wrap">{{ store.asr?.detail || "—" }}</td>
        </tr>
        <tr>
          <th>硬字幕识别（OCR）</th>
          <td>
            <n-tag size="tiny" :type="store.ocr?.available ? 'success' : 'warning'" :bordered="false">
              {{ store.ocr?.available ? "就绪" : "未就绪" }}
            </n-tag>
          </td>
          <td class="dd-wrap">{{ store.ocr?.detail || "—" }}</td>
        </tr>
      </tbody>
    </table>

    <pre class="dd-mono" style="margin: 12px 0 0; padding: 10px; background: rgba(128,128,128,0.08); border-radius: 6px">pip install faster-whisper          # 语音转字幕
pip install rapidocr_onnxruntime    # 硬字幕识别
pip install pyJianYingDraft         # 剪映草稿导出</pre>
  </div>

  <div class="dd-card">
    <div class="dd-card-title">云端字幕（可选）</div>
    <div class="dd-card-desc">
      本地 whisper 慢但免费；云端按量计费、几十秒出结果。两条路可随时切换。
      这里按「OpenAI 兼容」的接口形状实现，你填自己买的任何一家服务都行。
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 84px">Base URL</span>
      <n-input
        v-model:value="cloudForm.baseUrl"
        size="small"
        style="width: 420px"
        placeholder="https://api.openai.com/v1"
      />
      <span class="dd-muted">多数服务需要带 /v1</span>
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 84px">API Key</span>
      <n-input
        v-model:value="cloudForm.apiKey"
        type="password"
        show-password-on="click"
        size="small"
        style="width: 420px"
        :placeholder="store.cloud?.hasKey ? `已保存（${store.cloud.keyHint}），留空表示不改动` : '粘贴你的 Key'"
      />
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 84px">模型</span>
      <n-input v-model:value="cloudForm.model" size="small" style="width: 220px" placeholder="whisper-1" />
      <span class="dd-muted" style="margin-left: 14px">语言</span>
      <n-select
        v-model:value="cloudForm.language"
        size="small"
        style="width: 130px"
        :options="[
          { label: '中文', value: 'zh' },
          { label: '英文', value: 'en' },
          { label: '自动', value: 'auto' },
        ]"
      />
    </div>

    <n-space style="margin-bottom: 12px">
      <n-button size="small" type="primary" :loading="cloudSaving" @click="saveCloud">保存</n-button>
      <n-button size="small" :loading="cloudTesting" :disabled="!store.cloud?.hasKey" @click="testCloud">
        测试连接
      </n-button>
    </n-space>

    <n-alert v-if="cloudResult" :type="cloudResult.ok ? 'success' : 'error'" style="margin-bottom: 12px" :title="cloudResult.ok ? '连接正常' : '连接失败'">
      <div class="dd-wrap">{{ cloudResult.detail }}</div>
      <div class="dd-mono dd-muted" style="margin-top: 6px">{{ cloudResult.endpoint }}</div>
    </n-alert>

    <n-alert v-if="store.cloud && store.cloud.warnings.length > 0" type="warning" title="需要注意">
      <ul style="margin: 0; padding-left: 18px">
        <li v-for="(w, i) in store.cloud.warnings" :key="i" class="dd-wrap">{{ w }}</li>
      </ul>
    </n-alert>
  </div>

  <div class="dd-card">
    <div class="dd-card-title">关于</div>
    <div class="dd-card-desc">纯本地运行，不上传任何素材与项目数据。</div>
    <table class="dd-table">
      <tbody>
        <tr>
          <th style="width: 120px">版本</th>
          <td>{{ store.info?.version || "—" }}</td>
        </tr>
        <tr>
          <th>数据目录</th>
          <td class="dd-wrap dd-mono">{{ store.info?.dataDir || "—" }}</td>
        </tr>
      </tbody>
    </table>

    <n-alert type="info" style="margin-top: 14px" title="合规提示">
      本项目只调用外部 ffmpeg，不内嵌其二进制。请自行选择合规构建（启用 x264/x265 的 GPL
      构建在分发时需遵守 GPL）。请勿把第三方商业软件自带的二进制、素材或模板复制进本项目。
    </n-alert>
  </div>
</template>
