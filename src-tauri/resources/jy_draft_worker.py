# -*- coding: utf-8 -*-
r"""生成剪映草稿 —— 切片排好、字幕打好、BGM 铺上，剪映打开就是成片结构。

**不需要装剪映**：草稿就是一个文件夹 + 几个 JSON，`pyJianYingDraft` 直接写出来。
剪映只是「打开」它，不参与生成。

产出结构（写在 `<剧目录>\\jy\\` 下）::

    jy\
        穿越护女夫妻闺蜜平辈\
            draft_content.json        ← 剪辑数据
            draft_meta_info.json      ← 草稿元信息
            draft_agency_config.json
            素材引用指向 qiepian\ 里的切片（绝对路径）

调用方式（stdin 一行 JSON → stdout 一行 JSON）::

    {"draft_root": "D:/.../jy",
     "draft_name": "借车之后",
     "width": 720, "height": 1280, "fps": 30,
     "clips": ["D:/.../qiepian/第01集/分镜_001.mp4", ...],
     "srt": "D:/.../字幕.srt",              // 可选
     "bgm": "D:/.../bgm.mp3",               // 可选
     "bgm_volume": 0.35}

## 为什么切片要按顺序排

剪映的主轨是**顺序铺开**的 —— 后一段的起点 = 前一段的终点累加。
所以这里必须逐段求出 `(start_us, duration_us)`，不能都从 0 开始（会全叠在一起）。
"""

from __future__ import annotations

import json
import os
import sys
import traceback

for _s in (sys.stdout, sys.stderr):
    try:
        _s.reconfigure(encoding="utf-8")
    except Exception:
        pass


def emit(payload):
    sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def probe_duration_us(path: str) -> int:
    """拿视频时长（微秒）。用 pymediainfo —— pyJianYingDraft 本来就依赖它。"""
    from pymediainfo import MediaInfo

    info = MediaInfo.parse(path)
    for t in info.tracks:
        if t.track_type == "Video" and t.duration:
            return int(round(float(t.duration) * 1000))  # 毫秒 → 微秒
    for t in info.tracks:
        if t.duration:
            return int(round(float(t.duration) * 1000))
    raise RuntimeError(f"读不出时长：{path}")


def render(req):
    from pyJianYingDraft import (
        AudioSegment,
        DraftFolder,
        TrackSpec,
        TrackType,
        TextSegment,
        TextStyle,
        VideoSegment,
        trange,
    )

    draft_root = (req.get("draft_root") or "").strip()
    draft_name = (req.get("draft_name") or "").strip()
    if not draft_root or not draft_name:
        raise RuntimeError("缺 draft_root 或 draft_name")

    clips = [c for c in (req.get("clips") or []) if c]
    if not clips:
        raise RuntimeError("没有切片可以排")

    width = int(req.get("width") or 720)
    height = int(req.get("height") or 1280)
    fps = int(req.get("fps") or 30)

    missing = [c for c in clips if not os.path.isfile(c)]
    if missing:
        raise RuntimeError("这些切片不在：" + "; ".join(missing[:3]))

    os.makedirs(draft_root, exist_ok=True)

    # 同名草稿先删掉 —— 重跑时不能越积越多
    folder = DraftFolder(draft_root)
    if folder.has_draft(draft_name):
        try:
            folder.remove(draft_name)
        except Exception:
            pass

    script = folder.create_draft(draft_name, width, height, fps=fps)

    # ---------- ① 视频轨：切片按顺序铺开 ----------
    #
    # **轨道用名字传，不用 TrackType** —— `append_track` 要的是 `TrackSpec`，
    # 传 `TrackType.video` 会报 `'TrackType' object has no attribute 'track_type'`。
    # 而 `add_segment(seg, "轨道名")` 找不到轨道时会自动建一条，最省事。
    # `TrackSpec(track_type, name)` —— `append_track` 要的是这个，不是 `TrackType`。
    # 踩过两次：传 `TrackType.video` 报 `no attribute 'track_type'`；
    # 传轨道名给 `add_segment` 报 `不存在名为 '主轨' 的轨道`（它不会自动建）。
    vtrack = script.append_track(TrackSpec(TrackType.video, "主轨"))
    VTRACK = vtrack
    ATRACK = "BGM"
    cursor_us = 0
    placed = []
    for clip in clips:
        dur = probe_duration_us(clip)
        if dur <= 0:
            continue
        seg = VideoSegment(clip, trange(cursor_us, dur))
        script.add_segment(seg, VTRACK)
        placed.append({"clip": os.path.basename(clip), "start_us": cursor_us, "dur_us": dur})
        cursor_us += dur

    total_us = cursor_us

    # ---------- ② 字幕轨 ----------
    srt = (req.get("srt") or "").strip()
    if srt and os.path.isfile(srt):
        # `import_srt` 直接吃 SRT，不用逐句建 TextSegment
        script.import_srt(srt, "字幕")

    # ---------- ③ BGM 轨 ----------
    bgm = (req.get("bgm") or "").strip()
    if bgm and os.path.isfile(bgm):
        bgm_dur = probe_duration_us(bgm)
        atrack = script.append_track(TrackSpec(TrackType.audio, ATRACK))
        # 音乐比片子短就循环铺，比片子长就裁到片子长度
        play_us = min(bgm_dur, total_us) if bgm_dur > 0 else total_us
        want = float(req.get("bgm_volume") or 0.35)
        offset = 0
        while offset < total_us:
            seg_len = min(bgm_dur if bgm_dur > 0 else total_us, total_us - offset)
            if seg_len <= 0:
                break
            script.add_segment(
                AudioSegment(bgm, trange(offset, seg_len), volume=want),
                ATRACK,
            )
            offset += seg_len
            if bgm_dur <= 0:
                break
        _ = play_us

    script.save()

    draft_dir = os.path.join(draft_root, draft_name)
    return {
        "ok": True,
        "draft_dir": draft_dir,
        "clips": len(placed),
        "total_sec": round(total_us / 1_000_000, 2),
        "has_srt": bool(srt and os.path.isfile(srt)),
        "has_bgm": bool(bgm and os.path.isfile(bgm)),
    }


def main():
    # **按字节读 + 显式 UTF-8** —— 中文 Windows 上 sys.stdin 是 GBK，
    # 剧名/路径会全变乱码（成本报告那边踩过）。
    raw = sys.stdin.buffer.readline().decode("utf-8", "replace")
    if not raw.strip():
        emit({"ok": False, "error": "没收到请求"})
        return 2
    try:
        req = json.loads(raw)
    except Exception as e:
        emit({"ok": False, "error": f"请求不是合法 JSON：{e}"})
        return 2
    try:
        emit(render(req))
        return 0
    except Exception as e:
        emit({
            "ok": False,
            "error": f"{type(e).__name__}: {e}",
            "trace": traceback.format_exc()[-1200:],
        })
        return 4


if __name__ == "__main__":
    raise SystemExit(main())
