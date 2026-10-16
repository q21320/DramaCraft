"""清理历史遗留的重复数据。

## 为什么需要

早期的流水线每次跑都新建工程、重新导入素材，于是同一个源目录下攒出好几个
工程。实测：三个工程指向同一个源目录，163 条素材里只有 65 个不同文件。

产出查询原来靠源目录关联工程，一次就把几次运行的数据全捞出来 ——
界面上同一集出现两三次，有的有字幕有的没有。

新代码已经不再产生这种数据了（复用工程、跳过已导入的素材），
但这个脚本用来把**已经产生的**收拾干净。

## 用法

    python tests/cleanup_duplicates.py          # 干跑，只看会做什么
    python tests/cleanup_duplicates.py --apply  # 真的动手

**默认干跑。** 删数据不可逆，先看清楚再执行。
"""

import os
import sqlite3
import sys

DB = os.path.join(os.environ["APPDATA"], "com.dramadist.app", "dramadist.db")
APPLY = "--apply" in sys.argv

con = sqlite3.connect(DB)
con.execute("PRAGMA foreign_keys = ON")


def q(sql, args=()):
    return list(con.execute(sql, args))


print("=== 清理前 ===")
print(f"  工程 {q('SELECT COUNT(*) FROM project')[0][0]}")
print(f"  素材 {q('SELECT COUNT(*) FROM media')[0][0]}"
      f"（不同文件 {q('SELECT COUNT(DISTINCT path) FROM media')[0][0]}）")
print(f"  切片 {q('SELECT COUNT(*) FROM segment')[0][0]}")
print(f"  字幕 {q('SELECT COUNT(*) FROM cue')[0][0]}")
print(f"  任务 {q('SELECT COUNT(*) FROM job')[0][0]}"
      f"（已取消 {q(chr(83)+chr(69)+chr(76)+chr(69)+chr(67)+chr(84)+' COUNT(*) FROM job WHERE status=\'canceled\'')[0][0]}）")

plan = []

# ---- 1. 同源目录的重复工程：留素材最多的那个 ----
print("\n=== 1. 重复工程 ===")
for (root,) in q("SELECT root_dir FROM project GROUP BY root_dir HAVING COUNT(*) > 1"):
    rows = q(
        "SELECT p.id, p.name, COUNT(m.id) n FROM project p "
        "LEFT JOIN media m ON m.project_id = p.id "
        "WHERE p.root_dir = ? GROUP BY p.id ORDER BY n DESC, p.id ASC",
        (root,),
    )
    keep, keep_name, keep_n = rows[0]
    print(f"  源目录 {root}")
    print(f"    保留 工程#{keep}（{keep_n} 条素材）")
    for pid, name, n in rows[1:]:
        print(f"    并入 工程#{pid}（{n} 条素材）")
        plan.append(("merge_project", pid, keep, root))

# ---- 2. 同一工程内的重复素材：同路径只留一条 ----
print("\n=== 2. 工程内重复素材 ===")
for (pid, path, n) in q(
    "SELECT project_id, path, COUNT(*) n FROM media "
    "GROUP BY project_id, path HAVING n > 1"
):
    ids = [r[0] for r in q(
        "SELECT id FROM media WHERE project_id=? AND path=? ORDER BY id", (pid, path))]
    print(f"  工程#{pid}  {os.path.basename(path)}  {n} 条 → 留 #{ids[0]}")
    for dup in ids[1:]:
        plan.append(("drop_media", dup, None, path))

# ---- 3. 重复工程里的素材搬到保留工程后，剩下的原件要删 ----
merging = [p[1] for p in plan if p[0] == "merge_project"]
keeps = {p[1]: p[2] for p in plan if p[0] == "merge_project"}
print("\n=== 3. 待并入工程的素材 ===")
for pid in merging:
    keep = keeps[pid]
    # 保留工程里已有同路径的，就是重复；直接删
    for (mid, path) in q("SELECT id, path FROM media WHERE project_id=?", (pid,)):
        exists = q(
            "SELECT id FROM media WHERE project_id=? AND path=? LIMIT 1", (keep, path)
        )
        if exists:
            plan.append(("drop_media", mid, None, path))
        else:
            plan.append(("move_media", mid, keep, path))

print(f"\n共 {len(plan)} 项改动：")
for kind, a, b, extra in plan:
    label = {
        "merge_project": "并入工程",
        "drop_media": "删除素材",
        "move_media": "改挂到工程",
    }[kind]
    print(f"  {label} #{a}" + (f" → #{b}" if b else "") + f"  {os.path.basename(extra)}")

# ---- 4. 清理无用任务 ----
dead_jobs = q(
    "SELECT COUNT(*) FROM job WHERE status IN ('canceled','failed')"
)[0][0]
print(f"\n=== 4. 可清理的任务 ===")
print(f"  已取消 / 已失败：{dead_jobs} 条（这些不会再跑，留着只是噪音）")

if not APPLY:
    print("\n（干跑模式，什么都没改。加 --apply 才真的执行）")
    sys.exit(0)

print("\n=== 开始执行 ===")
cur = con.cursor()

for kind, a, b, extra in plan:
    if kind == "move_media":
        cur.execute("UPDATE media SET project_id=? WHERE id=?", (b, a))
    elif kind == "drop_media":
        # 连带它的切片与字幕一起删 —— 素材没了，这些也没有意义
        cur.execute("DELETE FROM cue WHERE media_id=?", (a,))
        cur.execute("DELETE FROM segment WHERE media_id=?", (a,))
        cur.execute("DELETE FROM media WHERE id=?", (a,))

# 搬空了的工程删掉
for kind, a, b, extra in plan:
    if kind == "merge_project":
        left = q("SELECT COUNT(*) FROM media WHERE project_id=?", (a,))[0][0]
        if left == 0:
            cur.execute("DELETE FROM project WHERE id=?", (a,))
            cur.execute("UPDATE job SET media_id = NULL WHERE media_id NOT IN (SELECT id FROM media)")

cur.execute("DELETE FROM job WHERE status IN ('canceled','failed')")
con.commit()

print("\n=== 清理后 ===")
print(f"  工程 {q('SELECT COUNT(*) FROM project')[0][0]}")
print(f"  素材 {q('SELECT COUNT(*) FROM media')[0][0]}"
      f"（不同文件 {q('SELECT COUNT(DISTINCT path) FROM media')[0][0]}）")
print(f"  切片 {q('SELECT COUNT(*) FROM segment')[0][0]}")
print(f"  字幕 {q('SELECT COUNT(*) FROM cue')[0][0]}")
print(f"  任务 {q('SELECT COUNT(*) FROM job')[0][0]}")
print("\n完成。跑过多次的流水线记录（pipeline_run）保留着，产出页面现在")
print("按各次运行自己的工程查，不会再串。")
