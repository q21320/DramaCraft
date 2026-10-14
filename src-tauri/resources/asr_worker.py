# -*- coding: utf-8 -*-
"""DramaDist ASR worker。

由 Rust 侧以子进程方式调用：stdin 读一行 JSON 请求，stdout 写一行 JSON 应答。
**无论成功失败都必须写一行 JSON**，不把 traceback 直接抛给宿主。

请求示例::

    {"audio": "E:/a.wav", "model": "small", "language": "zh",
     "model_dir": "C:/Users/x/AppData/Roaming/com.dramadist.app/models",
     "device": "auto", "compute_type": "int8"}

应答示例::

    {"ok": true, "language": "zh", "duration": 12.3, "model": "small",
     "segments": [{"start": 0.0, "end": 2.5, "text": "你好"}]}

依赖（用户自行安装）::

    pip install faster-whisper

许可：faster-whisper 与 Whisper 权重均为 MIT。
"""

from __future__ import annotations

import json
import sys
import traceback

# 强制 UTF-8 输出：中文 Windows 上管道默认是 GBK，会让中文字幕变乱码。
# 宿主同时会设 PYTHONUTF8=1，这里再兜一层，方便手跑调试。
for _stream in (sys.stdout, sys.stderr):
    try:
        _stream.reconfigure(encoding="utf-8")
    except Exception:
        pass

EXIT_BAD_REQUEST = 2
EXIT_NO_DEPENDENCY = 3
EXIT_FAILED = 4


def emit(payload: dict) -> None:
    sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def cuda_available() -> bool:
    try:
        import ctranslate2

        return ctranslate2.get_cuda_device_count() > 0
    except Exception:
        return False


def main() -> int:
    raw = sys.stdin.readline()
    if not raw.strip():
        emit({"ok": False, "error": "没有收到请求（stdin 为空）"})
        return EXIT_BAD_REQUEST
    try:
        req = json.loads(raw)
    except Exception as exc:
        emit({"ok": False, "error": f"请求不是合法 JSON：{exc}"})
        return EXIT_BAD_REQUEST

    audio = str(req.get("audio") or "").strip()
    if not audio:
        emit({"ok": False, "error": "缺少 audio 字段"})
        return EXIT_BAD_REQUEST

    try:
        from faster_whisper import WhisperModel
    except Exception:
        emit(
            {
                "ok": False,
                "error": "未安装 faster-whisper。请在设置里指定的这个 Python 中执行：pip install faster-whisper",
            }
        )
        return EXIT_NO_DEPENDENCY

    try:
        model_name = str(req.get("model") or "small").strip() or "small"

        device = str(req.get("device") or "auto").strip().lower() or "auto"
        if device == "auto":
            device = "cuda" if cuda_available() else "cpu"

        compute_type = str(req.get("compute_type") or "").strip()
        if not compute_type:
            compute_type = "float16" if device == "cuda" else "int8"

        kwargs = {"device": device, "compute_type": compute_type}
        model_dir = str(req.get("model_dir") or "").strip()
        if model_dir:
            kwargs["download_root"] = model_dir

        model = WhisperModel(model_name, **kwargs)

        language = str(req.get("language") or "zh").strip()
        if language in ("", "auto"):
            language = None

        segments, info = model.transcribe(
            audio,
            language=language,
            vad_filter=True,
            beam_size=5,
        )

        collected = []
        for seg in segments:
            text = (seg.text or "").strip()
            if not text:
                continue
            collected.append(
                {"start": float(seg.start), "end": float(seg.end), "text": text}
            )

        emit(
            {
                "ok": True,
                "language": getattr(info, "language", "") or "",
                "duration": float(getattr(info, "duration", 0.0) or 0.0),
                "model": model_name,
                "segments": collected,
            }
        )
        return 0
    except Exception as exc:  # noqa: BLE001 —— 兜底，绝不让栈直接泄给宿主
        emit(
            {
                "ok": False,
                "error": f"{type(exc).__name__}: {exc}",
                "trace": traceback.format_exc()[-1200:],
            }
        )
        return EXIT_FAILED


if __name__ == "__main__":
    raise SystemExit(main())
