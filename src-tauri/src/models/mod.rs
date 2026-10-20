//! 前后端共享的数据结构。
//!
//! 约定：所有结构体一律 `#[serde(rename_all = "camelCase")]`，
//! 与前端 `src/types/index.ts` 一一对应。

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: i64,
    pub name: String,
    pub root_dir: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Media {
    pub id: i64,
    pub project_id: i64,
    pub path: String,
    pub name: String,
    pub duration_ms: i64,
    pub width: i64,
    pub height: i64,
    pub has_audio: bool,
    pub size_bytes: i64,
    pub format_name: String,
    pub created_at: String,
    /// 去重后的文件；空串表示还没去重过。
    /// 切片优先读它 —— 真实软件的流程就是「先去重再切片」。
    #[serde(default)]
    pub processed_path: String,
}

/// ffprobe 探测结果（尚未落库）
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct MediaProbe {
    pub duration_ms: i64,
    pub width: i64,
    pub height: i64,
    pub has_audio: bool,
    pub size_bytes: i64,
    pub format_name: String,
}

impl MediaProbe {
    /// 和库里的记录比一比：**文件被重下过时宽高/时长都可能变**。
    ///
    /// 比范围包括文件大小 —— 大小都没变就不用去动库。
    pub fn matches(&self, m: &Media) -> bool {
        self.duration_ms == m.duration_ms
            && self.width == m.width
            && self.height == m.height
            && self.has_audio == m.has_audio
            && self.size_bytes == m.size_bytes
            && self.format_name == m.format_name
    }
}

/// 一片的切点
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlicePlan {
    pub index: i64,
    pub start_ms: i64,
    pub end_ms: i64,
}

impl SlicePlan {
    pub fn duration_ms(&self) -> i64 {
        self.end_ms - self.start_ms
    }
}

/// 切片方式。前端发送 `{ mode: "everySeconds", seconds: 60 }` 这类形状。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum SliceMode {
    /// 每 N 秒一片，尾部不足一片并入上一片
    EverySeconds { seconds: f64 },
    /// 总时长等分 N 份
    EqualParts { parts: i64 },
    /// 手动切点（毫秒），自动补齐首尾
    Manual { points: Vec<i64> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Segment {
    pub id: i64,
    pub media_id: i64,
    pub index_no: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub out_path: String,
    pub size_bytes: i64,
    pub status: String,
    pub message: String,
    /// 画面处理前的原始文件路径；为空表示还没处理过。用于「还原」。
    #[serde(default)]
    pub original_path: String,
}

// ---------------- 画面处理（差异化） ----------------

/// 画面处理选项。每个开关独立，全部处于默认值时表示不做任何处理。
///
/// 越界数值会在 [`crate::transform::normalize`] 里被夹到安全范围，
/// 不会把用户输入原样拼进命令行。
/// 片头标识的位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LabelPosition {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
    TopCenter,
    BottomCenter,
}

/// 片头标识：**合规功能，不是去重手段**。
///
/// 《人工智能生成合成内容标识办法》要求 AI 生成 / 合成的内容显著标识。
/// 画面经过 AI 处理（去字幕、AI 生成封面或文案等）后发布，应当标识。
///
/// 这和「改画面躲审核」方向正相反 —— 它是**主动披露**。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HeaderLabel {
    /// 显示的文字，默认「AI创作」
    pub text: String,
    /// 片头显示多少秒
    pub seconds: f64,
    pub position: LabelPosition,
    /// 字号 = 画面高度 × 这个比例
    pub size_ratio: f64,
}

