#!/usr/bin/env python3
"""去重的最后一遍：Lab a*/b* 对抗噪声 + DCT 低频系数扰动（规范 24/25）。

为什么不在 ffmpeg 滤镜链里做：
- ffmpeg 没有 8x8 DCT 系数级的滤镜；
- Lab 的 a*/b* 通道也要逐帧改，`eq`/`hue` 只能整体调，改不了这两个轴。

做法：
  ffmpeg 解码 -> rawvideo(bgr24) -> 本脚本逐帧处理 -> ffmpeg 重编码
音频从原文件直接 `-c:a copy`，不重新编码，避免二次音质损失。
PTS 微扰在这一遍重新施加（rawvideo 解码后原来的抖动已经没了）。

输入输出路径都交给 ffmpeg，Python 自己不按路径读视频 —— Windows 上
OpenCV 的 VideoCapture 对中文路径不稳，绕开它。
"""

import argparse
import json
import math
import os
import subprocess
import sys
import tempfile

import cv2
import numpy as np

# Windows 上 Python 再拉 ffmpeg 时别弹黑框（Rust 那边只能管住 Python 自己）
NO_WINDOW = 0x08000000 if os.name == "nt" else 0

# OpenCV 和 x264 都会抢核；这里只留一路，编码器那边自己开线程
cv2.setNumThreads(1)


def parse_rate(text: str) -> float:
    """把 ffprobe 的 `30000/1001` 变成 29.97；坏值退回 30。"""
    try:
        if "/" in text:
            num, den = text.split("/", 1)
            value = float(num) / float(den) if float(den) else 0.0
        else:
            value = float(text)
    except (TypeError, ValueError, ZeroDivisionError):
        return 30.0
    if not math.isfinite(value) or value <= 0.0 or value > 240.0:
        return 30.0
    return value


def probe(ffprobe: str, path: str, wanted_fps: float):
    """拿宽高和帧率。只读视频流，不碰音频。

    **帧率优先用调用方传进来的标称值。** 成片带了 PTS 微扰之后，
    ffprobe 的 `r_frame_rate` 会变成 240（所有帧间隔的最小公倍数），
    `avg_frame_rate` 也被抖动拉低；照它们编码会补帧或拖长。
    主链路用的是哪个 fps，这里就还用哪个。
    """
    cmd = [
        ffprobe, "-v", "error", "-select_streams", "v:0",
        "-show_entries", "stream=width,height,r_frame_rate,avg_frame_rate",
        "-of", "json", path,
    ]
    result = subprocess.run(cmd, capture_output=True, text=True, creationflags=NO_WINDOW)
    if result.returncode != 0:
        raise RuntimeError(f"ffprobe 读不了输入：{result.stderr.strip()[-1200:]}")
    try:
        stream = json.loads(result.stdout)["streams"][0]
        width = int(stream["width"])
        height = int(stream["height"])
    except (KeyError, IndexError, TypeError, ValueError) as exc:
        raise RuntimeError(f"ffprobe 没给宽高：{result.stdout[:500]}") from exc
    if width <= 0 or height <= 0:
        raise RuntimeError(f"输入分辨率不合法：{width}x{height}")
    if wanted_fps > 0.0:
        fps = wanted_fps
    else:
        fps = parse_rate(stream.get("avg_frame_rate", "0/0"))
        if fps <= 0.0 or fps > 240.0:
            fps = parse_rate(stream.get("r_frame_rate", "30/1"))
    return width, height, fps


def dct_matrix(n: int = 8) -> np.ndarray:
    """正交 DCT-II 矩阵：D @ X @ D.T 是正变换，D.T @ C @ D 是反变换。"""
    k = np.arange(n, dtype=np.float64)
    matrix = math.sqrt(2.0 / n) * np.cos(
        math.pi * (2.0 * k[None, :] + 1.0) * k[:, None] / (2.0 * n)
    )
    matrix[0, :] = math.sqrt(1.0 / n)
    return matrix.astype(np.float32)


DCT = dct_matrix()
DCT_T = DCT.T.copy()
# 低频位置：直流不动（动了整块亮度会跳），只碰它旁边这几项
LOW_FREQ = ((0, 1), (1, 0), (1, 1), (0, 2), (2, 0))


