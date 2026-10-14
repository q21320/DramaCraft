//! ASR 字幕桥：把转写丢给独立 Python 子进程。
//!
//! 为什么走子进程而不是 Rust 内嵌推理：
//! 1. Whisper 生态（faster-whisper / CTranslate2）成熟且许可干净（MIT）；
//! 2. 模型加载吃内存、可能崩，放子进程不会拖死 GUI；
//! 3. 将来换引擎（whisper.cpp / 云端 API）只需替换这一层。
//!
//! ## 协议
//!
//! 子进程读 **stdin 一行 JSON**（`AsrRequest`），写 **stdout 一行 JSON**（`AsrResponse`）。
//! 编码、工作目录与超时这三件事统一由 [`crate::proc`] 处理。

use crate::error::{AppError, AppResult};
use crate::models::{AsrRequest, AsrResponse, Cue, RuntimeStatus};
use crate::proc;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

pub const DEFAULT_MODEL: &str = "small";
pub const DEFAULT_TIMEOUT_SECS: u64 = 1800;

pub fn default_timeout() -> Duration {
    Duration::from_secs(DEFAULT_TIMEOUT_SECS)
}

/// 检查指定 Python 是否具备 faster-whisper。
pub fn status(python: &str) -> RuntimeStatus {
    let python = python.trim();
    if python.is_empty() {
        return RuntimeStatus {
            available: false,
            python: String::new(),
            detail: "未指定 Python 运行时，ASR 功能不可用。请在「设置」里选择 python.exe".into(),
        };
    }

    let exe = Path::new(python);
    if !exe.is_file() {
        return RuntimeStatus {
            available: false,
            python: python.to_string(),
            detail: "指定的 Python 路径不存在，请到「设置」重新选择".into(),
        };
    }

    let probe = "import faster_whisper, sys; print(faster_whisper.__version__)";
    let mut cmd: Command = proc::python_command(exe);
    cmd.arg("-c").arg(probe);

    match proc::no_window(&mut cmd).output() {
        Ok(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            RuntimeStatus {
                available: true,
                python: python.to_string(),
                detail: format!("faster-whisper {ver} 就绪"),
            }
        }
        Ok(_) => RuntimeStatus {
            available: false,
            python: python.to_string(),
            detail: "该 Python 里没有安装 faster-whisper，可执行：pip install faster-whisper".into(),
        },
        Err(err) => RuntimeStatus {
            available: false,
            python: python.to_string(),
            detail: format!("无法执行该 Python：{err}"),
        },
    }
}

/// 调用 worker 转写。超时会终止子进程并报错，不会一直挂着。
pub fn transcribe(
    python: &Path,
    worker: &Path,
    req: &AsrRequest,
    timeout: Duration,
) -> AppResult<AsrResponse> {
    let value = proc::run_json_worker(python, worker, req, timeout)?;
    serde_json::from_value(value)
        .map_err(|e| AppError::Process(format!("ASR 应答结构不符合约定：{e}")))
}

