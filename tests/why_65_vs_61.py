"""63 vs 61：工程#9 到底把什么当成了剧集。"""

import os
import re
import sqlite3

DB = os.path.join(os.environ["APPDATA"], "com.dramadist.app", "dramadist.db")
SRC = r"D:\BaiduNetdiskDownload\弟弟结婚，我的新车竟成了贺礼"
con = sqlite3.connect(DB)

print("=== 源目录现在有什么 ===")
real = {}          # 集号 -> 文件名
others = []
for f in sorted(os.listdir(SRC)):
    full = os.path.join(SRC, f)
    if not os.path.isfile(full):
        continue
    if f.lower().endswith(".mp4"):
        m = re.search(r"第(\d+)集", f)
        if m:
            real[int(m.group(1))] = f
        else:
            others.append(("无集号 mp4", f))
    else:
        others.append(("非 mp4", f))

nums = sorted(real)
print(f"  规范命名的 mp4: {len(real)} 个")
print(f"  集号范围: {nums[0]} ~ {nums[-1]}" if nums else "  （没有）")
missing = [n for n in range(nums[0], nums[-1] + 1) if n not in real]
if missing:
    print(f"  !! 中间缺号: {missing}")
print("  其他文件:")
for kind, f in others:
    print(f"    [{kind}] {f}")

print("\n=== 各工程导入了多少、都是什么 ===")
for pid in (8, 9, 10):
    rows = list(
        con.execute(
            "SELECT id, name, path FROM media WHERE project_id=? ORDER BY id", (pid,)
        )
    )
    if not rows:
        print(f"  工程#{pid}: 无")
        continue
    names = {}
    for _id, name, _path in rows:
        m = re.search(r"第(\d+)集", name)
        if m:
            names.setdefault(int(m.group(1)), []).append(name)
        else:
            names.setdefault(-1, []).append(name)
    nums_p = sorted(n for n in names if n > 0)
    dup = {n: v for n, v in names.items() if len(v) > 1}
    print(f"  工程#{pid}: {len(rows)} 条素材，集号 {len(nums_p)} 个"
          + (f"，范围 {nums_p[0]}~{nums_p[-1]}" if nums_p else ""))
    if dup:
        print(f"      !! 同一集号导入多次: {list(dup)[:8]}")
    if -1 in names:
        print(f"      !! 没有集号的素材 {len(names[-1])} 条: {names[-1][:4]}")
    extra = [n for n in nums_p if n not in real]
    if extra:
        print(f"      !! 集号在源目录里不存在: {extra[:10]}")
    gone = [n for n in real if n not in nums_p]
    if gone:
        print(f"      源目录里有但没导入: {gone[:10]}")
