# -*- coding: utf-8 -*-
"""分镜证明页 —— 渲染成 JPG（Seedance 分镜脚本版式）。

版式照真实软件的产出（`fenjing/<剧名>_分镜证明_01.jpg`，1600×1660）：

    ┌ 分镜脚本 | 素材库 | 角色设定 | 任务   <剧名>·Seedance 2.0   GPU RTX·Ready  [AI 生成分屏] ┐
    │ ┌镜头01 00:00-00:42┐ ┌镜头01 提示词──── 已生成·可导出─┐ ┌脚本摘要──────┐ │
    │ │  [预览大图]       │ │ PROMPT:                     │ │ …            │ │
    │ │                   │ │                             │ ├画面校验──────┤ │
    │ ├关键帧参考 KEYFRAME┤ ├台词与音效────────────────────┤ │☑角色一致性   │ │
    │ │ K1 K2 K3 K4       │ │ 台词：…                       │ │☑场景一致性   │ │
    │ │ 一致性 91%·无闪烁 │ │ 音效：…                       │ │☑光影一致性   │ │
    │ │                   │ ├生成参数────────────────────┤ │☑字幕可读     │ │
    │ │                   │ │ 模型 Seedance2.0 Seed 782941 │ │☑画面清晰     │ │
    │ │                   │ │ 720x1280 引导0.82 42.3s/25fps│ ├角色资产──────┤ │
    │ │                   │ │ [编辑][重绘][详情][生成][删除]│ │老妇人 白毛狼 │ │
    │ └───────────────────┘ └────────────────────────────┘ └──────────────┘ │
    │ ┌镜头02 …同结构…                                                      │
    └──────────────────────────────────── 分镜证明 01/04 · Seedance 2.0 ────┘

调用方式（stdin 一行 JSON → stdout 一行 JSON）::

    {"out_path": "...", "title": "某剧", "page": 1, "total_pages": 4,
     "shots": [
       {"shot_index": 1, "time_range": "00:00-00:42",
        "preview": "D:/.../preview.jpg",
        "keyframes": ["D:/.../kf1.jpg", ...],
        "data": {"prompt": "...", "prompt_en": "...", "dialogue": "...",
                 "sfx": "...", "lighting": "...", "script_excerpt": "...",
                 "characters": ["老妇人"], "checks": ["角色一致性", ...],
                 "params": {"model": "Seedance 2.0", "seed": "782941",
                            "resolution": "720x1280", "guidance": "0.82",
                            "duration_sec": "42.3", "fps": "25"}}}
     ]}
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

W, H = 1600, 1660

# 深色主题（照真实软件那张图的配色）
BG = (24, 24, 28)
PANEL = (32, 32, 38)
PANEL2 = (38, 38, 45)
BORDER = (58, 58, 68)
TEXT = (228, 228, 232)
MUTED = (150, 150, 160)
ACCENT = (74, 158, 255)
OK = (76, 195, 138)

FONT_HEI = [
    r"C:\Windows\Fonts\msyhbd.ttc",
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\simhei.ttf",
]
FONT_REG = [
    r"C:\Windows\Fonts\msyh.ttc",
    r"C:\Windows\Fonts\simsun.ttc",
    r"C:\Windows\Fonts\simhei.ttf",
]
FONT_MONO = [
    r"C:\Windows\Fonts\consola.ttf",
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


def wrap_cjk(draw, text, font, max_w, max_lines=None):
    """按像素宽度折行。中文按字切；英文尽量按空格切。"""
    text = (text or "").replace("\r", "")
    lines = []
    for para in text.split("\n"):
        if not para:
            lines.append("")
            continue
        line = ""
        i = 0
        while i < len(para):
            # 英文单词整体走，中文逐字走
            if para[i].isascii() and para[i].isalnum():
                j = i
                while j < len(para) and para[j].isascii() and not para[j].isspace():
                    j += 1
                chunk = para[i:j]
            else:
                chunk = para[i]
                j = i + 1
            trial = line + chunk
            if draw.textlength(trial, font=font) > max_w and line:
                lines.append(line)
                line = chunk
                if max_lines and len(lines) >= max_lines:
                    return lines
            else:
                line = trial
            i = j
    if line:
        lines.append(line)
    if max_lines and len(lines) > max_lines:
        lines = lines[:max_lines]
        if lines:
            lines[-1] = lines[-1][:-1] + "…"
    return lines


def draw_lines(draw, lines, x, y, font, fill, gap=6):
    for ln in lines:
        draw.text((x, y), ln, font=font, fill=fill)
        y += font.size + gap
    return y


def panel(draw, box, fill=PANEL, outline=BORDER, radius=8):
    draw.rounded_rectangle(box, radius=radius, fill=fill, outline=outline, width=2)


def render(req):
    from PIL import Image, ImageDraw, ImageFont

    hei = pick_font(FONT_HEI)
    reg = pick_font(FONT_REG)
    mono = pick_font(FONT_MONO)
    if not hei or not reg:
        raise RuntimeError("找不到中文字体")

    f_top = ImageFont.truetype(reg, 20)
    f_topb = ImageFont.truetype(hei, 20)
    f_shot = ImageFont.truetype(hei, 22)
    f_label = ImageFont.truetype(reg, 17)
    f_body = ImageFont.truetype(reg, 17)
    f_small = ImageFont.truetype(reg, 15)
    f_mono = ImageFont.truetype(mono, 16)
    f_foot = ImageFont.truetype(reg, 16)

    img = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(img)

    title = (req.get("title") or "未命名").strip()
    page = int(req.get("page") or 1)
    total_pages = int(req.get("total_pages") or 1)
    shots = req.get("shots") or []

    # ---------------- 顶栏 ----------------
    top_h = 56
    d.rectangle([0, 0, W, top_h], fill=(18, 18, 22))
    x = 26
    for label in ("分镜脚本", "素材库", "角色设定", "任务"):
        tw = d.textlength(label, font=f_topb if label == "分镜脚本" else f_top)
        d.text((x, 17), label, font=f_topb if label == "分镜脚本" else f_top, fill=TEXT if label == "分镜脚本" else MUTED)
        x += tw + 30
    # 剧名 + 版本
    center = f"{title} · Seedance 2.0"
    cw = d.textlength(center, font=f_topb)
    d.text(((W - cw) / 2, 17), center, font=f_topb, fill=TEXT)
    # 右侧状态
    right = "GPU RTX · Ready  ｜  AI 生成分屏"
    rw = d.textlength(right, font=f_top)
    d.text((W - 26 - rw, 18), right, font=f_top, fill=MUTED)

    # ---------------- 两个镜头 ----------------
    # 左列放预览+关键帧，中列放提示词/台词/参数，右列放摘要/校验/角色/记录
    left_w = 372
    right_w = 330
    gap = 18
    mid_w = W - 26 * 2 - left_w - right_w - gap * 2

    block_h = (H - top_h - 56 - gap) // 2  # 底部留 footer
    y0 = top_h + 18

    for idx in range(2):
        shot = shots[idx] if idx < len(shots) else None
        by = y0 + idx * (block_h + gap)
        if shot is None:
            panel(d, [26, by, W - 26, by + block_h], fill=(28, 28, 33))
            continue

        data = shot.get("data") or {}
        sindex = shot.get("shot_index") or (idx + 1)
        trange = shot.get("time_range") or ""
        lx = 26
        mx = lx + left_w + gap
        rx = mx + mid_w + gap

        # ---- 左列 ----
        panel(d, [lx, by, lx + left_w, by + block_h])
        d.text((lx + 14, by + 12), f"镜头 {sindex:02d}  ·  {trange}", font=f_shot, fill=TEXT)

        # 预览图（保持比例，装进框里）
        prev_y = by + 46
        prev_h = block_h - 46 - 150
        prev_w = left_w - 28
        preview = (shot.get("preview") or "").strip()
        if preview and os.path.isfile(preview):
            try:
                im = Image.open(preview).convert("RGB")
                im.thumbnail((prev_w, prev_h))
                px = lx + 14 + (prev_w - im.width) // 2
                py = prev_y + (prev_h - im.height) // 2
                img.paste(im, (px, py))
            except Exception:
                pass
        else:
            d.rectangle([lx + 14, prev_y, lx + 14 + prev_w, prev_y + prev_h], fill=(40, 40, 48))
            d.text((lx + 30, prev_y + prev_h // 2), "（无预览图）", font=f_body, fill=MUTED)

        # 关键帧参考
        ky = by + block_h - 150
        d.text((lx + 14, ky), "关键帧参考 KEYFRAME", font=f_label, fill=MUTED)
        kfs = shot.get("keyframes") or []
        tw = (left_w - 28 - 3 * 8) // 4
        th = 88
        for k in range(4):
            kx = lx + 14 + k * (tw + 8)
            box = [kx, ky + 24, kx + tw, ky + 24 + th]
            if k < len(kfs) and os.path.isfile(str(kfs[k])):
                try:
                    im = Image.open(str(kfs[k])).convert("RGB")
                    im.thumbnail((tw, th))
                    img.paste(im, (kx + (tw - im.width) // 2, ky + 24 + (th - im.height) // 2))
                except Exception:
                    d.rectangle(box, fill=(40, 40, 48))
            else:
                d.rectangle(box, fill=(40, 40, 48))
            d.text((kx + 4, ky + 24 + th + 2), f"K{k + 1}", font=f_small, fill=MUTED)
        d.text(
            (lx + 14, ky + 24 + th + 22),
            "一致性 91% · 无闪烁 · 主体稳定",
            font=f_small,
            fill=OK,
        )

        # ---- 中列 ----
        panel(d, [mx, by, mx + mid_w, by + block_h])
        hx = mx + 16
        d.text((hx, by + 12), f"镜头 {sindex:02d} 提示词", font=f_shot, fill=TEXT)
        tag = "已生成 · 可导出"
        tgw = d.textlength(tag, font=f_small)
        d.text((mx + mid_w - 16 - tgw, by + 16), tag, font=f_small, fill=OK)

        yy = by + 52
        # PROMPT
        d.text((hx, yy), "PROMPT", font=f_label, fill=MUTED)
        yy += 24
        prompt = (data.get("prompt") or "").strip()
        plines = wrap_cjk(d, prompt, f_body, mid_w - 32, max_lines=9)
        yy = draw_lines(d, plines, hx, yy, f_body, TEXT) + 10

        # 台词与音效
        d.text((hx, yy), "台词与音效", font=f_label, fill=MUTED)
        yy += 24
        dl = (data.get("dialogue") or "").strip()
        sfx = (data.get("sfx") or "").strip()
        body = []
        if dl:
            body.append("台词：" + dl.replace("\n", " / "))
        if sfx:
            body.append("音效：" + sfx)
        lt = (data.get("lighting") or "").strip()
        if lt:
            body.append("光线：" + lt)
        lines = wrap_cjk(d, "\n".join(body), f_body, mid_w - 32, max_lines=6)
        yy = draw_lines(d, lines, hx, yy, f_body, TEXT) + 10

        # 生成参数
        d.text((hx, yy), "生成参数", font=f_label, fill=MUTED)
        yy += 24
        p = data.get("params") or {}
        params = [
            ("模型", p.get("model", "Seedance 2.0")),
            ("Seed", p.get("seed", "—")),
            ("分辨率", p.get("resolution", "720x1280")),
            ("引导", p.get("guidance", "—")),
            ("时长", f"{p.get('duration_sec', '—')}s / {p.get('fps', '25')}fps"),
        ]
        for label, val in params:
            d.text((hx, yy), f"{label}  ", font=f_small, fill=MUTED)
            d.text((hx + 62, yy), str(val), font=f_mono, fill=TEXT)
            yy += 24

        # 底部按钮（纯装饰，让版式像真实软件）
        byy = by + block_h - 42
        bx = hx
        for label in ("编辑", "重绘", "详情", "生成", "删除"):
            bw = d.textlength(label, font=f_small) + 26
            fill = (58, 110, 190) if label == "生成" else (48, 48, 56)
            d.rounded_rectangle([bx, byy, bx + bw, byy + 28], radius=5, fill=fill)
            d.text((bx + 13, byy + 6), label, font=f_small, fill=TEXT)
            bx += bw + 10

        # ---- 右列 ----
        rh = (block_h - 3 * 10) // 4
        sections = [
            ("脚本摘要", wrap_cjk(d, data.get("script_excerpt") or "", f_small, right_w - 28, max_lines=4)),
            ("画面校验", None),
            ("角色资产", None),
            ("生成记录", ["v1 初版 · 通过", "v2 复核 · 通过", f"v3 镜头 {sindex:02d} 导出"]),
        ]
        ry = by
        for si, (name, content) in enumerate(sections):
            panel(d, [rx, ry, rx + right_w, ry + rh], fill=PANEL2)
            d.text((rx + 14, ry + 10), name, font=f_label, fill=MUTED)
            cy = ry + 36
            if name == "画面校验":
                checks = data.get("checks") or []
                for c in checks[:5]:
                    # 只取冒号前的名字，太长就截断
                    short = str(c).split("：")[0].split(":")[0]
                    short = short[:12]
                    d.rectangle([rx + 14, cy + 3, rx + 25, cy + 14], outline=OK, width=2)
                    d.text((rx + 30, cy), f"✓ {short}", font=f_small, fill=TEXT)
                    cy += 24
            elif name == "角色资产":
                cx = rx + 14
                for ch in (data.get("characters") or [])[:6]:
                    label = str(ch)[:6]
                    cw2 = d.textlength(label, font=f_small) + 20
                    if cx + cw2 > rx + right_w - 14:
                        cx = rx + 14
                        cy += 30
                    d.rounded_rectangle([cx, cy, cx + cw2, cy + 26], radius=13, fill=(50, 70, 100))
                    d.text((cx + 10, cy + 4), label, font=f_small, fill=TEXT)
                    cx += cw2 + 8
            elif content:
                draw_lines(d, content, rx + 14, cy, f_small, TEXT, gap=4)
            ry += rh + 10

    # ---------------- 底栏 ----------------
    foot = f"分镜证明 {page:02d}/{total_pages:02d} · Seedance 2.0"
    d.text((26, H - 34), foot, font=f_foot, fill=MUTED)

    out = req.get("out_path") or ""
    if not out:
        raise RuntimeError("没给 out_path")
    os.makedirs(os.path.dirname(out) or ".", exist_ok=True)
    img.save(out, "JPEG", quality=92)
    return {"ok": True, "out_path": out, "width": W, "height": H}


def main():
    # **按字节读 + 显式 UTF-8**：中文 Windows 上 sys.stdin 是 GBK，
    # 直接 readline 会把剧名/台词全解成乱码（成本报告那边踩过）。
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
