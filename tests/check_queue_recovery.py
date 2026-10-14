"""断点续跑验证：塞一条假的 running 任务，然后由应用启动时自动退回。

用法：
    python check_queue_recovery.py seed    # 塞入假 running
    python check_queue_recovery.py check   # 检查是否被退回
"""

import os
import sqlite3
import sys
from datetime import datetime

DB = os.path.join(os.environ["APPDATA"], "com.dramadist.app", "dramadist.db")
mode = sys.argv[1] if len(sys.argv) > 1 else "check"

con = sqlite3.connect(DB)
now = datetime.now().strftime("%Y-%m-%d %H:%M:%S")

if mode == "seed":
    con.execute(
        "INSERT INTO job(media_id, kind, payload, status, message, attempts, created_at, updated_at)"
        " VALUES(?,?,?,?,?,?,?,?)",
        (None, "verify", '{"mediaId":1}', "running", "", 1, now, now),
    )
    con.commit()
    print("已塞入一条假的 running 任务，id =", con.execute("SELECT last_insert_rowid()").fetchone()[0])
elif mode == "check":
    rows = con.execute(
        "SELECT id, kind, status, message, attempts FROM job ORDER BY id DESC LIMIT 5"
    ).fetchall()
    print("最近的队列任务：")
    for r in rows:
        print(
            "  id={} kind={} status={} attempts={} message={}".format(
                r[0], r[1], r[2], r[4], r[3]
            )
        )
    running = con.execute("SELECT COUNT(*) FROM job WHERE status='running'").fetchone()[0]
    pending = con.execute("SELECT COUNT(*) FROM job WHERE status='pending'").fetchone()[0]
    print(f"running={running} pending={pending}")
    print("RESULT =", "PASS" if running == 0 and pending >= 1 else "FAIL")
else:
    print("未知模式：", mode)
    sys.exit(2)
