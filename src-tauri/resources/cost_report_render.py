# -*- coding: utf-8 -*-
"""成本配置比例情况报告 —— 渲染成 JPG。

版式照真实软件的产出（`zhengming/<剧名>_成本配置比例情况报告.jpg`，
1241×1754，A4 竖版）：

    成本配置比例情况报告            ← 居中大标题
    我公司提审的 1 部"其他微短剧"，详情见下：
    ┌────┬──────┬────────┬────────┬────────┐
    │序号│名称  │集数时长│总投资额│片酬占比│
    └────┴──────┴────────┴────────┴────────┘
    以上微短剧制作完成的…均符合《…意见》要求。
    我公司郑重承诺拥有上述 1 部剧目的播出版权…
                       [公章图片]  报审机构：<机构名>
                                  法定代表人或总编辑：<签名图片>
                                          2026 年 10 月 16 日

**报审机构 / 法定代表人 / 公章 / 签名 全部由调用方传入，不预填任何公司名。**
真实软件那份产出里印着它自己的公司名和签名 —— 那是用户自己的材料，
不能写死在代码里。

调用方式（stdin 一行 JSON → stdout 一行 JSON）::

    {"out_path": "...", "title": "某剧", "episodes": 28, "minutes": 33,
     "invest_wan": 1, "actor_ratio": 30,
     "org": "某某公司", "legal_person": "张三", "date": "2026 年 10 月 16 日",
     "seal_path": "D:/公章.png", "sign_path": "D:/签名.png"}
"""

from __future__ import annotations

import json
import os
import sys
import traceback

for _s in (sys.stdout, sys.stderr):
    try:
        _s.reconfigure(encoding="utf-8")
    except Exception:
        pass

# A4 竖版 150dpi ≈ 1241×1754（和真实软件的产出一样）
W, H = 1241, 1754

# 字体按优先级找，中文 Windows 上必然有
FONT_SONG = [
    r"C:\Windows\Fonts\simfang.ttf",   # 仿宋
    r"C:\Windows\Fonts\simsun.ttc",    # 宋体
    r"C:\Windows\Fonts\msyh.ttc",
]
FONT_HEI = [
    r"C:\Windows\Fonts\simhei.ttf",    # 黑体（标题用）
    r"C:\Windows\Fonts\msyhbd.ttc",
    r"C:\Windows\Fonts\msyh.ttc",
]


def pick_font(paths):
    for p in paths:
        if os.path.isfile(p):
            return p
    return None


def emit(payload):
    sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def draw_text_wrap(draw, text, x, y, max_w, font, fill=(0, 0, 0), line_gap=10):
    """按像素宽度折行，返回下一行的 y。中文按字符切就行（不像英文要按词）。"""
    line = ""
    for ch in text:
        trial = line + ch
        if draw.textlength(trial, font=font) > max_w and line:
            draw.text((x, y), line, font=font, fill=fill)
            y += font.size + line_gap
            line = ch
        else:
            line = trial
    if line:
        draw.text((x, y), line, font=font, fill=fill)
        y += font.size + line_gap
    return y


