# 获取 FFmpeg 并放进内置目录
#
# 用法：
#   pwsh -File scripts/fetch-ffmpeg.ps1              # 默认下载 gyan.dev 的 release-essentials
#   pwsh -File scripts/fetch-ffmpeg.ps1 -Source path # 从本地已有的 ffmpeg.exe / ffprobe.exe 复制
#
# 内置二进制不进版本库（158 MB 太大），所以仓库里只有这个脚本。
# 想自己编译或者换一份构建，把 ffmpeg.exe / ffprobe.exe 放进
# src-tauri/resources/bin/ 就行，程序会优先用它们。

param(
    [string]$Source = "",
    [string]$Url = "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root "src-tauri\resources\bin"
New-Item -ItemType Directory -Force -Path $dest | Out-Null

function Report($ffmpeg) {
    Write-Host ""
    Write-Host "已放置："
    Get-ChildItem $dest -Filter *.exe | ForEach-Object {
        Write-Host ("  {0}  {1} MB" -f $_.Name, [math]::Round($_.Length / 1MB, 1))
    }
    Write-Host ""
    Write-Host "版本："
    & $ffmpeg -version 2>&1 | Select-Object -First 1
    Write-Host ""
    Write-Host "SHA256（请记下来，日后核对用）："
    Get-ChildItem $dest -Filter *.exe | ForEach-Object {
        Write-Host ("  {0}  {1}" -f $_.Name, (Get-FileHash $_.FullName -Algorithm SHA256).Hash)
    }
    Write-Host ""
    Write-Host "注意：这份构建通常是 GPL 版。自用没问题；"
    Write-Host "      对外分发本软件时需要遵守 GPL，详见 $dest\README.txt"
}

if ($Source -ne "") {
    # 从本地已有的二进制复制
    $ffmpeg = Join-Path $Source "ffmpeg.exe"
    $ffprobe = Join-Path $Source "ffprobe.exe"
    if (-not (Test-Path $ffmpeg)) { throw "找不到 $ffmpeg" }
    if (-not (Test-Path $ffprobe)) { throw "找不到 $ffprobe" }
    Copy-Item $ffmpeg $dest -Force
    Copy-Item $ffprobe $dest -Force
    Write-Host "已从 $Source 复制"
    Report (Join-Path $dest "ffmpeg.exe")
    exit 0
}

# 从官方构建下载
$cache = Join-Path $root ".cache"
New-Item -ItemType Directory -Force -Path $cache | Out-Null
$zip = Join-Path $cache "ffmpeg-release-essentials.zip"

if (-not (Test-Path $zip)) {
    Write-Host "下载 $Url"
    Write-Host "（文件较大，约 90 MB。慢的话可以先用 -Source 指向本地已有的 ffmpeg）"
    Invoke-WebRequest -Uri $Url -OutFile $zip -UseBasicParsing
}

$extract = Join-Path $cache "extracted"
if (Test-Path $extract) { Remove-Item $extract -Recurse -Force }
Expand-Archive -Path $zip -DestinationPath $extract -Force

$found = Get-ChildItem $extract -Recurse -Filter ffmpeg.exe | Select-Object -First 1
if (-not $found) { throw "解压后没找到 ffmpeg.exe" }
$binDir = $found.DirectoryName

Copy-Item (Join-Path $binDir "ffmpeg.exe") $dest -Force
Copy-Item (Join-Path $binDir "ffprobe.exe") $dest -Force

# 构建包里的许可证一并带上
foreach ($name in "LICENSE", "COPYING.GPLv3.txt", "COPYING.GPLv3") {
    $lic = Join-Path $binDir $name
    if (Test-Path $lic) { Copy-Item $lic (Join-Path $dest "COPYING.GPLv3.txt") -Force }
}

Report (Join-Path $dest "ffmpeg.exe")
