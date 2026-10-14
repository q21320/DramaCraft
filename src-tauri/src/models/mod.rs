//! 前后端共享的数据结构。
//!
//! 约定：所有结构体一律 `#[serde(rename_all = "camelCase")]`，
//! 与前端 `src/types/index.ts` 一一对应。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub root_dir: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Media {
    pub id: i64,
    pub project_id: i64,
    pub path: String,
    pub name: String,
    pub duration_ms: i64,
    pub width: i64,
    pub height: i64,
    pub has_audio: bool,
    pub size_bytes: i64,
    pub format_name: String,
    pub created_at: String,
}

/// ffprobe 探测结果（尚未落库）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MediaProbe {
    pub duration_ms: i64,
    pub width: i64,
    pub height: i64,
    pub has_audio: bool,
    pub size_bytes: i64,
    pub format_name: String,
}

/// 一片的切点
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlicePlan {
    pub index: i64,
    pub start_ms: i64,
    pub end_ms: i64,
}

impl SlicePlan {
    pub fn duration_ms(&self) -> i64 {
        self.end_ms - self.start_ms
    }
}

/// 切片方式。前端发送 `{ mode: "everySeconds", seconds: 60 }` 这类形状。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum SliceMode {
    /// 每 N 秒一片，尾部不足一片并入上一片
    EverySeconds { seconds: f64 },
    /// 总时长等分 N 份
    EqualParts { parts: i64 },
    /// 手动切点（毫秒），自动补齐首尾
    Manual { points: Vec<i64> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub id: i64,
    pub media_id: i64,
    pub index_no: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub out_path: String,
    pub size_bytes: i64,
    pub status: String,
    pub message: String,
    /// 画面处理前的原始文件路径；为空表示还没处理过。用于「还原」。
    #[serde(default)]
    pub original_path: String,
}

// ---------------- 画面处理（差异化） ----------------

/// 画面处理选项。每个开关独立，全部处于默认值时表示不做任何处理。
///
/// 越界数值会在 [`crate::transform::normalize`] 里被夹到安全范围，
/// 不会把用户输入原样拼进命令行。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformOptions {
    /// 水平镜像
    pub mirror: bool,
    /// 四边各裁掉的比例（0.00 ~ 0.10），裁完再放大回原尺寸
    pub crop_ratio: f64,
    /// 整体缩放（0.96 ~ 1.04）
    pub scale_ratio: f64,
    /// 亮度增量（-0.05 ~ 0.05）
    pub brightness: f64,
    /// 对比度（0.95 ~ 1.05）
    pub contrast: f64,
    /// 饱和度（0.95 ~ 1.05）
    pub saturation: f64,
    /// 噪点强度（0 ~ 10）
    pub noise: u32,
    /// 播放速度（0.90 ~ 1.10）
    pub speed: f64,
    /// 音量（0.80 ~ 1.20）
    pub volume: f64,
}

impl Default for TransformOptions {
    /// 中性值：什么都不改。注意不能 derive —— `f64::default()` 是 0.0，
    /// 而对比度/饱和度的中性值是 1.0，用派生默认值会把画面压成全黑。
    fn default() -> Self {
        Self {
            mirror: false,
            crop_ratio: 0.0,
            scale_ratio: 1.0,
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            noise: 0,
            speed: 1.0,
            volume: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformReport {
    pub total: i64,
    pub processed: i64,
    pub failed: i64,
    pub errors: Vec<String>,
    pub segments: Vec<Segment>,
}

// ---------------- 字幕 ----------------

/// 一条字幕。`id` 为 0 表示尚未落库；文本可含换行（SRT 的多行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cue {
    pub id: i64,
    pub media_id: i64,
    pub index_no: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

// ---------------- 外部工具 ----------------

/// 外部工具路径（全部可由用户在设置里覆盖）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolPaths {
    pub ffmpeg: String,
    pub ffprobe: String,
    pub python: String,
}

/// 工具可用性报告
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolReport {
    pub name: String,
    pub path: String,
    pub available: bool,
    pub version: String,
    pub detail: String,
}

// ---------------- ASR ----------------

/// ASR 运行时状态
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AsrStatus {
    pub available: bool,
    pub python: String,
    pub detail: String,
}

/// 发给 Python worker 的请求（一行 JSON）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AsrRequest {
    pub audio: String,
    pub model: String,
    pub language: String,
    /// 模型下载/缓存目录，留空则用 worker 默认
    pub model_dir: String,
    /// auto / cpu / cuda
    pub device: String,
    /// 例如 int8 / float16 / int8_float16
    pub compute_type: String,
}

/// worker 返回的应答
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AsrResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub segments: Vec<AsrSegment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AsrSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// 一次转写的结果概要（返回给前端）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AsrOutcome {
    pub cue_count: i64,
    pub language: String,
    pub model: String,
    pub duration_ms: i64,
    pub log: String,
}

// ---------------- 导出 ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum AspectMode {
    /// 保持原始画幅
    Keep,
    /// 转 9:16 竖屏，上下用模糊背景填充
    VerticalBlur,
    /// 转 9:16 竖屏，等比放大后居中裁切
    VerticalCrop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOptions {
    pub media_id: i64,
    pub out_path: String,
    /// 是否把当前字幕烧进画面
    pub burn_subtitles: bool,
    pub aspect: AspectMode,
    /// 只导出这一段（毫秒）；两端都为 0 表示整片
    pub start_ms: i64,
    pub end_ms: i64,
    pub crf: u32,
    pub preset: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOutcome {
    pub out_path: String,
    pub size_bytes: i64,
    pub duration_ms: i64,
    pub filter: String,
    pub log: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub data_dir: String,
}
