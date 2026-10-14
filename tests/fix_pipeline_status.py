"""把库里状态与实际产出对不上的记录修正过来。

背景：剧目状态以前只会被写成 running，永远不会变成 done，
所以任务早跑完了、界面还显示「进行中」。
这个脚本按实际任务情况把状态补齐。
"""

import os
import sqlite3

DB = os.path.join(os.environ["APPDATA"], "com.dramadist.app", "dramadist.db")
con = sqlite3.connect(DB)

print("=== 修正前 ===")
for r in con.execute("SELECT id, status, stage FROM pipeline_drama"):
    print(f"  drama#{r[0]} {r[1]} 到={r[2]}")
for r in con.execute("SELECT id, status FROM pipeline_run"):
    print(f"  run#{r[0]} {r[1]}")

fixed = 0
for (drama_id,) in con.execute(
    "SELECT id FROM pipeline_drama WHERE status IN ('running','pending')"
).fetchall():
    left = con.execute(
        "SELECT COUNT(*) FROM job WHERE pipeline_drama_id=? AND status IN ('pending','running')",
        (drama_id,),
    ).fetchone()[0]
    if left > 0:
        continue
    failed = con.execute(
        "SELECT COUNT(*) FROM job WHERE pipeline_drama_id=? AND status='failed'",
        (drama_id,),
    ).fetchone()[0]
    status = "failed" if failed else "done"
    message = f"有 {failed} 个任务失败" if failed else "全部步骤已完成"
    con.execute(
        "UPDATE pipeline_drama SET status=?, message=?, updated_at=datetime('now') WHERE id=?",
        (status, message, drama_id),
    )
    fixed += 1
    print(f"  修正 drama#{drama_id} -> {status}（剩余任务 {left}，失败 {failed}）")

# 收口流水线状态
for (run_id,) in con.execute(
    "SELECT id FROM pipeline_run WHERE status='running'"
).fetchall():
    rows = con.execute(
        "SELECT status FROM pipeline_drama WHERE run_id=?", (run_id,)
    ).fetchall()
    if not rows:
        continue
    if any(s[0] in ("pending", "running") for s in rows):
        continue
    status = "failed" if any(s[0] == "failed" for s in rows) else "done"
    con.execute(
        "UPDATE pipeline_run SET status=?, updated_at=datetime('now') WHERE id=?",
        (status, run_id),
    )
    print(f"  收口 run#{run_id} -> {status}")

con.commit()

print("\n=== 修正后 ===")
for r in con.execute("SELECT id, status, stage, substr(message,1,40) FROM pipeline_drama"):
    print(f"  drama#{r[0]} {r[1]} 到={r[2]}  {r[3]}")
for r in con.execute("SELECT id, status FROM pipeline_run"):
    print(f"  run#{r[0]} {r[1]}")
print(f"\n共修正 {fixed} 条剧目状态")
