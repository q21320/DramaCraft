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
/// 内置 ASR 运行时所在的子目录名（嵌入式 Python + faster-whisper + 模型）
pub const ASR_RUNTIME_DIR: &str = "asr_runtime";

/// 各平台二进制各自的子目录名：`resources/bin/<这里>/ffmpeg`。
///
/// Windows 版直接放在 `bin/` 根下（历史布局，安装包已经这样发了，不动它），
/// 其他平台按名字分开放，这样 macOS / Linux 的二进制能和 Windows 的 `.exe`
/// 并存：仓库同时服务多端时，`bin/` 里只放本机能跑的那几个是做不到的。
pub fn platform_tag() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        // 只按操作系统分，不按 CPU 分：同一份 `bin/macos/` 给 Apple Silicon 用，
        // CPU 后缀那条路留作以后真要分开发布时的扩展点。
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}

static BUNDLED_DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();

/// 启动时登记内置目录（可以有多个：bin 放 ffmpeg，asr_runtime 放 python）。
///
/// 面板上的「内置」标注以 `bin` / `asr_runtime` 两个根目录为界，下面这些
/// 平台子目录都算内置 —— 界面看起来是一条路，文件在路上放哪儿随便。
///
/// - `bin/<非 windows 平台>/`：macOS / Linux 的 ffmpeg、ffprobe
/// - `asr_runtime/<非 windows 平台>/bin/`：嵌入式 Python（python-build-standalone
///   的 install_only 布局：`python/bin/python`、`lib/python3.x/site-packages`）
///
/// **Windows 一个子目录都不展开**：它那套运行时就在 `asr_runtime\python.exe`
/// 和 `bin\ffmpeg.exe` 根下，查找顺序与旧版完全一致。
///
/// 进程生命周期内只设一次，之后所有解析都能看到它们。
pub fn set_bundled_dirs(dirs: Vec<PathBuf>) {
    let tag = platform_tag();
    let mut expanded = Vec::new();
    for dir in dirs {
        if tag == "windows" {
            expanded.push(dir);
            continue;
        }
        let name = dir.file_name().and_then(|n| n.to_str());
        if name == Some(BUNDLED_DIR) {
            let platform_dir = dir.join(tag);
            if platform_dir.is_dir() {
                expanded.push(platform_dir);
            }
        }
        if name == Some(ASR_RUNTIME_DIR) {
            let platform_dir = dir.join(tag);
            // python-build-standalone：解释器在 <平台>/bin/python
            let platform_bin = platform_dir.join("bin");
            if platform_bin.is_dir() {
                expanded.push(platform_bin);
            }
            if platform_dir.is_dir() {
                expanded.push(platform_dir);
            }
        }
        expanded.push(dir);
    }
    let _ = BUNDLED_DIRS.set(expanded);
}

pub fn bundled_dirs() -> &'static [PathBuf] {
    BUNDLED_DIRS.get().map(|v| v.as_slice()).unwrap_or(&[])
}

/// 资源根目录：开发时就是源码里的 `resources/`，打包后在启动阶段登记。
///
/// 存的是 `bin` / `asr_runtime` 的**父目录**，只为了在运行时能反推出
/// `resources/asr_runtime/<平台>/models/` 这类没被登记成"内置工具目录"的位置。
static RESOURCES_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// 启动时由 `lib.rs` 调一次。
pub fn set_resources_root(root: PathBuf) {
    let _ = RESOURCES_ROOT.set(root);
}

/// 资源根目录（可能为 None：测试进程里没登记、也没有源码目录时）。
pub fn resources_root() -> Option<PathBuf> {
    if let Some(root) = RESOURCES_ROOT.get() {
        return Some(root.clone());
    }
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources");
    dev.is_dir().then_some(dev)
}

/// 资源目录下本平台内置运行时的路径。
///
/// 返回 `(ffmpeg/ffprobe 所在目录, asr_runtime 的平台目录)`：
/// - Windows：`bin/` 与 `asr_runtime/` 本身（根下就是 exe）
/// - 其他平台：`bin/<平台>/`、`asr_runtime/<平台>/`（解释器在 `<平台>/bin/python`）
pub fn bundled_paths(resources: &Path) -> (PathBuf, PathBuf) {
    let bin = resources.join(BUNDLED_DIR);
    let runtime = resources.join(ASR_RUNTIME_DIR);
    let tag = platform_tag();
    if tag == "windows" {
        (bin, runtime)
    } else {
        (bin.join(tag), runtime.join(tag))
    }
}