impl Default for HeaderLabel {
    fn default() -> Self {
        Self {
            text: "AI创作".to_string(),
            seconds: 3.0,
            position: LabelPosition::TopLeft,
            size_ratio: 0.034,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformOptions {
    /// 水平镜像
    pub mirror: bool,
    /// 四边各裁掉的比例（0.00 ~ 0.10），裁完再放大回原尺寸
    pub crop_ratio: f64,
    /// 整体缩放（0.96 ~ 1.04）
    pub scale_ratio: f64,
    /// 亮度增量（-0.05 ~ 0.05）
    pub brightness: f64,
    /// 对比度（0.95 ~ 1.05）
    pub contrast: f64,
    /// 饱和度（0.95 ~ 1.05）
    pub saturation: f64,
    /// 噪点强度（0 ~ 10）
    pub noise: u32,
    /// 播放速度（0.90 ~ 1.10）
    pub speed: f64,
    /// 音量（0.80 ~ 1.20）
    pub volume: f64,
    /// 片头标识；`None` 表示不加
    #[serde(default)]
    pub header_label: Option<HeaderLabel>,

    // ---------------- 下面这些照着真实软件那 13 项去重手段补的 ----------------
    //
    // 它的日志把每一项的强度都写出来了：
    //   帧混合 tmix=124，裁切=116，变速=117，帧率项=122，剧名角标=120，
    //   几何位移=124，亮度/对比/饱和=118，色相=122，噪点=121，锐化=119，
    //   微水印=122，去头尾=121，音频指纹=115
    //
    // 每一项都加上，用户才有得选；强度随机是为了让每一片都不一样。

    /// 色相偏移（度，约 -10~10）
    #[serde(default)]
    pub hue: f64,
    /// 锐化强度（0~1.5，0 = 关）
    #[serde(default)]
    pub sharpen: f64,
    /// 帧混合帧数（0 = 关，2~3 有效）。**画质代价明显**，但确实是最强的去重手段之一
    #[serde(default)]
    pub tmix: u32,
    /// 输出最大高度（0 = 不缩放）。真实软件统一出 720p —— 省一半时间与空间
    #[serde(default)]
    pub max_height: i64,
    /// 统一帧率（0 = 不变）。统一到 30fps 本身也是一项去重
    #[serde(default)]
    pub fps: f64,
    /// 几何位移（像素）。正数往右/下移。
    ///
    /// 真实软件注明是「几何位移(原微旋转)」—— 它原来用微旋转，后来换成位移。
    /// 旋转会在四角留黑边，位移不会。这里也用位移。
    #[serde(default)]
    pub shift_x: i64,
    #[serde(default)]
    pub shift_y: i64,
    /// 剧名角标（一直显示在角落），和「片头标识」是两回事
    #[serde(default)]
    pub corner_label: Option<CornerLabel>,

    // ---- 编码参数 ----
    /// 关键帧间隔（0 = 用编码器默认）。真实软件固定 60
    #[serde(default)]
    pub gop: i64,
    /// B 帧数（-1 = 用编码器默认）。真实软件在 0/1/2 之间随机
    #[serde(default = "default_bframes")]
    pub bframes: i64,
    /// 目标码率（bps，0 = 走 CRF）。真实软件要求成片 ≥6Mbps
    #[serde(default)]
    pub bitrate_bps: i64,
    /// CRF 质量档（0 = 用默认 20，19~23 常用）。
    /// 只在**没有目标码率**时生效 —— 给了码率就走受限 VBR，CRF 会被编码器忽略
    #[serde(default)]
    pub crf: i64,

    // ---------------- 音频 / 时序 / 纹理的实质性处理 ----------------
    //
    // 这几项不是「调个参数」那么轻：单纯改音量对音频指纹几乎无效，
    // 抽帧和扫光动的才是比对真正看的东西。

    /// 音频变调比例（1.0 = 不变，0.99~1.01）。不改时长，只挪频谱
    #[serde(default = "default_pitch")]
    pub pitch: f64,
    /// 抽帧间隔：每 N 帧丢掉 1 帧（0 = 不抽）。丢掉的帧由 fps 重采样补一帧，
    /// 时间轴不变，但逐帧比对会对不上
    #[serde(default)]
    pub drop_every: u32,
    /// 抽帧相位（0 ~ drop_every-1），让每集抽在不同位置
    #[serde(default)]
    pub drop_phase: u32,
    /// 动态扫光透明度（0 = 关，0.02~0.06）。极淡的亮带横向扫过，
    /// 改变像素级纹理
    #[serde(default)]
    pub sweep: f64,

    /// 微旋转角度（度，-2.0~2.0，0 = 不转）。
    ///
    /// 旋转后会自动放大再裁回，四角不留黑边 —— 放大多少由画面比例算。
    #[serde(default)]
    pub rotate_deg: f64,
    /// 四角之一的小色块透明度（0 = 关，0.01~0.05）。
    /// 极小的半透明色块，肉眼几乎看不见，但那一块的像素值变了
    #[serde(default)]
    pub patch_alpha: f64,
    /// 小色块在哪个角：0 左上、1 右上、2 左下、3 右下
    #[serde(default)]
    pub patch_corner: u8,
    /// 音频整体延迟（毫秒，0~200）：开头垫一段静音
    #[serde(default)]
    pub audio_delay_ms: i64,
    /// 立体声左右声道相位差（毫秒，0~3）。单声道只用第一值，不会出错
    #[serde(default)]
    pub stereo_phase_ms: i64,
    /// 极低电平白噪声振幅（0 = 关，约 0.0004~0.001，≈ -68dBFS）
    #[serde(default)]
    pub white_noise: f64,
    /// 16~20kHz 高频噪声振幅（0 = 关，0.001~0.006，独立于白噪）
    #[serde(default)]
    pub high_noise: f64,
    /// 立体声宽度（1.0 = 不变，0.97~1.03）。只动 Side 声道，不改音调语速
    #[serde(default = "default_stereo_width")]
    pub stereo_width: f64,
    /// PTS 微扰幅度（毫秒，0 = 关，2~5ms）。只落在部分帧上，远小于一帧间隔
    #[serde(default)]
    pub pts_jitter_ms: f64,
    /// 每隔多少帧做一次 PTS 微扰（0 = 关，4~8）
    #[serde(default)]
    pub pts_jitter_every: u32,
    /// 微扰落在间隔里的第几帧（0 ~ every-1），让每集错开
    #[serde(default)]
    pub pts_jitter_phase: u32,
    /// x264 场景切换阈值（0 = 默认 40，20~60 随机）。
    /// 它决定画面切镜头时提不提前插关键帧 —— 关键帧位置因此不是固定节奏
    #[serde(default)]
    pub scenecut: i64,
    /// Lab a*/b* 通道的低强度对抗噪声 σ（0 = 关，2.0~3.0）。
    ///
    /// **不在 ffmpeg 滤镜链里**：需要逐帧 Python pass，只在整集去重时执行
    #[serde(default)]
    pub lab_noise: f64,
    /// Y 通道 8x8 DCT 低频系数扰动幅度（0 = 关，2.5~4.0）。
    /// 同样走逐帧 Python pass
    #[serde(default)]
    pub dct_noise: f64,
    /// 去重轮数（0 = 用默认 2）。
    ///
    /// **只有整集去重会看它**：第 1 轮全量处理，之后每轮**只重摇画面** ——
    /// 变速/变调/音量/音频延迟全部锁在中性值，所以时长不变、音频只压一次。
    /// 单步工具页不传这个字段，永远只跑一轮。
    #[serde(default)]
    pub rounds: i64,
}

/// 变调的中性值是 1.0 —— `f64::default()` 的 0.0 会被当成「降一半调」
fn default_pitch() -> f64 {
    1.0
}

fn default_bframes() -> i64 {
    -1
}

fn default_stereo_width() -> f64 {
    1.0
}

/// 画在角落的剧名角标。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CornerLabel {
    pub text: String,
    #[serde(default = "default_corner_position")]
    pub position: LabelPosition,
    /// 字号占画面高度的比例
    #[serde(default = "default_corner_size")]
    pub size_ratio: f64,
    /// 不透明度 0~1。角标要低调，默认半透明
    #[serde(default = "default_corner_opacity")]
    pub opacity: f64,
}

fn default_corner_position() -> LabelPosition {
    LabelPosition::BottomRight
}
fn default_corner_size() -> f64 {
    0.022
}
fn default_corner_opacity() -> f64 {
    0.55
}

impl Default for TransformOptions {
    /// 中性值：什么都不改。注意不能 derive —— `f64::default()` 是 0.0，
    /// 而对比度/饱和度的中性值是 1.0，用派生默认值会把画面压成全黑。
    fn default() -> Self {
        Self {
            mirror: false,
            crop_ratio: 0.0,
            scale_ratio: 1.0,
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            noise: 0,
            speed: 1.0,
            volume: 1.0,
            header_label: None,
            hue: 0.0,
            sharpen: 0.0,
            tmix: 0,
            max_height: 0,
            fps: 0.0,
            shift_x: 0,
            shift_y: 0,
            corner_label: None,
            gop: 0,
            bframes: -1,
            bitrate_bps: 0,
            crf: 0,
            pitch: 1.0,
            drop_every: 0,
            drop_phase: 0,
            sweep: 0.0,
            rotate_deg: 0.0,
            patch_alpha: 0.0,
            patch_corner: 0,
            audio_delay_ms: 0,
            stereo_phase_ms: 0,
            white_noise: 0.0,
            high_noise: 0.0,
            stereo_width: 1.0,
            pts_jitter_ms: 0.0,
            pts_jitter_every: 0,
            pts_jitter_phase: 0,
            scenecut: 0,
            lab_noise: 0.0,
            dct_noise: 0.0,
            rounds: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransformReport {
    pub total: i64,
    pub processed: i64,
    pub failed: i64,
    pub errors: Vec<String>,
    pub segments: Vec<Segment>,
    /// 成片没达到平台上传规范的地方（≤500MB / ≥720p / ≥4Mbps / ≥30s）。
    /// **只是提示**：短片（比如 10 秒分镜）本来就不满足时长要求。
    #[serde(default)]
    pub spec_problems: Vec<crate::transform::SpecProblem>,
}

// ---------------- 发布 ----------------

/// 一个素材在某个平台、某个账号上的发布设置与状态（本地台账，不上传任何东西）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishPlan {
    pub id: i64,
    pub media_id: i64,
    pub platform: String,
    /// 账号名。同一平台可以有多个账号，各自独立记账、互不覆盖。
    pub account: String,
    pub title: String,
    pub description: String,
    /// 标签，逗号分隔的原始文本
    pub tags: String,
    /// 自定义封面图；留空则导出时从视频抽帧
    pub cover_path: String,
    /// draft | ready | published | failed
    pub status: String,
    pub note: String,
    pub updated_at: String,
}

/// 一个「平台 × 账号」的台账汇总
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishAccount {
    pub platform: String,
    pub platform_label: String,
    pub account: String,
    pub total: i64,
    pub draft: i64,
    pub ready: i64,
    pub published: i64,
    pub failed: i64,
    pub last_updated: String,
}

/// 发布包里的一条片段
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishClipEntry {
    pub index_no: i64,
    pub file: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub duration_ms: i64,
    /// 该片段的字幕全文（便于直接复制去当简介）
    pub subtitle: String,
    pub suggested_title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishMetadata {
    pub platform: String,
    pub platform_label: String,
    /// 发布账号。同一平台多个账号时，发布包会按账号分目录，避免互相覆盖。
    pub account: String,
    pub media_name: String,
    pub title: String,
    pub description: String,
    pub tags: Vec<String>,
    pub cover: String,
    pub width: i64,
    pub height: i64,
    pub total_duration_ms: i64,
    pub exported_at: String,
    pub clips: Vec<PublishClipEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishPackageReport {
    pub out_dir: String,
    pub clip_count: i64,
    pub subtitle_count: i64,
    pub cover_path: String,
    pub metadata_path: String,
    pub readme_path: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformOption {
    pub key: String,
    pub label: String,
    /// 该平台是否已有公开的发布接口（用于界面上的诚实提示）
    pub has_public_api: bool,
    pub note: String,
}

// ---------------- 流水线 ----------------

/// 源目录里扫描到的一集
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceEpisode {
    pub path: String,
    pub file_name: String,
    /// 解析出的集号；认不出为 null
    pub episode: Option<i64>,
    pub size_bytes: i64,
}

/// 源目录里扫描到的一部剧
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceDrama {
    pub name: String,
    pub dir: String,
    pub episode_count: i64,
    pub episodes: Vec<SourceEpisode>,
    /// 简介正文（已读出来）
    pub synopsis: String,
    pub synopsis_path: String,
    pub cover: String,
    /// 扫描时发现的问题，逐条说清
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceScan {
    pub root: String,
    pub total_episodes: i64,
    pub dramas: Vec<SourceDrama>,
    pub warnings: Vec<String>,
}

/// 流水线里可以做的一步。顺序即执行顺序。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineStage {
    pub key: String,
    pub label: String,
    /// 这一步是不是默认勾选
    pub enabled_by_default: bool,
    /// 一句话说明它做什么
    pub detail: String,
}

/// 流水线跑出来的一个切片。用于「看效果」——产出必须能当场看，
/// 否则用户只能自己去资源管理器里翻。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputSlice {
    pub segment_id: i64,
    pub index_no: i64,
    /// 当前文件（画面处理后指向处理版）
    pub path: String,
    pub size_bytes: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub status: String,
    pub message: String,
    /// 这一段里的字幕文本，按时间排好
    pub cues: Vec<String>,
}

/// 一部剧的产出（一集一行，下面挂着它的切片）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputEpisode {
    pub media_id: i64,
    pub name: String,
    pub duration_ms: i64,
    pub width: i64,
    pub height: i64,
    pub slices: Vec<OutputSlice>,
}

/// 起一条流水线之后拿到的结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartOutcome {
    pub run_id: i64,
    pub drama_count: i64,
    pub imported: i64,
    /// 排进队列的任务数
    pub queued: i64,
    /// 读不出来、被跳过的文件及原因
    pub skipped: Vec<String>,
    /// 复用了几个已有工程（不是新建的）
    pub reused_projects: i64,
    /// 复用了多少条已有素材（没有重复导入）
    pub reused_media: i64,
}

impl StartOutcome {
    /// 一集都没能用上吗？
    ///
    /// **必须把「复用的」也算上**：加了「同源目录复用已有素材」之后，
    /// 第二次跑同一批素材时 `imported` 本来就是 0（全都复用了）。
    /// 只看 imported 会把正常情况误判成失败 —— 这个 bug 真发生过。
    pub fn imported_nothing(&self) -> bool {
        self.imported + self.reused_media == 0
    }
}

/// 一部剧在流水线里的进度
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineDrama {
    pub id: i64,
    pub run_id: i64,
    pub name: String,
    pub src_dir: String,
    pub out_dir: String,
    pub episode_count: i64,
    /// 最后完成的步骤 key；空串表示还没开始
    pub stage: String,
    /// pending | running | done | failed | skipped
    pub status: String,
    pub message: String,
    pub updated_at: String,
    /// 本次运行用的是哪个工程（v9 起记录；老记录为 null）
    #[serde(default)]
    pub project_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineRun {
    pub id: i64,
    pub root_dir: String,
    pub out_dir: String,
    /// 勾选的步骤，JSON 数组
    pub stages: String,
    pub status: String,
    pub drama_count: i64,
    pub created_at: String,
    pub updated_at: String,
    pub dramas: Vec<PipelineDrama>,
}

/// 封面生成请求。
///
/// 两条路：配了 AI 服务就走**图生图**（能擦掉原封面上的旧剧名），
/// 没配就**本地合成**（用标题条把旧字盖住 + 写新剧名）。
/// 后者离线可用、不产生 AI 内容，代价是盖住的那块画面损失一点。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverRequest {
    /// 原封面（素材包里的 poster.jpg）
    pub poster: String,
    /// 新剧名
    pub title: String,
    pub out_path: String,
    pub width: i64,
    pub height: i64,
    /// 标题条位置
    pub position: LabelPosition,
    /// 批量生成时按剧名分目录用；单张可以留空
    #[serde(default)]
    pub out_root: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverReport {
    pub out_path: String,
    pub width: i64,
    pub height: i64,
    pub size_bytes: i64,
    /// `ai` 或 `local`
    pub engine: String,
    pub notes: Vec<String>,
}

/// 改写剧名 / 简介的结果。
///
/// 顺序照真实软件来：**先改简介，再按新简介起剧名** ——
/// 反过来的话剧名是根据旧简介起的，两者对不上。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RewriteOutcome {
    pub original_title: String,
    pub new_title: String,
    pub original_synopsis: String,
    pub new_synopsis: String,
    /// 用的哪个服务商（让用户知道花了谁的钱）
    pub provider: String,
    /// 落在了哪些文件上
    pub saved: Vec<String>,
}

// ---------------- AI 服务配置 ----------------

/// 一个 AI 服务商。Key 只存在本地设置里，发给前端时一律打码。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProvider {
    /// `ark` / `dashscope` / `custom`
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub base_url: String,
    /// 语言模型（改写剧名简介、生成文案用）
    pub text_model: String,
    /// 生图模型（封面生成用）
    pub image_model: String,
    /// API Key。**不发给前端**，只发 keyHint。
    #[serde(default)]
    pub api_key: String,
}

/// 给界面看的服务商（Key 已打码）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiProviderView {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub base_url: String,
    pub text_model: String,
    pub image_model: String,
    pub has_key: bool,
    /// 例如 `ark-****9f2c`
    pub key_hint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConfigView {
    pub providers: Vec<AiProviderView>,
    /// 当前启用哪一个；空串表示都没启用
    pub active: String,
    /// 配置上值得提醒的地方
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiTestResult {
    pub ok: bool,
    pub provider: String,
    pub endpoint: String,
    pub elapsed_ms: i64,
    pub message: String,
}

// ---------------- 用户自备素材库 ----------------

/// 素材库里的一条素材。**全部由用户导入**，程序不内置任何第三方素材。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    pub id: i64,
    /// audio | lut
    pub kind: String,
    pub name: String,
    pub path: String,
    pub category: String,
    /// 逗号分隔
    pub tags: String,
    /// 音频时长；LUT 为 0
    pub duration_ms: i64,
    pub size_bytes: i64,
    /// 额外信息（LUT 的尺寸等），JSON 字符串
    pub meta: String,
    pub note: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetKindOption {
    pub key: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetImportReport {
    pub imported: i64,
    /// 被跳过的文件及原因 —— 不静默丢弃
    pub skipped: Vec<String>,
    pub summary: AssetSummary,
}

/// 导入前扫描到的文件
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredAsset {
    pub path: String,
    pub file_name: String,
    pub kind: String,
    pub size_bytes: i64,
    /// 扫描时就发现的问题（例如 .cube 被截断），导入时同样会拦下
    pub problem: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AssetSummary {
    pub audio: i64,
    pub lut: i64,
    pub total: i64,
    pub total_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetApplyReport {
    pub out_path: String,
    pub size_bytes: i64,
    pub duration_ms: i64,
    pub filter: String,
    pub warnings: Vec<String>,
}

/// `.cube` 文件头解析结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CubeInfo {
    pub title: String,
    /// 3D LUT 的边长；1D LUT 时为 0
    pub size: i64,
    /// 1D LUT 的长度；3D 时为 0
    pub size_1d: i64,
    pub domain_min: Vec<f64>,
    pub domain_max: Vec<f64>,
    pub data_lines: i64,
    pub expected_lines: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DocxReport {
    pub out_path: String,
    pub size_bytes: i64,
    /// cost | ip
    pub kind: String,
    /// 文档里的块数量，用于界面确认"不是一份空文档"
    pub block_count: i64,
    pub warnings: Vec<String>,
}

/// 成本明细表的一行
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostDocRequest {
    pub drama_name: String,
    pub producer: String,
    pub date: String,
    pub total: String,
    pub rows: Vec<CostDocRow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostDocRow {
    pub item: String,
    pub quantity: String,
    pub unit_price: String,
    pub amount: String,
}

/// 权属声明
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IpDocRequest {
    pub drama_name: String,
    pub episode_count: i64,
    pub total_duration: String,
    pub owner: String,
    pub date: String,
}

// ---------------- 证明材料 ----------------

/// 模板图上的一个字段
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofField {
    /// 取值键，例如 drama_name / episodes / investment
    pub key: String,
    /// 界面上显示的名字
    pub label: String,
    /// 先擦除的矩形 `[x1, y1, x2, y2]`（模板上的示例文字），可选
    #[serde(default)]
    pub erase_rect: Option<Vec<i64>>,
    /// 擦除用的颜色，默认白色
    #[serde(default = "default_erase_color")]
    pub erase_color: String,
    /// 文字绘制区域 `[x, y, w, h]`
    pub box_rect: Vec<i64>,
    pub font_size: i64,
    pub min_font_size: i64,
    /// `#RRGGBB`
    pub color: String,
    /// left | center | right
    pub align: String,
    /// top | center | bottom
    pub valign: String,
    /// 是否按区域宽度自动换行
    pub wrap: bool,
    /// 行距倍数，1.0 表示不额外加空
    pub line_spacing: f64,
}

fn default_erase_color() -> String {
    "#FFFFFF".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofLayout {
    /// 模板图片路径
    pub template: String,
    /// 模板尺寸 `[w, h]`，用于界面定位参考
    pub image_size: Vec<i64>,
    /// 字体文件路径
    pub font_path: String,
    pub fields: Vec<ProofField>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofFieldPreview {
    pub key: String,
    pub label: String,
    pub lines: Vec<String>,
    pub font_size: i64,
    /// 是否因为放不下而缩小过字号
    pub shrunk: bool,
    /// 区域放不下（缩到最小字号仍然溢出）
    pub overflow: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofPreview {
    pub fields: Vec<ProofFieldPreview>,
    pub filter: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProofRenderReport {
    pub out_path: String,
    pub size_bytes: i64,
    pub field_count: i64,
    pub filter: String,
    pub warnings: Vec<String>,
}

// ---------------- 云端字幕（用户自备 Key） ----------------

/// 给前端看的云端配置。**永远不含完整 API Key** —— Key 只留在后端。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudConfigView {
    pub base_url: String,
    pub model: String,
    pub language: String,
    pub has_key: bool,
    /// 打码后的 Key，例如 `sk-****abcd`
    pub key_hint: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudTranscribeReport {
    pub cue_count: i64,
    pub language: String,
    pub duration_ms: i64,
    pub endpoint: String,
    pub audio_bytes: i64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudTestResult {
    pub ok: bool,
    pub endpoint: String,
    pub detail: String,
}

// ---------------- 完成通知（邮件） ----------------

/// 给前端看的邮件配置。**永远不含完整授权码** —— 只返回打码后的提示。
/// 没有单独的开关：填全了就发，没填全就不发。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailConfigView {
    pub host: String,
    pub port: u16,
    pub username: String,
    /// 实际收件人；空串表示「发给自己」
    pub to: String,
    pub has_password: bool,
    /// 打码后的授权码
    pub password_hint: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailTestResult {
    pub ok: bool,
    pub detail: String,
}

/// 整集去重的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DedupReport {
    pub media_id: i64,
    /// 去重产物。**这才是发布用的东西**，切片是它的下游。
    pub out_path: String,
    pub size_bytes: i64,
    /// 成片实际分辨率（没配 ffprobe 时是 0）。报告要求写出实际值，
    /// 而不是“应该缩到了 720p”
    #[serde(default)]
    pub width: i64,
    #[serde(default)]
    pub height: i64,
    /// 这一集到底改了什么 —— 给日志看的效果差异。
    /// 每片参数按序号生成、故意各不相同，列出来用户才知道这步真干了活。
    #[serde(default)]
    pub changes: String,
    /// 实际跑了几轮画面重摇（规范 26）。
    /// 第 1 轮含变速/变调/音频，第 2 轮起只重摇画面，**时长不变**
    #[serde(default)]
    pub rounds: i64,
    /// 处理后的实际时长（毫秒，探测失败时是 0）。
    /// 多轮处理要能看出时长有没有变，不能只看文件大小
    #[serde(default)]
    pub duration_ms: i64,
    /// 没达到平台规范的地方（只是提示）
    #[serde(default)]
    pub spec_problems: Vec<crate::transform::SpecProblem>,
}

/// 分镜证明的产出。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoryboardReport {
    /// 拼出了几张证明图
    pub sheets: i64,
    /// 一共几个镜头
    pub shots: i64,
    /// 分镜描述是不是由视觉模型反推的
    pub used_ai: bool,
    pub frames_dir: String,
    pub notes: Vec<String>,
}
// ---------------- 运行日志 ----------------

/// 本地去字幕的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesubtitleReport {
    /// 产出文件。没去成就是空串。
    pub out_path: String,
    /// 找到的字幕带；没找到就是 `None`。
    pub band: Option<crate::desubtitle::SubtitleBand>,
    /// 擦掉的所有文字区域：字幕在前，水印/台标在后。
    ///
    /// 用户的原话是「字幕 水印都要擦的」—— 一块字幕、一块右下角水印，
    /// 每块一个 `delogo`。`band` 保留给只关心字幕的旧调用方。
    #[serde(default)]
    pub bands: Vec<crate::desubtitle::SubtitleBand>,
    pub sampled_frames: i64,
    pub engine: String,
    /// 到底去没去
    pub removed: bool,
    /// 给人看的一句话
    pub note: String,
}
/// 成本配置比例情况报告里的一行（一部剧）。
///
/// 照 `zhengming/成本图表.docx` 那份模板的列定的：
/// 序号 / 微短剧名称 / 集数和总时长 / 总投资额（需＜100万）/ 演员总片酬占比（需＜40%）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostReportRow {
    pub name: String,
    pub episodes: i64,
    pub total_minutes: f64,
    /// 总投资额（万元）。表单要求 ＜100 万。
    pub invest_wan: f64,
    /// 演员总片酬占比（百分数）。表单要求 ＜40%。
    pub actor_ratio: f64,
}
/// 一行运行日志。
///
/// 流水线跑起来是几十上百个任务，用户需要能**回头看**
/// 「刚才到底做了什么、卡在哪一步」。任务表看的是状态，日志看的是过程。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunLog {
    pub id: i64,
    /// 属于哪次流水线运行；手工操作产生的日志没有 run
    pub run_id: Option<i64>,
    pub drama_id: Option<i64>,
    /// info | ok | warn | error
    pub level: String,
    pub message: String,
    /// 这一行对应的产出文件（没有就是空串）。
    ///
    /// 用户的想法：「产出的内容在日志里尽量能直接点击播放」——
    /// 对。产出不该是另一个列表，它就该长在产生它的那一行上。
    #[serde(default)]
    pub artifact: String,
    pub created_at: String,
}

// ---------------- 任务队列 ----------------

/// 队列里的一条任务。`payload` 是 JSON，形状由 `kind` 决定。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: i64,
    pub media_id: Option<i64>,
    /// slice | transform | ocr | asr | export | draft | publish | verify
    pub kind: String,
    pub payload: String,
    /// pending | running | done | failed | canceled
    pub status: String,
    pub message: String,
    pub attempts: i64,
    pub created_at: String,
    pub updated_at: String,
    /// 属于哪部剧（流水线任务才有）
    #[serde(default)]
    pub pipeline_drama_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct JobCounts {
    pub pending: i64,
    pub running: i64,
    pub done: i64,
    pub failed: i64,
    pub canceled: i64,
    pub total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueReport {
    pub processed: i64,
    pub done: i64,
    pub failed: i64,
    pub remaining: i64,
    /// 停止原因：跑完了 / 被暂停 / 被要求停止 / 达到本次上限
    pub stopped_because: String,
    pub messages: Vec<String>,
    /// 本次碰到的流水线运行
    ///
    /// 完成通知邮件要按「哪部剧成了、哪部剧没成」写，
    /// 而 `messages` 只是任务级流水（还截在 50 条），认不出剧。
    #[serde(default)]
    pub run_ids: Vec<i64>,
    /// 失败任务的 `(任务类型, 原因)`
    ///
    /// 邮件要写「哪一步没成」，不能只写个数。
    #[serde(default)]
    pub failures: Vec<JobFailure>,
}

/// 一条失败记录：哪个任务、哪一步、为什么。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobFailure {
    pub job_id: i64,
    pub kind: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueState {
    pub running: bool,
    pub paused: bool,
    pub counts: JobCounts,
}

// ---------------- 剧目与集 ----------------

/// 从文件名解析出来的一条素材
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesFile {
    pub path: String,
    pub file_name: String,
    /// 解析出的剧目名（认不出时是去掉扩展名的原名）
    pub series: String,
    /// 解析出的集号；认不出为 null
    pub episode: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesGroup {
    pub series: String,
    pub episodes: Vec<SeriesFile>,
}

/// 一集的边界
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeBoundary {
    pub index_no: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub duration_ms: i64,
    /// high / low：片头模板太短时只给 low
    pub confidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeDetection {
    pub boundaries: Vec<EpisodeBoundary>,
    pub sampled_frames: i64,
    pub template_frames: i64,
    pub total_duration_ms: i64,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResplitReport {
    pub episode_count: i64,
    pub out_dir: String,
    pub files: Vec<String>,
    pub total_duration_ms: i64,
}

// ---------------- OCR 硬字幕识别 ----------------

/// 逐帧 OCR 结果。
///
/// 这个结构同时是**子进程协议形状**，所以用 snake_case —— 与 asr / jy 两个
/// worker 的 JSON 字段风格保持一致；前端拿不到它，不需要 camelCase。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct OcrFrame {
    pub time_ms: i64,
    pub text: String,
    /// 这一帧识别到的文字外接框（像素坐标）。
    ///
    /// 原来只有文字、没有框 —— 于是「字幕在哪」只能靠猜。
    /// 硬字幕修复必须先知道准确位置：框大了会把画面本身糊掉，框小了盖不住字。
    #[serde(default)]
    pub boxes: Vec<OcrBox>,
}

/// 一块被识别出文字的矩形区域。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OcrBox {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrOptions {
    /// 抽帧频率（帧/秒）。硬字幕一般停留 1 秒以上，2 帧/秒足够。
    pub sample_fps: f64,
    /// 只识别画面底部这个比例（1.0 = 全画面）。硬字幕几乎都在底部。
    pub bottom_ratio: f64,
    /// 低于这个置信度的识别结果丢弃
    pub min_confidence: f64,
    /// 相邻帧文本相似度高于它就当作同一句
    pub merge_threshold: f64,
    /// 短于这个时长的字幕丢弃（去掉一闪而过的误识别）
    pub min_duration_ms: i64,
    /// 抽帧上限，防止超长视频把磁盘撑爆
    pub max_frames: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrReport {
    pub frames_sampled: i64,
    pub frames_with_text: i64,
    pub cue_count: i64,
    pub engine: String,
    pub duration_ms: i64,
    pub warnings: Vec<String>,
}

// ---------------- 去重效果验证 ----------------

/// 单帧比对结果
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilaritySample {
    pub index: i64,
    /// 汉明距离（0..=64）
    pub distance: u32,
    pub similarity: f64,
}

/// 两个文件的相似度报告
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimilarityReport {
    pub before: String,
    pub after: String,
    pub samples_compared: i64,
    pub mean_distance: f64,
    pub mean_similarity: f64,
    pub verdict: String,
    pub samples: Vec<SimilaritySample>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyItem {
    pub segment_id: i64,
    pub index_no: i64,
    pub original_path: String,
    pub current_path: String,
    pub report: SimilarityReport,
}

/// 一个素材下全部已处理片段的验证汇总
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyReport {
    pub checked: i64,
    /// 还没做过画面处理、无需验证的片段数
    pub unchanged: i64,
    pub failed: i64,
    pub mean_similarity: f64,
    pub verdict: String,
    pub items: Vec<VerifyItem>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

// ---------------- 字幕 ----------------

/// 一条字幕。`id` 为 0 表示尚未落库；文本可含换行（SRT 的多行）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cue {
    pub id: i64,
    pub media_id: i64,
    pub index_no: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

// ---------------- 外部工具 ----------------

/// 外部工具路径（全部可由用户在设置里覆盖）
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolPaths {
    pub ffmpeg: String,
    pub ffprobe: String,
    pub python: String,
}

/// 工具可用性报告
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolReport {
    pub name: String,
    pub path: String,
    pub available: bool,
    pub version: String,
    pub detail: String,
    /// 是否来自随程序分发的内置目录
    #[serde(default)]
    pub bundled: bool,
}

// ---------------- ASR ----------------

/// 某个可选 Python 运行时的能力状态。
///
/// ASR（faster-whisper）、OCR（rapidocr）、剪映草稿（pyJianYingDraft）各自一份，
/// 形状相同，前端可以用同一套组件渲染。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeStatus {
    pub available: bool,
    pub python: String,
    pub detail: String,
}

/// 发给 Python worker 的请求（一行 JSON）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AsrRequest {
    pub audio: String,
    pub model: String,
    pub language: String,
    /// 模型下载/缓存目录，留空则用 worker 默认
    pub model_dir: String,
    /// auto / cpu / cuda
    pub device: String,
    /// 例如 int8 / float16 / int8_float16
    pub compute_type: String,
}

/// worker 返回的应答
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AsrResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub segments: Vec<AsrSegment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AsrSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// 一次转写的结果概要（返回给前端）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AsrOutcome {
    pub cue_count: i64,
    pub language: String,
    pub model: String,
    pub duration_ms: i64,
    pub log: String,
}

// ---------------- 导出 ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum AspectMode {
    /// 保持原始画幅
    Keep,
    /// 转 9:16 竖屏，上下用模糊背景填充
    VerticalBlur,
    /// 转 9:16 竖屏，等比放大后居中裁切
    VerticalCrop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOptions {
    pub media_id: i64,
    pub out_path: String,
    /// 是否把当前字幕烧进画面
    pub burn_subtitles: bool,
    pub aspect: AspectMode,
    /// 只导出这一段（毫秒）；两端都为 0 表示整片
    pub start_ms: i64,
    pub end_ms: i64,
    pub crf: u32,
    pub preset: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOutcome {
    pub out_path: String,
    pub size_bytes: i64,
    pub duration_ms: i64,
    pub filter: String,
    pub log: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub data_dir: String,
}

// ---------------- 在线短剧（番茄） ----------------

/// 番茄接口里的一条在线短剧（原始字段是 `video_data` 里那坨）。
/// 只挑界面要用的字段，内部结构不往前端泄。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnlineDrama {
    pub vid: String,
    pub series_id: String,
    pub title: String,
    pub desc: String,
    pub cover: String,
    /// 秒
    pub duration_sec: i64,
    pub play_count: i64,
    /// 热度原文（「1578万」这种）；接口没给时为空
    pub heat: String,
    pub score: String,
    pub episode_count: i64,
    pub tags: Vec<String>,
    pub copyright: String,
    pub is_new: bool,
}

/// 「最新上线」一页：游标和 session 都要原样带回去才能继续翻。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanqieCategoryPage {
    pub items: Vec<OnlineDrama>,
    pub has_more: bool,
    pub next_offset: i64,
    pub session_id: String,
}

/// 分类接口 `select_items` 的 8 个筛选键。
/// 字段名必须保持 snake_case，跟接口请求体一致。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FanqieSelectItems {
    #[serde(default)]
    pub genre: Vec<String>,
    #[serde(default)]
    pub sort: Vec<String>,
    #[serde(default)]
    pub category_dim_theme: Vec<String>,
    #[serde(default)]
    pub category_dim_role: Vec<String>,
    #[serde(default)]
    pub category_dim_epoch: Vec<String>,
    #[serde(default)]
    pub gender: Vec<String>,
    #[serde(default)]
    pub online_time: Vec<String>,
    #[serde(default)]
    pub creation_status: Vec<String>,
}

/// 筛选面板里的一个可选项，ID 是接口下发的 `selector_item_id`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanqieSelectorItem {
    pub id: String,
    pub name: String,
}

/// 筛选面板的一行：体裁 / 主题 / 设定 / 背景 / 排序 / 受众 / 时间…
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanqieSelectorRow {
    pub key: String,
    pub label: String,
    pub items: Vec<FanqieSelectorItem>,
}

/// 一部在线短剧的详情：标题、简介、封面和全部分集。
/// 优先取 App 详情接口；拿不到时退回官网 player 页，`source` 标明来源。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanqieSeriesDetail {
    pub series_id: String,
    pub title: String,
    pub intro: String,
    /// 封面原地址；可能是 HEIC，前端可交给 `fanqie_cover` 转成 JPEG
    pub cover: String,
    /// 上架日期（`YYYY-MM-DD`）。**列表接口不返回这个字段**，
    /// 只有详情接口的 `create_time` 有；拿不到就是空串。
    #[serde(default)]
    pub online_time: String,
    pub episode_count: i64,
    pub episodes: Vec<FanqieEpisode>,
    /// app / web
    pub source: String,
}

/// 一部剧里的一集。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanqieEpisode {
    pub vid: String,
    /// 从 1 开始的集号
    pub index: i64,
    pub title: String,
    /// 秒；接口没给时为 0
    pub duration_sec: i64,
}

/// 一集可播放的直链。走的是官网公开流，明文 MP4。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanqiePlayInfo {
    pub url: String,
    /// 例如 360p
    pub quality: String,
    /// web / web-fallback
    pub source: String,
}

/// App 播放接口给的元信息：时长、清晰度列表、流是否加密。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanqieVideoInfo {
    pub vid: String,
    pub duration_sec: f64,
    pub qualities: Vec<String>,
    pub encrypted: bool,
}

/// 一集准备好的本地可播文件。
///
/// App 的流是 CENC 加密的 HEVC，Rust 侧用 `spade_a` 派生的 key 交给 ffmpeg
/// 解密并转成 H.264，落到缓存目录；前端拿 `path` 交给 `convertFileSrc` 播放。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanqiePreparedVideo {
    pub vid: String,
    /// 本地 mp4 绝对路径
    pub path: String,
    /// 取自平台流的清晰度，例如 480p
    pub quality: String,
    /// 转码后的编码，固定 h264
    pub codec: String,
    pub duration_sec: f64,
    /// app：本次下载解密；cache：命中本地缓存
    pub source: String,
}

/// 批量下载时的一集。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanqieDownloadEpisode {
    pub vid: String,
    /// 从 1 开始的集号
    pub index: i64,
}

/// 请求把一部剧的全部分集下载到流水线源目录。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanqieDownloadRequest {
    pub series_id: String,
    pub title: String,
    /// 简介，写进 `<剧名>/intro.txt`；流水线靠这个文件拿剧情
    #[serde(default)]
    pub intro: String,
    /// 封面地址；写进 `<剧名>/poster.jpg`，没拿到就留空
    #[serde(default)]
    pub cover: String,
    pub episodes: Vec<FanqieDownloadEpisode>,
}

