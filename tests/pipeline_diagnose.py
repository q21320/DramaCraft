"""按任务类型和错误原因汇总失败情况。"""

import os
import sqlite3
from collections import Counter

DB = os.path.join(os.environ["APPDATA"], "com.dramadist.app", "dramadist.db")
con = sqlite3.connect(DB)

print("=== 按 类型 + 状态 汇总 ===")
for row in con.execute(
    "SELECT kind, status, COUNT(*) FROM job GROUP BY kind, status ORDER BY kind, status"
):
    print(f"  {row[0]:<10} {row[1]:<10} {row[2]}")

print("\n=== 失败原因（按出现次数）===")
seen = Counter()
for (kind, msg, run_id) in con.execute(
    "SELECT kind, message, pipeline_drama_id FROM job WHERE status='failed'"
):
    # 去掉集号/文件名之类的差异，只留错误主干
    key = (kind, (msg or "")[:110])
    seen[key] += 1

for (kind, msg), n in seen.most_common(10):
    print(f"  [{n:>2} 次] {kind}: {msg}")

print("\n=== 成功的那一条 ===")
for row in con.execute(
    "SELECT kind, message FROM job WHERE status='done' LIMIT 5"
):
    print(f"  {row[0]}: {row[1]}")

print("\n=== 切片为什么只有 3 条 ===")
for row in con.execute(
    "SELECT media_id, index_no, start_ms, end_ms, status, message FROM segment ORDER BY id"
):
    print(f"  media={row[0]} 第{row[1]}片 {row[2]}-{row[3]}ms {row[4]} {row[5][:50]}")

print("\n=== 素材库 ===")
n = con.execute("SELECT COUNT(*) FROM media").fetchone()[0]
print(f"  media 记录 {n} 条")
for row in con.execute("SELECT id, name, duration_ms, width, height FROM media ORDER BY id LIMIT 5"):
    print(f"  #{row[0]} {row[1]}  {row[2]}ms  {row[3]}x{row[4]}")
if n > 5:
    print(f"  ... 共 {n} 条")
