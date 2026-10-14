# 稳定启动 DramaDist（开发版）
#
# 为什么需要这个脚本：
# `npm run tauri:dev` 会**监视 src-tauri 的每个文件**，只要源码有一丁点改动
# 就重新编译并**重启应用**。我改代码的时候，用户正用着的窗口会被反复关掉重开，
# 看着就像卡死 —— 实际上应用没问题。
#
# 这个脚本改成两步，互不干扰：
#   1. 单独起 Vite（前端热更新，但不碰 Rust）
#   2. 直接运行已经编译好的 exe（debug 构建走 devUrl，会连上第 1 步的 Vite）
#
# 结果：我随便改 Rust 源码，用户那个窗口都不会动。

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

Write-Output "=== 1. 停掉可能还在跑的旧实例 ==="
Get-Process -Name dramadist -ErrorAction SilentlyContinue | Stop-Process -Force
# 只杀掉占用 1420 的 node，别把用户别的 node 进程误杀
$busy = Get-NetTCPConnection -LocalPort 1420 -State Listen -ErrorAction SilentlyContinue
if ($busy) {
    Write-Output "  端口 1420 被占用，PID $($busy.OwningProcess)"
    Stop-Process -Id $busy.OwningProcess -Force -ErrorAction SilentlyContinue
}
Start-Sleep -Seconds 2

Write-Output "=== 2. 起 Vite（后台，不监视 Rust）==="
Start-Process -FilePath "cmd.exe" -ArgumentList "/c", "npm run dev" -WorkingDirectory $root -WindowStyle Minimized

Write-Output "  等它就绪..."
$ok = $false
for ($i = 0; $i -lt 30; $i++) {
    Start-Sleep -Seconds 1
    try {
        $r = Invoke-WebRequest -Uri "http://127.0.0.1:1420/" -TimeoutSec 2 -UseBasicParsing
        if ($r.StatusCode -eq 200) { $ok = $true; break }
    } catch { }
}
if (-not $ok) {
    Write-Output "  失败：Vite 30 秒内没起来"
    exit 1
}
Write-Output "  Vite 就绪"

Write-Output "=== 3. 启动应用 ==="
$exe = Join-Path $root "src-tauri\target\debug\dramadist.exe"
if (-not (Test-Path $exe)) {
    Write-Output "  失败：找不到 $exe，先跑一次 cargo build"
    exit 1
}
$err = Join-Path $env:TEMP "dramadist_start.log"
$p = Start-Process -FilePath $exe -PassThru -RedirectStandardError $err
Start-Sleep -Seconds 8
if ($p.HasExited) {
    Write-Output "  应用启动后立刻退出了，错误日志："
    Get-Content $err -Encoding UTF8 -ErrorAction SilentlyContinue | Select-Object -Last 15 | ForEach-Object { "    $_" }
    exit 1
}
Write-Output "  应用已启动，PID $($p.Id)"
Write-Output ""
Write-Output "这个窗口可以关掉 —— 应用和 Vite 会继续跑。"
Write-Output "要停：任务管理器结束 dramadist.exe，以及那个 npm/node 窗口。"