/// 一部剧的下载结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanqieDownloadReport {
    pub series_id: String,
    pub title: String,
    /// 剧集目录（绝对路径）
    pub dir: String,
    pub downloaded: i64,
    pub skipped: i64,
    /// 失败集：`第 3 集：原因`
    pub failed: Vec<String>,
}

/// 批量下载的当前状态。
///
/// **状态住后端，不住页面**：下载是几分钟起步的长任务，页面刷新、
/// 切页都不该把它连同进度一起抹掉。这里同时存在内存（`AppState.download`）
/// 和 `setting` 表（`fanqie.download.state`）里，应用重开后还能告诉用户
/// 「上次下载到哪、为什么停了」。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FanqieDownloadState {
    pub running: bool,
    /// 当前是第几部（0 基）
    pub series_index: i64,
    pub series_total: i64,
    pub title: String,
    /// 当前这部剧已处理完的集数
    pub episode: i64,
    /// 当前这部剧的总集数
    pub episodes: i64,
    /// 整批累计「下好 + 跳过」的集数
    pub done: i64,
    /// 整批累计失败的集数
    pub failed: i64,
    pub message: String,
    pub root: String,
    pub error: String,
    /// 实际落盘的剧名（下载根里还躺着以前下的剧，流水线只该跑这几部）
    pub landed: Vec<String>,
    /// 此刻正在下载的集（“第03集”）。
    ///
    /// 并发 3 时面板只显示一条 message，看起来像“一次只下了一集”——
    /// 这个字段让前端能把并行的那几集都摆出来。
    pub inflight: Vec<String>,
}

/// 批量下载时前端传过来的一部剧。分集列表由后端自己拉 ——
/// 页面关掉了，流程还得知道下一集是什么。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanqieDownloadPick {
    pub series_id: String,
    pub title: String,
    #[serde(default)]
    pub episode_count: i64,
}

/// 一整批下载的结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FanqieDownloadBatchReport {
    pub root: String,
    pub landed: Vec<String>,
    pub downloaded: i64,
    pub skipped: i64,
    /// 失败项：`第 3 集：原因` 或 `《剧名》：原因`
    pub failed: Vec<String>,
}