/// worker 的段落 → 字幕条目。
///
/// 清洗规则统一在 [`crate::subtitle::cues_from_timed_text`]，与云端字幕共用一份。
pub fn response_to_cues(resp: &AsrResponse, media_id: i64) -> Vec<Cue> {
    let items: Vec<(f64, f64, String)> = resp
        .segments
        .iter()
        .map(|s| (s.start, s.end, s.text.clone()))
        .collect();
    crate::subtitle::cues_from_timed_text(&items, media_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AsrSegment;
    use std::time::Instant;

    fn seg(start: f64, end: f64, text: &str) -> AsrSegment {
        AsrSegment {
            start,
            end,
            text: text.to_string(),
        }
    }

    fn resp(segments: Vec<AsrSegment>) -> AsrResponse {
        AsrResponse {
            ok: true,
            error: String::new(),
            language: "zh".into(),
            duration: 12.0,
            model: "tiny".into(),
            segments,
        }
    }

    fn req() -> AsrRequest {
        AsrRequest {
            audio: "E:/fake.wav".into(),
            model: "tiny".into(),
            language: "zh".into(),
            model_dir: String::new(),
            device: "cpu".into(),
            compute_type: "int8".into(),
        }
    }

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

    #[test]
    fn converts_segments_to_cues_and_renumbers() {
        let cues = response_to_cues(
            &resp(vec![seg(2.0, 3.5, "第二句"), seg(0.0, 1.25, "第一句")]),
            7,
        );
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "第一句");
        assert_eq!(cues[0].start_ms, 0);
        assert_eq!(cues[0].end_ms, 1_250);
        assert_eq!(cues[0].index_no, 1);
        assert_eq!(cues[1].index_no, 2);
        assert_eq!(cues[1].media_id, 7);
    }

    #[test]
    fn drops_blank_text_and_fixes_inverted_and_negative_ranges() {
        let cues = response_to_cues(
            &resp(vec![
                seg(1.0, 2.0, "   "),
                seg(5.0, 3.0, "倒置"),
                seg(-1.0, 0.5, "负起点"),
            ]),
            1,
        );
        assert_eq!(cues.len(), 2);
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (0, 500));
        assert_eq!((cues[1].start_ms, cues[1].end_ms), (3_000, 5_000));
    }

    /// 用一个假 worker 打通「写 stdin → 读 stdout → 解析」整条管道。
    /// 找不到 python 就跳过，不让测试套件变红。
    fn any_python() -> Option<std::path::PathBuf> {
        if let Ok(p) = std::env::var("DRAMADIST_TEST_PYTHON") {
            let path = std::path::PathBuf::from(p);
            if path.is_file() {
                return Some(path);
            }
        }
        crate::tools::which("python3").or_else(|| crate::tools::which("python"))
    }

    fn write_stub(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("dramadist_asr_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn talks_to_a_stub_worker_over_stdio() {
        let Some(python) = any_python() else {
            eprintln!("跳过：本机没有可用的 python");
            return;
        };
        let worker = write_stub(
            "stub_ok.py",
            r#"
import json, sys
req = json.loads(sys.stdin.readline())
print("这是一行无关的日志，宿主应当忽略", flush=True)
sys.stdout.write(json.dumps({
    "ok": True, "language": "zh", "duration": 1.0, "model": req.get("model", ""),
    "segments": [{"start": 0.0, "end": 0.8, "text": "你好"}]
}, ensure_ascii=False) + "\n")
sys.stdout.flush()
"#,
        );
        let out = transcribe(&python, &worker, &req(), Duration::from_secs(30)).unwrap();
        assert_eq!(out.model, "tiny");
        assert_eq!(out.segments.len(), 1);
        let cues = response_to_cues(&out, 3);
        assert_eq!(cues[0].text, "你好", "中文必须原样往返（编码回归）");
    }

    #[test]
    fn worker_failure_surfaces_the_reason() {
        let Some(python) = any_python() else {
            eprintln!("跳过：本机没有可用的 python");
            return;
        };
        let worker = write_stub(
            "stub_fail.py",
            r#"
import json, sys
json.loads(sys.stdin.readline())
sys.stdout.write(json.dumps({"ok": False, "error": "未安装 faster-whisper"}) + "\n")
sys.stdout.flush()
"#,
        );
        let err = transcribe(&python, &worker, &req(), Duration::from_secs(30))
            .unwrap_err()
            .to_string();
        assert!(err.contains("faster-whisper"), "实际：{err}");
    }

    #[test]
    fn hanging_worker_is_killed_by_timeout() {
        let Some(python) = any_python() else {
            eprintln!("跳过：本机没有可用的 python");
            return;
        };
        let worker = write_stub("stub_hang.py", "import time\ntime.sleep(60)\n");
        let started = Instant::now();
        let err = transcribe(&python, &worker, &req(), Duration::from_secs(2))
            .unwrap_err()
            .to_string();
        assert!(err.contains("超时"), "实际：{err}");
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "超时后应立即返回，实际耗时 {:?}",
            started.elapsed()
        );
    }
}
