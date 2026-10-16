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

/// 哪些功能走 AI。
///
/// 刻意做成显式开关而不是"配了 Key 就自动用"：
/// 用 AI 会产生费用、产出属于 AI 生成内容（有标识义务），
/// 这两件事都该由用户点头。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiUsage {
    /// 封面生成（图生图）
    pub cover_generation: bool,
    /// 剧名 / 简介改写
    pub title_rewrite: bool,
    /// 云端字幕识别
    pub subtitle_cloud: bool,
}

impl Default for AiUsage {
    fn default() -> Self {
        Self {
            cover_generation: false,
            title_rewrite: false,
            subtitle_cloud: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConfigView {
    pub providers: Vec<AiProviderView>,
    /// 当前启用哪一个；空串表示都没启用
    pub active: String,
    pub usage: AiUsage,
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

// ---------------- 运行日志 ----------------

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