def add_lab_noise(frame: np.ndarray, sigma: float, rng: np.random.Generator) -> np.ndarray:
    """在 Lab 的 a*/b* 上各加一层低强度高斯噪声，L 不动。

    OpenCV 的 8 位 Lab 把 a*/b* 存成 0~255（128 是中点），
    sigma 1~3 大约相当于原色度轴的 1%~2%，属于肉眼很难指认、
    但逐像素比对会变的量级。
    """
    lab = cv2.cvtColor(frame, cv2.COLOR_BGR2LAB).astype(np.float32)
    h, w = lab.shape[:2]
    noise = rng.normal(0.0, sigma, (h, w, 2)).astype(np.float32)
    lab[..., 1:] += noise
    np.clip(lab, 0.0, 255.0, out=lab)
    return cv2.cvtColor(lab.astype(np.uint8), cv2.COLOR_LAB2BGR)


def add_dct_noise(frame: np.ndarray, amp: float, rng: np.random.Generator) -> np.ndarray:
    """在 Y 通道的 8x8 DCT 低频系数上加微小扰动。

    只改低频的 5 个位置，幅度几个灰度级：
    - 高频不动，避免块边缘出现振铃；
    - 直流不动，避免整块忽明忽暗；
    - 每个 8x8 块独立取符号，块与块之间不会形成规则纹理。
    """
    ycrcb = cv2.cvtColor(frame, cv2.COLOR_BGR2YCrCb)
    y = ycrcb[..., 0].astype(np.float32)
    bh, bw = y.shape[0] // 8, y.shape[1] // 8
    if bh == 0 or bw == 0:
        return frame
    h8, w8 = bh * 8, bw * 8
    blocks = y[:h8, :w8].reshape(bh, 8, bw, 8).transpose(0, 2, 1, 3)
    coeff = DCT @ blocks @ DCT_T
    for i, j in LOW_FREQ:
        coeff[..., i, j] += rng.normal(0.0, amp, (bh, bw)).astype(np.float32)
    rebuilt = DCT_T @ coeff @ DCT
    y[:h8, :w8] = np.clip(
        rebuilt.transpose(0, 2, 1, 3).reshape(h8, w8), 0.0, 255.0
    ).astype(np.uint8)
    ycrcb[..., 0] = y.astype(np.uint8)
    return cv2.cvtColor(ycrcb, cv2.COLOR_YCrCb2BGR)


