"""用 Windows 自带的中文 TTS 造测试语音，然后拿它对比两个识别引擎。

用法::

    python tests/compare_asr.py tts      # 只造语音
    python tests/compare_asr.py whisper  # 跑 faster-whisper
    python tests/compare_asr.py funasr   # 跑 FunASR Paraformer
    python tests/compare_asr.py all
"""

import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RT = os.path.join(ROOT, "src-tauri", "resources", "asr_runtime")
PY = os.path.join(RT, "python.exe")
FFMPEG = os.path.join(ROOT, "src-tauri", "resources", "bin", "ffmpeg.exe")
WORK = os.path.join(os.environ["TEMP"], "dd_asr_compare")
WAV = os.path.join(WORK, "speech.wav")

# 短剧字幕里典型的一句话：有标点、有数字、有专有名词
SENTENCE = "第三集，林小雨在雨里等了整整两个小时，她终于说出那句话：我不会再回头了。"

mode = sys.argv[1] if len(sys.argv) > 1 else "all"
results: list = []


def make_tts() -> None:
    os.makedirs(WORK, exist_ok=True)
    ps = f"""
Add-Type -AssemblyName System.Speech
$s = New-Object System.Speech.Synthesis.SpeechSynthesizer
$s.SelectVoice('Microsoft Huihui Desktop')
$s.Rate = 0
$fmt = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(
    16000,
    [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen,
    [System.Speech.AudioFormat.AudioChannel]::Mono)
$s.SetOutputToWaveFile('{WAV}', $fmt)
$s.Speak('{SENTENCE}')
$s.Dispose()
"""
    subprocess.run(["powershell", "-NoProfile", "-Command", ps], check=True)
    size = os.path.getsize(WAV)
    print(f"TTS 生成：{WAV}  {size / 1024:.0f} KB")
    print(f"原文：{SENTENCE}")
    # 确认不是一段静音
    probe = subprocess.run(
        [FFMPEG, "-hide_banner", "-i", WAV, "-af", "volumedetect", "-f", "null", "-"],
        capture_output=True, text=True,
    )
    for line in probe.stderr.splitlines():
        if "mean_volume" in line or "max_volume" in line:
            print("  " + line.split("] ")[-1])


def run_whisper(size: str = "small") -> None:
    model = os.path.join(RT, "models", size)
    if not os.path.isfile(os.path.join(model, "model.bin")):
        print(f"跳过 whisper：{model} 里没有模型")
        return
    worker = os.path.join(ROOT, "src-tauri", "resources", "asr_worker.py")
    import json
    import time
    req = json.dumps({
        "audio": WAV, "model": model, "language": "zh",
        "device": "cpu", "compute_type": "int8",
    })
    t0 = time.time()
    out = subprocess.run([PY, worker], input=req, capture_output=True, text=True,
                         encoding="utf-8", cwd=WORK)
    elapsed = time.time() - t0

    text = ""
    segs = []
    try:
        payload = json.loads(out.stdout.strip().splitlines()[-1])
        segs = payload.get("segments", [])
        text = "".join(s.get("text", "") for s in segs)
    except Exception as exc:
        text = f"(解析失败: {exc}) {out.stdout.strip()[:200]}"

    results.append({
        "engine": f"faster-whisper {size}",
        "seconds": round(elapsed, 2),
        "text": text,
        "segments": [{"start": s.get("start"), "end": s.get("end"), "text": s.get("text")}
                     for s in segs],
    })
    print(f"--- faster-whisper {size} ---")
    print(f"  用时 {elapsed:.1f}s，{len(segs)} 段")
    if out.stderr.strip():
        print("  stderr:", out.stderr.strip()[:300])


def run_funasr() -> None:
    models = os.path.join(os.environ["TEMP"], "funasr_models")
    para = os.path.join(models, "paraformer")
    vad = os.path.join(models, "vad")
    if not os.path.isfile(os.path.join(para, "model_quant.onnx")):
        print("跳过 FunASR：模型不在", para)
        return

    script = f'''
import sys, os, time, json
sys.path.insert(0, {RT!r})
from funasr_onnx import Paraformer, Fsmn_vad

report = {{}}
t0 = time.time()
vad = Fsmn_vad({vad!r}, batch_size=1, quantize=True)
segs = vad({WAV!r})
report["vad_seconds"] = round(time.time() - t0, 2)
report["vad"] = str(segs)

t0 = time.time()
model = Paraformer({para!r}, batch_size=1, quantize=True)
res = model({WAV!r})
report["asr_seconds"] = round(time.time() - t0, 2)
report["raw"] = str(res)
if isinstance(res, list) and res:
    first = res[0]
    report["text"] = first if isinstance(first, str) else (
        first.get("text", "") if isinstance(first, dict) else str(first))
else:
    report["text"] = ""
print("@@JSON@@" + json.dumps(report, ensure_ascii=False))
'''
    path = os.path.join(WORK, "run_funasr.py")
    os.makedirs(WORK, exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write(script)
    out = subprocess.run([PY, path], capture_output=True, text=True,
                         encoding="utf-8", errors="replace", cwd=WORK)
    print("--- FunASR Paraformer ---")
    for line in (out.stdout or "").splitlines():
        if line.startswith("@@JSON@@"):
            import json as _json
            info = _json.loads(line[8:])
            results.append({
                "engine": "FunASR Paraformer-large (onnx)",
                "seconds": info.get("asr_seconds"),
                "text": info.get("text", ""),
                "vad": info.get("vad", ""),
            })
            print(f"  识别用时 {info.get('asr_seconds')}s，VAD {info.get('vad_seconds')}s")
        elif line.strip():
            print("  " + line[:200])
    if out.returncode != 0:
        print("  退出码", out.returncode)
        print("  stderr:", (out.stderr or "")[-600:])
        results.append({"engine": "FunASR Paraformer-large (onnx)",
                        "seconds": None, "text": f"(失败) {(out.stderr or '')[-300:]}"})


def similarity(a: str, b: str) -> float:
    """按字符集合算个粗略相似度，够用来对比两个引擎谁更接近原文。"""
    import difflib
    return difflib.SequenceMatcher(None, a, b).ratio()


def write_report() -> None:
    """把结果写成 UTF-8 文件 —— 控制台是 GBK，直接打印中文会糊。"""
    import json
    path = os.path.join(WORK, "report.json")
    payload = {"原句": SENTENCE, "结果": results}
    for item in results:
        item["与原文相似度"] = round(similarity(SENTENCE, item.get("text", "")), 3)
    lines = [f"原句：{SENTENCE}", ""]
    for item in results:
        lines.append(f"【{item['engine']}】用时 {item.get('seconds')}s  "
                     f"相似度 {item.get('与原文相似度')}")
        lines.append(f"  {item.get('text')}")
        lines.append("")
    lines.append(json.dumps(payload, ensure_ascii=False, indent=2))
    with open(path, "w", encoding="utf-8") as f:
        f.write("\n".join(lines))
    print(f"报告写入 {path}")


if mode in ("tts", "all"):
    make_tts()
if mode in ("whisper", "all"):
    for size in (os.environ.get("WHISPER_SIZES", "tiny,small").split(",")):
        run_whisper(size.strip())
if mode in ("funasr", "all"):
    run_funasr()

if results:
    write_report()
