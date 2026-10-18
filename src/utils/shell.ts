/**
 * 把「浏览器」从窗口里赶出去。
 *
 * Tauri 的窗口底下是 WebView2，本质上还是个 Edge：右键弹「刷新 / 另存为 /
 * 检查元素」，F5 或 Ctrl+R 整页重载（没提交的输入全没了），Ctrl+F 顶出一条
 * 查找栏，Ctrl+滚轮把界面放大，拖个文件进来直接把应用替换成那个文件。
 * 这些在浏览器里是功能，在桌面应用里每一处都是「这玩意儿是个网页」的破绽。
 *
 * Windows 上的大头在 Rust 侧关（`src-tauri/src/lib.rs` 的 `tame_webview`，
 * 直接改 WebView2 设置）：`AreBrowserAcceleratorKeysEnabled = false` 之后
 * F5 / Ctrl+R 根本不进页面，JS 连 keydown 都收不到。这里补的是页面侧的兜底：
 *
 *   - 别的平台没有 WebView2 设置；
 *   - Alt+←/→、Ctrl+滚轮这些 WebView2 不一定拦；
 *   - 拖文件、拖图片进窗口时的默认跳转。
 *
 * **输入法、编辑用的组合键一个都不能碰**（Ctrl+C/V/X/Z/Y/A、方向键……）：
 * 用户分不清「应用的快捷键」和「系统的快捷键」，按不出来就是坏了。
 */
export function installShellGuards(): void {
  // 右键菜单。输入框里的「复制 / 粘贴」也一起关 —— 桌面应用统一用
  // Ctrl+C / Ctrl+V，菜单里那个「检查元素」才是真正出戏的。
  document.addEventListener("contextmenu", (e) => e.preventDefault());

  // 拼写检查默认开着：中英混排的路径、提示语底下全是红波浪线
  document.documentElement.spellcheck = false;

  document.addEventListener(
    "keydown",
    (e) => {
      const key = e.key.toLowerCase();
      const mod = e.ctrlKey || e.metaKey;

      // F3 查找、F5 刷新、F7 光标浏览、F11 全屏、F12 开发者工具
      if (!mod && !e.altKey && ["f3", "f5", "f7", "f11", "f12"].includes(key)) {
        e.preventDefault();
        return;
      }

      // Alt+←/→：浏览器式的前进 / 后退
      if (e.altKey && (key === "arrowleft" || key === "arrowright")) {
        e.preventDefault();
        return;
      }

      if (!mod) return;

      // Ctrl+Shift+I / J / C：开发者工具的另一套入口
      if (e.shiftKey && ["i", "j", "c"].includes(key)) {
        e.preventDefault();
        return;
      }

      // Ctrl+F 查找、Ctrl+G 下一个、Ctrl+P 打印、Ctrl+S 保存网页、
      // Ctrl+O 打开文件、Ctrl+U 查看源码、Ctrl+D 收藏、Ctrl+R 刷新
      if ("fgpsoudr".includes(key)) {
        e.preventDefault();
        return;
      }

      // Ctrl + + / - / 0：网页缩放
      if (["+", "-", "=", "0"].includes(key)) {
        e.preventDefault();
      }
    },
    // 捕获阶段：万一有组件 stopPropagation，也得先被这里看见
    true,
  );

  // Ctrl + 滚轮：WebView2 关闭缩放后这里基本收不到，留着管别的平台
  document.addEventListener(
    "wheel",
    (e) => {
      if (e.ctrlKey) e.preventDefault();
    },
    { passive: false, capture: true },
  );

  // 把文件拖进窗口：浏览器默认会「打开」它，整个界面被替换掉且回不来。
  // Tauri 默认接管了原生拖放，这里再把 HTML5 那一套堵死，双保险。
  for (const type of ["dragover", "drop", "dragstart"]) {
    document.addEventListener(type, (e) => e.preventDefault());
  }
}
