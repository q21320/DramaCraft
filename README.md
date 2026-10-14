# DramaDist · 短剧切片分发工作台

一款**纯本地**的短剧素材处理与分发桌面应用：把一部长剧素材自动切成可发布的短片段，
生成字幕轴，导出成片，后续接入剪映草稿与多平台发布。

> 本项目是**独立实现**：不包含任何第三方商业软件的代码、素材、文案或数据表。

## 技术栈

| 层 | 选型 |
| --- | --- |
| 桌面框架 | Tauri v2 |
| 后端 | Rust（edition 2021） |
| 前端 | Vue 3 + TypeScript + Vite |
| UI | Naive UI |
| 状态 | Pinia |
| 存储 | SQLite（rusqlite，bundled） |
| 媒体 | FFmpeg / FFprobe（**已随程序内置**，也可在设置里换成你自己的） |
| 字幕 | **ASR 运行时已内置**（嵌入式 Python 3.13 + faster-whisper + tiny 模型）；硬字幕 OCR 用外部 Python |
| 剪映草稿 | pyJianYingDraft（Apache-2.0，外部 Python 运行时，可选） |

## 当前进度

| 链路 | 状态 |
| --- | --- |
| 项目管理 / 素材库 | ✅ 已实现 |
| FFprobe 媒体探测 | ✅ 已实现 |
| 切片计划（按时长 / 等分 / 手动） | ✅ 已实现 |
| FFmpeg 切片执行（流拷贝 + 回退转码） | ✅ 已实现 |
| ASR 字幕轴（faster-whisper 子进程） | ✅ 已实现（**运行时与 tiny 模型已内置**，装上即用） |
| 字幕编辑 / SRT 导入导出 | ✅ 已实现 |
| 硬字幕识别（OCR） | ✅ 已实现（需自备 Python + rapidocr_onnxruntime） |
| 云端字幕（用户自备 Key） | ✅ 已实现（OpenAI 兼容接口，本地 mock 端到端验证） |
| 证明材料生成（模板图填数据） | ✅ 已实现（模板 + 字段坐标，ffmpeg 渲染） |
| 证明文档生成（成本明细 / 权属声明 docx） | ✅ 已实现（自写 OOXML + ZIP，零新增依赖） |
| 用户自备素材库（音效 / BGM / LUT） | ✅ 已实现（**不内置任何第三方素材**） |
| 剧目重组 / 集数重切 | ✅ 已实现（按文件名解析 + 片头指纹） |
| 批量任务队列 / 断点续跑 | ✅ 已实现（重启后**自动接着跑**；用户主动暂停过则不自动跑） |
| 成片导出（烧字幕 / 竖屏 9:16 / 区间） | ✅ 已实现 |
| 剪映草稿导出 | ✅ 已实现（需自备 Python + pyJianYingDraft） |
| 画面处理（镜像/裁切/调色/变速） | ✅ 已实现 |
| 去重效果验证（感知哈希比对） | ✅ 已实现 |
| 发布包导出（视频/封面/字幕/元数据/说明） | ✅ 已实现 |
| 发布台账（本地记录各平台发布状态） | ✅ 已实现 |
| 多账号台账（同平台多账号独立记账） | ✅ 已实现 |
| 自动上传到平台 | ⬜ **不提供**（原因见下） |
| 片头片尾拼接 | ⬜ 计划中 |
| 批量任务队列与断点续跑 | ⬜ 计划中 |

### 为什么没有「一键自动发布」

不是因为做不了，而是因为**这条路本身不可靠且有风险**：

- **微信视频号没有公开的发布 API**，只能人工或用微信自家工具；
- **抖音 / 快手 / B站开放平台**有发布接口，但需要企业资质并审核通过；
- **网页自动化**（模拟点击上传）绕开平台风控，存在**封号风险**，且违反平台协议。

所以 DramaDist 的做法是：**把「准备」做到极致，把「发送」留给你**。
「发布」页会按平台整理出完整发布包（视频、封面、字幕、标题、简介、标签、说明文件），
上传完再回来标记状态、记台账。等你自己拿到官方资质，随时可以在这层接上接口。

详见 [docs/ROADMAP.md](docs/ROADMAP.md) 与 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。

## 开发

```bash
npm install
npm run tauri:dev      # 开发模式（Vite + Rust 后端）
npm run build          # 仅前端构建 + 类型检查
npm run tauri:build    # 打包（Windows: NSIS / MSI）
```