/// 在资源目录里找内置 Python：`<运行时平台目录>/bin/python`（python-build-standalone 的
/// install_only 布局），找不到再看平台目录本身（解释器直接放在那儿的情况）。
pub fn bundled_python(resources: &Path) -> Option<PathBuf> {
    let (_, runtime) = bundled_paths(resources);
    binary_in(&runtime.join("bin"), "python").or_else(|| binary_in(&runtime, "python"))
}

/// 在资源目录里找内置 ffmpeg / ffprobe。
pub fn bundled_ffmpeg(resources: &Path) -> (Option<PathBuf>, Option<PathBuf>) {
    let (bin, _) = bundled_paths(resources);
    (binary_in(&bin, "ffmpeg"), binary_in(&bin, "ffprobe"))
}

/// 某个工具是否来自任一内置目录（界面上如实标注"内置"）
pub fn is_bundled(path: &Path) -> bool {
    bundled_dirs().iter().any(|dir| path.starts_with(dir))
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
    // 而 PATH 上可能是任何一个残缺构建（比如 Python 3.8 装不了 faster-whisper）。
    let mut dirs: Vec<PathBuf> = bundled_dirs().to_vec();
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
        // 非 Windows 上 `ffmpeg.exe` 不是可执行文件，同一组用例按平台用对应文件名
        let exe = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
        std::fs::write(builtin.join(exe), b"builtin").unwrap();
        std::fs::write(elsewhere.join(exe), b"path").unwrap();

        // 内置目录排在前面，就应当拿到内置那份
        let got = resolve_from(&[builtin.clone(), elsewhere.clone()], "ffmpeg").unwrap();
        assert_eq!(got, builtin.join(exe));

        // 内置目录里没有时才轮到后面的
        let got = resolve_from(&[builtin.clone(), elsewhere.clone()], "ffprobe");
        assert!(got.is_none(), "两边都没有就不该编出一个路径");

        let probe = if cfg!(windows) { "ffprobe.exe" } else { "ffprobe" };
        std::fs::write(builtin.join(probe), b"builtin").unwrap();
        let got = resolve_from(&[builtin.clone(), elsewhere], "ffprobe").unwrap();
        assert_eq!(got, builtin.join(probe));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn binary_lookup_handles_extensionless_and_windows_names() {
        let dir = std::env::temp_dir().join("dramadist_tools_names");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tool.exe"), b"x").unwrap();
        std::fs::write(dir.join("plain"), b"x").unwrap();

        // 无后缀的文件各平台都认（macOS / Linux 的可执行文件就长这样）
        assert_eq!(binary_in(&dir, "plain"), Some(dir.join("plain")));
        // `.exe` 只有 Windows 会去试 —— 其他平台上它是个普通文件，不该被当成命令
        if cfg!(windows) {
            assert_eq!(binary_in(&dir, "tool"), Some(dir.join("tool.exe")));
        } else {
            assert!(binary_in(&dir, "tool").is_none());
        }
        assert!(binary_in(&dir, "missing").is_none());
        // 目录名不该被当成可执行文件
        assert!(binary_in(&dir, "sub").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bundled_flag_is_false_when_no_bundled_dir_is_registered() {
        // 单元测试进程里没有登记内置目录，任何路径都不该被标成"内置"
        assert!(!is_bundled(Path::new("C:/whatever/ffmpeg.exe")));
    }

    #[test]
    fn bundled_dirs_are_searched_in_order() {
        let root = std::env::temp_dir().join("dramadist_tools_multi");
        let _ = std::fs::remove_dir_all(&root);
        let first = root.join("bin");
        let second = root.join("asr_runtime");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let exe = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
        std::fs::write(first.join(exe), b"x").unwrap();
        const PY: &str = if cfg!(windows) { "python.exe" } else { "python" };
        std::fs::write(second.join(PY), b"x").unwrap();

        // 两个目录各有各的工具，合起来都能找到
        let dirs = vec![first.clone(), second.clone()];
        assert_eq!(resolve_from(&dirs, "ffmpeg"), Some(first.join(exe)));
        assert_eq!(resolve_from(&dirs, "python"), Some(second.join(PY)));

        let _ = std::fs::remove_dir_all(&root);
    }

    /// 内置运行时在不在、能不能真的跑起来。
    ///
    /// 两件事合在一个测试里：`set_bundled_dirs` 是**进程级一次性**的，
    /// 拆成两个测试会互相抢注册，跑的顺序一变就假失败。
    ///
    /// 布局：
    /// - Windows：`bin/ffmpeg.exe`、`asr_runtime/python.exe`（根下）
    /// - macOS / Linux：`bin/<平台>/ffmpeg`、`asr_runtime/<平台>/bin/python`
    ///
    /// 断言用「解析结果 = 找到的那个文件」，不写死后缀，也不写死子目录层级，
    /// 否则非 Windows 平台上 `bin/` 根下的 `*.exe` 会让测试"假装通过"。
    #[test]
    fn end_to_end_bundled_runtimes_are_found_and_runnable() {
        let resources = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources");
        let bin = resources.join(BUNDLED_DIR);
        let runtime = resources.join(ASR_RUNTIME_DIR);
        let tag = platform_tag();
        // 本平台二进制所在目录；Windows 上就是 bin 根下
        let bin_dir = if tag == "windows" {
            bin.clone()
        } else {
            bin.join(tag)
        };
        // 与 set_bundled_dirs 保持同一套候选
        let runtime_dir = if tag == "windows" {
            runtime.clone()
        } else {
            runtime.join(tag)
        };
        let runtime_bin = if tag == "windows" {
            runtime.clone()
        } else {
            runtime_dir.join("bin")
        };

        let bundled_ffmpeg = binary_in(&bin_dir, "ffmpeg");
        let bundled_ffprobe = binary_in(&bin_dir, "ffprobe");
        let bundled_python = binary_in(&runtime_bin, "python")
            .or_else(|| binary_in(&runtime_dir, "python"));

        let mut dirs: Vec<PathBuf> = Vec::new();
        // 登记的是资源根下的两个目录（真实调用点也这么传），子目录由 set_bundled_dirs 展开
        if bundled_ffmpeg.is_some() || bundled_ffprobe.is_some() {
            dirs.push(bin.clone());
        }
        if bundled_python.is_some() {
            dirs.push(runtime.clone());
        }
        if dirs.is_empty() {
            eprintln!("跳过：resources 下没有内置运行时（用 scripts/ 里的脚本获取）");
            return;
        }
        set_bundled_dirs(dirs);

        // --- ffmpeg ---
        if let (Some(ffmpeg), Some(ffprobe)) = (bundled_ffmpeg, bundled_ffprobe) {
            // 关键：设置里什么都没配（空字符串），也应当解析到内置那份
            let got = resolve("", "ffmpeg").expect("没解析到内置 ffmpeg");
            assert_eq!(got, ffmpeg, "内置 ffmpeg 应当优先于 PATH");
            assert!(is_bundled(&got), "应当被标记为内置");

            let got = resolve("", "ffprobe").expect("没解析到内置 ffprobe");
            assert_eq!(got, ffprobe);

            // 光有文件不算数，真的跑得起来才算
            let report = probe_tool("ffmpeg", &ffmpeg);
            assert!(report.available, "内置 ffmpeg 跑不起来：{}", report.detail);
            assert!(report.version.starts_with("ffmpeg version"), "{}", report.version);
            assert!(report.bundled, "应当被标记为内置：{}", report.path);
        }

        // --- 内置 ASR 运行时 ---
        if let Some(python) = bundled_python {
            // 用户没配 Python 时，应当解析到内置的那一份
            let got = resolve("", "python").expect("没解析到内置 python");
            assert_eq!(got, python);
            assert!(is_bundled(&got), "内置 python 应当被标记为内置：{}", got.display());

            let report = probe_tool("python", &python);
            assert!(report.available, "内置 python 跑不起来：{}", report.detail);
            assert!(report.bundled);

            // 模型也应当随运行时一起在
            let model = runtime_dir.join("models").join("tiny").join("model.bin");
            assert!(model.is_file(), "内置模型缺失：{}", model.display());
        }

        // 用户显式指定时仍然听用户的
        let exe = std::env::current_exe().unwrap();
        let got = resolve(&exe.to_string_lossy(), "definitely-not-a-tool");
        assert_eq!(got.as_deref(), Some(exe.as_path()));
    }
}
