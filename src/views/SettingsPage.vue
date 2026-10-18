<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import { NButton, NInput, NInputNumber, NSelect, NSpace, NTag, useMessage } from "naive-ui";
import { open } from "@tauri-apps/plugin-dialog";
import { open as openUrl } from "@tauri-apps/plugin-shell";
import { api } from "@/api";
import { useAppStore } from "@/stores/app";
import type { EmailConfigView, ToolPaths } from "@/types";

const store = useAppStore();
const message = useMessage();

const draft = ref<ToolPaths>({ ffmpeg: "", ffprobe: "", python: "" });
const saving = ref(false);
const detecting = ref(false);

// 成本报告落款。存在 `ui.cost.report` 里（和云端配置一样走 ui 状态 KV），
// 流水线排 cost 任务时读它填进 payload。
const COST_KEY = "cost.report";
const costForm = ref({ org: "", legalPerson: "", sealPath: "", signPath: "" });
const costSaving = ref(false);

// 剪映草稿的 BGM。存在 `ui.draft.audio` 里，和成本落款一样走 ui 状态 KV。
const DRAFT_AUDIO_KEY = "draft.audio";
const draftAudio = ref<{ bgmPath: string; bgmVolume: number }>({ bgmPath: "", bgmVolume: 0.35 });
const draftAudioSaving = ref(false);

async function loadDraftAudio() {
  try {
    const raw = await api.loadUiState(DRAFT_AUDIO_KEY);
    if (!raw) return;
    const v = JSON.parse(raw) as Partial<typeof draftAudio.value>;
    draftAudio.value = {
      bgmPath: v.bgmPath ?? "",
      bgmVolume: typeof v.bgmVolume === "number" ? v.bgmVolume : 0.35,
    };
  } catch {
    // 读不出来当没配过
  }
}

async function saveDraftAudio() {
  draftAudioSaving.value = true;
  try {
    await api.saveUiState(DRAFT_AUDIO_KEY, JSON.stringify(draftAudio.value));
    message.success("剪映草稿音频已保存");
  } catch (e) {
    message.error(String(e));
  } finally {
    draftAudioSaving.value = false;
  }
}

async function pickBgm() {
  const picked = await open({
    multiple: false,
    filters: [{ name: "音频", extensions: ["mp3", "wav", "m4a", "aac", "flac"] }],
  });
  if (typeof picked === "string") draftAudio.value.bgmPath = picked;
}

async function loadCostForm() {
  try {
    const raw = await api.loadUiState(COST_KEY);
    if (!raw) return;
    const v = JSON.parse(raw) as Partial<typeof costForm.value>;
    costForm.value = {
      org: v.org ?? "",
      legalPerson: v.legalPerson ?? "",
      sealPath: v.sealPath ?? "",
      signPath: v.signPath ?? "",
    };
  } catch {
    // 读不出来就当没配过，不影响其他功能
  }
}

async function saveCostForm() {
  costSaving.value = true;
  try {
    await api.saveUiState(COST_KEY, JSON.stringify(costForm.value));
    message.success("成本报告落款已保存");
  } catch (e) {
    message.error(String(e));
  } finally {
    costSaving.value = false;
  }
}

async function pickCostFile(field: "sealPath" | "signPath") {
  const picked = await open({
    multiple: false,
    filters: [{ name: "图片", extensions: ["png", "jpg", "jpeg", "webp"] }],
  });
  if (typeof picked === "string") costForm.value[field] = picked;
}

// 下载目录：一次选定，从「视频」页下载的所有剧都放这里。
// 没设置过时，第一次下载会先弹目录选择。
const downloadRoot = ref("");
const downloadRootSaving = ref(false);

async function loadDownloadRoot() {
  try {
    downloadRoot.value = await api.fanqieDownloadRoot();
  } catch {
    // 读不出来当没配过
  }
}

