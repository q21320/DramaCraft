# 从 ModelScope 下载 faster-whisper 模型，放进内置模型目录
#
# 为什么是 ModelScope 而不是 HuggingFace：
#   HuggingFace 官方与 hf-mirror 在测试机器上均超时，模型拿不到。
#   ModelScope 是阿里的模型站，国内直连很快，而且有 faster-whisper 的镜像。
#
# 用法：
#   pwsh -File scripts/fetch-model.ps1 -Size small
#   pwsh -File scripts/fetch-model.ps1 -Size base -Repo Systran/faster-whisper-base

param(
    [string]$Size = "small",
    [string]$Repo = "",
    [switch]$Force
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root "src-tauri\resources\asr_runtime\models\$Size"

if ($Repo -eq "") { $Repo = "pengzhendong/faster-whisper-$Size" }

# faster-whisper 需要的四个文件，少一个都加载不了
$files = @("model.bin", "config.json", "tokenizer.json", "vocabulary.txt")

if ((Test-Path (Join-Path $dest "model.bin")) -and -not $Force) {
    Write-Host "已存在：$dest（要重下加 -Force）"
    exit 0
}

New-Item -ItemType Directory -Force -Path $dest | Out-Null

Write-Host "从 ModelScope 下载 $Repo → $dest"
$base = "https://www.modelscope.cn/api/v1/models/$Repo/repo?Revision=master&FilePath="

foreach ($name in $files) {
    $out = Join-Path $dest $name
    Write-Host -NoNewline "  $name ... "
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    try {
        Invoke-WebRequest -Uri ($base + $name) -OutFile $out -TimeoutSec 1800 -UseBasicParsing
        $sw.Stop()
        $mb = [math]::Round((Get-Item $out).Length / 1MB, 1)
        Write-Host ("{0} MB  {1:N0}s" -f $mb, $sw.Elapsed.TotalSeconds)
    } catch {
        Write-Host "失败"
        throw "下载 $name 失败：$($_.Exception.Message)"
    }
}

# 自检：四个文件都在，model.bin 不是空的
foreach ($name in $files) {
    $f = Join-Path $dest $name
    if (-not (Test-Path $f) -or (Get-Item $f).Length -eq 0) {
        throw "缺少或为空：$f"
    }
}
$total = (Get-ChildItem $dest -File | Measure-Object Length -Sum).Sum
Write-Host ""
Write-Host ("完成：{0} 共 {1:N1} MB" -f $Size, ($total / 1MB))
