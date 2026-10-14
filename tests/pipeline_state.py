"""查看/清理上次流水线失败留下的任务。

用法::

    python tests/pipeline_state.py show     # 看现状
    python tests/pipeline_state.py reset    # 清掉队列与流水线记录，重新来过
"""

import os
import sqlite3
import sys

DB = os.path.join(os.environ["APPDATA"], "com.dramadist.app", "dramadist.db")
mode = sys.argv[1] if len(sys.argv) > 1 else "show"

con = sqlite3.connect(DB)

if mode == "show":
    print("队列任务：")
    for row in con.execute(
        "SELECT status, COUNT(*) FROM job GROUP BY status ORDER BY status"
    ):
        print(f"  {row[0]:<10} {row[1]}")
    print("\n流水线运行：")
    for row in con.execute(
        "SELECT id, status, root_dir FROM pipeline_run ORDER BY id DESC LIMIT 5"
    ):
        print(f"  #{row[0]} {row[1]}  {row[2]}")
    print("\n剧目进度：")
    for row in con.execute(
        "SELECT run_id, name, status, stage, message FROM pipeline_drama ORDER BY id"
    ):
        msg = (row[4] or "")[:60]
        print(f"  run#{row[0]} {row[1]}  状态={row[2]}  到={row[3] or '未开始'}  {msg}")
    print("\n切片记录：")
    n = con.execute("SELECT COUNT(*) FROM segment").fetchone()[0]
    ok = con.execute("SELECT COUNT(*) FROM segment WHERE status='ok'").fetchone()[0]
    print(f"  共 {n} 条，成功 {ok} 条")
    print("\n字幕：")
    n = con.execute("SELECT COUNT(*) FROM cue").fetchone()[0]
    print(f"  共 {n} 条")

elif mode == "reset":
    for table in ("job", "pipeline_drama", "pipeline_run", "cue", "segment", "media", "project"):
        con.execute(f"DELETE FROM {table}")
    con.commit()
    print("已清空队列、流水线记录与素材库 —— 可以重新跑一遍")
else:
    print("未知模式：", mode)
    sys.exit(2)
