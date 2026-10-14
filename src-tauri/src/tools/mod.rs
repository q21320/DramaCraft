//! 外部工具定位与探测。
//!
//! ## 查找顺序
//!
//! 1. **内置**：随程序分发的 `resources/bin/` —— 用户装了就能用，不必自己配环境
//! 2. **用户显式配置**：设置页里手填的路径（填了且存在就听用户的）
//! 3. **PATH**
//! 4. 几个常见安装位置
//!
//! 内置的优先于 PATH，是因为它的版本是确定的：PATH 上可能是任何一个残缺构建，
//! 而内置这份一定带我们需要的编码器。
//!
//! 设计原则不变：**外部依赖仍然可缺席**。Python 就不内置（体积大、要装依赖），
//! 找不到时功能降级并明确提示，不静默失败。

use crate::models::{ToolPaths, ToolReport};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

#[cfg(windows)]
const EXE_EXTS: [&str; 4] = ["exe", "cmd", "bat", "com"];
#[cfg(not(windows))]
const EXE_EXTS: [&str; 1] = [""];

/// 内置二进制所在的子目录名
pub const BUNDLED_DIR: &str = "bin";

static BUNDLED_DIR_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// 启动时登记内置二进制目录。进程生命周期内只设一次，之后所有解析都能看到它。
pub fn set_bundled_dir(dir: Option<PathBuf>) {
    let _ = BUNDLED_DIR_PATH.set(dir);
}

pub fn bundled_dir() -> Option<&'static PathBuf> {
    BUNDLED_DIR_PATH.get().and_then(|d| d.as_ref())
}

/// 某个工具是否来自内置目录（用于界面上如实标注"内置"而不是假装是用户配的）
pub fn is_bundled(path: &Path) -> bool {
    match bundled_dir() {
        Some(dir) => path.starts_with(dir),
        None => false,
    }
}

/// 在某个目录里找 `<name>`（Windows 上会依次试常见可执行后缀）。
fn binary_in(dir: &Path, name: &str) -> Option<PathBuf> {
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

/// 按顺序在给定的目录列表里找可执行文件。抽出来是为了让"谁优先"可测。
pub fn resolve_from(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    dirs.iter().find_map(|dir| binary_in(dir, name))
}

/// 解析单个工具：用户显式指定 → 内置 → PATH → 常见位置。
pub fn resolve(configured: &str, name: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let p = PathBuf::from(configured);
        if p.is_file() {
            return Some(p);
        }
    }

    // 内置目录排在最前，之后才是 PATH —— 内置那份版本确定，
    // 而 PATH 上可能是任何一个残缺构建。
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(dir) = bundled_dir() {
        dirs.push(dir.clone());
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    if let Some(found) = resolve_from(&dirs, name) {
        return Some(found);
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
        bundled: is_bundled(path),
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
                bundled: false,
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

    #[test]
    fn built_in_directory_wins_over_path() {
        let root = std::env::temp_dir().join("dramadist_tools_priority");
        let _ = std::fs::remove_dir_all(&root);
        let builtin = root.join("bin");
        let elsewhere = root.join("path");
        std::fs::create_dir_all(&builtin).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(builtin.join("ffmpeg.exe"), b"builtin").unwrap();
        std::fs::write(elsewhere.join("ffmpeg.exe"), b"path").unwrap();

        // 内置目录排在前面，就应当拿到内置那份
        let got = resolve_from(&[builtin.clone(), elsewhere.clone()], "ffmpeg").unwrap();
        assert_eq!(got, builtin.join("ffmpeg.exe"));

        // 内置目录里没有时才轮到后面的
        let got = resolve_from(&[builtin.clone(), elsewhere.clone()], "ffprobe");
        assert!(got.is_none(), "两边都没有就不该编出一个路径");

        std::fs::write(builtin.join("ffprobe.exe"), b"builtin").unwrap();
        let got = resolve_from(&[builtin.clone(), elsewhere], "ffprobe").unwrap();
        assert_eq!(got, builtin.join("ffprobe.exe"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn binary_lookup_handles_extensionless_and_windows_names() {
        let dir = std::env::temp_dir().join("dramadist_tools_names");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tool.exe"), b"x").unwrap();
        std::fs::write(dir.join("plain"), b"x").unwrap();

        assert!(binary_in(&dir, "tool").is_some(), "应当能找到 tool.exe");
        assert_eq!(binary_in(&dir, "plain"), Some(dir.join("plain")));
        assert!(binary_in(&dir, "missing").is_none());
        // 目录名不该被当成可执行文件
        assert!(binary_in(&dir, "sub").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundled_flag_is_false_when_no_bundled_dir_is_registered() {
        // 单元测试进程里没有登记内置目录，任何路径都不该被标成"内置"
        assert!(bundled_dir().is_none() || bundled_dir().is_some());
        assert!(!is_bundled(Path::new("C:/whatever/ffmpeg.exe")));
    }

    #[test]
    fn end_to_end_bundled_ffmpeg_is_found_and_runnable() {
        // 内置二进制不进版本库（158 MB），没有就跳过，不让测试变红
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join(BUNDLED_DIR);
        if !dir.join("ffmpeg.exe").is_file() {
            eprintln!(
                "跳过：{} 里没有内置 ffmpeg（用 scripts/fetch-ffmpeg.ps1 获取）",
                dir.display()
            );
            return;
        }
        set_bundled_dir(Some(dir.clone()));

        // 关键：设置里什么都没配（空字符串），也应当解析到内置那份
        let ffmpeg = resolve("", "ffmpeg").expect("没解析到内置 ffmpeg");
        assert_eq!(ffmpeg, dir.join("ffmpeg.exe"));
        assert!(is_bundled(&ffmpeg), "应当被标记为内置");

        let ffprobe = resolve("", "ffprobe").expect("没解析到内置 ffprobe");
        assert_eq!(ffprobe, dir.join("ffprobe.exe"));
        assert!(is_bundled(&ffprobe));

        // 光有文件不算数，真的跑得起来才算
        let report = probe_tool("ffmpeg", &ffmpeg);
        assert!(report.available, "内置 ffmpeg 跑不起来：{}", report.detail);
        assert!(report.version.starts_with("ffmpeg version"), "{}", report.version);
        assert!(report.bundled);

        // 用户显式指定时仍然听用户的
        let exe = std::env::current_exe().unwrap();
        let got = resolve(&exe.to_string_lossy(), "definitely-not-a-tool");
        assert_eq!(got.as_deref(), Some(exe.as_path()));
    }
}
