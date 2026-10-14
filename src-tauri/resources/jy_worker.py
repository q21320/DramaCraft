# -*- coding: utf-8 -*-
"""DramaDist 剪映草稿 worker。

由 Rust 侧以子进程方式调用：stdin 读一行 JSON 请求，stdout 写一行 JSON 应答。
**无论成功失败都必须写一行 JSON**，不把 traceback 直接抛给宿主。

依赖（用户自行安装，Apache-2.0）::

    pip install pyJianYingDraft

请求示例::

    {"draft_root": "D:/JianyingPro/drafts",
     "draft_name": "都市爽剧A_切片",
     "width": 1080, "height": 1920, "fps": 30,
     "allow_replace": true,
     "clips": [{"path": "E:/out/001.mp4", "duration_ms": 60000}, ...],
     "cues":  [{"start_ms": 0, "end_ms": 2000, "text": "第一句"}, ...]}

应答示例::

    {"ok": true, "draft_path": "...", "draft_name": "...",
     "clip_count": 3, "cue_count": 5, "duration_ms": 180000, "warnings": []}
"""

from __future__ import annotations

import json
import os
import sys
import traceback

# 强制 UTF-8：中文 Windows 上管道默认 GBK，会让中文字幕变乱码。
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

    draft_root = str(req.get("draft_root") or "").strip()
    draft_name = str(req.get("draft_name") or "").strip()
    if not draft_root:
        emit({"ok": False, "error": "缺少 draft_root（草稿根目录）"})
        return EXIT_BAD_REQUEST
    if not draft_name:
        emit({"ok": False, "error": "缺少 draft_name（草稿名）"})
        return EXIT_BAD_REQUEST

    clips = req.get("clips") or []
    if not isinstance(clips, list) or not clips:
        emit({"ok": False, "error": "clips 不能为空"})
        return EXIT_BAD_REQUEST

    try:
        import pyJianYingDraft as jy
    except Exception:
        emit(
            {
                "ok": False,
                "error": "未安装 pyJianYingDraft。请在设置里指定的这个 Python 中执行：pip install pyJianYingDraft",
            }
        )
        return EXIT_NO_DEPENDENCY

    warnings: list[str] = []

    def warn(message: str) -> None:
        warnings.append(message)

    try:
        width = int(req.get("width") or 1080)
        height = int(req.get("height") or 1920)
        fps = int(req.get("fps") or 30)

        os.makedirs(draft_root, exist_ok=True)
        folder = jy.DraftFolder(draft_root)
        script = folder.create_draft(
            draft_name,
            width,
            height,
            fps=fps,
            maintrack_adsorb=True,
            allow_replace=True,
        )

        # 1) 视频轨：每个切片一段，顺序拼接
        video_track = script.append_track(jy.TrackSpec(jy.TrackType.video))
        cursor_us = 0
        used_clips = 0
        for index, clip in enumerate(clips, start=1):
            path = str(clip.get("path") or "").strip()
            if not path:
                warn(f"第 {index} 个片段没有路径，已跳过")
                continue
            if not os.path.isfile(path):
                warn(f"第 {index} 个片段文件不存在，已跳过：{path}")
                continue

            duration_ms = int(clip.get("duration_ms") or 0)
            if duration_ms <= 0:
                warn(f"第 {index} 个片段时长为 0，已跳过：{os.path.basename(path)}")
                continue

            duration_us = duration_ms * 1000
            segment = jy.VideoSegment(
                path,
                jy.trange(cursor_us, duration_us),
                source_timerange=jy.trange(0, duration_us),
            )
            script.add_segment(segment, video_track)
            cursor_us += duration_us
            used_clips += 1

        if used_clips == 0:
            emit({"ok": False, "error": "没有任何可用的片段（路径不存在或时长为 0）"})
            return EXIT_FAILED

        # 2) 文本轨：字幕，时间轴已由宿主换算成草稿时间
        cue_count = 0
        cues = req.get("cues") or []
        if isinstance(cues, list) and cues:
            text_track = script.append_track(jy.TrackSpec(jy.TrackType.text))
            style = jy.TextStyle(
                size=float(req.get("font_size") or 8.0),
                align=1,
                auto_wrapping=True,
                max_line_width=0.9,
            )
            for cue in cues:
                text = str(cue.get("text") or "").strip()
                if not text:
                    continue
                start_us = int(cue.get("start_ms") or 0) * 1000
                end_us = int(cue.get("end_ms") or 0) * 1000
                if end_us <= start_us:
                    warn(f"字幕区间非法，已跳过：{text[:12]}")
                    continue
                # 超出成片长度的字幕裁掉，避免剪映里出现"幽灵"片段
                if start_us >= cursor_us:
                    warn(f"字幕超出成片长度，已跳过：{text[:12]}")
                    continue
                end_us = min(end_us, cursor_us)
                script.add_segment(
                    jy.TextSegment(text, jy.trange(start_us, end_us - start_us), style=style),
                    text_track,
                )
                cue_count += 1

        script.save()
        draft_path = os.path.join(draft_root, draft_name)

        emit(
            {
                "ok": True,
                "draft_path": draft_path,
                "draft_name": draft_name,
                "clip_count": used_clips,
                "cue_count": cue_count,
                "duration_ms": cursor_us // 1000,
                "warnings": warnings,
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
