"""看工程#9 里那 4 条重复素材的完整路径 —— 重复是怎么来的。"""

import os
import sqlite3

DB = os.path.join(os.environ["APPDATA"], "com.dramadist.app", "dramadist.db")
con = sqlite3.connect(DB)

print("=== 工程#9 里第 1~4 集的全部记录 ===")
for r in con.execute(
    "SELECT id, name, path, duration_ms, size_bytes FROM media "
    "WHERE project_id=9 AND (name LIKE '%第01集%' OR name LIKE '%第02集%' "
    "OR name LIKE '%第03集%' OR name LIKE '%第04集%') ORDER BY name, id"
):
    print(f"  #{r[0]}  {r[1]}")
    print(f"        path   = {r[2]}")
    print(f"        时长 {r[3]}ms  大小 {r[4]}")

print("\n=== 这些重复记录各自有切片吗 ===")
for r in con.execute(
    "SELECT m.id, m.name, "
    "(SELECT COUNT(*) FROM segment s WHERE s.media_id=m.id), "
    "(SELECT COUNT(*) FROM cue c WHERE c.media_id=m.id) "
    "FROM media m WHERE m.project_id=9 AND m.name LIKE '%第0[1-4]集%' ORDER BY m.name, m.id"
):
    print(f"  media#{r[0]} {r[1][:38]:<40} 切片 {r[2]}  字幕 {r[3]}")

print("\n=== 工程#9 的时长分布（看重复的是不是同一份文件）===")
for r in con.execute(
    "SELECT name, COUNT(*) n, GROUP_CONCAT(DISTINCT duration_ms) durs, "
    "COUNT(DISTINCT path) paths FROM media WHERE project_id=9 "
    "GROUP BY name HAVING n>1 ORDER BY name"
):
    print(f"  {r[0][:42]:<44} {r[1]} 条  时长集合={r[2]}  不同路径={r[3]}")
