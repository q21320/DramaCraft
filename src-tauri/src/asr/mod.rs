//! ASR 字幕桥（v0.2 接入）。
//!
//! 为什么走独立 Python 子进程而不是 Rust 内嵌推理：
//! 1. Whisper 的生态（faster-whisper / CTranslate2）成熟且许可干净（MIT）；
//! 2. 模型加载吃内存、可能崩，放子进程不会拖死 GUI；
//! 3. 将来换引擎（whisper.cpp / 云端 API）只需替换这一层。
//!
//! ## 子进程协议（约定，v0.2 实现）
//!
//! 请求（stdin 一行 JSON）：
//! ```json
//! { "audio": "E:/tmp/a.wav", "model": "small", "language": "zh", "out": "E:/tmp/a.json" }
//! ```
//! 应答（stdout 一行 JSON）：
//! ```json
//! { "ok": true, "out": "E:/tmp/a.json", "segments": 128 }
//! ```
//! 失败：`{ "ok": false, "error": "..." }`，绝不抛栈到宿主进程。

use crate::models::AsrStatus;
use std::path::Path;
use std::process::Command;

/// 检查指定 Python 是否具备 faster-whisper。
pub fn status(python: &str) -> AsrStatus {
    let python = python.trim();
    if python.is_empty() {
        return AsrStatus {
            available: false,
            python: String::new(),
            detail: "未指定 Python 运行时，ASR 功能未启用（v0.2 接入）".into(),
        };
    }

    let exe = Path::new(python);
    if !exe.is_file() {
        return AsrStatus {
            available: false,
            python: python.to_string(),
            detail: "指定的 Python 路径不存在，请到「设置」重新选择".into(),
        };
    }

    let probe = "import faster_whisper, sys; print(faster_whisper.__version__)";
    match Command::new(exe).arg("-c").arg(probe).output() {
        Ok(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            AsrStatus {
                available: true,
                python: python.to_string(),
                detail: format!("faster-whisper {ver} 就绪（字幕链路的接入在 v0.2）"),
            }
        }
        Ok(_) => AsrStatus {
            available: false,
            python: python.to_string(),
            detail: "该 Python 里没有安装 faster-whisper，可执行：pip install faster-whisper".into(),
        },
        Err(err) => AsrStatus {
            available: false,
            python: python.to_string(),
            detail: format!("无法执行该 Python：{err}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_python_reports_unavailable_without_panicking() {
        let s = status("   ");
        assert!(!s.available);
        assert!(s.python.is_empty());
        assert!(!s.detail.is_empty());
    }

    #[test]
    fn missing_python_path_reports_unavailable() {
        let s = status("Z:/nope/python.exe");
        assert!(!s.available);
        assert!(s.detail.contains("不存在"), "实际：{}", s.detail);
    }
}
