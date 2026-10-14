"""设置/读取队列的自动续跑开关。

用法::

    python tests/set_auto_resume.py off    # 模拟用户主动暂停过
    python tests/set_auto_resume.py on     # 恢复自动续跑
    python tests/set_auto_resume.py show
"""

import os
import sqlite3
import sys
from datetime import datetime

DB = os.path.join(os.environ["APPDATA"], "com.dramadist.app", "dramadist.db")
KEY = "queue.auto_resume"
mode = (sys.argv[1] if len(sys.argv) > 1 else "show").strip().lower()

con = sqlite3.connect(DB)
if mode == "show":
    rows = con.execute("SELECT key, value FROM setting WHERE key = ?", (KEY,)).fetchall()
    print(f"{KEY} =", rows[0][1] if rows else "(未设置，默认自动续跑)")
elif mode in ("on", "off"):
    con.execute(
        "INSERT INTO setting(key, value) VALUES(?, ?) "
        "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        (KEY, mode),
    )
    con.commit()
    print(f"{KEY} = {mode}")
else:
    print("未知模式：", mode)
    sys.exit(2)
