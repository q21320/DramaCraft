# 获取 ASR 运行时并放进内置目录
#
# 用法：
#   pwsh -File scripts/fetch-asr-runtime.ps1 -Source "E:\chaoren\asr_runtime"
#   OCR / 剪映依赖默认从 .venv\Lib\site-packages 补，可用 -DepsSource 指向别处
#
# 为什么是"从别处复制"而不是"下载"：
#   这套运行时由三部分组成 —— 官方 Python 嵌入式包、PyPI 上的 faster-whisper
#   及其依赖、以及一个 Whisper 模型。三者在国内都不好下：
#   HuggingFace 与 hf-mirror 实测超时，模型拿不到。
#   所以脚本支持从任意一份已有的运行时复制，并做必要的裁剪。
#
# 复制哪些、不复制哪些：
#   要  —— 嵌入式 Python 解释器、Lib/site-packages（除 pip/setuptools）、模型，
#          以及 OCR / 剪映依赖（ASR 源运行时里通常没有，从项目 .venv 补）
#   不要 —— 第三方程序自己的 worker 脚本（那是它的代码，我们用自己的）
#
# 全部组件的许可都是允许再分发的：Python 是 PSF、faster-whisper 与 ctranslate2
# 是 MIT、Whisper 权重是 MIT、numpy 是 BSD、onnxruntime 是 MIT、tokenizers 是
# Apache-2.0。详见生成出来的 README.txt。

