"""用 OCR 验证画面处理有没有毁掉硬字幕。

思路：拿**同一个切片**的原片（001.mp4）和画面处理版（001_t.mp4），
在同一时刻各抽一帧做 OCR：

- 原片没有镜像，字幕应当能正常读出中文
- 处理版如果被镜像过，字是反的，OCR 只能读出乱码或读不出来

这正是"只有拿真实素材才测得出"的那类问题 —— 源片带硬编码字幕，
而画面处理里的镜像会把画面里所有文字翻转。

用法::

    python tests/check_mirror_damage.py [对数]
"""

import json
import os
import subprocess
import sys
import tempfile

RT = r"E:\project\DramaDist\src-tauri\target\release\resources"
FFMPEG = os.path.join(RT, "bin", "ffmpeg.exe")
PYTHON = os.path.join(RT, "asr_runtime", "python.exe")
OCR_WORKER = os.path.join(RT, "ocr_worker.py")

OUT_ROOT = r"D:\BaiduNetdiskDownload\弟弟结婚，我的新车竟成了贺礼二创"
# 抽这一秒的帧：避开片头，字幕一般已经出现
SAMPLE_SEC = 3


def find_pairs(limit):
    """找出「原片 + 处理版」成对的切片。"""
    pairs = []
    for dirpath, _, files in os.walk(OUT_ROOT):
        for f in sorted(files):
            if not f.endswith(".mp4") or f.endswith("_t.mp4"):
                continue
            src = os.path.join(dirpath, f)
            dst = src[: -len(".mp4")] + "_t.mp4"
            if os.path.exists(dst):
                pairs.append((src, dst))
            if len(pairs) >= limit:
                return pairs
    return pairs


def grab_frame(video, dst):
    cmd = [
        FFMPEG, "-hide_banner", "-loglevel", "error", "-y",
        "-ss", str(SAMPLE_SEC), "-i", video, "-frames:v", "1", dst,
    ]
    subprocess.run(cmd, capture_output=True)
    return os.path.exists(dst)


def ocr(image):
    # 注意入参是 frames 数组，不是单个 image —— 第一次写错过
    req = json.dumps(
        {"frames": [{"path": image, "time_ms": SAMPLE_SEC * 1000}], "lang": "ch"},
        ensure_ascii=False,
    )
    r = subprocess.run(
        [PYTHON, OCR_WORKER], input=(req + "\n").encode("utf-8"),
        capture_output=True, timeout=300,
    )
    out = r.stdout.decode("utf-8", "replace")
    for ln in out.splitlines():
        ln = ln.strip()
        if ln.startswith("{"):
            try:
                return json.loads(ln)
            except Exception:
                pass
    return {"ok": False, "error": r.stderr.decode("utf-8", "replace")[:200]}


def read_text(image):
    res = ocr(image)
    if not res.get("ok"):
        return None, res.get("error", "OCR 失败")
    frames = res.get("frames") or []
    txt = " ".join((f.get("text") or "").strip() for f in frames).strip()
    return txt, None


def cjk_ratio(text):
    if not text:
        return 0.0
    return sum(1 for c in text if "\u4e00" <= c <= "\u9fff") / len(text)


def main():
    limit = int(sys.argv[1]) if len(sys.argv) > 1 else 6
    pairs = find_pairs(limit)
    if not pairs:
        print("找不到成对的切片（原片 + _t 处理版）")
        return 1

    print(f"对比 {len(pairs)} 对切片，各抽第 {SAMPLE_SEC} 秒那一帧\n")
    tmp = tempfile.mkdtemp(prefix="dd_ocr_")
    kept = damaged = unknown = 0

    for i, (src, dst) in enumerate(pairs):
        a = os.path.join(tmp, f"{i}_src.png")
        b = os.path.join(tmp, f"{i}_dst.png")
        if not (grab_frame(src, a) and grab_frame(dst, b)):
            print(f"  [抽帧失败] {os.path.basename(src)}")
            continue

        t_src, e1 = read_text(a)
        t_dst, e2 = read_text(b)
        if e1 or e2:
            print(f"  [OCR 报错] {e1 or e2}")
            continue

        r_src, r_dst = cjk_ratio(t_src or ""), cjk_ratio(t_dst or "")
        name = os.path.basename(src)
        if not t_src and not t_dst:
            unknown += 1
            print(f"  [两边都没文字] {name}")
            continue
        # 原片读得出、处理版读不出（或中文占比明显掉下来）= 处理把字幕毁了
        if r_src >= 0.6 and r_dst < 0.6:
            damaged += 1
            print(f"  [!! 被毁] {name}")
            print(f"        原片  «{t_src[:48]}»  中文 {r_src:.0%}")
            print(f"        处理版 «{t_dst[:48]}»  中文 {r_dst:.0%}")
        else:
            kept += 1
            print(f"  [字幕完好] {name}")
            print(f"        原片  «{t_src[:48]}»")
            print(f"        处理版 «{t_dst[:48]}»")

    print(f"\n汇总：字幕完好 {kept} 对，被毁 {damaged} 对，无文字 {unknown} 对")
    if damaged:
        print("被毁的那些就是被镜像过的 —— 画面里的字全反了。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
