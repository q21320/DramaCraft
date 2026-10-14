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


# ---------------- 中文后处理 ----------------

# Whisper 对中文有个老毛病：同一段话，有时出简体，有时出繁体，
# 而且标点用的是半角逗号。实测同一句「第三集，林小雨…」，
# tiny 出简体但错字多，small 内容几乎全对却整句繁体。
# 所以这里统一转成简体，并把半角标点换成中文全角。
try:
    from opencc import OpenCC

    _T2S = OpenCC("t2s")
except Exception:  # 没装也不该让整个识别失败
    _T2S = None

# 只动这几个：句点不碰，免得把 "3.5" 这种小数改成 "3。5"
_PUNCT = {",": "，", "?": "？", "!": "！", ":": "：", ";": "；"}


def _has_cjk(text: str) -> bool:
    return any("\u4e00" <= ch <= "\u9fff" for ch in text)


def normalize_chinese(text: str) -> str:
    """繁体转简体 + 标点全角化。非中文内容原样返回。"""
    if not text or not _has_cjk(text):
        return text
    if _T2S is not None:
        try:
            text = _T2S.convert(text)
        except Exception:
            pass
    out = "".join(_PUNCT.get(ch, ch) for ch in text)
    # 结尾的句点按中文习惯改成句号
    if out.endswith(".") and _has_cjk(out[:-1]):
        out = out[:-1] + "。"
    return out


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
        compute_type = str(req.get("compute_type") or "").strip()

        kwargs = {}
        model_dir = str(req.get("model_dir") or "").strip()
        if model_dir:
            kwargs["download_root"] = model_dir

        want_cuda = device in ("auto", "cuda")

        def build(dev: str, ctype: str):
            return WhisperModel(model_name, device=dev, compute_type=ctype, **kwargs)

        model = None
        used_cuda = False
        if want_cuda:
            # 「有显卡」不等于「能在上面跑」。先按 CUDA 建一次看看 ——
            # 注意构造成功**不代表** CUDA 可用（库是惰性加载的，见下面的推理兜底）。
            try:
                model = build("cuda", compute_type or "float16")
                used_cuda = True
            except Exception as exc:
                if device == "cuda":
                    # 用户明确要求 GPU，那就如实报错，不要偷偷换掉
                    raise
                print(f"[asr] CUDA 不可用（{type(exc).__name__}: {exc}），改用 CPU", file=sys.stderr)
                model = None

        if model is None:
            model = build("cpu", compute_type or "int8")

        language = str(req.get("language") or "zh").strip()
        if language in ("", "auto"):
            language = None

        def run_inference(m):
            """跑一次识别并把结果全部取出。

            必须在这里就把生成器消费掉 —— `transcribe()` 返回的是**惰性生成器**，
            CUDA 库也是惰性加载的。两个「惰性」叠在一起的结果是：
            构造模型时一切正常，错到 `for seg in segments` 才炸。
            """
            segs, inf = m.transcribe(
                audio,
                language=language,
                vad_filter=True,
                beam_size=5,
            )
            return list(segs), inf

        try:
            segments, info = run_inference(model)
        except Exception as exc:
            # 「有显卡」不等于「能在上面跑」：ctranslate2 需要 cuBLAS 与 cuDNN，
            # 而运行时里没有打包这些库，报的是
            # `Library cublas64_12.dll is not found or cannot be loaded`。
            #
            # 这个错**只在第一次推理时**才浮出来（构造模型时不会），
            # 所以只能在这里兜。用户显式指定 cuda 时如实报错，不偷偷换掉。
            if not used_cuda or device == "cuda":
                raise
            print(
                f"[asr] CUDA 推理失败（{type(exc).__name__}: {exc}），改用 CPU 重跑一次",
                file=sys.stderr,
            )
            model = build("cpu", "int8")
            used_cuda = False
            segments, info = run_inference(model)

        collected = []
        simplify = req.get("simplified")
        simplify = True if simplify is None else bool(simplify)
        for seg in segments:
            text = (seg.text or "").strip()
            if not text:
                continue
            if simplify:
                text = normalize_chinese(text)
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
