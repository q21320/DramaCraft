"""清空工具路径设置 —— 模拟"全新安装、什么都没配"的状态。

用法::

    python tests/clear_tool_settings.py
"""

import os
import sqlite3

DB = os.path.join(os.environ["APPDATA"], "com.dramadist.app", "dramadist.db")
con = sqlite3.connect(DB)

rows = con.execute(
    "SELECT key, value FROM setting WHERE key LIKE '%tool%' OR key LIKE '%ffmpeg%' "
    "OR key LIKE '%python%' OR key LIKE '%ffprobe%'"
).fetchall()
print("清空前的工具设置：", rows if rows else "(空)")

con.execute(
    "DELETE FROM setting WHERE key LIKE '%tool%' OR key LIKE '%ffmpeg%' "
    "OR key LIKE '%python%' OR key LIKE '%ffprobe%'"
)
con.commit()

left = con.execute(
    "SELECT key, value FROM setting WHERE key LIKE '%tool%' OR key LIKE '%ffmpeg%' "
    "OR key LIKE '%python%' OR key LIKE '%ffprobe%'"
).fetchall()
print("清空后：", left if left else "(空)")
print("RESULT = PASS" if not left else "RESULT = FAIL")