async function pickDownloadRoot() {
  const picked = await open({
    directory: true,
    multiple: false,
    title: "选择下载目录（以后所有剧都放这里）",
  });
  if (typeof picked !== "string" || !picked) return;
  downloadRootSaving.value = true;
  try {
    downloadRoot.value = await api.setFanqieDownloadRoot(picked);
    message.success("下载目录已保存");
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    downloadRootSaving.value = false;
  }
}
// 完成通知邮件：队列跑完后发一封。
// 授权码和 AI Key 一样只留在后端，前端只拿打码后的提示。
const email = ref<EmailConfigView | null>(null);
const emailForm = ref({
  host: "",
  port: 465,
  username: "",
  to: "",
  password: "",
});
const emailSaving = ref(false);
const emailTesting = ref(false);
const emailPreset = ref<string | null>(null);

const EMAIL_PRESETS = [
  { label: "QQ 邮箱", value: "qq" },
  { label: "网易 163", value: "163" },
  { label: "Gmail", value: "gmail" },
  { label: "Outlook", value: "outlook" },
];

/**
 * 每个邮箱「怎么开通 SMTP / 拿授权码」的官方帮助页。
 *
 * 用户选了常用邮箱之后，光自动填服务器和端口不够 —— 真正会卡住的是
 * 「授权码从哪来」。这一步只能去各家的帮助中心，所以直接把入口给出来，
 * 省得用户自己搜。
 */
const EMAIL_HELP: Record<string, { url: string; hint: string }> = {
  qq: {
    // **这条链接是实测打开的**：标题就叫「如何开启 POP3/SMTP/IMAP
    // 服务并获取授权码？」。之前用的是 `service.mail.qq.com` 那个老地址，
    // 会重定向到帮助中心首页 —— 用户点进去还得自己翻。
    url: "https://help.mail.qq.com/detail/0/1087",
    // 新版 QQ 邮箱的入口是「账号与安全」，不是老版本的「账户」。
    // 用户自己找都找不到，所以把四步写全。
    hint: "QQ 邮箱：设置 → 账号与安全 → POP3/IMAP/SMTP 服务 → 开启 → 短信验证 → 得到 16 位授权码",
  },
  "163": {
    url: "https://help.mail.163.com/",
    hint: "网易邮箱：设置 → POP3/SMTP/IMAP → 开启 SMTP → 新增授权码",
  },
  gmail: {
    url: "https://myaccount.google.com/apppasswords",
    hint: "Gmail：先开两步验证，再生成「应用专用密码」",
  },
  outlook: {
    url: "https://account.microsoft.com/security",
    hint: "Outlook：安全 → 高级安全选项 → 应用密码",
  },
};

/** 选了预设之后要显示的那条指引（没选就是 null）。 */
const emailHelp = computed(() => (emailPreset.value ? EMAIL_HELP[emailPreset.value] ?? null : null));

async function openHelp(url: string) {
  try {
    await openUrl(url);
  } catch (e) {
    message.error(`打不开浏览器：${e}`);
  }
}

const EMAIL_HOSTS: Record<string, { host: string; port: number }> = {
  qq: { host: "smtp.qq.com", port: 465 },
  "163": { host: "smtp.163.com", port: 465 },
  gmail: { host: "smtp.gmail.com", port: 465 },
  outlook: { host: "smtp.office365.com", port: 587 },
};

function applyEmailPreset(value: string | null) {
  if (!value) return;
  const preset = EMAIL_HOSTS[value];
  if (!preset) return;
  emailForm.value.host = preset.host;
  emailForm.value.port = preset.port;
}

async function loadEmail() {
  try {
    const v = await api.emailConfig();
    email.value = v;
    emailForm.value = {
      host: v.host,
      port: v.port,
      username: v.username,
      to: v.to,
      password: "",
    };
  } catch {
    // 读不出来当没配过
  }
}

