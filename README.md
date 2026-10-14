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
| FFmpeg 切片执行（流拷贝） | ✅ 已实现 |
| ASR 字幕轴 | 🚧 接口就绪，待接入运行时 |
| 成片导出（烧字幕 / 拼接） | ⬜ 计划中 |
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

## 首次使用

1. 启动应用，进入 **设置**，指定 `ffmpeg.exe` 与 `ffprobe.exe` 的路径。
   - 需要 **LGPL/GPL 合规** 的自建 FFmpeg（不要使用来源不明或第三方软件自带的二进制）。
2. 进入 **素材库**，添加视频文件，应用会自动探测时长与分辨率。
3. 进入 **切片工作台**，选择切分方式，生成计划 → 执行切片。
4. 输出片段默认写入项目目录下的 `output/`。

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
