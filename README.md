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
| 媒体 | FFmpeg / FFprobe（外部可执行文件，用户可指定路径） |
| 字幕 | faster-whisper（外部 Python 运行时，可选） |

## 当前进度

| 链路 | 状态 |
| --- | --- |
| 项目管理 / 素材库 | ✅ 已实现 |
| FFprobe 媒体探测 | ✅ 已实现 |
| 切片计划（按时长 / 等分 / 手动） | ✅ 已实现 |
| FFmpeg 切片执行（流拷贝 + 回退转码） | ✅ 已实现 |
| ASR 字幕轴（faster-whisper 子进程） | ✅ 已实现（需自备 Python + faster-whisper） |
| 字幕编辑 / SRT 导入导出 | ✅ 已实现 |
| 成片导出（烧字幕 / 竖屏 9:16 / 区间） | ✅ 已实现 |
| 片头片尾拼接 | ⬜ 计划中 |
| 剪映草稿导出 | ⬜ 计划中 |
| 多平台发布 | ⬜ 计划中 |
| 视频去重 | ⬜ 计划中 |

详见 [docs/ROADMAP.md](docs/ROADMAP.md) 与 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)。

## 开发

```bash
npm install
npm run tauri:dev      # 开发模式（Vite + Rust 后端）
npm run build          # 仅前端构建 + 类型检查
npm run tauri:build    # 打包（Windows: NSIS / MSI）
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
| Rust 单元测试 | 57 项通过（切片计划边界、SRT 解析与投影、滤镜构造与转义、DB 迁移与级联、子进程协议与超时、编码健壮性） |
| 端到端冒烟 | 通过（真实 ffmpeg：12 秒素材切片；6 秒素材流拷贝 + 竖屏 1080×1920 烧字幕导出） |
| 前端类型检查 + 构建 | 通过（`vue-tsc --noEmit` + `vite build`） |

## 首次使用

1. 启动应用，进入 **设置**，指定 `ffmpeg.exe` 与 `ffprobe.exe` 的路径。
   - 需要 **LGPL/GPL 合规** 的自建 FFmpeg（不要使用来源不明或第三方软件自带的二进制）。
2. 进入 **素材库**，添加视频文件，应用会自动探测时长与分辨率。
3. 进入 **切片工作台**，选择切分方式，生成计划 → 执行切片。
4. 进入 **字幕**，用 ASR 生成字幕轴，或导入现成 SRT 后逐条校对。
5. 进入 **导出**，选择画幅（原画幅 / 竖屏模糊 / 竖屏裁切）与是否烧字幕，导出成片。

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

- **FFmpeg**：本项目只调用外部 ffmpeg，不内嵌。请自行选择合规构建；
  若启用 libx264/x265 等 GPL 组件，分发时需遵守 GPL。
- **不要**把第三方商业软件自带的二进制、音效、模板、字体复制进本项目。
- ASR/OCR 模型选用 MIT / Apache-2.0 许可者（如 faster-whisper + Whisper、RapidOCR + PP-OCR）。
- 平台发布请优先使用各平台**官方开放平台 API**；网页自动化存在封号与协议风险。