async function saveEmail() {
  emailSaving.value = true;
  try {
    const saved = await api.saveEmailConfig({
      ...emailForm.value,
      password: emailForm.value.password || null,
    });
    email.value = saved;
    // 没填收件人时后端会回落成发件邮箱，同步回表单让用户看到实际收件人
    emailForm.value = {
      host: saved.host,
      port: saved.port,
      username: saved.username,
      to: saved.to,
      password: "",
    };
    const ready = Boolean(saved.host && saved.username && saved.hasPassword);
    message.success(
      ready
        ? "邮件通知已配好，队列跑完会自动发"
        : "已保存；SMTP、发件邮箱、授权码填全后自动生效",
    );
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    emailSaving.value = false;
  }
}

async function testEmail() {
  emailTesting.value = true;
  try {
    const r = await api.emailTest({
      host: emailForm.value.host,
      port: emailForm.value.port,
      username: emailForm.value.username,
      to: emailForm.value.to,
      password: emailForm.value.password || null,
    });
    if (r.ok) message.success(r.detail, { duration: 6000 });
    else message.error(r.detail, { duration: 8000 });
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    emailTesting.value = false;
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
  // 成本报告落款：一次配好，每部剧都用
  void loadCostForm();
  void loadDraftAudio();
  void loadDownloadRoot();
  void loadEmail();
});
</script>

