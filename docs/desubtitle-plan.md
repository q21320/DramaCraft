# AI 去字幕：双路方案（已定）

## 结论

```
配了云端服务的 Key  →  走云端（效果好）
没配 / 调用失败      →  退回本地 delogo（有横带，但字幕没了）
```

**原则：没有云端服务不能变成「功能不可用」，只能是「效果差一点」。**

## 为什么这么定

用户问过一句很关键的：「如果使用的人没有 gpu 是不是会出问题？」

盘过一遍，本地这条路的短板是**机器依赖**：

| | 云端 | 本地 |
|---|---|---|
| 效果 | 专门做这个的服务，针对性强 | `delogo` 用周围像素插值，**留一道发白横带** |
| 机器要求 | 零 | 好效果要 N 卡 |
| 配置成本 | 填个 Key | 装 CUDA 运行时（这次在版本和 DLL 依赖上耗了几小时）|
| 边际成本 | 按次收费 | 0 |
| 数据 | 素材要上传 | 不出本机 |

**而且有个硬旁证**：真实软件走的就是线上（日志里 `已提交异步任务 task_id=t_...`
—— 开拍的 `videoscreenclear`）。**那个作者是实际用这套流程吃饭的人，他选线上。**

## 本地这条路留着干什么

1. **兜底** —— 云端挂了/额度用尽，至少还能出片
2. **数据不出本机** —— 素材是版权内容，有人不愿意传第三方
3. **长期免费** —— 每次 4 集，跑很多次的话费用会累

## 已经做完的（本地这一半）

- OCR 定位字幕带（RapidOCR，CPU，12 帧约 5~10 秒）
- `delogo` 去除，编码走 `pick_h264_encoder`（有 N 卡用 NVENC，没有自动退回 libx264）
- 只做前 4 集（照真实软件），切片跟着去字幕的子集走

## 云端这一半：需要接口说明

日志里能看到完整形态：

    AI去字幕：选中 4 集 → ...\quzimu（任务 videoscreenclear · 并发 4）
    已提交异步任务 task_id=t_mt1a3i5n7bc54928d3-215a-4191-84bf-39e4a98c3c6f（第1集.mp4）
    （轮询…）
    已保存：...\quzimu\第1集_quzimu.mp4（24436953 bytes）
    AI去字幕结束：成功 4，失败 0

是**异步任务**模式：提交 → 拿 task_id → 轮询 → 下载。

需要三样东西：

1. **提交任务的地址**（和它的请求体格式）
2. **查询任务的地址**（和它返回的状态字段）
3. **鉴权方式** —— AccessKey + SecretKey？签名怎么算？还是 Bearer Token？

有这三样，照 `src/cloud/mod.rs` 那套模式加一个 adapter 就行
（那套已经有：配置存取、Key 打码、HTTP 状态码翻译、结果解析）。

## 实现要点

`desubtitle_media` 里加一层路由：

```rust
// 伪代码
match cloud_desubtitle::submit(&cfg, &input, &out).await {
    Ok(_) => return Ok(report.with_path("cloud")),
    Err(e) => {
        // **失败要记日志、要继续**，不能因为云端挂了就什么都不出
        log_warn(format!("云端去字幕失败（{e}），改用本地 delogo"));
    }
}
// 落到本地
local_delogo(...)
```

**结果里要写清楚这次走的哪条路** —— 用户看一眼就知道效果差是为什么：

    去字幕完成（…）｜字幕带 760×107，编码 h264_nvenc，本地 delogo
    去字幕完成（…）｜云端 videoscreenclear

## 已清掉的（走双路之后用不到）

    onnxruntime-gpu 1.20.2     ← 装回 CPU 版 onnxruntime 1.31.0
    nvidia-cuda-runtime-cu12
    nvidia-cublas-cu12
    nvidia-cudnn-cu12
    nvidia-cufft-cu12
    nvidia-cuda-nvrtc-cu12
    nvidia-nvjitlink-cu12
    coloredlogs / humanfriendly / pyreadline3 / sympy / mpmath   ← GPU 版的连带依赖
    models/lama_fp32.onnx      （198 MB）

**共约 2.2 GB。** venv 从 29 个包瘦到 17 个。

**注意**：删 `onnxruntime-gpu` **必须同时装回 CPU 版 `onnxruntime`** ——
它们是同名包，不装回去 OCR 直接坏。

要恢复 GPU 环境的话，`docs/gpu-setup.md` 里有完整清单和踩坑记录
（关键是锁 `1.20.2`、以及别漏 `nvidia-cufft-cu12`）。
