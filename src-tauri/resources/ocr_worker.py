# -*- coding: utf-8 -*-
"""DramaDist OCR worker —— 识别画面里的硬字幕。

由 Rust 侧以子进程方式调用：stdin 读一行 JSON 请求，stdout 写一行 JSON 应答。
**无论成功失败都必须写一行 JSON**，不把 traceback 直接抛给宿主。

依赖（用户自行安装，Apache-2.0）::

    pip install rapidocr_onnxruntime

请求示例::

    {"frames": [{"path": "E:/tmp/000001.jpg", "time_ms": 0}, ...],
     "min_confidence": 0.5}

应答示例::

    {"ok": true, "engine": "rapidocr_onnxruntime",
     "frames": [{"time_ms": 0, "text": "你好世界"}, ...]}

设计要点：**单帧识别失败不中断整批** —— 一千帧里有一张坏图，不该让整条任务白跑。
"""

from __future__ import annotations

import json
import sys
import traceback

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


def to_float(value, fallback: float = 0.0) -> float:
    """RapidOCR 的置信度是**字符串**，必须显式转换。"""
    try:
        return float(value)
    except Exception:
        return fallback


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

    frames = req.get("frames") or []
    if not isinstance(frames, list) or not frames:
        emit({"ok": False, "error": "frames 不能为空"})
        return EXIT_BAD_REQUEST

    try:
        from rapidocr_onnxruntime import RapidOCR
    except Exception:
        emit(
            {
                "ok": False,
                "error": "未安装 rapidocr_onnxruntime。请在设置里指定的这个 Python 中执行：pip install rapidocr_onnxruntime",
            }
        )
        return EXIT_NO_DEPENDENCY

    try:
        min_confidence = to_float(req.get("min_confidence"), 0.5)
        engine = RapidOCR()

        collected = []
        failed_frames = 0
        for item in frames:
            path = str(item.get("path") or "")
            time_ms = int(item.get("time_ms") or 0)
            text = ""

            if path:
                try:
                    result = engine(path)
                    payload = result[0] if isinstance(result, tuple) else result
                    if payload:
                        parts = []
                        for line in payload:
                            if not line or len(line) < 2:
                                continue
                            score = to_float(line[2], 1.0) if len(line) > 2 else 1.0
                            if score < min_confidence:
                                continue
                            piece = str(line[1]).strip()
                            if piece:
                                parts.append(piece)
                        text = " ".join(parts)
                except Exception:
                    # 单帧失败（坏图 / 解码异常）跳过，继续下一帧
                    failed_frames += 1
                    text = ""

            collected.append({"time_ms": time_ms, "text": text})

        response = {
            "ok": True,
            "engine": "rapidocr_onnxruntime",
            "frames": collected,
        }
        if failed_frames:
            response["failed_frames"] = failed_frames
        emit(response)
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
