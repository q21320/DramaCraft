# DramaDist 架构

## 分层

```
┌──────────────────────────────────────────────┐
│  Vue 3 前端（WebView）                        │
│  views / layouts / stores / router            │
└───────────────┬──────────────────────────────┘
                │  Tauri invoke（JSON，camelCase）
┌───────────────▼──────────────────────────────┐
│  Rust 后端（commands.rs = 唯一边界）           │
│  ┌────────┬────────┬────────┬────────┐        │
│  │ media  │  asr   │  db    │ tools  │        │
│  └────────┴────────┴────────┴────────┘        │
└───────┬──────────────┬───────────────┬────────┘
        │              │               │
   ffmpeg/ffprobe   python(ASR)     SQLite
   子进程            子进程          本地文件
```

## 设计约束

1. **前后端唯一边界是 `commands.rs`**。前端不直接碰数据库和文件系统逻辑，便于将来换 UI。
2. **重活一律走子进程**。FFmpeg 与 ASR 都是子进程；主进程只负责编排、落库、上报进度。
   ASR 用独立 Python 运行时，避免模型崩溃拖死 GUI。
3. **序列化统一 camelCase**。Rust 结构体打 `#[serde(rename_all = "camelCase")]`，
   前端 `types/index.ts` 与之一一对应。
4. **数据库迁移靠 `PRAGMA user_version`**，只前进不回退，旧库能直接打开。
5. **外部依赖可缺席**。ffmpeg / python 找不到时，功能降级并在设置页明确提示，不静默失败。
6. **界面诚实**：未接入的步骤显示"待接入"并说明原因，不假装成功。

## 数据模型（v0.1）

| 表 | 用途 | 关键字段 |
| --- | --- | --- |
| `project` | 一个分发项目 | `name`, `root_dir`, `created_at` |
| `media` | 导入的源素材 | `project_id`, `path`, `duration_ms`, `width`, `height`, `has_audio` |
| `segment` | 切片结果 | `media_id`, `index_no`, `start_ms`, `end_ms`, `out_path`, `status` |
| `cue` | 字幕条目（v2 新增） | `media_id`, `index_no`, `start_ms`, `end_ms`, `text` |
| `setting` | 键值配置 | `key`, `value`（工具路径等） |

迁移靠 `PRAGMA user_version` 只前进不回退：v1 建的库打开后会自动补上 `cue` 表，旧数据不动。

## 切片计划算法

三种模式，全部产出 `Vec<SlicePlan>`（`start_ms` / `end_ms` / `index`）：

- `everySeconds(n)`：从 0 开始每 n 秒一片，最后一片不足 n 秒时**并入上一片**（避免 0.3 秒碎片）。
- `equalParts(k)`：总时长等分 k 份，余数毫秒均摊到前若干片，保证首尾精确闭合。
- `manual(points)`：用户给定切点，自动补齐首尾并去重、排序、丢弃非法区间。

边界规则：任何片长 < `min_ms`（默认 500ms）的片段被丢弃并记入日志，不产生空文件。

## 错误处理

- 后端统一返回 `Result<T, AppError>`；`AppError` 序列化为 `{ code, message }`。
- 子进程失败时保留 **stderr 片段**（截断到合理长度）写进 `task.log`，便于用户自查。
- 所有文件写入使用"临时文件 + 原子重命名"，避免半成品污染输出目录。

## 踩过的坑（都是真事故，别重犯）

### 1. 子进程编码：中文 Windows 上 Python 默认写 GBK

Python 在管道里的 stdout 编码跟随系统区域设置。中文 Windows 上是 **GBK**，
而 Rust 侧按 UTF-8 读 —— 结果不是乱码，而是 `read_to_string` 直接失败、
**整段输出被丢空**，表现为"worker 没有返回结果"（退出码还是 0，特别难查）。

两处一起改才对：

- Rust 侧给子进程设 `PYTHONUTF8=1` 与 `PYTHONIOENCODING=utf-8`；
- Rust 侧读取改用 `read_to_end` + `from_utf8_lossy`，即使对面不听话也拿得到内容；
- worker 自带 `sys.stdout.reconfigure(encoding="utf-8")` 兜底，方便手跑调试。

回归测试：`asr::tests::talks_to_a_stub_worker_over_stdio`（用真实 Python 跑假 worker，
断言中文原样往返）。

### 2. Windows 路径里的冒号会毁掉 filtergraph

`subtitles=C:\a\b.srt` 里的 `:` 是 filtergraph 的分隔符，需要转义成一团乱麻。
解法根本不用转义：**把 ffmpeg 的工作目录设为字幕所在目录，滤镜里只写文件名**
（`subtitles='b.srt'`），路径里的冒号就永远不会进入滤镜串。

### 3. `&lock_db(&state)?` 过不了类型检查

`&MutexGuard<Connection>` 在这个位置不会自动解引用成 `&Connection`。
统一写成先绑定再用：`let conn = lock_db(&state)?;` 然后 `&conn`。

### 4. 被引用的 composite tsconfig 不能 `noEmit`

`vue-tsc --noEmit` 会报 `TS6310`。要么给被引用工程开启 `emitDeclarationOnly`，
要么（更简单）直接不要那条 `references`。
