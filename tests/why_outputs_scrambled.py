"""查产出数据为什么错乱。"""

import os
import sqlite3

DB = os.path.join(os.environ["APPDATA"], "com.dramadist.app", "dramadist.db")
SRC = r"D:\BaiduNetdiskDownload\弟弟结婚，我的新车竟成了贺礼"
con = sqlite3.connect(DB)

print("=== 任务 ===")
for r in con.execute("SELECT status, COUNT(*) FROM job GROUP BY status"):
    print(f"  {r[0]:<10} {r[1]}")

print("\n=== 各次运行 ===")
for r in con.execute("SELECT id, status FROM pipeline_run ORDER BY id"):
    print(f"  run#{r[0]}  {r[1]}")
    for d in con.execute(
        "SELECT status, stage, episode_count, substr(message,1,44) FROM pipeline_drama WHERE run_id=?",
        (r[0],),
    ):
        print(f"      剧目 {d[0]} 到={d[1]} {d[2]}集  {d[3]}")

print("\n=== 产出查询当前会返回多少行 ===")
n = con.execute(
    "SELECT COUNT(*) FROM media m JOIN project p ON p.id=m.project_id WHERE p.root_dir=?",
    (SRC,),
).fetchone()[0]
uniq = con.execute("SELECT COUNT(DISTINCT path) FROM media").fetchone()[0]
print(f"  当前逻辑返回 {n} 行")
print(f"  实际不同文件 {uniq} 个")
print(f"  → 界面上一集重复出现 {n // max(uniq,1)} 次左右")

print("\n=== 每个工程的切片/字幕 ===")
for (pid, name) in con.execute("SELECT id, name FROM project ORDER BY id"):
    seg = con.execute(
        "SELECT COUNT(*) FROM segment s JOIN media m ON m.id=s.media_id WHERE m.project_id=?",
        (pid,),
    ).fetchone()[0]
    cue = con.execute(
        "SELECT COUNT(*) FROM cue c JOIN media m ON m.id=c.media_id WHERE m.project_id=?",
        (pid,),
    ).fetchone()[0]
    print(f"  工程#{pid} {name[:24]}  切片 {seg}  字幕 {cue}")

print("\n=== 按集名看重复（前 5 个）===")
for r in con.execute(
    "SELECT name, COUNT(*) n FROM media GROUP BY name HAVING n>1 ORDER BY n DESC LIMIT 5"
):
    print(f"  {r[1]} 条  {r[0][:46]}")
