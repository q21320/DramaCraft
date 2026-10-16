"""查界面上会不会出现 markdown 星号。

用户看到过两次 `**会改变集数**` 这样的原始星号 —— Vue 模板不认 markdown，
后端塞进界面的纯文本也不认。写成 `**加粗**` 只会原样显示，很难看。

两类来源都要查：
1. `.vue` 模板里直接写的
2. 后端**渲染到界面上的字符串**（步骤说明、按钮文案之类）

编译和类型检查都发现不了，只能静态扫。
"""

import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SRC = os.path.join(ROOT, "src")
RUST = os.path.join(ROOT, "src-tauri", "src")

# 会渲染到界面上的中文字符串才算。Rust 里注释写 markdown 无所谓（编译后不可见），
# 所以只查字符串字面量。
MD = re.compile(r"\*\*[^*\n]{1,80}\*\*")

problems = []

# ---- 1. vue 模板 ----
for dirpath, _, files in os.walk(SRC):
    for f in files:
        if not f.endswith(".vue"):
            continue
        path = os.path.join(dirpath, f)
        text = open(path, encoding="utf-8-sig").read()
        idx = text.find("<template>")
        if idx < 0:
            continue
        # HTML 注释不会渲染，不算问题
        tpl = re.sub(r'<!--.*?-->', '', text[idx:], flags=re.S)
        for m in MD.finditer(tpl):
            problems.append((os.path.relpath(path, ROOT), "模板", m.group(0)))

# ---- 2. Rust 里会显示到界面的字符串 ----
# 判据：整行里出现 **…**，且这一行不是注释（不以 // 或 /// 开头）
for dirpath, _, files in os.walk(RUST):
    for f in files:
        if not f.endswith(".rs"):
            continue
        path = os.path.join(dirpath, f)
        for n, line in enumerate(open(path, encoding="utf-8-sig"), 1):
            stripped = line.strip()
            if stripped.startswith("//"):
                continue
            for m in MD.finditer(line):
                problems.append((f"{os.path.relpath(path, ROOT)}:{n}", "字符串", m.group(0)))

print("=== 界面上会出现原始星号的地方 ===")
if not problems:
    print("  没有 —— 干净")
for where, kind, what in problems:
    print(f"  [{kind}] {where}")
    print(f"      {what}")

print(f"\n共 {len(problems)} 处")
sys.exit(1 if problems else 0)