param(
    [string]$Source = "",
    [string]$ModelSize = "tiny",
    # OCR / 剪映依赖从哪里补；默认用项目的 .venv
    [string]$DepsSource = ""
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root "src-tauri\resources\asr_runtime"
if ($DepsSource -eq "") {
    $DepsSource = Join-Path $root ".venv\Lib\site-packages"
}

if ($Source -eq "") {
    Write-Host "需要 -Source 指向一份已有的 ASR 运行时目录，例如："
    Write-Host '  pwsh -File scripts/fetch-asr-runtime.ps1 -Source "E:\some\asr_runtime"'
    Write-Host ""
    Write-Host "该目录应当包含 python.exe、Lib\site-packages\ 与 whisper_models\。"
    exit 2
}
if (-not (Test-Path (Join-Path $Source "python.exe"))) {
    throw "在 $Source 里找不到 python.exe，这不是一份 ASR 运行时目录"
}
if (-not (Test-Path (Join-Path $Source "Lib\site-packages"))) {
    throw "在 $Source 里找不到 Lib\site-packages，这不是一份完整的 ASR 运行时目录"
}

# 先在源目录里确认模型存在，再删旧运行时；否则参数写错会把现有运行时删空
$modelsRoot = Join-Path $Source "whisper_models"
$snap = Get-ChildItem $modelsRoot -Recurse -Directory -ErrorAction SilentlyContinue |
    Where-Object { $_.Name -match "faster-whisper-$ModelSize$" } |
    Select-Object -First 1
if (-not $snap) {
    throw "在 $modelsRoot 里找不到 $ModelSize 模型；现有运行时未改动。请换 -ModelSize，或先跑 scripts/fetch-model.ps1。"
}

Write-Host "从 $Source 复制到 $dest"

if (Test-Path $dest) { Remove-Item $dest -Recurse -Force }
New-Item -ItemType Directory -Force -Path $dest | Out-Null

# 1) 解释器本体
Write-Host "[1/4] 复制解释器"
Get-ChildItem $Source -File | Where-Object {
    $_.Extension -in '.exe', '.dll', '.pyd', '.zip', '.cat' -or $_.Name -like '*._pth'
} | ForEach-Object { Copy-Item $_.FullName $dest -Force }

# 2) site-packages，去掉装包工具和缓存
Write-Host "[2/4] 复制依赖"
$skip = 'pip', 'setuptools', '_distutils_hack', 'pkg_resources', 'wheel'
$sp = Join-Path $dest "Lib\site-packages"
New-Item -ItemType Directory -Force -Path $sp | Out-Null
Get-ChildItem (Join-Path $Source "Lib\site-packages") -ErrorAction SilentlyContinue |
    Where-Object { $skip -notcontains $_.Name } |
    ForEach-Object { Copy-Item $_.FullName $sp -Recurse -Force -ErrorAction SilentlyContinue }

Get-ChildItem $sp -Recurse -Directory -Filter '__pycache__' -ErrorAction SilentlyContinue |
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
Get-ChildItem $sp -Recurse -File -Filter '*.pyc' -ErrorAction SilentlyContinue |
    Remove-Item -Force -ErrorAction SilentlyContinue

# 删掉 setuptools 留下的 .pth —— 它引用的 _distutils_hack 已经被删了，
# 不清掉会导致每次启动都往 stderr 喷一段异常，干扰诊断。
$pth = Join-Path $sp 'distutils-precedence.pth'
if (Test-Path $pth) { Remove-Item $pth -Force; Write-Host "      已清理 distutils-precedence.pth" }

# 3) 模型：从 HF 缓存里摊平成一个普通目录
# $snap 已在删除旧运行时之前校验过，避免模型缺失时把现有运行时删掉
Write-Host "[3/4] 复制模型（$ModelSize）"

$snapshot = Get-ChildItem $snap.FullName -Directory -Filter 'snapshots' -ErrorAction SilentlyContinue |
    Select-Object -First 1
$from = if ($snapshot) {
    Get-ChildItem $snapshot.FullName -Directory | Select-Object -First 1
} else { $snap }
if (-not $from) { throw "找不到 $ModelSize 的模型文件" }

$to = Join-Path $dest "models\$ModelSize"
New-Item -ItemType Directory -Force -Path $to | Out-Null
Get-ChildItem $from.FullName -File | ForEach-Object { Copy-Item $_.FullName $to -Force }

# 校验：模型文件必须在，否则等于白拷
if (-not (Test-Path (Join-Path $to "model.bin"))) {
    throw "复制后 $to 里没有 model.bin，模型没拷全"
}
if ($ModelSize -ne "small") {
    Write-Warning "应用默认模型是 small；这次复制的是 $ModelSize，运行前请再跑 scripts/fetch-model.ps1 -Size small。"
}

# 4) 补 OCR / 剪映依赖
#
# ASR 源运行时通常只装 faster-whisper。OCR（ocr_worker.py）还要
# rapidocr_onnxruntime + cv2 + Pillow + pyclipper + shapely + six；
# 剪映（jy_worker.py / jy_draft_worker.py）还要 pyJianYingDraft +
# pymediainfo + uiautomation + comtypes + imageio。
# 这些包不在 ASR 源里，从项目的 .venv 补；只补缺的，不覆盖已有版本。
Write-Host "[4/4] 补 OCR / 剪映依赖"

$depsOk = $false
if (-not (Test-Path $DepsSource)) {
    Write-Warning "找不到依赖源：$DepsSource"
    Write-Warning "OCR / 剪映依赖没补，这两类功能会在运行时缺包。"
} else {
    $depsPython = Join-Path $root ".venv\Scripts\python.exe"
    $runtimeVer = (& (Join-Path $dest "python.exe") -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')")
    $runtimeVer = "$runtimeVer".Trim()
    $depsVer = ""
    if (Test-Path $depsPython) {
        $depsVer = (& $depsPython -c "import sys; print(f'{sys.version_info.major}.{sys.version_info.minor}')")
        $depsVer = "$depsVer".Trim()
    }

    if ($depsVer -and $runtimeVer -ne $depsVer) {
        Write-Warning "运行时是 Python $runtimeVer，依赖源是 Python $depsVer，版本不一致，跳过复制。"
        Write-Warning "请用同版本的 site-packages，或把 -DepsSource 指向匹配的目录。"
    } else {
        if (-not $depsVer) {
            Write-Warning "找不到 .venv 的 python.exe，无法校验 Python 版本，仍尝试复制。"
        }

        $extra = @(
            "rapidocr_onnxruntime", "cv2", "PIL", "pyclipper", "shapely", "shapely.libs", "six.py",
            "pymediainfo", "pyJianYingDraft", "uiautomation", "comtypes", "imageio",
            "yaml", "numpy", "numpy.libs", "onnxruntime"
        )
        foreach ($name in $extra) {
            $target = Join-Path $sp $name
            if (Test-Path $target) { continue }
            $from = Join-Path $DepsSource $name
            if (Test-Path $from) {
                Copy-Item $from $sp -Recurse -Force
                Write-Host "      + $name"
            } else {
                Write-Warning "依赖源里没有 $name"
            }
        }

        # dist-info 不影响 import，但留着版本信息更完整
        $distPatterns = @(
            "rapidocr_onnxruntime-*", "opencv_python-*", "pillow-*", "pyclipper-*",
            "shapely-*", "six-*", "pymediainfo-*", "pyjianyingdraft-*",
            "uiautomation-*", "comtypes-*", "imageio-*", "PyYAML-*", "numpy-*", "onnxruntime-*"
        )
        foreach ($pattern in $distPatterns) {
            Get-ChildItem $DepsSource -Directory -Filter $pattern -ErrorAction SilentlyContinue |
                ForEach-Object {
                    $toDist = Join-Path $sp $_.Name
                    if (-not (Test-Path $toDist)) {
                        Copy-Item $_.FullName $sp -Recurse -Force
                    }
                }
        }
        $depsOk = $true
    }
}

# 再清一遍缓存：.venv 里的包会带 __pycache__
Get-ChildItem $sp -Recurse -Directory -Filter '__pycache__' -ErrorAction SilentlyContinue |
    Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
Get-ChildItem $sp -Recurse -File -Filter '*.pyc' -ErrorAction SilentlyContinue |
    Remove-Item -Force -ErrorAction SilentlyContinue

$total = (Get-ChildItem $dest -Recurse -File | Measure-Object Length -Sum).Sum
Write-Host ""
Write-Host ("完成：" + [math]::Round($total / 1MB, 1) + " MB，" + (Get-ChildItem $dest -Recurse -File).Count + " 个文件")
Write-Host ""
Write-Host "自检："
& (Join-Path $dest "python.exe") -c "import faster_whisper; print('  faster-whisper', faster_whisper.__version__)"
if ($depsOk) {
    & (Join-Path $dest "python.exe") -c "import rapidocr_onnxruntime, cv2, PIL, six, pyclipper, shapely; print('  OCR 依赖 OK')"
    & (Join-Path $dest "python.exe") -c "import pymediainfo, pyJianYingDraft, uiautomation, comtypes, imageio; print('  剪映依赖 OK')"
    & (Join-Path $dest "python.exe") -c "from pymediainfo import MediaInfo; assert MediaInfo.can_parse(); print('  MediaInfo OK')"
} else {
    Write-Warning "OCR / 剪映依赖没有补齐，自检跳过；这两类功能会在运行时缺包。"
}
