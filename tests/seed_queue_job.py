"""往真实库里塞一条待跑的切片任务，用来验证「队列自动续跑」真的会跑。

用法::

    python tests/seed_queue_job.py seed     # 造素材 + 塞一条 pending 任务
    python tests/seed_queue_job.py check    # 看任务跑完没有、切片出来了没有
"""

import json
import os
import sqlite3
import subprocess
import sys
from datetime import datetime

DB = os.path.join(os.environ["APPDATA"], "com.dramadist.app", "dramadist.db")
ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "src-tauri", "resources", "bin")
FFMPEG = os.path.join(BIN, "ffmpeg.exe")
FFPROBE = os.path.join(BIN, "ffprobe.exe")
WORK = os.path.join(os.environ["TEMP"], "dd_queue_e2e")

mode = sys.argv[1] if len(sys.argv) > 1 else "check"


def now() -> str:
    return datetime.now().strftime("%Y-%m-%d %H:%M:%S")


if mode == "seed":
    os.makedirs(WORK, exist_ok=True)
    video = os.path.join(WORK, "queue_test.mp4")
    subprocess.run(
        [
            FFMPEG, "-hide_banner", "-loglevel", "error", "-y",
            "-f", "lavfi", "-i", "testsrc=size=320x240:rate=15",
            "-t", "6", "-c:v", "libx264", "-preset", "ultrafast",
            "-pix_fmt", "yuv420p", video,
        ],
        check=True,
    )

    probe = subprocess.run(
        [FFPROBE, "-v", "error", "-show_entries", "format=duration",
         "-of", "csv=p=0", video],
        capture_output=True, text=True, check=True,
    )
    duration_ms = int(float(probe.stdout.strip() or 0) * 1000)

    out_dir = os.path.join(WORK, "out")
    os.makedirs(out_dir, exist_ok=True)
    for name in os.listdir(out_dir):
        os.remove(os.path.join(out_dir, name))

    con = sqlite3.connect(DB)
    # 清掉上次的痕迹，保证这次是干净的一次
    for table in ("job", "segment", "media", "project"):
        con.execute(f"DELETE FROM {table}")
    con.commit()

    stamp = now()
    project_id = con.execute(
        "INSERT INTO project(name, root_dir, created_at) VALUES(?,?,?)",
        ("队列验证工程", WORK, stamp),
    ).lastrowid
    media_id = con.execute(
        "INSERT INTO media(project_id, path, name, duration_ms, width, height, has_audio,"
        " size_bytes, format_name, created_at) VALUES(?,?,?,?,?,?,?,?,?,?)",
        (project_id, video, os.path.basename(video), duration_ms, 320, 240, 0,
         os.path.getsize(video), "mp4", stamp),
    ).lastrowid

    payload = json.dumps(
        {"mediaId": media_id, "mode": {"mode": "equalParts", "parts": 3}, "outDir": out_dir}
    )
    con.execute(
        "INSERT INTO job(media_id, kind, payload, status, message, attempts, created_at, updated_at)"
        " VALUES(?,?,?,?,?,?,?,?)",
        (media_id, "slice", payload, "pending", "", 0, stamp, stamp),
    )
    con.commit()
    print(f"已塞入待跑任务：media_id={media_id}，素材 {duration_ms}ms，输出 {out_dir}")
    print("接着启动应用，它应当自动把这条任务跑掉。")

elif mode == "check":
    con = sqlite3.connect(DB)
    jobs = con.execute(
        "SELECT id, kind, status, attempts, message FROM job ORDER BY id"
    ).fetchall()
    print("任务：")
    for j in jobs:
        print(f"  #{j[0]} {j[1]} status={j[2]} attempts={j[3]} msg={j[4]!r}")

    segs = con.execute(
        "SELECT index_no, start_ms, end_ms, out_path, size_bytes, status FROM segment ORDER BY index_no"
    ).fetchall()
    print(f"切片：{len(segs)} 条")
    for s in segs:
        print(f"  第{s[0]}片 {s[1]}-{s[2]}ms  {s[4] / 1024:.0f} KB  {s[5]}  {os.path.basename(s[3])}")

    out_dir = os.path.join(WORK, "out")
    files = sorted(os.listdir(out_dir)) if os.path.isdir(out_dir) else []
    print(f"输出目录里的文件：{files}")

    done = len(jobs) == 1 and jobs[0][2] == "done"
    sliced = len(segs) >= 3 and all(s[5] == "ok" for s in segs)
    on_disk = len(files) >= 3
    print("RESULT =", "PASS" if (done and sliced and on_disk) else "FAIL")
    if not done:
        print("  原因：任务没有跑到 done")
    if not sliced:
        print("  原因：切片记录不完整")
    if not on_disk:
        print("  原因：磁盘上没有对应的切片文件")
else:
    print("未知模式：", mode)
    sys.exit(2)
