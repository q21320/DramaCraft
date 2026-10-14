"""流水线卡住时的现场取证。"""

import os
import sqlite3
import time
from datetime import datetime

DB = os.path.join(os.environ["APPDATA"], "com.dramadist.app", "dramadist.db")
con = sqlite3.connect(DB)

print("=== 任务状态 ===")
for r in con.execute("SELECT status, COUNT(*) FROM job GROUP BY status"):
    print(f"  {r[0]:<10} {r[1]}")

print("\n=== 正在跑的任务（关键）===")
rows = list(
    con.execute(
        "SELECT id, kind, updated_at, media_id, substr(message,1,80) "
        "FROM job WHERE status='running'"
    )
)
if not rows:
    print("  没有 running 的任务")
for r in rows:
    print(f"  #{r[0]} {r[1]}  media={r[3]}")
    print(f"      updated_at = {r[2]}")
    try:
        t = datetime.fromisoformat(r[2])
        age = (datetime.now() - t).total_seconds()
        print(f"      已经 {age:.0f} 秒没动过（{age/60:.1f} 分钟）")
    except Exception as e:
        print(f"      （时间解析失败 {e}）")
    if r[4]:
        print(f"      message = {r[4]}")

print("\n=== 最近 8 条有更新的任务 ===")
for r in con.execute(
    "SELECT id, kind, status, updated_at, substr(message,1,50) FROM job ORDER BY updated_at DESC LIMIT 8"
):
    print(f"  #{r[0]:<4} {r[1]:<10} {r[2]:<10} {r[3]}  {r[4]}")

print("\n=== 各阶段的产出 ===")
print(f"  素材 media : {con.execute('SELECT COUNT(*) FROM media').fetchone()[0]}")
print(f"  切片 segment: {con.execute('SELECT COUNT(*) FROM segment').fetchone()[0]}")
print(f"  字幕 cue   : {con.execute('SELECT COUNT(*) FROM cue').fetchone()[0]}")

print("\n=== 流水线 ===")
for r in con.execute(
    "SELECT id, status, created_at, updated_at FROM pipeline_run ORDER BY id DESC LIMIT 3"
):
    print(f"  #{r[0]} {r[1]}  建于 {r[2]}  更新于 {r[3]}")
for r in con.execute(
    "SELECT run_id, status, stage, substr(message,1,60) FROM pipeline_drama ORDER BY id DESC LIMIT 3"
):
    print(f"  run#{r[0]} {r[1]} 到={r[2]}  {r[3]}")

print("\n=== 被处理过的片段（画面处理）===")
n = con.execute(
    "SELECT COUNT(*) FROM segment WHERE original_path IS NOT NULL AND original_path != ''"
).fetchone()[0]
print(f"  已处理 {n} 个")
for r in con.execute(
    "SELECT id, media_id, index_no, status, substr(message,1,40) FROM segment ORDER BY id DESC LIMIT 5"
):
    print(f"  #{r[0]} media={r[1]} 第{r[2]}片 {r[3]} {r[4]}")