可选：想在本机验证剪映草稿链路，可以建一个项目内虚拟环境装依赖（`.venv/` 已在 .gitignore 中）：

```bash
python -m venv .venv
.venv/Scripts/pip install pyJianYingDraft
```

Rust 侧单元测试：

```bash
cd src-tauri
cargo test
```

**端到端冒烟测试**（需要本机有 ffmpeg；默认跳过，不设环境变量不会失败）：

```powershell
$env:DRAMADIST_TEST_FFMPEG  = "C:\ffmpeg\bin\ffmpeg.exe"
$env:DRAMADIST_TEST_FFPROBE = "C:\ffmpeg\bin\ffprobe.exe"
cd src-tauri; cargo test end_to_end -- --nocapture
```

它会自己生成一段 12 秒带音轨的测试素材，跑通「探测 → 计划 → 切片 → 校验产出 → 检查临时文件残留」全流程。

### 当前测试状态

| 项目 | 结果 |
| --- | --- |
| Rust 单元测试 | 269 项通过（切片计划边界、SRT 解析与投影、滤镜构造与转义、画面处理参数与边界、感知哈希与相似度、OCR 文本归一化与逐帧合并、文件名集号解析与剧目分组、片头定位与指纹聚类、草稿时间轴映射、发布包与多账号台账、云端适配器配置与错误映射、证明材料排版（中文折行禁则/缩字号/溢出判定）、docx 写出（CRC32/ZIP 结构/XML 转义与非法字符/表格补齐与列宽归一）、`.cube` 结构校验与混音/调色链构造、任务队列状态机与断点恢复、DB 迁移与级联、子进程协议/超时/编码） |
| 端到端（真实 ffmpeg） | 通过：切片；竖屏烧字幕导出 1080×1920；画面处理；发布包生成（按平台/账号分层）；相似度验证；OCR 读取硬字幕；三集连播素材的集边界检测与按集切分；证明图渲染（再用 OCR 把渲染上去的字读回来做闭环验证）；配 BGM（校验成片时长贴着素材而非 BGM）；套 LUT（**用感知哈希确认画面真的变了**） |
| 端到端（真实 HTTP + 本地 mock 服务） | 通过：multipart 上传、Bearer 鉴权头、请求字段齐全、音频字节数、响应解析成字幕；以及「没配 Key 时一个字节都不发」 |
| 独立实现校验（docx） | 通过：自己写的 docx 交给 **Python 的 zipfile + ElementTree** 打开并读出正文；再用官方检查器（python-docx + lxml）判定 `pass`；最后用 LibreOffice 渲染成图，`missingFonts` 为空、排版正确 |
| 运行时迁移 | 通过：真实库逐级升到 v7；**断点续跑实测**：塞入假 `running` 任务 → 重启应用 → 自动退回 `pending`；**零配置实测**：清空设置表后启动，ffmpeg / ffprobe / python 均解析到内置目录 |
| 内置运行时端到端 | 通过：用内置的 Python + faster-whisper + tiny 模型跑完整转写链路；内置运行时被识别为「内置」且报告 ASR 可用 |
| 端到端（真实 pyJianYingDraft） | 通过：两个切片 + 两条字幕 → 草稿目录生成，内容含中文文本，总时长 4000ms |
| 运行时迁移 | 通过：真实库 v2→v3→v4 逐级自动升级，旧数据不丢 |
| 前端类型检查 + 构建 | 通过（`vue-tsc --noEmit` + `vite build`） |

## 首次使用

**装上就能用，不需要先配环境** —— FFmpeg 与 FFprobe 已随程序内置。

1. 启动应用，直接进 **素材库**，添加视频文件，应用会自动探测时长与分辨率。
   - 想换用你自己那份 FFmpeg，到 **设置** 里填路径即可；填了就以你的为准。
2. 进入 **切片工作台**，选择切分方式，生成计划 → 执行切片。
3. 进入 **字幕**，用 ASR 生成字幕轴，或导入现成 SRT 后逐条校对。
4. 进入 **导出**，选择画幅（原画幅 / 竖屏模糊 / 竖屏裁切）与是否烧字幕，导出成片。
5. 进入 **剪映草稿**，选择剪映的草稿目录，一键把切片与字幕拼成一份剪映草稿。
6. 进入 **画面处理**，按档位或手动给切片做差异化处理；原件保留，随时可还原。
7. 进入 **发布**，填写标题/简介/标签，导出发布包，人工上传后回来标记状态。

