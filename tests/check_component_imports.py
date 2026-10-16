"""扫一遍所有 .vue：模板里用到的 naive-ui 组件，import 里有没有。

naive-ui 是**按需引入**的（main.ts 里没全局注册），所以漏引入不会报错，
只会渲染不出东西 —— 用户点「播放」没反应，就是这么来的。

这类 bug 编译期查不出来，只能静态扫。
"""

import os
import re

SRC = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "src")

# n-modal -> NModal；n-input-number -> NInputNumber
def tag_to_component(tag: str) -> str:
    return "N" + "".join(p.capitalize() for p in tag[2:].split("-"))


problems = []
for dirpath, _, files in os.walk(SRC):
    for f in files:
        if not f.endswith(".vue"):
            continue
        path = os.path.join(dirpath, f)
        text = open(path, encoding="utf-8").read()

        idx = text.find("<template>")
        if idx < 0:
            continue
        template = text[idx:]
        script = text[:idx]

        used = {tag_to_component(t) for t in re.findall(r"<(n-[a-z0-9-]+)", template)}
        # 只看 naive-ui 的（useMessage 这类函数不算组件）
        used = {u for u in used if u != "N"}

        # 从 naive-ui 的 import 块里取
        m = re.search(r'import\s*\{([^}]*)\}\s*from\s*"naive-ui"', script, re.S)
        imported = set()
        if m:
            for name in m.group(1).split(","):
                name = name.strip()
                if name:
                    imported.add(name)

        missing = sorted(used - imported)
        if missing:
            problems.append((os.path.relpath(path, SRC), missing))

print("=== 模板里用了但没引入的 naive-ui 组件 ===")
if not problems:
    print("  没有 —— 所有页面都引入齐了")
for rel, missing in problems:
    print(f"  {rel}")
    for name in missing:
        print(f"      {name}")

print(f"\n共 {len(problems)} 个文件有问题")
