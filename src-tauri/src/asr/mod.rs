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
//! 成功失败都必须写应答，不把 traceback 直接抛给宿主；宿主只认 stdout 里最后一个
//! 以 `{` 开头的行，其余（含进度日志）忽略。

use crate::error::{AppError, AppResult};
use crate::models::{AsrRequest, AsrResponse, AsrStatus, Cue};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const DEFAULT_MODEL: &str = "small";
pub const DEFAULT_TIMEOUT_SECS: u64 = 1800;
const STDERR_KEEP: usize = 600;

pub fn default_timeout() -> Duration {
    Duration::from_secs(DEFAULT_TIMEOUT_SECS)
}

fn tail(text: &str) -> String {
    let t = text.trim();
    if t.chars().count() <= STDERR_KEEP {
        return t.to_string();
    }
    let skip = t.chars().count() - STDERR_KEEP;
    format!("…{}", t.chars().skip(skip).collect::<String>())
}

#[cfg(windows)]
fn no_window(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW)
}

#[cfg(not(windows))]
fn no_window(cmd: &mut Command) -> &mut Command {
    cmd
}

/// 检查指定 Python 是否具备 faster-whisper。
pub fn status(python: &str) -> AsrStatus {
    let python = python.trim();
    if python.is_empty() {
        return AsrStatus {
            available: false,
            python: String::new(),
            detail: "未指定 Python 运行时，ASR 功能不可用。请在「设置」里选择 python.exe".into(),
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
    match no_window(Command::new(exe).arg("-c").arg(probe)).output() {
        Ok(out) if out.status.success() => {
            let ver = String::from_utf8_lossy(&out.stdout).trim().to_string();
            AsrStatus {
                available: true,
                python: python.to_string(),
                detail: format!("faster-whisper {ver} 就绪"),
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

/// 调用 worker 转写。超时会终止子进程并报错，不会一直挂着。
pub fn transcribe(
    python: &Path,
    worker: &Path,
    req: &AsrRequest,
    timeout: Duration,
) -> AppResult<AsrResponse> {
    if !python.is_file() {
        return Err(AppError::ToolMissing("python".into()));
    }
    if !worker.is_file() {
        return Err(AppError::Msg(format!(
            "找不到 ASR worker 脚本：{}",
            worker.display()
        )));
    }

    let payload = serde_json::to_string(req)?;
    let mut child = no_window(
        Command::new(python)
            .arg(worker)
            // 中文 Windows 上 Python 默认用 GBK 写管道，这里强制 UTF-8，
            // 否则识别出来的中文字幕会变成乱码（甚至整段丢失）。
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .spawn()
    .map_err(|e| AppError::Process(format!("无法启动 Python 子进程：{e}")))?;

    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::Process("无法打开 worker 的 stdin".into()))?;
        stdin.write_all(payload.as_bytes())?;
        stdin.write_all(b"\n")?;
        // stdin 在此 drop，worker 读到 EOF 后开始干活
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AppError::Process("无法打开 worker 的 stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AppError::Process("无法打开 worker 的 stderr".into()))?;
    let out_reader = std::thread::spawn(move || read_all(stdout));
    let err_reader = std::thread::spawn(move || read_all(stderr));

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(AppError::Process(format!(
                        "ASR 超时（超过 {} 秒），已终止子进程",
                        timeout.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(150));
            }
            Err(e) => return Err(AppError::Process(format!("等待 ASR 子进程失败：{e}"))),
        }
    };

    let stdout_text = out_reader.join().unwrap_or_default();
    let stderr_text = err_reader.join().unwrap_or_default();

    // worker 约定：无论成功失败都吐一行 JSON
    let line = stdout_text
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'));
    let Some(line) = line else {
        return Err(AppError::Process(format!(
            "ASR worker 没有返回结果（退出码 {:?}）：{}",
            status.code(),
            tail(&stderr_text)
        )));
    };

    let parsed: AsrResponse = serde_json::from_str(line.trim()).map_err(|e| {
        AppError::Process(format!("ASR 输出无法解析：{e}；原始输出：{}", tail(line)))
    })?;

    if !parsed.ok {
        let reason = if parsed.error.trim().is_empty() {
            "ASR 失败（worker 未给出原因）".to_string()
        } else {
            parsed.error.clone()
        };
        return Err(AppError::Process(reason));
    }
    Ok(parsed)
}

fn read_all<R: Read>(mut reader: R) -> String {
    // 按字节读再宽松解码：万一子进程还是吐了非 UTF-8（比如用户手改过 worker），
    // 也要拿到能看的内容，而不是因为解码失败把整段输出丢掉。
    let mut buf = Vec::new();
    let _ = reader.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).to_string()
}

/// worker 的段落 → 字幕条目。纯函数，便于测试。
///
/// 规则：丢弃空文本；倒置区间自动交换；负数起点夹到 0；按起点排序后重新编号。
pub fn response_to_cues(resp: &AsrResponse, media_id: i64) -> Vec<Cue> {
    let mut cues: Vec<Cue> = resp
        .segments
        .iter()
        .filter_map(|seg| {
            let text = seg.text.trim();
            if text.is_empty() {
                return None;
            }
            let start = (seg.start.max(0.0) * 1000.0).round() as i64;
            let end = (seg.end.max(0.0) * 1000.0).round() as i64;
            Some(Cue {
                id: 0,
                media_id,
                index_no: 0,
                start_ms: start.min(end),
                end_ms: start.max(end),
                text: text.to_string(),
            })
        })
        .collect();

    cues.sort_by_key(|c| (c.start_ms, c.end_ms));
    for (i, cue) in cues.iter_mut().enumerate() {
        cue.index_no = i as i64 + 1;
    }
    cues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AsrSegment;

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

    /// 子进程输出不是 UTF-8 时也要能拿到内容，而不是整段丢掉。
    /// （真实事故：中文 Windows 上 Python 用 GBK 写管道，read_to_string 直接失败。）
    #[test]
    fn read_all_survives_invalid_utf8() {
        let bytes: Vec<u8> = vec![0xff, 0xfe, b'{', b'"', b'k', b'"', b':', b'1', b'}'];
        let text = read_all(std::io::Cursor::new(bytes));
        assert!(text.contains("{\"k\":1}"), "实际：{text}");
    }

    #[test]
    fn drops_blank_text_and_fixes_inverted_and_negative_ranges() {        let cues = response_to_cues(
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
    /// 需要一台有 python 的机器；找不到就跳过，不让测试套件变红。
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
        let req = AsrRequest {
            audio: "E:/fake.wav".into(),
            model: "tiny".into(),
            language: "zh".into(),
            model_dir: String::new(),
            device: "cpu".into(),
            compute_type: "int8".into(),
        };
        let out = transcribe(&python, &worker, &req, Duration::from_secs(30)).unwrap();
        assert_eq!(out.model, "tiny");
        assert_eq!(out.segments.len(), 1);
        let cues = response_to_cues(&out, 3);
        assert_eq!(cues[0].text, "你好");
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
        let req = AsrRequest {
            audio: "E:/fake.wav".into(),
            model: "tiny".into(),
            language: "zh".into(),
            model_dir: String::new(),
            device: "cpu".into(),
            compute_type: "int8".into(),
        };
        let err = transcribe(&python, &worker, &req, Duration::from_secs(30))
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
        let req = AsrRequest {
            audio: "E:/fake.wav".into(),
            model: "tiny".into(),
            language: "zh".into(),
            model_dir: String::new(),
            device: "cpu".into(),
            compute_type: "int8".into(),
        };
        let started = Instant::now();
        let err = transcribe(&python, &worker, &req, Duration::from_secs(2))
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
