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
}

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

/// ASR 运行时状态
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AsrStatus {
    pub available: bool,
    pub python: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub data_dir: String,
}
