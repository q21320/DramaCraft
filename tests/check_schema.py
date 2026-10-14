"""一次性检查：运行时创建的数据库 schema 是否符合预期。

用法：先启动一次应用，再执行本脚本。
"""

import os
import sqlite3
import sys

EXPECTED_VERSION = 7
EXPECTED_TABLES = {"project", "media", "segment", "cue", "setting", "publish_plan", "job", "asset"}
EXPECTED_ASSET_COLUMNS = {
    "id",
    "kind",
    "name",
    "path",
    "category",
    "tags",
    "duration_ms",
    "size_bytes",
    "meta",
    "note",
    "created_at",
    "updated_at",
}
EXPECTED_PUBLISH_PLAN_COLUMNS = {
    "id",
    "media_id",
    "platform",
    "account",
    "title",
    "description",
    "tags",
    "cover_path",
    "status",
    "note",
    "updated_at",
}
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

publish_columns = {r[1] for r in con.execute("PRAGMA table_info(publish_plan)")}
print("publish_plan columns =", sorted(publish_columns))
missing_publish = EXPECTED_PUBLISH_PLAN_COLUMNS - publish_columns
if missing_publish:
    problems.append(f"publish_plan 缺少列：{sorted(missing_publish)}")

asset_columns = {r[1] for r in con.execute("PRAGMA table_info(asset)")}
print("asset columns =", sorted(asset_columns))
missing_asset = EXPECTED_ASSET_COLUMNS - asset_columns
if missing_asset:
    problems.append(f"asset 缺少列：{sorted(missing_asset)}")

if problems:
    for p in problems:
        print("FAIL:", p)
    print("RESULT = FAIL")
    sys.exit(1)

print("RESULT = PASS")
