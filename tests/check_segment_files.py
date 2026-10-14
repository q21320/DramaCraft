"""切片文件的落盘情况：库里有几条，磁盘上真有几个。"""

import os
import sqlite3
from collections import Counter

DB = os.path.join(os.environ["APPDATA"], "com.dramadist.app", "dramadist.db")
con = sqlite3.connect(DB)

print("=== 每个片段的实际路径 ===")
rows = list(
    con.execute(
        "SELECT s.id, s.media_id, s.index_no, s.out_path, s.original_path, s.status "
        "FROM segment s ORDER BY s.id"
    )
)
for r in rows:
    exists = os.path.isfile(r[3]) if r[3] else False
    print(f"  #{r[0]:<3} media={r[1]:<3} 第{r[2]}片 {'有' if exists else '缺失'}  {r[3]}")
    if r[4]:
        print(f"        原片 {r[4]}")

paths = [r[3] for r in rows if r[3]]
print(f"\n库里有 {len(rows)} 条，路径去重后 {len(set(paths))} 个")
dup = {p: n for p, n in Counter(paths).items() if n > 1}
if dup:
    print("\n!! 路径重复（后写的把先写的覆盖了）：")
    for p, n in dup.items():
        print(f"   {n} 次  {p}")

print("\n=== 输出目录实际结构 ===")
root = r"D:\BaiduNetdiskDownload\弟弟结婚，我的新车竟成了贺礼二创"
for dirpath, dirnames, filenames in os.walk(root):
    rel = dirpath.replace(root, "") or "\\"
    print(f"  {rel}  ({len(filenames)} 个文件)")
    for f in filenames[:4]:
        print(f"      {f}")
    if len(filenames) > 4:
        print(f"      ... 共 {len(filenames)} 个")
