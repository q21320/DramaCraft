# GPU 推理环境（LaMa 修复 / OCR 用）

这份文档记录**怎么把这台机器上的 ONNX GPU 跑通** —— 中间踩了不少坑，
下次重装照着做就行。

## 结论先说

| | CPU | GPU |
|---|---|---|
| LaMa 单次推理（512×512） | 2392 ms | **142 ms** |
| 一集 82 秒（2460 帧）估算 | **98 分钟** | **5.8 分钟** |

**提速 16.8 倍。** CPU 上做逐帧修复不现实，GPU 上可以接受。

## 装机清单（全部装进 venv，在 E 盘，约 2.4 GB）

```powershell
# 1. 卸掉 CPU 版（它和 GPU 版冲突，同名包）
E:\project\DramaDist\.venv\Scripts\python.exe -m pip uninstall -y onnxruntime

# 2. 装 GPU 版 —— **必须用 1.20.2，不能用最新的 1.31**
E:\project\DramaDist\.venv\Scripts\python.exe -m pip install onnxruntime-gpu==1.20.2

# 3. CUDA 12 运行库（注意全是 cu12，不是 cu13）
E:\project\DramaDist\.venv\Scripts\python.exe -m pip install `
  nvidia-cuda-runtime-cu12 nvidia-cublas-cu12 nvidia-cudnn-cu12 nvidia-cufft-cu12
```

### 为什么是 1.20.2 而不是最新的 1.31

`onnxruntime-gpu 1.31` 要求 **CUDA 13**，而 PyPI 上的 `nvidia-cublas-cu13`
**只有 0.0.1 这个占位版本**（装不了）—— 路是死的。
`1.20.2` 要 CUDA 12，而 CUDA 12 的包齐全。

### 为什么必须装 nvidia-cufft-cu12

漏了它的话，加载 `onnxruntime_providers_cuda.dll` 会报
`LoadLibrary failed with error 126`（**模块找不到**），
而错误信息**不会告诉你缺的是哪个**。

排查办法：**读那个 DLL 的 PE 导入表**，逐个核对依赖在不在。
当时的结果是 18 个依赖里**只缺 `cufft64_11.dll`** 一个：

    ✓ cublasLt64_12.dll  ✓ cublas64_12.dll  ✓ cudart64_12.dll
    ✓ cudnn64_9.dll      ✓ MSVCP140.dll     ✓ VCRUNTIME140.dll
    ✗ cufft64_11.dll     ← 就它

**能读导入表就别猜。**

## 代码里怎么用

CUDA 的 DLL 在 `site-packages\nvidia\*\bin`，**不在 PATH 上**。
Python 3.8+ 加载 DLL 时**忽略 PATH**，必须显式加搜索目录：

```python
import glob, os, site

HANDLES = []          # ← **必须留住引用**，否则句柄被 GC，目录就失效
for d in sorted(glob.glob(os.path.join(site.getsitepackages()[0], "nvidia", "*", "bin"))):
    HANDLES.append(os.add_dll_directory(d))

import onnxruntime as ort
sess = ort.InferenceSession(model_path,
                            providers=["CUDAExecutionProvider", "CPUExecutionProvider"])
```

**provider 顺序有讲究**：CUDA 放前面才会用它；只写 CUDA 的话，
某台机器没有 N 卡会直接报错，所以后面必须跟 CPU 兜底。

## 一个反直觉的实测：**OCR 用 GPU 反而更慢**

同样的 12 帧，RapidOCR：

    12 帧：CPU 8.6s  →  CUDA 9.5s     慢 10%
    42 帧：CPU 36.6s →  CUDA 44.1s    慢 20%

原因：装的是 **mobile 小模型**（det 2.3MB + rec 10.2MB），单帧计算量本来就小，
而每帧 1080×1920 的**上传 + 小算子调度开销盖过了算力收益**。

**GPU 的甜区是大模型 + 大批量，不是小模型 + 十几帧。**
所以 RapidOCR 继续走 CPU（它的 `config.yaml` 里 `use_cuda: false`，是默认值）。

LaMa 就不同了：512×512 的修复网络计算量大，GPU 快 16.8 倍 ✓

## 模型文件

放在 `models/`（已 gitignore）：

    models/lama_fp32.onnx   198 MB
    下载：https://huggingface.co/Carve/LaMa-ONNX/resolve/main/lama_fp32.onnx

输入 `image[1,3,512,512]` + `mask[1,1,512,512]`，输出 `[1,3,512,512]`，
都是 float32、范围 0~1。