def render(req):
    from PIL import Image, ImageDraw, ImageFont

    song = pick_font(FONT_SONG)
    hei = pick_font(FONT_HEI)
    if not song or not hei:
        raise RuntimeError("找不到中文字体（仿宋/宋体/黑体都没有）")

    img = Image.new("RGB", (W, H), "white")
    d = ImageDraw.Draw(img)

    f_title = ImageFont.truetype(hei, 46)
    f_body = ImageFont.truetype(song, 32)
    f_head = ImageFont.truetype(song, 28)
    f_cell = ImageFont.truetype(song, 30)

    margin = 70
    y = 110

    # ---- 标题 ----
    title = "成本配置比例情况报告"
    tw = d.textlength(title, font=f_title)
    d.text(((W - tw) / 2, y), title, font=f_title, fill=(0, 0, 0))
    y += 90

    # ---- 提审说明 ----
    n = 1  # 一部剧
    d.text((margin, y), f"我公司提审的 {n} 部“其他微短剧”，详情见下：", font=f_body, fill=(0, 0, 0))
    y += 66

    # ---- 表格 ----
    cols = [
        ("序号", 90),
        ("微短剧名称", 340),
        ("集数和总时长", 240),
        ("总投资额\n（需＜100 万）", 210),
        ("演员总片酬占比\n（需＜40%）", 0),
    ]
    total_fixed = sum(c[1] for c in cols[:-1])
    # 最后一列吃掉剩余宽度，**但不能算出负数** ——
    # 第一次就因为固定列加起来超了可用宽度，报 `x1 must be >= x0`。
    last_w = max(140, W - 2 * margin - total_fixed)
    cols[-1] = (cols[-1][0], last_w)

    head_h = 130
    row_h = 150
    x0, x1 = margin, W - margin

    # 表头
    x = x0
    for name, cw in cols:
        d.rectangle([x, y, x + cw, y + head_h], outline=(0, 0, 0), width=2)
        lines = name.split("\n")
        ty = y + (head_h - len(lines) * 34) / 2
        for ln in lines:
            lw = d.textlength(ln, font=f_head)
            d.text((x + (cw - lw) / 2, ty), ln, font=f_head, fill=(0, 0, 0))
            ty += 36
        x += cw
    y += head_h

    # 数据行
    def fmt_num(v):
        return str(int(v)) if abs(v - round(v)) < 0.005 else f"{v:.2f}".rstrip("0").rstrip(".")

    values = [
        "1",
        req.get("title", ""),
        f"{req.get('episodes', 0)} 集，共 {req.get('minutes', 0)} 分钟",
        f"{fmt_num(req.get('invest_wan', 0))} 万元",
        f"{fmt_num(req.get('actor_ratio', 0))}%",
    ]
    x = x0
    for (name, cw), val in zip(cols, values):
        d.rectangle([x, y, x + cw, y + row_h], outline=(0, 0, 0), width=2)
        # 名称那列可能长，折行显示
        if name == "微短剧名称":
            lines = []
            line = ""
            for ch in val:
                if d.textlength(line + ch, font=f_cell) > cw - 24 and line:
                    lines.append(line)
                    line = ch
                else:
                    line += ch
            if line:
                lines.append(line)
            ty = y + (row_h - len(lines) * 38) / 2
            for ln in lines:
                lw = d.textlength(ln, font=f_cell)
                d.text((x + (cw - lw) / 2, ty), ln, font=f_cell, fill=(0, 0, 0))
                ty += 38
        else:
            lw = d.textlength(val, font=f_cell)
            d.text((x + (cw - lw) / 2, y + (row_h - f_cell.size) / 2), val, font=f_cell, fill=(0, 0, 0))
        x += cw
    y += row_h + 70

    # ---- 两段正文 ----
    body1 = ("以上微短剧制作完成的总投资额和演员总片酬占比均符合"
             "《关于电视剧网络剧制作成本配置比例的意见》要求。")
    y = draw_text_wrap(d, body1, margin, y, W - 2 * margin, f_body, line_gap=14)
    y += 20
    body2 = (f"我公司郑重承诺拥有上述 {n} 部剧目的播出版权，并承诺上述内容真实有效。"
             "如因材料真实性、演员片酬占比等问题产生的不良后果，我公司愿承担相关责任。")
    y = draw_text_wrap(d, body2, margin, y, W - 2 * margin, f_body, line_gap=14)

    # ---- 落款 ----
    seal_y = max(y + 60, H - 430)
    org = (req.get("org") or "").strip()
    legal = (req.get("legal_person") or "").strip()
    date = (req.get("date") or "").strip()
    if not date:
        date = "年    月    日"

    line_x = W - margin - 620
    org_line = f"报审机构：{org}"
    d.text((line_x, seal_y), org_line, font=f_body, fill=(0, 0, 0))

    # **公章盖在「报审机构」这一行的右侧空白处。**
    #
    # 踩过：之前按固定 x 贴，结果章压在「许昌简耀科技有限公司」上面，
    # 公司名被糊掉。真实的盖章件是**章盖在公司名右边**的空白处、
    # 略微压住落款行 —— 所以按文字实际宽度算位置。
    seal_path = (req.get("seal_path") or "").strip()
    if seal_path and os.path.isfile(seal_path):
        try:
            seal = Image.open(seal_path).convert("RGBA")
            seal.thumbnail((300, 300))
            org_w = d.textlength(org_line, font=f_body)
            sx = int(line_x + org_w + 24)
            sy = int(seal_y - seal.size[1] * 0.45)
            # 别贴出纸外
            sx = min(sx, W - margin - seal.size[0])
            sy = max(sy, 0)
            img.paste(seal, (sx, sy), seal)
        except Exception:
            pass

    sign_y = seal_y + 110
    d.text((line_x, sign_y), "法定代表人或总编辑：", font=f_body, fill=(0, 0, 0))
    sign_path = (req.get("sign_path") or "").strip()
    # **签名优先用图片。**
    #
    # 踩过：先判断 `legal` 有没有字，有就打字、不打图 —— 结果传了签名图也
    # 只显示印刷体名字，看着一眼假。手写签名是图片，不是文字。
    lw = d.textlength("法定代表人或总编辑：", font=f_body)
    pasted = False
    if sign_path and os.path.isfile(sign_path):
        try:
            sign = Image.open(sign_path).convert("RGBA")
            sign.thumbnail((240, 100))
            img.paste(sign, (int(line_x + lw + 6), int(sign_y - 26)), sign)
            pasted = True
        except Exception:
            pasted = False
    if not pasted and legal:
        d.text((line_x + lw, sign_y), legal, font=f_body, fill=(0, 0, 0))

    date_y = sign_y + 120
    dw = d.textlength(date, font=f_body)
    d.text((W - margin - dw, date_y), date, font=f_body, fill=(0, 0, 0))

    out = req.get("out_path") or ""
    if not out:
        raise RuntimeError("没给 out_path")
    os.makedirs(os.path.dirname(out) or ".", exist_ok=True)
    img.save(out, "JPEG", quality=92)
    return {"ok": True, "out_path": out, "width": W, "height": H}


def main():
    # **必须按字节读、显式 UTF-8 解码。**
    #
    # `sys.stdin.readline()` 在中文 Windows 上按 **GBK(cp936)** 解码，
    # 而调用方写进来的是 UTF-8 —— 剧名会变成一串乱码（借车之后 → 鏅鸿溅娣插儧）。
    # 踩过：第一次渲染出来的报告，剧名、机构名、日期全是乱码。
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
        emit(render(req))
        return 0
    except Exception as e:
        emit({"ok": False, "error": f"{type(e).__name__}: {e}",
              "trace": traceback.format_exc()[-800:]})
        return 4


if __name__ == "__main__":
    raise SystemExit(main())
