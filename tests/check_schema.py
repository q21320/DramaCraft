"""一次性检查：运行时创建的数据库 schema 是否符合预期。"""
import os
import sqlite3

db = os.path.join(os.environ["APPDATA"], "com.dramadist.app", "dramadist.db")
con = sqlite3.connect(db)

print("user_version =", con.execute("PRAGMA user_version").fetchone()[0])

tables = sorted(
    r[0]
    for r in con.execute(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'"
    )
)
print("tables =", tables)

print("cue columns =", [r[1] for r in con.execute("PRAGMA table_info(cue)")])
print("media columns =", [r[1] for r in con.execute("PRAGMA table_info(media)")])

indexes = sorted(
    r[0]
    for r in con.execute(
        "SELECT name FROM sqlite_master WHERE type='index' AND name NOT LIKE 'sqlite_%'"
    )
)
print("indexes =", indexes)

expected = {"project", "media", "segment", "cue", "setting"}
missing = expected - set(tables)
print("missing tables =", sorted(missing) if missing else "无")
print("RESULT =", "PASS" if not missing else "FAIL")