<template>
  <div class="dd-card">
    <div class="dd-card-title">下载目录</div>
    <div class="dd-card-desc">
      从「视频」页下载的短剧都放在这里：每部剧一个子文件夹（加原名），
      里面有 <span class="dd-mono">剧名.txt / intro.txt / poster.jpg / 第NN集.mp4 / 二创</span>。
      第一次下载时没设置会先让你选。
    </div>
    <div class="dd-row" style="margin-bottom: 4px">
      <n-input :value="downloadRoot" readonly size="small" style="width: 480px" placeholder="还没有设置" />
      <n-button size="small" :loading="downloadRootSaving" @click="pickDownloadRoot">选择目录</n-button>
    </div>
  </div>

  <!--
    完成通知：整个队列跑完后发一封邮件。
    QQ / 163 要用「授权码」，不是登录密码 —— 这个坑得在界面上说清楚。
  -->
  <div class="dd-card">
    <div class="dd-card-title">完成通知（邮件）</div>
    <div class="dd-card-desc">
      整个队列跑完后发一封结果邮件（成功 / 失败 / 剩余），人不在电脑前也能知道结果。
      <b>没有开关：填全了就发，没填全就不发。</b>
      用你现有的邮箱即可，不需要额外服务；QQ / 163 的密码要填「授权码」，不是登录密码。
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 90px">常用邮箱</span>
      <n-select
        v-model:value="emailPreset"
        size="small"
        clearable
        placeholder="选一个自动填服务器和端口"
        :options="EMAIL_PRESETS"
        style="width: 220px"
        @update:value="applyEmailPreset"
      />
      <n-button v-if="emailHelp" size="tiny" text type="primary" @click="openHelp(emailHelp.url)">
        怎么开通 / 拿授权码？
      </n-button>
    </div>

    <div v-if="emailHelp" class="dd-row" style="margin: -4px 0 10px 100px">
      <span class="dd-muted" style="font-size: 12px">{{ emailHelp.hint }}</span>
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 90px">发件邮箱</span>
      <n-input v-model:value="emailForm.username" size="small" style="width: 300px" placeholder="you@qq.com" />
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 90px">SMTP</span>
      <n-input v-model:value="emailForm.host" size="small" style="width: 240px" placeholder="smtp.qq.com" />
      <n-input-number v-model:value="emailForm.port" size="small" :min="1" :max="65535" style="width: 110px" />
      <span class="dd-muted" style="font-size: 12px">465 = SSL；587 = STARTTLS</span>
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 90px">授权码</span>
      <n-input
        v-model:value="emailForm.password"
        type="password"
        show-password-on="click"
        size="small"
        style="width: 300px"
        :placeholder="
          email?.hasPassword
            ? `已保存 ${email.passwordHint}，留空不改动`
            : 'QQ / 163 填授权码，不是登录密码'
        "
      />
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 90px">收件邮箱</span>
      <n-input v-model:value="emailForm.to" size="small" style="width: 300px" placeholder="留空 = 发给发件邮箱" />
    </div>

    <div
      v-if="email && email.warnings.length > 0"
      class="dd-muted"
      style="font-size: 12px; margin-bottom: 10px"
    >
      <div v-for="(w, i) in email.warnings" :key="i">· {{ w }}</div>
    </div>

    <n-space>
      <n-button size="small" type="primary" :loading="emailSaving" @click="saveEmail">保存</n-button>
      <n-button size="small" :loading="emailTesting" @click="testEmail">发送测试邮件</n-button>
    </n-space>
  </div>

  <!--
    这些都是「装好基本不用动」的东西。默认收起 ——
    日常只用到上面的下载目录和下面的成本报告落款。
  -->
  <details class="dd-card dd-advanced">
    <summary>
      <span class="dd-card-title" style="margin: 0">高级设置</span>
      <span class="dd-muted" style="font-size: 12px">
        外部工具、可用性、可选运行时、剪映草稿音频（一般不用动）
      </span>
    </summary>

    <div class="dd-adv-body">
      <section class="dd-adv-sec">
        <div class="dd-card-title" style="font-size: 13px">外部工具</div>
        <div class="dd-card-desc">
          ffmpeg / ffprobe 已随程序内置，装好即可用，无需自己配环境。
          只有想换用别的版本时，才需要在这里填路径（填了就以你的为准）。
        </div>

        <div class="dd-row" style="margin-bottom: 10px">
          <span class="dd-muted" style="width: 70px">ffmpeg</span>
          <n-input v-model:value="draft.ffmpeg" size="small" style="width: 480px" placeholder="留空即用内置版本" />
          <n-button size="small" @click="pick('ffmpeg')">选择</n-button>
        </div>

        <div class="dd-row" style="margin-bottom: 10px">
          <span class="dd-muted" style="width: 70px">ffprobe</span>
          <n-input v-model:value="draft.ffprobe" size="small" style="width: 480px" placeholder="留空即用内置版本" />
          <n-button size="small" @click="pick('ffprobe')">选择</n-button>
        </div>

        <div class="dd-row" style="margin-bottom: 14px">
          <span class="dd-muted" style="width: 70px">Python</span>
          <n-input v-model:value="draft.python" size="small" style="width: 480px" placeholder="用于 ASR / OCR，可留空" />
          <n-button size="small" @click="pick('python')">选择</n-button>
        </div>

        <n-space>
          <n-button size="small" type="primary" :loading="saving" @click="save">保存并检测</n-button>
          <n-button size="small" :loading="detecting" @click="redetect">重新检测</n-button>
        </n-space>
      </section>

      <section class="dd-adv-sec">
        <div class="dd-card-title" style="font-size: 13px">可用性</div>
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
              <td>
                {{ r.name }}
                <n-tag v-if="r.bundled" size="tiny" type="info" :bordered="false">内置</n-tag>
              </td>
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
      </section>

      <section class="dd-adv-sec">
        <div class="dd-card-title" style="font-size: 13px">可选运行时</div>
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

        <pre class="dd-mono"
          style="margin: 12px 0 0; padding: 10px; background: rgba(128,128,128,0.08); border-radius: 6px">pip install faster-whisper          # 语音转字幕