def read_frame(stream, size: int):
    """从管道里读满一帧；EOF 返回 None。read(n) 不保证一次读满。"""
    chunks = []
    remaining = size
    while remaining > 0:
        chunk = stream.read(remaining)
        if not chunk:
            return None
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def encoder_args(args, width: int, height: int, fps: float) -> list:
    cmd = [
        args.ffmpeg, "-hide_banner", "-loglevel", "error", "-nostats", "-y",
        "-f", "rawvideo", "-pix_fmt", "bgr24",
        "-s", f"{width}x{height}", "-r", f"{fps:.6f}", "-i", "pipe:0",
        "-i", args.input,
        "-map", "0:v:0", "-map", "1:a:0?",
        # medium 比主链路那遍 veryfast 慢，但这是第二遍编码，
        # 用快档会把上一遍已经压过的细节再糊一次
        "-c:v", "libx264", "-preset", "medium", "-pix_fmt", "yuv420p",
    ]
    if args.bitrate > 0:
        kbps = max(1, args.bitrate // 1000)
        cmd += [
            "-b:v", f"{kbps}k",
            "-minrate", f"{kbps}k",
            "-maxrate", f"{kbps * 4 // 3}k",
            "-bufsize", f"{kbps * 2}k",
        ]
    else:
        cmd += ["-crf", str(args.crf if args.crf > 0 else 20)]
    if args.gop > 0:
        cmd += ["-g", str(args.gop)]
    if args.bframes >= 0:
        cmd += ["-bf", str(args.bframes)]
    if args.scenecut > 0:
        cmd += ["-x264-params", f"scenecut={args.scenecut}"]
    if args.pts_ms > 0.0 and args.pts_every > 0:
        every = max(2, args.pts_every)
        phase = args.pts_phase % every
        cmd += [
            "-vf",
            f"settb=1/90000,setpts=PTS+{args.pts_ms / 1000.0:.6f}/TB"
            f"*eq(mod(N\\,{every})\\,{phase})",
            "-enc_time_base", "1/90000",
        ]
    cmd += [
        "-c:a", "copy",
        "-movflags", "+faststart",
        "-map_metadata", "-1", "-map_chapters", "-1",
        "-metadata", f"creation_time={args.creation_time}",
        # **不要 -shortest。**
        # 输入音频是 AAC 帧对齐的，往往比视频最后一条 PTS 短十几毫秒；
        # 加了 -shortest 会把最后 1~3 帧视频切掉（实测 90 帧变 87 帧）。
        # 这里视频帧必须一帧不少，音频早结束十几毫秒听不出来。
        args.output,
    ]
    return cmd


def run(args) -> int:
    width, height, fps = probe(args.ffprobe, args.input, args.fps)
    frame_size = width * height * 3

    dec_err = tempfile.TemporaryFile()
    enc_err = tempfile.TemporaryFile()
    decoder = subprocess.Popen(
        [
            args.ffmpeg, "-hide_banner", "-loglevel", "error", "-nostdin",
            "-i", args.input, "-map", "0:v:0",
            # passthrough：不按 CFR 补帧/丢帧，输入几条就吐几条。
            # 默认的 vsync 会把 89 条包补成 90 帧，逐轮叠加会越跑越多。
            "-fps_mode", "passthrough",
            "-f", "rawvideo", "-pix_fmt", "bgr24", "pipe:1",
        ],
        stdout=subprocess.PIPE,
        stderr=dec_err,
        creationflags=NO_WINDOW,
    )
    encoder = subprocess.Popen(
        encoder_args(args, width, height, fps),
        stdin=subprocess.PIPE,
        stdout=subprocess.DEVNULL,
        stderr=enc_err,
        creationflags=NO_WINDOW,
    )

    rng = np.random.default_rng()
    frames = 0
    write_failed = None
    try:
        assert decoder.stdout is not None and encoder.stdin is not None
        while True:
            raw = read_frame(decoder.stdout, frame_size)
            if raw is None:
                break
            frame = np.frombuffer(raw, dtype=np.uint8).reshape((height, width, 3))
            if args.lab > 0.0:
                frame = add_lab_noise(frame, args.lab, rng)
            if args.dct > 0.0:
                frame = add_dct_noise(frame, args.dct, rng)
            try:
                encoder.stdin.write(frame.tobytes())
            except (BrokenPipeError, OSError) as exc:
                write_failed = exc
                break
            frames += 1
    finally:
        if decoder.stdout:
            decoder.stdout.close()
        dec_code = decoder.wait()
        if encoder.stdin:
            try:
                encoder.stdin.close()
            except BrokenPipeError:
                pass
        enc_code = encoder.wait()

    if write_failed is not None:
        enc_err.seek(0)
        detail = enc_err.read().decode("utf-8", "replace").strip()[-2000:]
        raise RuntimeError(f"编码器提前退出：{write_failed}；{detail}")
    if dec_code != 0:
        dec_err.seek(0)
        detail = dec_err.read().decode("utf-8", "replace").strip()[-2000:]
        raise RuntimeError(f"解码失败（退出码 {dec_code}）：{detail}")
    if enc_code != 0:
        enc_err.seek(0)
        detail = enc_err.read().decode("utf-8", "replace").strip()[-2000:]
        raise RuntimeError(f"重编码失败（退出码 {enc_code}）：{detail}")
    if frames == 0:
        raise RuntimeError("一帧都没解出来")

    print(
        f"advanced_noise: {frames} 帧，{width}x{height} @ {fps:.3f}fps，"
        f"Lab σ={args.lab:.2f}，DCT amp={args.dct:.2f}",
        file=sys.stderr,
    )
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--ffmpeg", required=True)
    parser.add_argument("--ffprobe", required=True)
    parser.add_argument("--fps", type=float, default=0.0)
    parser.add_argument("--lab", type=float, default=0.0)
    parser.add_argument("--dct", type=float, default=0.0)
    parser.add_argument("--bitrate", type=int, default=0)
    parser.add_argument("--crf", type=int, default=20)
    parser.add_argument("--gop", type=int, default=0)
    parser.add_argument("--bframes", type=int, default=-1)
    parser.add_argument("--scenecut", type=int, default=0)
    parser.add_argument("--pts-ms", type=float, default=0.0)
    parser.add_argument("--pts-every", type=int, default=0)
    parser.add_argument("--pts-phase", type=int, default=0)
    parser.add_argument("--creation-time", default="2023-01-01T00:00:00Z")
    args = parser.parse_args()

    try:
        return run(args)
    except Exception as exc:  # noqa: BLE001 - 错误要原样带回给 Rust
        print(f"advanced_noise: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
