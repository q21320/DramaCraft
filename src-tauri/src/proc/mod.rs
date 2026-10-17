//! Python 子进程桥的公共实现。
//!
//! 三件事必须一起做，每一件都是踩过坑才加上的：
//!
//! 1. **强制 UTF-8**：中文 Windows 上 Python 往管道写 stdout 用的是 GBK，
//!    宿主按 UTF-8 读会解码失败、整段输出丢失（退出码还是 0，极难查）。
//! 2. **指定干净的工作目录**：子进程默认继承宿主的 CWD。如果 CWD 里恰好有
//!    别的 Python 版本的 `.pyd` / `python3x.dll`（比如同一个盘上装了另一个
//!    打包成 exe 的 Python 程序），子进程 import 会直接报
//!    "Module use of python38.dll conflicts with this version of Python"。
//! 3. **超时终止**：模型或第三方库卡死时，不能把界面一起拖住。

use crate::error::{AppError, AppResult};
use serde::Serialize;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const STDERR_KEEP: usize = 800;

pub fn tail(text: &str) -> String {
    let t = text.trim();
    if t.chars().count() <= STDERR_KEEP {
        return t.to_string();
    }
    let skip = t.chars().count() - STDERR_KEEP;
    format!("…{}", t.chars().skip(skip).collect::<String>())
}

#[cfg(windows)]
pub fn no_window(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW)
}

#[cfg(not(windows))]
pub fn no_window(cmd: &mut Command) -> &mut Command {
    cmd
}

/// 把工作目录钉在一个干净的地方：优先脚本所在目录，否则用临时目录。
fn pin_cwd(cmd: &mut Command, preferred: Option<&Path>) {
    match preferred {
        Some(dir) if dir.is_dir() => {
            cmd.current_dir(dir);
        }
        _ => {
            cmd.current_dir(std::env::temp_dir());
        }
    }
}

/// `python -c ...` 这类内联探测：UTF-8 环境 + 临时目录作为 CWD。
pub fn python_command(python: &Path) -> Command {
    let mut cmd = Command::new(python);
    cmd.env("PYTHONUTF8", "1")
        .env("PYTHONIOENCODING", "utf-8")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    pin_cwd(&mut cmd, None);
    cmd
}

/// 跑一个脚本：UTF-8 环境 + 以脚本所在目录为 CWD。
pub fn python_script_command(python: &Path, script: &Path) -> Command {
    let mut cmd = python_command(python);
    cmd.arg(script);
    pin_cwd(&mut cmd, script.parent());
    cmd
}

/// 按字节读再宽松解码：即使对面吐了非 UTF-8，也要拿到能看的内容。
/// 用系统默认程序打开一个文件。
///
/// 应用内的播放器适合快速过一眼；要看细节、拖时间轴，还是系统播放器顺手。
pub fn open_with_system(path: &Path) -> AppResult<()> {
    #[cfg(windows)]
    {
        // 注意 cmd /c start 的第一个参数是窗口标题 —— 传空串占位，
        // 否则带引号的路径会被当成标题，什么都不会打开。
        let mut cmd = Command::new("cmd");
        no_window(&mut cmd);
        cmd.args(["/C", "start", ""]).arg(path);
        cmd.spawn()
            .map_err(|e| AppError::Msg(format!("打不开文件：{e}")))?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        Command::new(opener)
            .arg(path)
            .spawn()
            .map_err(|e| AppError::Msg(format!("打不开文件：{e}")))?;
        Ok(())
    }
}

/// 在资源管理器里定位并选中这个文件。
pub fn reveal_in_explorer(path: &Path) -> AppResult<()> {
    #[cfg(windows)]
    {
        // **路径必须用引号包起来。**
        //
        // 踩过：`explorer /select,D:\a b\c.mp4` 或路径里带逗号、全角标点时，
        // explorer 解析不了这个参数，于是**退回打开「文档」文件夹** ——
        // 用户点「打开文件夹」看到的是文档目录，还以为程序坏了。
        //
        // Rust 的 `arg()` 会按它自己的规则转义，而 explorer 只认
        // 「/select,"完整路径"」这种原始形式，所以用 `raw_arg` 原样传。
        use std::os::windows::process::CommandExt as _;
        let mut cmd = Command::new("explorer");
        no_window(&mut cmd);
        cmd.raw_arg(format!("/select,\"{}\"", path.to_string_lossy()));
        // explorer 选中成功也常返回非 0，所以不看退出码
        cmd.spawn()
            .map_err(|e| AppError::Msg(format!("打不开资源管理器：{e}")))?;
        return Ok(());
    }
    #[cfg(not(windows))]
    {
        let parent = path.parent().unwrap_or(path);
        Command::new("xdg-open")
            .arg(parent)
            .spawn()
            .map_err(|e| AppError::Msg(format!("打不开文件管理器：{e}")))?;
        Ok(())
    }
}

pub fn read_all<R: Read>(mut reader: R) -> String {
    let mut buf = Vec::new();
    let _ = reader.read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).to_string()
}

