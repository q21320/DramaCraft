<script setup lang="ts">
import { computed, onMounted, ref } from "vue";
import {
  NAlert,
  NButton,
  NCheckbox,
  NInput,
  NInputNumber,
  NSelect,
  NSpace,
  NTag,
  useMessage,
} from "naive-ui";
import { open, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { api } from "@/api";
import type { ProofField, ProofLayout, ProofPreview, ProofRenderReport } from "@/types";

const message = useMessage();

const layout = ref<ProofLayout | null>(null);
const layoutPath = ref("");
const values = ref<Record<string, string>>({});
const preview = ref<ProofPreview | null>(null);
const report = ref<ProofRenderReport | null>(null);
const outPath = ref("");
const busy = ref(false);

const fieldCount = computed(() => layout.value?.fields.length ?? 0);

const imageSizeText = computed(() => {
  const size = layout.value?.imageSize ?? [];
  return size.length === 2 ? `${size[0]} × ${size[1]}` : "未知";
});

function newField(): ProofField {
  return {
    key: `field_${(layout.value?.fields.length ?? 0) + 1}`,
    label: "新字段",
    eraseRect: null,
    eraseColor: "#FFFFFF",
    boxRect: [100, 100, 300, 60],
    fontSize: 20,
    minFontSize: 12,
    color: "#333333",
    align: "center",
    valign: "center",
    wrap: true,
    lineSpacing: 1.1,
  };
}

async function init() {
  try {
    layout.value = await api.proofDefaultLayout();
    syncValues();
  } catch (e) {
    message.error((e as Error).message);
  }
}

/** 保证每个字段都有对应的取值输入框 */
function syncValues() {
  if (!layout.value) return;
  const next: Record<string, string> = {};
  for (const f of layout.value.fields) {
    next[f.key] = values.value[f.key] ?? "";
  }
  values.value = next;
}

async function pickTemplate() {
  const picked = await open({
    multiple: false,
    title: "选择模板图片",
    filters: [{ name: "图片", extensions: ["png", "jpg", "jpeg", "bmp"] }],
  });
  if (typeof picked === "string" && layout.value) {
    layout.value.template = picked;
    preview.value = null;
    report.value = null;
  }
}

async function pickFont() {
  const picked = await open({
    multiple: false,
    title: "选择字体文件",
    filters: [{ name: "字体", extensions: ["ttf", "ttc", "otf"] }],
  });
  if (typeof picked === "string" && layout.value) {
    layout.value.fontPath = picked;
    preview.value = null;
  }
}

async function loadLayout() {
  const picked = await open({
    multiple: false,
    title: "打开布局文件",
    filters: [{ name: "布局", extensions: ["json"] }],
  });
  if (typeof picked !== "string") return;
  try {
    layout.value = await api.proofLoadLayout(picked);
    layoutPath.value = picked;
    values.value = {};
    syncValues();
    preview.value = null;
    report.value = null;
    message.success("布局已载入");
  } catch (e) {
    message.error((e as Error).message);
  }
}

async function saveLayout() {
  if (!layout.value) return;
  const picked = await saveDialog({
    title: "保存布局",
    defaultPath: "proof_layout.json",
    filters: [{ name: "布局", extensions: ["json"] }],
  });
  if (typeof picked !== "string") return;
  try {
    await api.proofSaveLayout(picked, layout.value);
    layoutPath.value = picked;
    message.success("布局已保存");
  } catch (e) {
    message.error((e as Error).message);
  }
}

function addField() {
  if (!layout.value) return;
  layout.value.fields.push(newField());
  syncValues();
}

function removeField(index: number) {
  if (!layout.value) return;
  const [removed] = layout.value.fields.splice(index, 1);
  if (removed) delete values.value[removed.key];
  preview.value = null;
}

function toggleErase(index: number, on: boolean) {
  if (!layout.value) return;
  const f = layout.value.fields[index];
  const [x, y, w, h] = f.boxRect;
  f.eraseRect = on ? [x - 4, y - 4, x + w + 4, y + h + 4] : null;
}

async function runPreview() {
  if (!layout.value) return;
  try {
    preview.value = await api.proofPreview(layout.value, values.value);
    report.value = null;
  } catch (e) {
    message.error((e as Error).message);
  }
}

async function pickOut() {
  const picked = await saveDialog({
    title: "保存证明图",
    defaultPath: "证明.png",
    filters: [{ name: "PNG 图片", extensions: ["png"] }],
  });
  if (typeof picked === "string") outPath.value = picked;
}

async function render() {
  if (!layout.value) return;
  if (!outPath.value.trim()) {
    message.warning("请先选择输出路径");
    return;
  }
  busy.value = true;
  report.value = null;
  try {
    report.value = await api.proofRender(layout.value, values.value, outPath.value.trim());
    message.success("证明图已生成");
  } catch (e) {
    message.error((e as Error).message);
  } finally {
    busy.value = false;
  }
}

onMounted(init);
</script>

<template>
  <div class="dd-card">
    <div class="dd-card-title">证明材料</div>
    <div class="dd-card-desc">
      报备、结算时对方常要一张「这部剧多少集、多长时间、投了多少」的图。
      这里用模板图 + 字段坐标一次配好，之后换个剧目名就能重出，不用手填。
      渲染走 ffmpeg，系统自带的中文字体即可。
    </div>

    <div v-if="layout" class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 64px">模板图</span>
      <n-input v-model:value="layout.template" size="small" style="width: 380px" placeholder="选择一张模板图片" />
      <n-button size="small" @click="pickTemplate">选择</n-button>
      <n-tag size="small" :bordered="false">约定尺寸 {{ imageSizeText }}</n-tag>
    </div>

    <div v-if="layout" class="dd-row" style="margin-bottom: 10px">
      <span class="dd-muted" style="width: 64px">字体</span>
      <n-input v-model:value="layout.fontPath" size="small" style="width: 380px" placeholder="中文字体文件" />
      <n-button size="small" @click="pickFont">选择</n-button>
      <span class="dd-muted">默认已挑一个系统里存在的中文字体</span>
    </div>

    <div v-if="layout" class="dd-row">
      <span class="dd-muted" style="width: 64px">布局文件</span>
      <n-input v-model:value="layoutPath" size="small" style="width: 380px" placeholder="保存后可反复使用" />
      <n-button size="small" @click="loadLayout">载入</n-button>
      <n-button size="small" @click="saveLayout">另存为</n-button>
    </div>
  </div>

  <div v-if="layout" class="dd-card">
    <div class="dd-row" style="justify-content: space-between; margin-bottom: 10px">
      <div>
        <div class="dd-card-title">字段（{{ fieldCount }}）</div>
        <div class="dd-card-desc">
          坐标以模板图左上角为原点。勾选「擦除」会先用底色盖掉模板上的示例文字，再写新内容。
        </div>
      </div>
      <n-button size="small" @click="addField">添加字段</n-button>
    </div>

    <div class="dd-scroll">
      <table class="dd-table">
        <thead>
          <tr>
            <th style="width: 110px">取值键</th>
            <th style="width: 110px">显示名</th>
            <th style="width: 230px">绘制区域 x / y / 宽 / 高</th>
            <th style="width: 90px">字号</th>
            <th style="width: 90px">最小字号</th>
            <th style="width: 100px">颜色</th>
            <th style="width: 100px">对齐</th>
            <th style="width: 80px">换行</th>
            <th style="width: 90px">擦除</th>
            <th style="width: 60px"></th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="(f, i) in layout.fields" :key="i">
            <td><n-input v-model:value="f.key" size="tiny" /></td>
            <td><n-input v-model:value="f.label" size="tiny" /></td>
            <td>
              <div class="dd-row" style="gap: 4px">
                <n-input-number
                  v-for="n in 4"
                  :key="n"
                  v-model:value="f.boxRect[n - 1]"
                  size="tiny"
                  :show-button="false"
                  style="width: 52px"
                />
              </div>
            </td>
            <td><n-input-number v-model:value="f.fontSize" size="tiny" :min="4" :max="400" :show-button="false" /></td>
            <td><n-input-number v-model:value="f.minFontSize" size="tiny" :min="4" :max="400" :show-button="false" /></td>
            <td><n-input v-model:value="f.color" size="tiny" /></td>
            <td>
              <n-select
                v-model:value="f.align"
                size="tiny"
                :options="[
                  { label: '居中', value: 'center' },
                  { label: '左', value: 'left' },
                  { label: '右', value: 'right' },
                ]"
              />
            </td>
            <td><n-checkbox v-model:checked="f.wrap" /></td>
            <td>
              <n-checkbox
                :checked="f.eraseRect !== null"
                @update:checked="(v: boolean) => toggleErase(i, v)"
              />
            </td>
            <td><n-button size="tiny" quaternary @click="removeField(i)">删</n-button></td>
          </tr>
        </tbody>
      </table>
    </div>
    <div v-if="fieldCount === 0" class="dd-empty">还没有字段。点「添加字段」开始。</div>
  </div>

  <div v-if="layout && fieldCount > 0" class="dd-card">
    <div class="dd-card-title">填值</div>
    <div class="dd-card-desc">这里填的内容会替换掉模板上的示例文字。</div>
    <div v-for="f in layout.fields" :key="f.key" class="dd-row" style="margin-bottom: 8px">
      <span class="dd-muted" style="width: 140px">{{ f.label }}（{{ f.key }}）</span>
      <n-input v-model:value="values[f.key]" size="small" style="width: 420px" :placeholder="`填写${f.label}`" />
    </div>

    <n-space style="margin-top: 12px">
      <n-button size="small" @click="runPreview">预览排版</n-button>
      <n-button size="small" type="primary" :loading="busy" @click="render">生成证明图</n-button>
      <n-input v-model:value="outPath" size="small" style="width: 320px" placeholder="输出 PNG 路径" />
      <n-button size="small" @click="pickOut">选择输出</n-button>
    </n-space>
  </div>

  <div v-if="preview" class="dd-card">
    <div class="dd-card-title">排版预览</div>
    <div class="dd-card-desc">只计算不生成，先看清会不会放不下。</div>
    <table class="dd-table">
      <thead>
        <tr>
          <th style="width: 180px">字段</th>
          <th style="width: 90px">实际字号</th>
          <th style="width: 200px">折行结果</th>
          <th>状态</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="f in preview.fields" :key="f.key">
          <td>{{ f.label }}</td>
          <td>
            {{ f.fontSize }}
            <n-tag v-if="f.shrunk" size="tiny" type="warning" :bordered="false">已缩小</n-tag>
          </td>
          <td class="dd-mono dd-wrap">{{ f.lines.join(" ⏎ ") }}</td>
          <td>
            <n-tag v-if="f.overflow" size="tiny" type="error" :bordered="false">放不下，会被裁掉</n-tag>
            <n-tag v-else size="tiny" type="success" :bordered="false">可以放下</n-tag>
          </td>
        </tr>
      </tbody>
    </table>

    <n-alert v-if="preview.warnings.length > 0" type="warning" title="注意" style="margin-top: 12px">
      <ul style="margin: 0; padding-left: 18px">
        <li v-for="(w, i) in preview.warnings" :key="i" class="dd-wrap">{{ w }}</li>
      </ul>
    </n-alert>
  </div>

  <div v-if="report" class="dd-card">
    <div class="dd-card-title">生成完成</div>
    <div class="dd-card-desc">
      输出到 <span class="dd-mono">{{ report.outPath }}</span>，
      填入 {{ report.fieldCount }} 个字段，{{ (report.sizeBytes / 1024).toFixed(0) }} KB
    </div>
    <n-alert v-if="report.warnings.length > 0" type="warning" title="注意">
      <ul style="margin: 0; padding-left: 18px">
        <li v-for="(w, i) in report.warnings" :key="i" class="dd-wrap">{{ w }}</li>
      </ul>
    </n-alert>
  </div>
</template>
