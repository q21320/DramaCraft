"""用 Python 独立的 zipfile + ElementTree 打开 docx，确认它真的能用。

我们自己写的 ZIP 与 OOXML 不能自己证明自己 —— 换一个实现能打开、能读出文字，
才算数。用法::

    python check_docx.py <path.docx>
"""

import sys
import xml.etree.ElementTree as ET
import zipfile

W = "{http://schemas.openxmlformats.org/wordprocessingml/2006/main}"
REQUIRED = [
    "[Content_Types].xml",
    "_rels/.rels",
    "word/document.xml",
    "word/styles.xml",
    "word/_rels/document.xml.rels",
    "docProps/core.xml",
]
EXPECTED_TEXT = ["短剧制作成本明细表", "都市爽剧", "编剧", "18000"]

path = sys.argv[1]
problems = []

try:
    archive = zipfile.ZipFile(path)
except Exception as exc:  # noqa: BLE001
    print("打不开这个 zip：", exc)
    print("RESULT = FAIL")
    sys.exit(1)

# CRC 校验：我们手写的 CRC32 只要错一位，这里就会报出来
bad = archive.testzip()
if bad:
    problems.append(f"CRC 校验失败：{bad}")

names = set(archive.namelist())
print("entries =", sorted(names))
for required in REQUIRED:
    if required not in names:
        problems.append(f"缺少 {required}")

texts = []
for name in sorted(names):
    if not (name.endswith(".xml") or name.endswith(".rels")):
        continue
    try:
        root = ET.fromstring(archive.read(name))
    except Exception as exc:  # noqa: BLE001
        problems.append(f"{name} 不是合法 XML：{exc}")
        continue
    texts.extend(node.text or "" for node in root.iter(f"{W}t"))

joined = "".join(texts)
print("正文片段 =", joined[:120])

for needle in EXPECTED_TEXT:
    if needle not in joined:
        problems.append(f"正文里找不到「{needle}」")

if "word/document.xml" in names:
    doc = ET.fromstring(archive.read("word/document.xml"))
    tables = list(doc.iter(f"{W}tbl"))
    rows = list(doc.iter(f"{W}tr"))
    print(f"表格 = {len(tables)}，行 = {len(rows)}")
    if not tables:
        problems.append("文档里没有表格")
    # 表头 1 行 + 2 条明细
    if len(rows) < 3:
        problems.append(f"表格行数不对：{len(rows)}")

archive.close()

if problems:
    for item in problems:
        print("问题：", item)
    print("RESULT = FAIL")
    sys.exit(1)

print("RESULT = PASS")