/// 一次「一行 JSON 进、一行 JSON 出」的子进程调用。
///
/// 返回完整应答对象；`ok == false` 时转成可读错误。超时会终止子进程。
pub fn run_json_worker(
    python: &Path,
    script: &Path,
    payload: &impl Serialize,
    timeout: Duration,
) -> AppResult<serde_json::Value> {
    if !python.is_file() {
        return Err(AppError::ToolMissing("python".into()));
    }
    if !script.is_file() {
        return Err(AppError::Msg(format!(
            "找不到 worker 脚本：{}",
            script.display()
        )));
    }

    let line = serde_json::to_string(payload)?;
    let mut spawn_cmd = python_script_command(python, script);
    // 我们需要把请求写进子进程；python_command 默认把 stdin 接到 null，这里覆盖回来。
    spawn_cmd.stdin(Stdio::piped());

    let mut child = no_window(&mut spawn_cmd)
        .spawn()
        .map_err(|e| AppError::Process(format!("无法启动 Python 子进程：{e}")))?;

    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::Process("无法打开 worker 的 stdin".into()))?;
        stdin.write_all(line.as_bytes())?;
        stdin.write_all(b"\n")?;
        // stdin 在这里 drop，worker 读到 EOF 后开始干活
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
                        "子进程超时（超过 {} 秒），已终止",
                        timeout.as_secs()
                    )));
                }
                std::thread::sleep(Duration::from_millis(150));
            }
            Err(e) => return Err(AppError::Process(format!("等待子进程失败：{e}"))),
        }
    };

    let stdout_text = out_reader.join().unwrap_or_default();
    let stderr_text = err_reader.join().unwrap_or_default();

    // worker 约定：无论成功失败都吐一行 JSON；日志行一律忽略
    let json_line = stdout_text
        .lines()
        .rev()
        .find(|l| l.trim_start().starts_with('{'));
    let Some(json_line) = json_line else {
        return Err(AppError::Process(format!(
            "worker 没有返回结果（退出码 {:?}）：{}",
            status.code(),
            tail(&stderr_text)
        )));
    };

    let value: serde_json::Value = serde_json::from_str(json_line.trim()).map_err(|e| {
        AppError::Process(format!("worker 输出无法解析：{e}；原始输出：{}", tail(json_line)))
    })?;

    let ok = value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    if !ok {
        let reason = value
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        return Err(AppError::Process(if reason.is_empty() {
            "worker 报告失败但没有给出原因".to_string()
        } else {
            reason
        }));
    }
    Ok(value)
}

/// 把文件路径写进 filtergraph 参数的写法，Windows 路径尤其需要：
/// 反斜杠转正斜杠、冒号转义、单引号按 ffmpeg 的规矩断开再转义，最后整体加引号。
pub fn filter_path(path: &str) -> String {
    let escaped = path
        .replace('\\', "/")
        .replace(':', "\\:")
        .replace('\'', "'\\''");
    format!("'{escaped}'")
}

#[cfg(test)]
mod filter_path_tests {
    use super::filter_path;

    #[test]
    fn escapes_windows_paths_for_filtergraphs() {
        assert_eq!(
            filter_path("C:/Windows/Fonts/simfang.ttf"),
            "'C\\:/Windows/Fonts/simfang.ttf'"
        );
        assert_eq!(filter_path(r"C:\luts\my.cube"), "'C\\:/luts/my.cube'");
        assert_eq!(filter_path("a/b.cube"), "'a/b.cube'");
    }

    #[test]
    fn single_quotes_are_escaped_not_left_to_break_the_graph() {
        // 直接塞进去会让引号提前闭合，必须写成 '\''
        assert_eq!(filter_path("a'b.cube"), "'a'\\''b.cube'");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_all_survives_invalid_utf8() {
        // 真实事故：对面用 GBK 写，宿主按 UTF-8 读会整段丢掉
        let bytes: Vec<u8> = vec![0xff, 0xfe, b'{', b'"', b'k', b'"', b':', b'1', b'}'];
        let text = read_all(std::io::Cursor::new(bytes));
        assert!(text.contains("{\"k\":1}"), "实际：{text}");
    }

    #[test]
    fn tail_keeps_the_end_and_truncates_huge_input() {
        let text = "x".repeat(2_000);
        let t = tail(&text);
        assert!(t.starts_with('…'));
        assert!(t.chars().count() <= STDERR_KEEP + 1);
        assert_eq!(tail("  hi  "), "hi");
    }

    #[test]
    fn script_command_pins_cwd_to_script_directory() {
        // 这条是防回归：CWD 里混进别的 Python 版本的 .pyd 会让子进程 import 崩
        let script = std::env::temp_dir().join("dramadist_proc_test").join("w.py");
        std::fs::create_dir_all(script.parent().unwrap()).unwrap();
        std::fs::write(&script, "print('x')").unwrap();
        let cmd = python_script_command(Path::new("python.exe"), &script);
        assert_eq!(cmd.get_current_dir(), script.parent());
    }

    #[test]
    fn missing_script_is_reported_before_spawning() {
        // 用一个存在的假 python，专门测"脚本缺失"这条分支（检查顺序是 python → 脚本）
        let dir = std::env::temp_dir().join("dramadist_proc_test");
        std::fs::create_dir_all(&dir).unwrap();
        let fake_python = dir.join("fake_python.exe");
        std::fs::write(&fake_python, b"not a real executable").unwrap();

        let err = run_json_worker(
            &fake_python,
            Path::new("Z:/nope/worker.py"),
            &serde_json::json!({}),
            Duration::from_secs(5),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("找不到 worker 脚本"), "实际：{err}");
    }

    #[test]
    fn missing_python_is_reported_too() {
        let err = run_json_worker(
            Path::new("Z:/nope/python.exe"),
            Path::new("Z:/nope/worker.py"),
            &serde_json::json!({}),
            Duration::from_secs(5),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("python"), "实际：{err}");
    }
}
