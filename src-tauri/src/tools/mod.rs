//! 外部工具定位与探测。
//!
//! 设计原则：**外部依赖可缺席**。找不到 ffmpeg / python 时功能降级并明确提示，
//! 不静默失败，也不内置来源不明的二进制。

use crate::models::{ToolPaths, ToolReport};
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
const EXE_EXTS: [&str; 4] = ["exe", "cmd", "bat", "com"];
#[cfg(not(windows))]
const EXE_EXTS: [&str; 1] = [""];

/// 在 PATH 中查找可执行文件。
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let direct = dir.join(name);
        if direct.is_file() {
            return Some(direct);
        }
        for ext in EXE_EXTS {
            if ext.is_empty() {
                continue;
            }
            let candidate = dir.join(format!("{name}.{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// 除了 PATH，再探几个常见安装位置，减少用户手工配置。
fn common_locations(name: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let file = format!("{name}.exe");
    let roots = [
        "C:/ffmpeg/bin",
        "D:/ffmpeg/bin",
        "E:/ffmpeg/bin",
        "C:/Program Files/ffmpeg/bin",
        "C:/Program Files (x86)/ffmpeg/bin",
    ];
    for r in roots {
        out.push(PathBuf::from(r).join(&file));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let base = PathBuf::from(local);
        out.push(base.join("Microsoft/WinGet/Links").join(&file));
    }
    if let Some(pf) = std::env::var_os("ProgramFiles") {
        out.push(PathBuf::from(pf).join("ffmpeg/bin").join(&file));
    }
    out
}

/// 解析单个工具：用户显式指定的路径优先，其次 PATH，最后常见位置。
pub fn resolve(configured: &str, name: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(p) = which(name) {
        return Some(p);
    }
    common_locations(name).into_iter().find(|p| p.is_file())
}

/// 首次启动时的默认工具路径（全部尽力而为，允许为空）。
pub fn initial_paths() -> ToolPaths {
    ToolPaths {
        ffmpeg: resolve("", "ffmpeg")
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        ffprobe: resolve("", "ffprobe")
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
        python: resolve("", "python")
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default(),
    }
}

/// 跑一次 `--version`，确认工具真的能用（不只是文件存在）。
pub fn probe_tool(name: &str, path: &Path) -> ToolReport {
    let mut report = ToolReport {
        name: name.to_string(),
        path: path.to_string_lossy().to_string(),
        available: false,
        version: String::new(),
        detail: String::new(),
    };

    match Command::new(path).arg("-version").output() {
        Ok(out) => {
            let text = if out.stdout.is_empty() {
                String::from_utf8_lossy(&out.stderr).to_string()
            } else {
                String::from_utf8_lossy(&out.stdout).to_string()
            };
            let first = text.lines().next().unwrap_or_default().trim().to_string();
            if out.status.success() || !first.is_empty() {
                report.available = true;
                report.version = first;
                report.detail = "可执行".into();
            } else {
                report.detail = format!("退出码 {:?}", out.status.code());
            }
        }
        Err(err) => {
            report.detail = format!("无法执行：{err}");
        }
    }
    report
}

/// 探测一组工具，返回逐个报告。缺失的项也会出现在结果里。
pub fn probe_all(paths: &ToolPaths) -> Vec<ToolReport> {
    let mut out = Vec::new();
    for (name, configured) in [
        ("ffmpeg", &paths.ffmpeg),
        ("ffprobe", &paths.ffprobe),
        ("python", &paths.python),
    ] {
        match resolve(configured, name) {
            Some(p) => out.push(probe_tool(name, &p)),
            None => out.push(ToolReport {
                name: name.to_string(),
                path: String::new(),
                available: false,
                version: String::new(),
                detail: format!("未找到 {name}，请到「设置」手动指定路径"),
            }),
        }
    }
    out
}

/// 取用某个工具的可执行路径，缺失时给出可读错误。
pub fn require(configured: &str, name: &str) -> Result<PathBuf, crate::error::AppError> {
    resolve(configured, name).ok_or_else(|| crate::error::AppError::ToolMissing(name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_prefers_existing_configured_path() {
        let exe = std::env::current_exe().unwrap();
        let got = resolve(&exe.to_string_lossy(), "definitely-not-a-tool");
        assert_eq!(got.as_deref(), Some(exe.as_path()));
    }

    #[test]
    fn resolve_ignores_bogus_configured_path() {
        // 配置了不存在的路径时不应直接返回它，而应回退到查找结果或 None
        let got = resolve("Z:/nope/definitely-missing.exe", "definitely-not-a-tool-xyz");
        assert!(got.is_none());
    }

    #[test]
    fn probe_missing_tool_is_reported_not_panicked() {
        // 配置了不存在的路径时：不 panic、三个工具都有报告、不可用的必须带可读原因。
        // 注意不能断言"全部不可用"——本机 PATH 上可能真的装了 python。
        let reports = probe_all(&ToolPaths {
            ffmpeg: "Z:/nope/ffmpeg.exe".into(),
            ffprobe: "Z:/nope/ffprobe.exe".into(),
            python: "Z:/nope/python.exe".into(),
        });
        assert_eq!(reports.len(), 3);
        for r in reports.iter().filter(|r| !r.available) {
            assert!(!r.detail.is_empty(), "{} 不可用但没有给出原因", r.name);
        }
    }

    #[test]
    fn bogus_configured_path_falls_back_instead_of_being_trusted() {
        // 用户填了错路径时，不应把它当成可用工具直接返回
        let reports = probe_all(&ToolPaths {
            ffmpeg: "Z:/nope/ffmpeg.exe".into(),
            ffprobe: "Z:/nope/ffprobe.exe".into(),
            python: "Z:/nope/python.exe".into(),
        });
        for r in &reports {
            assert_ne!(
                r.path, "Z:/nope/ffmpeg.exe",
                "{} 不应直接采用不存在的配置路径",
                r.name
            );
        }
    }
}