### 关于内置的 FFmpeg

`src-tauri/resources/bin/` 里放的是 **FFmpeg 7.1.3（GPLv3 构建）**，
随安装包一起分发。详见该目录下的 `README.txt`（含 SHA256、完整编译配置与许可说明）。

- **自己用：没有任何影响。** GPL 的义务只在「分发」时触发。
- **要对外分发这个软件：** 包含 GPL 组件的软件整体要按 GPL 分发 —— 需随附对应源码、
  保留版权与许可证、不得加更严的限制。想绕开可以换 LGPL 构建（但没有 libx264）
  或改为让使用者自备 FFmpeg。
- 二进制不进版本库（158 MB）。clone 之后用 `scripts/fetch-ffmpeg.ps1` 获取：

```powershell
pwsh -File scripts/fetch-ffmpeg.ps1                    # 从官方构建下载
pwsh -File scripts/fetch-ffmpeg.ps1 -Source E:\ffmpeg\bin   # 或从本地已有的复制
```

### 关于内置的 ASR 运行时

`src-tauri/resources/asr_runtime/`（327 MB）里是完整的语音转字幕运行时：
嵌入式 Python 3.13 + faster-whisper 1.2.1 + ctranslate2 + onnxruntime + Whisper tiny 模型。
**不需要你装 Python，也不需要 pip install 任何东西。**

- 全部组件都是宽松许可（PSF / MIT / BSD / Apache-2.0），分发本软件**不因此产生开源义务**
- 模型只有 tiny（最小最快，准确率一般）。想换更大的规格，把模型目录放到
  `models\<规格名>\` 下，程序启动时会自动列出来
- 二进制不进版本库，clone 后用 `scripts/fetch-asr-runtime.ps1` 获取

> 为什么脚本是「从本地复制」而不是「下载」：这套东西国内下不全 ——
> Python 嵌入式包和 PyPI 都通，但模型所在的 HuggingFace 与 hf-mirror 实测均超时。

### 想用硬字幕 OCR / 剪映草稿

### 想导出剪映草稿

同样用自备的 Python：

```bash
pip install pyJianYingDraft
```

草稿目录填剪映「全局设置 → 草稿位置」指向的那个目录。生成后打开剪映即可看到，
可以继续手工加转场、滤镜、封面，再导出。

> 草稿格式是剪映的私有格式且随版本变化，所以本项目**不自己维护格式模板**，
> 而是复用专门跟这个格式的开源库（Apache-2.0）。我们只负责把项目数据映射成草稿结构，
> 这部分是纯逻辑，有完整单元测试。

### 想用 ASR 自动生成字幕

需要自备一个 Python（3.9+）并安装依赖：

```bash
pip install faster-whisper
```

然后在 **设置 → Python** 里指向这个 `python.exe`。首次识别会自动下载所选模型
（tiny 约 75 MB，small 约 480 MB），之后走本地缓存，全程不联网推理。

> 模型运行在**独立子进程**里：即使模型崩了，界面也不会跟着挂。超时会自动终止并报错。

## 目录结构

```
DramaDist/
├─ src/                  # Vue 前端
│  ├─ views/             # 页面
│  ├─ layouts/           # 布局与导航
│  ├─ stores/            # Pinia 状态
│  ├─ router/            # 路由
│  └─ styles/            # 全局样式
├─ src-tauri/            # Rust 后端
│  └─ src/
│     ├─ commands.rs     # Tauri 命令（前后端边界）
│     ├─ db/             # SQLite 与 schema 迁移
│     ├─ media/          # ffprobe 探测、切片计划与执行
│     ├─ asr/            # ASR 桥（faster-whisper 子进程）
│     ├─ tools/          # 外部工具定位（ffmpeg/ffprobe/python）
│     └─ models/         # 数据结构
├─ docs/                 # 架构与路线图
└─ tests/                # 前端测试
```

## 许可与合规提示

- **FFmpeg**：已随程序内置（GPLv3 构建，见 `src-tauri/resources/bin/README.txt`）。
  自用无影响；对外分发时需遵守 GPL —— 或改用 LGPL 构建 / 让使用者自备。
- **不要**把第三方商业软件自带的二进制、音效、模板、字体复制进本项目。
- ASR/OCR 模型选用 MIT / Apache-2.0 许可者（如 faster-whisper + Whisper、RapidOCR + PP-OCR）。
- 平台发布请优先使用各平台**官方开放平台 API**；网页自动化存在封号与协议风险。
