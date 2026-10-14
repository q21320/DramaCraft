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
| `task` | 长任务状态 | `kind`, `ref_id`, `status`, `progress`, `message`, `log` |
| `setting` | 键值配置 | `key`, `value`（工具路径、模型路径等） |

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
