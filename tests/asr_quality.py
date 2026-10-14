"""看 ASR 的实际产出质量。"""

import os
import sqlite3

DB = os.path.join(os.environ["APPDATA"], "com.dramadist.app", "dramadist.db")
con = sqlite3.connect(DB)

print("=== 按素材看字幕条数 ===")
for r in con.execute(
    """
    SELECT m.id, m.name, COUNT(c.id) AS n
      FROM media m LEFT JOIN cue c ON c.media_id = m.id
     GROUP BY m.id HAVING n > 0 ORDER BY m.id LIMIT 20
    """
):
    print(f"  #{r[0]:<4} {r[1][:42]:<44} {r[2]} 条")

print("\n=== 第 1 集全部字幕（看识别质量）===")
first = con.execute(
    """
    SELECT media_id FROM cue GROUP BY media_id ORDER BY media_id LIMIT 1
    """
).fetchone()
if first:
    mid = first[0]
    for r in con.execute(
        "SELECT start_ms, end_ms, text FROM cue WHERE media_id=? ORDER BY start_ms LIMIT 25",
        (mid,),
    ):
        s, e = r[0] / 1000, r[1] / 1000
        print(f"  [{s:6.1f}-{e:6.1f}] {r[2]}")

print("\n=== 随机抽 3 条看有没有繁体和乱码 ===")
for r in con.execute(
    "SELECT media_id, start_ms, text FROM cue ORDER BY RANDOM() LIMIT 3"
):
    print(f"  media#{r[0]} {r[1]/1000:.1f}s  {r[2]}")

print("\n=== 全部字幕的正文字数 ===")
tot = con.execute("SELECT COUNT(*), SUM(LENGTH(text)) FROM cue").fetchone()
print(f"  {tot[0]} 条，共 {tot[1]} 字，平均 {tot[1]/max(tot[0],1):.1f} 字/条")

# 繁体字抽查（常见繁体）
trad = "這來們個時說對後點裡發現開關門問題電話"
n = 0
for ch in trad:
    c = con.execute("SELECT COUNT(*) FROM cue WHERE text LIKE ?", (f"%{ch}%",)).fetchone()[0]
    n += c
print(f"  常见繁体字出现次数: {n}（应当接近 0，说明繁转简生效）")