pip install rapidocr_onnxruntime    # 硬字幕识别
pip install pyJianYingDraft         # 剪映草稿导出</pre>
      </section>

      <!--
        BGM 是一次配好、每部剧都用的东西。真实软件也是这么做的 ——
        它有个「音乐库」，要用户自己放音乐文件进去，然后随机抽。
      -->
      <section class="dd-adv-sec">
        <div class="dd-card-title" style="font-size: 13px">剪映草稿音频</div>
        <div class="dd-card-desc">
          生成剪映草稿时铺在 BGM 轨上的音乐。<b>不填也能出草稿</b>，
          只是没有 BGM 轨（切片自带的对白和音效还在）。
          支持 mp3 / wav / m4a。
        </div>

        <div class="dd-row" style="margin-bottom: 10px">
          <span class="dd-muted" style="width: 90px">BGM 文件</span>
          <n-input v-model:value="draftAudio.bgmPath" size="small" style="width: 420px" placeholder="留空则不加 BGM 轨" />
          <n-button size="small" @click="pickBgm">选择</n-button>
        </div>

        <div class="dd-row" style="margin-bottom: 14px">
          <span class="dd-muted" style="width: 90px">BGM 音量</span>
          <n-input-number v-model:value="draftAudio.bgmVolume" size="small" :min="0" :max="1" :step="0.05"
            style="width: 120px" />
          <span class="dd-muted" style="margin-left: 8px; font-size: 12px">0~1，默认 0.35（别盖过对白）</span>
        </div>

        <n-space>
          <n-button size="small" type="primary" :loading="draftAudioSaving" @click="saveDraftAudio">保存</n-button>
        </n-space>
      </section>
    </div>
  </details>

  <!--
    成本报告的落款 —— **一次配好，每部剧都用**，所以放在设置里，
    不放流水线页。流水线页是「每跑一次可能变」的东西。
  -->
  <div class="dd-card">
    <div class="dd-card-title">成本报告落款</div>
    <div class="dd-card-desc">
      生成「成本配置比例情况报告」时盖在落款处的信息。
      <b>不填也能出报告</b>，只是落款空着，需要自己补。
      公章和签名接受 PNG（推荐透明底）或 JPG。
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 90px">报审机构</span>
      <n-input v-model:value="costForm.org" size="small" style="width: 420px" placeholder="填公司全称，留空则落款处空白" />
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 90px">法定代表人</span>
      <n-input v-model:value="costForm.legalPerson" size="small" style="width: 420px" placeholder="填姓名，留空则落款处空白" />
    </div>

    <div class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 90px">公章图片</span>
      <n-input v-model:value="costForm.sealPath" size="small" style="width: 420px" placeholder="留空则不盖公章" />
      <n-button size="small" @click="pickCostFile('sealPath')">选择</n-button>
    </div>

    <div class="dd-row" style="margin-bottom: 14px">
      <span class="dd-muted" style="width: 90px">签名图片</span>
      <n-input v-model:value="costForm.signPath" size="small" style="width: 420px" placeholder="留空则不贴签名图（仍会打上名字）" />
      <n-button size="small" @click="pickCostFile('signPath')">选择</n-button>
    </div>

    <n-space>
      <n-button size="small" type="primary" :loading="costSaving" @click="saveCostForm">保存</n-button>
    </n-space>
  </div>
  <!-- <div class="dd-card">
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
      <div class="dd-wrap">
        <b>内置了 FFmpeg</b>（{{ store.toolReports.find((r) => r.name === "ffmpeg")?.version || "版本见上表" }}），
        它是 GPLv3 授权的自由软件，随程序一起分发，所以你不必自己装。
      </div>
      <div class="dd-wrap" style="margin-top: 6px">
        <b>自己用：没有任何影响。</b>GPL 的义务只在「分发」时触发。
      </div>
      <div class="dd-wrap" style="margin-top: 6px">
        <b>要把这个软件发给别人：</b>包含 GPL 组件的软件整体要按 GPL 分发
        —— 需随附对应源码、保留版权与许可证、不得加更严的限制。
        换用 LGPL 构建或改为外部指定路径都可以绕开，见
        <span class="dd-mono">resources/bin/README.txt</span>。
      </div>
      <div class="dd-wrap" style="margin-top: 6px">
        <b>本项目不含任何第三方素材</b>：音效、BGM、LUT 全部由使用者自行导入，
        证明材料的模板图与字体也用使用者自己的。
      </div>
    </n-alert>
  </div> -->
</template>
