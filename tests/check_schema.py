"""一次性检查：运行时创建的数据库 schema 是否符合预期。

用法：先启动一次应用，再执行本脚本。
"""

import os
import sqlite3
import sys

EXPECTED_VERSION = 4
EXPECTED_TABLES = {"project", "media", "segment", "cue", "setting", "publish_plan"}
EXPECTED_SEGMENT_COLUMNS = {
    "id",
    "media_id",
    "index_no",
    "start_ms",
    "end_ms",
    "out_path",
    "size_bytes",
    "status",
    "message",
    "original_path",
}

db = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
    os.environ["APPDATA"], "com.dramadist.app", "dramadist.db"
)
con = sqlite3.connect(db)

version = con.execute("PRAGMA user_version").fetchone()[0]
print("db =", db)
print("user_version =", version)

tables = sorted(
    r[0]
    for r in con.execute(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
    )
)
print("tables =", tables)

segment_columns = {r[1] for r in con.execute("PRAGMA table_info(segment)")}
print("segment columns =", sorted(segment_columns))
print("cue columns =", [r[1] for r in con.execute("PRAGMA table_info(cue)")])

indexes = sorted(
    r[0]
    for r in con.execute(
        "SELECT name FROM sqlite_master WHERE type='index' AND name NOT LIKE 'sqlite_%'"
    )
)
print("indexes =", indexes)

problems = []
if version != EXPECTED_VERSION:
    problems.append(f"user_version 应为 {EXPECTED_VERSION}，实际 {version}")
missing_tables = EXPECTED_TABLES - set(tables)
if missing_tables:
    problems.append(f"缺少表：{sorted(missing_tables)}")
missing_columns = EXPECTED_SEGMENT_COLUMNS - segment_columns
if missing_columns:
    problems.append(f"segment 缺少列：{sorted(missing_columns)}")

if problems:
    for p in problems:
        print("FAIL:", p)
    print("RESULT = FAIL")
    sys.exit(1)

print("RESULT = PASS")
