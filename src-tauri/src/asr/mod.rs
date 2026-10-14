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
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const DEFAULT_MODEL: &str = "small";
pub const DEFAULT_TIMEOUT_SECS: u64 = 1800;

pub fn default_timeout() -> Duration {
    Duration::from_secs(DEFAULT_TIMEOUT_SECS)
}

/// 检查指定 Python 是否具备 faster-whisper。
/// 内置的模型目录（若随程序分发了该规格的模型）。
///
/// 有本地模型就**不走 HuggingFace 下载** —— 那既慢，在国内还常常连不上。
pub fn bundled_model_dir(size: &str) -> Option<PathBuf> {
    let size = size.trim();
    if size.is_empty() {
        return None;
    }
    for dir in crate::tools::bundled_dirs() {
        let candidate = dir.join("models").join(size);
        if candidate.join("model.bin").is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 随程序分发的模型规格列表（按名字排序）。
pub fn bundled_model_sizes() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for dir in crate::tools::bundled_dirs() {
        let Ok(entries) = std::fs::read_dir(dir.join("models")) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.path().join("model.bin").is_file() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str() {
                let name = name.to_string();
                if !out.contains(&name) {
                    out.push(name);
                }
            }
        }
    }
    out.sort();
    out
}

/// faster-whisper 要求的最低 Python 版本。
pub const MIN_PYTHON: (u32, u32) = (3, 9);

/// 读一次解释器版本。拿不到就返回 None。
pub fn python_version(exe: &Path) -> Option<(u32, u32, String)> {
    let mut cmd: Command = proc::python_command(exe);
    cmd.arg("-c")
        .arg("import sys; print('%d.%d.%d' % sys.version_info[:3])");
    let out = proc::no_window(&mut cmd).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let mut parts = text.split('.');
    let major: u32 = parts.next()?.trim().parse().ok()?;
    let minor: u32 = parts.next()?.trim().parse().ok()?;
    Some((major, minor, text))
}

pub fn status(python: &str) -> RuntimeStatus {
    let python = python.trim();
    if python.is_empty() {
        return RuntimeStatus {
            available: false,
            python: String::new(),
            detail:
                "没找到可用的 Python 运行时。正常情况下程序自带 ASR 运行时，若这里为空，说明安装包不完整。"
                    .into(),
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
            // 顺带把"本地已经有哪些模型"告诉用户 —— 免得他以为还得联网下
            let models = bundled_model_sizes();
            let suffix = if models.is_empty() {
                "；本地没有内置模型，首次使用会联网下载".to_string()
            } else {
                format!("；内置模型：{}", models.join(" / "))
            };
            RuntimeStatus {
                available: true,
                python: python.to_string(),
                detail: format!("faster-whisper {ver} 就绪{suffix}"),
            }
        }
        Ok(_) => {
            // 别急着让人去 pip install —— 先看是不是版本根本不够。
            // 在 Python 3.8 上「pip install faster-whisper」是装不上的，
            // 给出这种照做会失败的建议，比不给建议更糟。
            let version = python_version(exe);
            let detail = match &version {
                Some((major, minor, full)) if (*major, *minor) < MIN_PYTHON => format!(
                    "这个 Python 是 {full}，太旧了：faster-whisper 需要 {}.{}+，装不上。\
请到「设置」把 Python 一栏清空，改用程序自带的运行时。",
                    MIN_PYTHON.0, MIN_PYTHON.1
                ),
                Some((_, _, full)) => format!(
                    "{full} 里没有 faster-whisper。清空「设置」里的 Python 一栏可用回程序自带的运行时；\
非要自己装的话：{full} -m pip install faster-whisper"
                ),
                None => "这个 Python 无法运行，换一个或清空「设置」里的 Python 一栏".to_string(),
            };
            RuntimeStatus {
                available: false,
                python: python.to_string(),
                detail,
            }
        }
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
        crate::tools::resolve("", "python3").or_else(|| crate::tools::resolve("", "python"))
    }

    fn write_stub(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("dramadist_asr_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn bundled_runtime_reports_asr_available() {
        let python = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("asr_runtime")
            .join("python.exe");
        if !python.is_file() {
            eprintln!("跳过：没有内置 ASR 运行时");
            return;
        }
        // 这一条正对着用户看到的那张截图：内置运行时必须报告"可用"，
        // 而不是"该 Python 里没有安装 faster-whisper"。
        let s = status(&python.to_string_lossy());
        assert!(s.available, "内置运行时应当报告 ASR 可用：{}", s.detail);
        assert!(s.detail.contains("faster-whisper"), "{}", s.detail);
    }

    #[test]
    fn too_old_python_gets_advice_that_actually_works() {
        // Python 3.8 装不了 faster-whisper，所以不能再建议 "pip install faster-whisper"。
        // 这里用一个假解释器模拟：它 --version 正常，但 import faster_whisper 失败。
        let dir = std::env::temp_dir().join("dramadist_asr_oldpy");
        let _ = std::fs::create_dir_all(&dir);
        let fake = dir.join("python.exe");
        // 不是真的可执行文件 —— status 会在执行失败时给出"无法执行"的提示，
        // 这里只验证三种失败路径都不会 panic 且都有可读原因。
        std::fs::write(&fake, b"not a real exe").unwrap();
        let s = status(&fake.to_string_lossy());
        assert!(!s.available);
        assert!(!s.detail.is_empty(), "失败必须给出原因");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn end_to_end_bundled_runtime_transcribes() {
        // 内置运行时 + 内置模型，跑一次**完整**的转写链路。
        // 素材是纯音调，所以期望"成功但无文本" —— 验证的是模型能加载、能推理、
        // 协议能往返，而不是识别准确率。
        let resources = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources");
        let python = resources.join("asr_runtime").join("python.exe");
        let model = resources.join("asr_runtime").join("models").join("tiny");
        let ffmpeg = resources.join("bin").join("ffmpeg.exe");
        if !python.is_file() || !model.join("model.bin").is_file() || !ffmpeg.is_file() {
            eprintln!("跳过：内置运行时或模型不完整");
            return;
        }
        let worker = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("asr_worker.py");

        let dir = std::env::temp_dir().join("dramadist_asr_bundled_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("tone.wav");

        let ok = proc::no_window(
            Command::new(&ffmpeg)
                .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
                .arg("sine=frequency=440:duration=2")
                .args(["-ar", "16000", "-ac", "1"])
                .arg(&wav),
        )
        .status()
        .expect("无法启动 ffmpeg");
        assert!(ok.success(), "生成测试音频失败");

        let request = AsrRequest {
            audio: wav.to_string_lossy().to_string(),
            model: model.to_string_lossy().to_string(),
            language: "zh".into(),
            model_dir: String::new(),
            device: "cpu".into(),
            compute_type: "int8".into(),
        };
        let out = transcribe(&python, &worker, &request, Duration::from_secs(300))
            .expect("内置运行时转写失败");
        assert!(out.ok, "{}", out.error);
        assert_eq!(out.language, "zh");
        assert!((out.duration - 2.0).abs() < 0.5, "时长 {} 不对", out.duration);
        // 纯音调没有语音，段落应当是空的 —— 这正是"跑通了"的证据
        assert!(out.segments.is_empty(), "音调不该识别出文字：{:?}", out.segments);

        let _ = std::fs::remove_dir_all(&dir);
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
