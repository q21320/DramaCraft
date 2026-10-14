DramaDist 内置的 FFmpeg
=======================

这两个可执行文件是 FFmpeg，随程序一起分发，所以你不必自己装环境。


是什么
------

  ffmpeg.exe    ffmpeg version 7.1.3-Jellyfin
  ffprobe.exe   ffprobe version 7.1.3-Jellyfin

  上游项目：https://ffmpeg.org/
  此构建来自 Jellyfin 项目的构建流水线（Jellyfin 是开源的媒体服务器，
  它公开发布这套 Windows 构建供任何人使用）。

  SHA256（用于核对是否为同一份文件）
    ffmpeg.exe   839138090AFCAC63735F3062D19FB72CFB442810AAD125B374E808140F9063F7
    ffprobe.exe  87191811BE1BF0AD488B6B188AA7DAB2B4D113FE37DF4A229EDDB0D09DB18D2E

  我们是原样分发的，没有修改过任何字节。


许可
----

FFmpeg 是自由软件。这个构建启用了 --enable-gpl 与 --enable-version3，
因此它按 **GNU 通用公共许可证第 3 版（GPLv3）** 授权。
许可证全文见同目录的 COPYING.GPLv3.txt。

这个构建的完整编译配置：

  --cc=clang --pkg-config-flags=--static --disable-ffplay --disable-debug
  --disable-doc --disable-sdl2 --enable-lto=thin --enable-gpl --enable-version3
  --enable-schannel --enable-iconv --enable-libxml2 --enable-zlib --enable-lzma
  --enable-gmp --enable-chromaprint --enable-libfreetype --enable-libfribidi
  --enable-libfontconfig --enable-libharfbuzz --enable-libass --enable-libbluray
  --enable-libmp3lame --enable-libopus --enable-libtheora --enable-libvorbis
  --enable-libopenmpt --enable-libwebp --enable-libvpx --enable-libzimg
  --enable-libx264 --enable-libx265 --enable-libsvtav1 --enable-libdav1d
  --enable-libfdk-aac --enable-opencl --enable-dxva2 --enable-d3d11va
  --enable-d3d12va --enable-amf --enable-libvpl --enable-ffnvcodec --enable-cuda
  --enable-cuda-llvm --enable-cuvid --enable-nvdec --enable-nvenc

  对应的源码可以从 https://ffmpeg.org/download.html 获取。
  Jellyfin 的构建脚本：https://github.com/jellyfin/jellyfin-ffmpeg


GPL 对你意味着什么
------------------

**自己用：没有任何影响。** GPL 的义务只在「分发」时触发。
你在自己电脑上跑这个软件，不需要做任何事。

**要把这个软件发给别人（含卖给别人）：GPL 的义务就生效了。**
GPLv3 要求，你分发包含 GPL 组件的软件时，必须：

  - 随软件提供完整的对应源代码；
  - 保留版权声明与许可证；
  - 不得对接收方施加比 GPL 更严格的限制。

  换句话说：**打包了 GPL 版 FFmpeg 的软件，整体要按 GPL 分发。**

  如果这不适合你，有两条路：
    1. 换成 LGPL 构建的 FFmpeg（但那种构建不含 libx264，
       H.264 编码要改用硬件编码器或 libopenh264，功能与体积都会变）；
    2. 不内置，让使用者自己装 FFmpeg（本项目的「设置」页支持手动指定路径）。

  这不是法律意见。真要对外分发，请咨询律师。


不用它行不行
------------

行。「设置」页里把 ffmpeg / ffprobe 的路径改成你自己的那份即可，
填写后会以你指定的为准，内置的只是「没配任何东西时的兜底」。


体积
----

两个文件合计约 158 MB。安装包用 lzma 压缩后大约 60–70 MB。
体积换的是「装上就能用」—— 我们认为这个交换是划算的。
