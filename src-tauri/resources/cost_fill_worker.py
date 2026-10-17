# -*- coding: utf-8 -*-
"""按模板图 + layout.json 填成本配置比例报告。

**思路跟原始软件一样**（这是从 `E:\\chaoren\\zhengming\\` 里看出来的）：

    muban.jpg   ← 空表格，公章和签名已经盖在上面了
    layout.json ← 每个字段的「擦除区」和「填写区」坐标
        ↓
    ① 按 erase_rect 把模板里残留的旧字擦白
    ② 按 box 写新字（仿宋 16px，灰 #757575，居中，自动折行）
    ③ 存 JPG

**章和签名来自模板图，不是我画的** —— 我画不出公章和手写签名。

stdin 一行 JSON，stdout 一行 JSON::

    {"template": "E:/chaoren/zhengming/muban.jpg",
     "layout": "E:/chaoren/zhengming/layout.json",
     "font": "E:/chaoren/zhengming/fonts/simfang.ttf",
     "out": "D:/out/成片_成本报告_filled.jpg",
     "fields": {"drama_name": "被理所当然的她",
                "episodes_duration": "4集，共6分钟",
                "investment": "1 万元",
                "actor_ratio": "0%"}}
"""

from __future__ import annotations

import io
import json
import os
import sys
import traceback

for _s in (sys.stdout, sys.stderr):
    try:
        _s.reconfigure(encoding="utf-8")
    except Exception:
        pass


def emit(payload):
    sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def wrap_lines(draw, text, font, max_w):
    """按宽度折行。中文没有空格，逐字量。"""
    lines, cur = [], ""
    for ch in text:
        if ch == "\n":
            lines.append(cur)
            cur = ""
            continue
        trial = cur + ch
        if draw.textlength(trial, font=font) <= max_w or not cur:
            cur = trial
        else:
            lines.append(cur)
            cur = ch
    if cur:
        lines.append(cur)
    return lines


def fit_font(draw, text, font_path, size, min_size, max_w, max_h, line_spacing):
    """从 size 往下试，直到这块字放得进 max_w × max_h。"""
    from PIL import ImageFont

    s = size
    while s >= min_size:
        font = ImageFont.truetype(font_path, s)
        lines = wrap_lines(draw, text, font, max_w)
        line_h = s * line_spacing
        if len(lines) * line_h <= max_h:
            return font, lines, line_h
        s -= 1
    font = ImageFont.truetype(font_path, min_size)
    return font, wrap_lines(draw, text, font, max_w), min_size * line_spacing


def run(req):
    from PIL import Image, ImageDraw

    template = (req.get("template") or "").strip()
    layout_path = (req.get("layout") or "").strip()
    out_path = (req.get("out") or "").strip()
    fields = req.get("fields") or {}

    if not os.path.isfile(template):
        raise RuntimeError(f"模板图不存在：{template}")
    if not os.path.isfile(layout_path):
        raise RuntimeError(f"layout.json 不存在：{layout_path}")
    if not out_path:
        raise RuntimeError("没给 out")

    # **用 utf-8-sig**：Windows 上另存的 JSON 常带 BOM，
    # 普通 utf-8 读会在第一个字符就炸。
    layout = json.load(open(layout_path, encoding="utf-8-sig"))
    want_w, want_h = layout.get("image_size") or [1280, 1656]

    img = Image.open(template).convert("RGB")
    # 模板尺寸和 layout 不一致就缩放 —— 坐标是按 layout 的尺寸给的
    if img.size != (want_w, want_h):
        img = img.resize((want_w, want_h), Image.LANCZOS)
    draw = ImageDraw.Draw(img)

    font_path = (req.get("font") or "").strip()
    if not font_path or not os.path.isfile(font_path):
        # 退回系统仿宋
        for cand in (
            r"C:\Windows\Fonts\simfang.ttf",
            r"C:\Windows\Fonts\FangSong.ttf",
        ):
            if os.path.isfile(cand):
                font_path = cand
                break
    if not font_path or not os.path.isfile(font_path):
        raise RuntimeError("找不到仿宋字体")

    spec = layout.get("fields") or {}
    filled = []
    for key, text in fields.items():
        text = (text or "").strip()
        if not text:
            continue
        cfg = spec.get(key)
        if not cfg:
            # layout 里没定义就不瞎写，记一条警告让调用方知道
            continue

        # ① 擦除：把这一格里的旧内容涂白
        #
        # **两种坐标格式不一样，别搞混**：
        #   erase_rect: [x1, y1, x2, y2]   两个角
        #   box:        [x, y, w, h]      起点 + 尺寸
        #
        # 踩过：全当成 [x,y,w,h] 处理，结果擦掉了一大块正文。
        er = cfg.get("erase_rect")
        if er:
            x1, y1, x2, y2 = er
            draw.rectangle([x1, y1, x2 - 1, y2 - 1], fill=(255, 255, 255))

        # ② 填写
        bx, by, bw, bh = cfg.get("box") or (0, 0, 100, 40)
        font, lines, line_h = fit_font(
            draw,
            text,
            font_path,
            int(cfg.get("font_size") or 16),
            int(cfg.get("min_font_size") or cfg.get("font_size") or 16),
            bw,
            bh,
            float(cfg.get("line_spacing") or 1.12),
        )
        color = cfg.get("color") or "#757575"
        total_h = len(lines) * line_h
        ty = by + (bh - total_h) / 2
        for line in lines:
            lw = draw.textlength(line, font=font)
            tx = bx + (bw - lw) / 2 if (cfg.get("align") or "center") == "center" else bx
            draw.text((tx, ty), line, font=font, fill=color)
            ty += line_h
        filled.append(key)

    os.makedirs(os.path.dirname(os.path.abspath(out_path)) or ".", exist_ok=True)
    img.save(out_path, "JPEG", quality=95)
    return {
        "ok": True,
        "out": out_path,
        "size": f"{img.size[0]}x{img.size[1]}",
        "filled": filled,
        "skipped": sorted(set(fields) - set(filled)),
    }


def main():
    raw = sys.stdin.buffer.readline().decode("utf-8", "replace")
    if not raw.strip():
        emit({"ok": False, "error": "没收到请求"})
        return 2
    try:
        req = json.loads(raw)
    except Exception as e:
        emit({"ok": False, "error": f"请求不是合法 JSON：{e}"})
        return 2
    try:
        emit(run(req))
        return 0
    except Exception as e:
        emit({"ok": False, "error": f"{type(e).__name__}: {e}",
              "trace": traceback.format_exc()[-800:]})
        return 4


if __name__ == "__main__":
    raise SystemExit(main())
