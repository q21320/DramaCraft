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
    # 拆分出来的原声音频（和 clips 一一对应；空串表示这一片没抽出来）
    audios = [a or "" for a in (req.get("audios") or [])]
    # **时长由调用方给**，不要自己猜。
    #
    # 踩过：`pymediainfo` 读不出很短的切片（0.5 秒的尾巴）的时长，
    # 抛 `读不出时长`，整个草稿生成失败。
    # 而调用方（Rust）本来就知道每片的 `end_ms - start_ms`，直接传就行。
    durations_ms = [int(x) for x in (req.get("durations_ms") or [])]
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
    for ci, clip in enumerate(clips):
        # 优先用调用方给的时长；没给才自己探（探不出来就当 10 秒，别整件事失败）
        if ci < len(durations_ms) and durations_ms[ci] > 0:
            dur = durations_ms[ci] * 1000
        else:
            try:
                dur = probe_duration_us(clip)
            except Exception:
                dur = 10_000_000
        if dur <= 0:
            continue
        # **拆了原声轨之后，视频轨必须静音** ——
        # 否则画面自带的音频 + 音轨上的音频会同时响，声音翻倍。
        vseg = VideoSegment(clip, trange(cursor_us, dur))
        if audios:
            vseg.volume = 0.0
        seg = vseg
        script.add_segment(seg, VTRACK)
        placed.append({"clip": os.path.basename(clip), "start_us": cursor_us, "dur_us": dur})
        cursor_us += dur

    total_us = cursor_us

    # ---------- ①b 原声整轨 ----------
    #
    # 真实软件有「整轨原声」这一条 —— 从视频里把音频拆出来单独放，
    # 这样音量、淡入淡出、和 BGM 的配比都能单独调。
    if audios:
        atrack = script.append_track(TrackSpec(TrackType.audio, "原声"))
        cursor2 = 0
        for i, a in enumerate(audios):
            if not a or not os.path.isfile(a):
                # 没抽出来就按对应切片的时长跳过，别让后面的音画错位
                cursor2 += placed[i]["dur_us"] if i < len(placed) else 0
                continue
            # 音频时长和对应切片一致（抽音频不改变时长）
            if i < len(durations_ms) and durations_ms[i] > 0:
                dur = durations_ms[i] * 1000
            else:
                try:
                    dur = probe_duration_us(a)
                except Exception:
                    dur = 10_000_000
            if dur <= 0:
                cursor2 += placed[i]["dur_us"] if i + 1 < len(placed) else 0
                continue
            script.add_segment(AudioSegment(a, trange(cursor2, dur)), atrack)
            cursor2 += dur

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

    # ---------- ④ 补 draft_name + 写使用说明 ----------
    #
    # `pyJianYingDraft` 生成的 `draft_meta_info.json` 里 `draft_name` 是**空的**；
    # 剪映的草稿列表按**文件夹名**显示，所以我们靠文件夹名带上剧名。
    # 这里顺手把 `draft_name` 也填上 —— 有的版本会读它。
    meta_path = os.path.join(draft_dir, "draft_meta_info.json")
    try:
        meta = json.load(open(meta_path, encoding="utf-8"))
        meta["draft_name"] = draft_name
        meta["draft_fold_path"] = draft_dir.replace("\\", "/")
        meta["draft_root_path"] = os.path.abspath(draft_root).replace("\\", "/")
        # **`tm_duration` 必须填。**
        #
        # 剪映的草稿列表读这个字段显示时长，`pyJianYingDraft` 生成时是 0 ——
        # 实测列表里显示「89.9M | 00:00」，点开才知道其实是 374 秒。
        # 单位是**微秒**，和 `draft_content.json` 里的 `duration` 一样。
        meta["tm_duration"] = total_us
        json.dump(meta, open(meta_path, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
    except Exception:
        pass

    readme = [
        "这个文件夹是剪映草稿（DramaDist 生成）。",
        "",
        "【怎么用】",
        "1. 把「" + draft_name + "」整个文件夹拷到剪映的草稿目录；",
        "   默认在 C:\\Users\\<你>\\AppData\\Local\\JianyingPro\\User Data\\Projects\\com.lveditor.draft",
        "2. 打开剪映，草稿列表里就有它了。",
        "",
        "【注意】",
        "草稿引用的是下面这些切片，**不要移动或改名**，否则剪映会提示素材丢失：",
    ]
    readme += ["    " + c for c in clips]
    readme += [
        "",
        "【本草稿内容】",
        "    视频轨：" + str(len(placed)) + " 个切片按顺序排好，总长 " + str(round(total_us / 1_000_000, 1)) + " 秒",
        "    字幕轨：" + ("有（" + os.path.basename(srt) + "）" if srt and os.path.isfile(srt) else "无"),
        "    BGM 轨：" + ("有（" + os.path.basename(bgm) + "）" if bgm and os.path.isfile(bgm) else "无"),
        "",
        "换台机器用：连上面那些切片一起搬，并且保持盘符路径一致；",
        "或者打开剪映后按提示手动「重新链接素材」。",
    ]
    with open(os.path.join(draft_dir, "说明.txt"), "w", encoding="utf-8") as f:
        f.write("\n".join(readme))
    return {
        "ok": True,
        "draft_dir": draft_dir,
        "clips": len(placed),
        "total_sec": round(total_us / 1_000_000, 2),
        "has_srt": bool(srt and os.path.isfile(srt)),
        "has_bgm": bool(bgm and os.path.isfile(bgm)),
        "audio_clips": sum(1 for a in audios if a and os.path.isfile(a)),
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
