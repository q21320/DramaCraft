"""看原封面上有什么字 —— 决定合成封面时要不要盖住它。"""

import json
import os
import subprocess
import sys

RT = r"E:\project\DramaDist\src-tauri\target\release\resources"
PYTHON = os.path.join(RT, "asr_runtime", "python.exe")
OCR_WORKER = os.path.join(RT, "ocr_worker.py")

poster = sys.argv[1] if len(sys.argv) > 1 else r"D:\BaiduNetdiskDownload\弟弟结婚，我的新车竟成了贺礼\poster.jpg"

req = json.dumps({"frames": [{"path": poster, "time_ms": 0}], "lang": "ch"}, ensure_ascii=False)
r = subprocess.run(
    [PYTHON, OCR_WORKER], input=(req + "\n").encode("utf-8"),
    capture_output=True, timeout=300,
)
out = r.stdout.decode("utf-8", "replace")
parsed = None
for ln in out.splitlines():
    ln = ln.strip()
    if ln.startswith("{"):
        try:
            parsed = json.loads(ln)
        except Exception:
            pass

print(f"封面: {poster}")
if not parsed:
    print("OCR 无输出")
    print(out[:400])
    print(r.stderr.decode("utf-8", "replace")[:400])
    sys.exit(1)

if not parsed.get("ok"):
    print("OCR 失败:", parsed.get("error"))
    sys.exit(1)

print("\n=== 识别到的文字 ===")
for fr in parsed.get("frames", []):
    txt = (fr.get("text") or "").strip()
    print(f"  整块: «{txt}»")
    for ln in fr.get("lines") or []:
        print(f"    - «{ln.get('text')}»  位置 {ln.get('box') or ln.get('bbox') or '?'}")
