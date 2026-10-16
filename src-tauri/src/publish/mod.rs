//! 发布：把切片整理成「可以直接上传」的发布包，并维护**本地**发布台账。
//!
//! ## 为什么这里不直接调平台接口
//!
//! - **微信视频号没有公开的发布 API**，只能人工，或用微信自己的工具；
//! - **抖音开放平台**需要企业资质，审核通过后才拿得到视频发布权限；
//! - **网页自动化**绕开平台风控，存在封号与违反平台协议的风险，不适合做进默认路径。
//!
//! 所以这个模块把「准备」做到位：视频、封面、字幕、标题、简介、标签按平台整理成目录树，
//! 剩下的上传动作交给人工，或交给用户自己申请到的官方接口。
//!
//! **本项目不伪造接口调用，也不假装发送成功。** 导出全程只在本地写文件。

use crate::error::{AppError, AppResult};
use crate::models::{
    PlatformOption, PublishClipEntry, PublishMetadata, PublishPackageReport, PublishPlan,
};
use crate::proc::no_window;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 支持的平台：(key, 中文名, 是否有公开发布 API, 说明)
pub const PLATFORMS: [(&str, &str, bool, &str); 4] = [
    (
        "douyin",
        "抖音",
        true,
        "抖音开放平台提供视频发布接口，需要企业资质并通过审核",
    ),
    (
        "wechat_channels",
        "微信视频号",
        false,
        "视频号没有公开的发布 API，需要人工上传或用微信自家工具",
    ),
    (
        "bilibili",
        "B站",
        true,
        "B站开放平台提供投稿接口，需要申请开发者权限",
    ),
    (
        "kuaishou",
        "快手",
        true,
        "快手开放平台提供视频发布接口，需要申请开发者权限",
    ),
];

pub const STATUS_DRAFT: &str = "draft";
pub const STATUS_READY: &str = "ready";
pub const STATUS_PUBLISHED: &str = "published";
pub const STATUS_FAILED: &str = "failed";

/// 没填账号时用它兜底。留一个空账号名只会让人看不懂台账。
pub const DEFAULT_ACCOUNT: &str = "默认账号";

/// 账号名归一化：去空白、空值兜底、限长。
pub fn normalize_account(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return DEFAULT_ACCOUNT.to_string();
    }
    trimmed.chars().take(40).collect()
}

pub fn platform_options() -> Vec<PlatformOption> {
    PLATFORMS
        .iter()
        .map(|(key, label, has_api, note)| PlatformOption {
            key: (*key).to_string(),
            label: (*label).to_string(),
            has_public_api: *has_api,
            note: (*note).to_string(),
        })
        .collect()
}

pub fn is_known_platform(key: &str) -> bool {
    PLATFORMS.iter().any(|(k, _, _, _)| *k == key)
}

pub fn platform_label(key: &str) -> String {
    PLATFORMS
        .iter()
        .find(|(k, _, _, _)| *k == key)
        .map(|(_, label, _, _)| (*label).to_string())
        .unwrap_or_else(|| key.to_string())
}

pub fn platform_note(key: &str) -> String {
    PLATFORMS
        .iter()
        .find(|(k, _, _, _)| *k == key)
        .map(|(_, _, _, note)| (*note).to_string())
        .unwrap_or_default()
}

/// 规范化状态值，未知值一律落到 `draft`。
pub fn normalize_status(raw: &str) -> String {
    match raw.trim().to_lowercase().as_str() {
        "ready" => STATUS_READY.to_string(),
        "published" => STATUS_PUBLISHED.to_string(),
        "failed" => STATUS_FAILED.to_string(),
        _ => STATUS_DRAFT.to_string(),
    }
}

/// 标签解析：逗号、中文逗号、顿号、空白、`#` 都算分隔符；去重且保持顺序。
pub fn split_tags(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for piece in raw.split(['#', ',', '，', '、', ' ', '\t', '\n', '\r']) {
        let tag = piece.trim();
        if tag.is_empty() {
            continue;
        }
        if !out.iter().any(|existing| existing == tag) {
            out.push(tag.to_string());
        }
    }
    out
}

/// 文件名清洗：去掉 Windows 非法字符与控制字符，压掉首尾空白与点。
pub fn safe_name(raw: &str, fallback: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if "\\/:*?\"<>|".contains(c) || (c as u32) < 0x20 {
                '_'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.').trim();
    if trimmed.is_empty() {
        return fallback.to_string();
    }
    trimmed.chars().take(60).collect()
}

/// 片段标题建议：`标题（2/12）`。只有一个片段时不加序号。
pub fn suggested_title(base: &str, index: i64, total: i64) -> String {
    let base = base.trim();
    if base.is_empty() {
        return format!("第 {index} 段");
    }
    if total <= 1 {
        return base.to_string();
    }
    format!("{base}（{index}/{total}）")
}

pub fn build_metadata(
    plan: &PublishPlan,
    media_name: &str,
    width: i64,
    height: i64,
    clips: Vec<PublishClipEntry>,
) -> PublishMetadata {
    let total_duration_ms = clips.iter().map(|c| c.duration_ms).sum();
    PublishMetadata {
        platform: plan.platform.clone(),
        platform_label: platform_label(&plan.platform),
        account: normalize_account(&plan.account),
        media_name: media_name.to_string(),
        title: plan.title.trim().to_string(),
        description: plan.description.trim().to_string(),
        tags: split_tags(&plan.tags),
        cover: "cover.jpg".to_string(),
        width,
        height,
        total_duration_ms,
        exported_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        clips,
    }
}

/// 发布包里的说明文件。用户在文件夹里就能看懂下一步做什么。
pub fn render_readme(meta: &PublishMetadata) -> String {
    let mut out = String::new();
    out.push_str("DramaDist 发布包\n");
    out.push_str("================\n\n");
    out.push_str(&format!("平台：{}\n", meta.platform_label));
    out.push_str(&format!("账号：{}\n", meta.account));
    out.push_str(&format!("素材：{}\n", meta.media_name));
    out.push_str(&format!(
        "标题：{}\n",
        if meta.title.is_empty() {
            "（未填写）"
        } else {
            &meta.title
        }
    ));
    if !meta.description.is_empty() {
        out.push_str(&format!("简介：{}\n", meta.description));
    }
    if !meta.tags.is_empty() {
        let tags: Vec<String> = meta.tags.iter().map(|t| format!("#{t}")).collect();
        out.push_str(&format!("标签：{}\n", tags.join(" ")));
    }
    out.push_str(&format!("画面：{}×{}\n", meta.width, meta.height));
    out.push_str(&format!(
        "片段：{} 个（clips/ 目录）\n",
        meta.clips.len()
    ));
    out.push_str(&format!("封面：{}\n", meta.cover));
    out.push_str(&format!("导出时间：{}\n", meta.exported_at));
    out.push('\n');

    if !meta.clips.is_empty() {
        out.push_str("片段清单\n--------\n");
        for clip in &meta.clips {
            out.push_str(&format!(
                "  {}  {}  {}ms  {}\n",
                clip.file, clip.suggested_title, clip.duration_ms, clip.start_end_label()
            ));
        }
        out.push('\n');
    }

    out.push_str("怎么用\n------\n");
    out.push_str("1. 打开对应平台的创作者后台（或 App），上传 clips/ 下的视频；\n");
    out.push_str("2. 标题、简介、标签可以直接从本文件或 metadata.json 复制；\n");
    out.push_str("3. 需要字幕时用 subtitles/ 下的同名 srt；已在画面里烧好字幕的可以忽略；\n");
    out.push_str("4. 发完回到 DramaDist 的「发布」页，把该平台标记为已发布，方便自己记账。\n\n");

    out.push_str("说明\n----\n");
    out.push_str("· 本次导出只在本地整理文件，没有向任何平台发送任何内容；\n");
    out.push_str("· 封面若未指定，是从第一个片段抽的一帧，建议按平台要求自行替换；\n");
    let note = platform_note(&meta.platform);
    if !note.is_empty() {
        out.push_str(&format!("· 平台提示：{note}。\n"));
    }
    out
}

impl PublishClipEntry {
    fn start_end_label(&self) -> String {
        format!("{} → {}", ms_label(self.start_ms), ms_label(self.end_ms))
    }
}

fn ms_label(ms: i64) -> String {
    let ms = ms.max(0);
    let h = ms / 3_600_000;
    let m = (ms % 3_600_000) / 60_000;
    let s = (ms % 60_000) / 1000;
    format!("{h:02}:{m:02}:{s:02}")
}

/// 要打进包里的一个片段
pub struct PackageClip {
    pub index_no: i64,
    pub source: PathBuf,
    /// 该片段的 SRT 文本；为空表示不打字幕
    pub srt: Option<String>,
}

/// 写出发布包。
///
/// 目录结构（**带账号一层**，同一平台多账号时互不覆盖）：
/// ```text
/// <out_root>/<platform>/<账号>/<素材名>/
/// ├─ clips/001.mp4 ...
/// ├─ subtitles/001.srt ...
/// ├─ cover.jpg
/// ├─ metadata.json
/// └─ README.txt
/// ```
pub fn write_package(
    ffmpeg: &Path,
    out_root: &Path,
    meta: &PublishMetadata,
    clips: &[PackageClip],
    custom_cover: Option<&Path>,
) -> AppResult<PublishPackageReport> {
    if clips.is_empty() {
        return Err(AppError::Msg("没有可打包的片段".into()));
    }
    if out_root.as_os_str().is_empty() {
        return Err(AppError::Msg("请先选择发布包输出目录".into()));
    }

    let root = out_root
        .join(&meta.platform)
        .join(safe_name(&meta.account, DEFAULT_ACCOUNT))
        .join(safe_name(&meta.media_name, "未命名素材"));
    let clips_dir = root.join("clips");
    let subs_dir = root.join("subtitles");
    std::fs::create_dir_all(&clips_dir)?;
    std::fs::create_dir_all(&subs_dir)?;

    let mut warnings: Vec<String> = Vec::new();
    let mut clip_count = 0i64;
    let mut subtitle_count = 0i64;
    let mut first_clip: Option<PathBuf> = None;

    for clip in clips {
        if !clip.source.is_file() {
            warnings.push(format!(
                "片段 {} 的源文件不存在，已跳过：{}",
                clip.index_no,
                clip.source.to_string_lossy()
            ));
            continue;
        }
        let dest = clips_dir.join(format!("{:03}.mp4", clip.index_no));
        std::fs::copy(&clip.source, &dest)?;
        if first_clip.is_none() {
            first_clip = Some(dest.clone());
        }
        clip_count += 1;

        if let Some(srt) = &clip.srt {
            if !srt.trim().is_empty() {
                std::fs::write(subs_dir.join(format!("{:03}.srt", clip.index_no)), srt)?;
                subtitle_count += 1;
            }
        }
    }

    if clip_count == 0 {
        return Err(AppError::Msg(
            "所有片段的源文件都不存在，没有可打包的内容".into(),
        ));
    }

    // 封面：优先用用户指定的图片，否则从第一个片段抽一帧
    let cover = root.join("cover.jpg");
    let mut cover_ok = false;
    if let Some(src) = custom_cover {
        if src.is_file() {
            std::fs::copy(src, &cover)?;
            cover_ok = true;
        } else {
            warnings.push(format!(
                "指定的封面不存在，改为从视频抽帧：{}",
                src.to_string_lossy()
            ));
        }
    }
    if !cover_ok {
        if let Some(first) = &first_clip {
            let status = no_window(
                Command::new(ffmpeg)
                    .arg("-hide_banner")
                    .arg("-loglevel")
                    .arg("error")
                    .arg("-y")
                    .arg("-ss")
                    .arg("0.5")
                    .arg("-i")
                    .arg(first.as_os_str())
                    .arg("-frames:v")
                    .arg("1")
                    .arg("-q:v")
                    .arg("2")
                    .arg(cover.as_os_str()),
            )
            .status();
            match status {
                Ok(s) if s.success() && cover.is_file() => cover_ok = true,
                _ => warnings.push(
                    "封面抽帧失败（片段可能太短），可以自己放一张 cover.jpg 进目录".into(),
                ),
            }
        }
    }
    if !cover_ok {
        warnings.push("发布包里暂时没有封面".into());
    }

    let metadata_path = root.join("metadata.json");
    std::fs::write(&metadata_path, serde_json::to_string_pretty(meta)?)?;

    let readme_path = root.join("README.txt");
    std::fs::write(&readme_path, render_readme(meta))?;

    Ok(PublishPackageReport {
        out_dir: root.to_string_lossy().to_string(),
        clip_count,
        subtitle_count,
        cover_path: if cover_ok {
            cover.to_string_lossy().to_string()
        } else {
            String::new()
        },
        metadata_path: metadata_path.to_string_lossy().to_string(),
        readme_path: readme_path.to_string_lossy().to_string(),
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> PublishPlan {
        PublishPlan {
            id: 0,
            media_id: 1,
            platform: "douyin".into(),
            account: "账号甲".into(),
            title: "都市爽剧 第 1 集".into(),
            description: "第一集高能开局".into(),
            tags: "#短剧, 推荐  爽文、短剧".into(),
            cover_path: String::new(),
            status: STATUS_DRAFT.into(),
            note: String::new(),
            updated_at: String::new(),
        }
    }

    fn entry(index: i64, duration: i64) -> PublishClipEntry {
        PublishClipEntry {
            index_no: index,
            file: format!("{index:03}.mp4"),
            start_ms: (index - 1) * 60_000,
            end_ms: (index - 1) * 60_000 + duration,
            duration_ms: duration,
            subtitle: format!("第 {index} 段的字幕"),
            suggested_title: suggested_title("都市爽剧 第 1 集", index, 3),
        }
    }

    #[test]
    fn platform_helpers_are_consistent() {
        assert!(is_known_platform("douyin"));
        assert!(!is_known_platform("tiktok"));
        assert_eq!(platform_label("wechat_channels"), "微信视频号");
        // 未知 key 原样返回，不 panic
        assert_eq!(platform_label("unknown"), "unknown");
        assert!(platform_note("wechat_channels").contains("没有公开"));

        let options = platform_options();
        assert_eq!(options.len(), PLATFORMS.len());
        // 视频号必须如实标注"没有公开 API"
        let channels = options
            .iter()
            .find(|o| o.key == "wechat_channels")
            .unwrap();
        assert!(!channels.has_public_api);
    }

    #[test]
    fn status_normalization_falls_back_to_draft() {
        assert_eq!(normalize_status("READY"), STATUS_READY);
        assert_eq!(normalize_status(" published "), STATUS_PUBLISHED);
        assert_eq!(normalize_status("failed"), STATUS_FAILED);
        assert_eq!(normalize_status("随便什么"), STATUS_DRAFT);
        assert_eq!(normalize_status(""), STATUS_DRAFT);
    }

    #[test]
    fn tags_split_dedup_and_keep_order() {
        let tags = split_tags("#短剧, 推荐  爽文、短剧\n#逆袭");
        assert_eq!(tags, vec!["短剧", "推荐", "爽文", "逆袭"]);
        assert!(split_tags("   ").is_empty());
        assert!(split_tags(",,,、").is_empty());
    }

    #[test]
    fn safe_name_strips_illegal_characters() {
        assert_eq!(safe_name("都市爽剧/第1集", "未命名"), "都市爽剧_第1集");
        assert_eq!(safe_name(r#"a\b:c*d?e"f<g>h|i"#, "x"), "a_b_c_d_e_f_g_h_i");
        assert_eq!(safe_name("  ...  ", "兜底"), "兜底");
        assert_eq!(safe_name("", "兜底"), "兜底");
        assert_eq!(safe_name(&"字".repeat(100), "x").chars().count(), 60);
    }

    #[test]
    fn suggested_title_numbers_only_when_multiple() {
        assert_eq!(suggested_title("标题", 1, 1), "标题");
        assert_eq!(suggested_title("标题", 2, 12), "标题（2/12）");
        // 标题为空时给一个可用的默认值
        assert_eq!(suggested_title("   ", 3, 5), "第 3 段");
    }

    #[test]
    fn metadata_aggregates_and_trims() {
        let meta = build_metadata(
            &plan(),
            "a.mp4",
            1080,
            1920,
            vec![entry(1, 60_000), entry(2, 30_000)],
        );
        assert_eq!(meta.platform_label, "抖音");
        assert_eq!(meta.total_duration_ms, 90_000);
        assert_eq!(meta.tags, vec!["短剧", "推荐", "爽文"]);
        assert_eq!(meta.cover, "cover.jpg");
        assert_eq!(meta.clips.len(), 2);
        assert!(!meta.exported_at.is_empty());
    }

    #[test]
    fn account_normalization_falls_back_to_default() {
        assert_eq!(normalize_account("  "), DEFAULT_ACCOUNT);
        assert_eq!(normalize_account(""), DEFAULT_ACCOUNT);
        assert_eq!(normalize_account("  账号甲  "), "账号甲");
        assert_eq!(normalize_account(&"长".repeat(80)).chars().count(), 40);
    }

    #[test]
    fn metadata_carries_the_account() {
        let meta = build_metadata(&plan(), "a.mp4", 1080, 1920, vec![]);
        assert_eq!(meta.account, "账号甲");
        let text = render_readme(&meta);
        assert!(text.contains("账号：账号甲"), "{text}");
    }

    #[test]
    fn readme_explains_next_steps_and_is_honest() {
        let meta = build_metadata(&plan(), "a.mp4", 1080, 1920, vec![entry(1, 60_000)]);
        let text = render_readme(&meta);
        assert!(text.contains("抖音"));
        assert!(text.contains("都市爽剧 第 1 集"));
        assert!(text.contains("#短剧 #推荐 #爽文"), "{text}");
        assert!(text.contains("001.mp4"));
        assert!(text.contains("clips/"));
        // 必须写明没有上传任何东西
        assert!(text.contains("没有向任何平台发送"), "{text}");
    }

    #[test]
    fn readme_handles_empty_optional_fields() {
        let mut p = plan();
        p.title = String::new();
        p.description = String::new();
        p.tags = String::new();
        let meta = build_metadata(&p, "a.mp4", 1920, 1080, vec![]);
        let text = render_readme(&meta);
        assert!(text.contains("（未填写）"));
        // 没有标签时不该出现空的「标签：」行
        assert!(!text.contains("标签：\n"), "{text}");
    }

    #[test]
    fn empty_clip_list_is_rejected() {
        let meta = build_metadata(&plan(), "a.mp4", 1080, 1920, vec![]);
        let err = write_package(
            Path::new("Z:/nope/ffmpeg.exe"),
            &std::env::temp_dir().join("dramadist_publish_none"),
            &meta,
            &[],
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("没有可打包的片段"), "实际：{err}");
    }

    // ---------------- 端到端（需要真实 ffmpeg，默认跳过） ----------------

    #[test]
    fn end_to_end_package_with_real_ffmpeg() {
        let Some(ffmpeg) = std::env::var("DRAMADIST_TEST_FFMPEG")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
        else {
            eprintln!("跳过：未设置 DRAMADIST_TEST_FFMPEG");
            return;
        };

        let dir = std::env::temp_dir().join("dramadist_publish_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();

        // 造两段素材
        let mut sources = Vec::new();
        for (name, freq) in [("001.mp4", "440"), ("002.mp4", "660")] {
            let out = dir.join("src").join(name);
            let status = no_window(
                Command::new(&ffmpeg).args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc=size=320x240:rate=15",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("sine=frequency={freq}"),
                    "-t",
                    "2",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-pix_fmt",
                    "yuv420p",
                    "-c:a",
                    "aac",
                    "-shortest",
                ]),
            )
            .arg(&out)
            .status()
            .expect("无法启动 ffmpeg");
            assert!(status.success(), "生成 {name} 失败");
            sources.push(out);
        }

        let clips: Vec<PackageClip> = sources
            .iter()
            .enumerate()
            .map(|(i, src)| PackageClip {
                index_no: i as i64 + 1,
                source: src.clone(),
                srt: Some(format!(
                    "1\n00:00:00,100 --> 00:00:01,500\n第 {} 句字幕\n",
                    i + 1
                )),
            })
            .collect();

        let entries: Vec<PublishClipEntry> = (1..=2)
            .map(|i| entry(i, 2_000))
            .collect();
        let meta = build_metadata(&plan(), "测试素材", 320, 240, entries);

        let out_root = dir.join("out");
        let report = write_package(&ffmpeg, &out_root, &meta, &clips, None).expect("打包含失败");

        assert_eq!(report.clip_count, 2);
        assert_eq!(report.subtitle_count, 2);
        assert!(!report.cover_path.is_empty(), "应当抽出一张封面");

        let root = PathBuf::from(&report.out_dir);
        assert!(root.is_dir());
        // 目录必须按「平台 / 账号 / 素材」分层，否则多账号发布会互相覆盖
        assert!(
            report.out_dir.contains("douyin") && report.out_dir.contains("账号甲"),
            "包目录应按平台与账号分层：{}",
            report.out_dir
        );
        assert!(out_root.join("douyin").join("账号甲").is_dir());
        assert!(root.join("clips").join("001.mp4").is_file());
        assert!(root.join("clips").join("002.mp4").is_file());
        assert!(root.join("subtitles").join("001.srt").is_file());
        assert!(root.join("cover.jpg").is_file());
        assert!(root.join("README.txt").is_file());

        // 封面必须是有效 JPEG（非空且有 SOI 标记）
        let cover = std::fs::read(&report.cover_path).unwrap();
        assert!(cover.len() > 500, "封面太小：{} 字节", cover.len());
        assert_eq!(&cover[..2], &[0xFF, 0xD8], "封面不是 JPEG");

        // metadata.json 必须能被解析回来
        let raw = std::fs::read_to_string(&report.metadata_path).unwrap();
        let parsed: PublishMetadata = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.platform, "douyin");
        assert_eq!(parsed.clips.len(), 2);
        assert_eq!(parsed.total_duration_ms, 4_000);
        assert_eq!(parsed.tags, vec!["短剧", "推荐", "爽文"]);
        assert_eq!(parsed.width, 320);

        // 字幕内容要真的写进去了
        let srt = std::fs::read_to_string(root.join("subtitles").join("002.srt")).unwrap();
        assert!(srt.contains("第 2 句字幕"), "{srt}");

        // README 里要有平台名与"没有上传"的声明
        let readme = std::fs::read_to_string(&report.readme_path).unwrap();
        assert!(readme.contains("抖音"));
        assert!(readme.contains("没有向任何平台发送"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn end_to_end_uses_custom_cover_when_given() {
        let Some(ffmpeg) = std::env::var("DRAMADIST_TEST_FFMPEG")
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
        else {
            eprintln!("跳过：未设置 DRAMADIST_TEST_FFMPEG");
            return;
        };

        let dir = std::env::temp_dir().join("dramadist_publish_cover_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 造一段素材，再把它自己当"自定义封面"（只验证复制行为，不关心图片语义）
        let src = dir.join("clip.mp4");
        let status = no_window(
            Command::new(&ffmpeg).args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=160x120:rate=10",
                "-t",
                "1",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-pix_fmt",
                "yuv420p",
            ]),
        )
        .arg(&src)
        .status()
        .expect("无法启动 ffmpeg");
        assert!(status.success());

        // 用 ffmpeg 生成一张真实的 jpg 作为自定义封面
        let custom = dir.join("my_cover.jpg");
        let status = no_window(
            Command::new(&ffmpeg).args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=64x64:rate=1",
                "-frames:v",
                "1",
            ]),
        )
        .arg(&custom)
        .status()
        .expect("无法启动 ffmpeg");
        assert!(status.success());

        let clips = vec![PackageClip {
            index_no: 1,
            source: src,
            srt: None,
        }];
        let meta = build_metadata(&plan(), "自定义封面", 160, 120, vec![entry(1, 1_000)]);
        let report = write_package(&ffmpeg, &dir.join("out"), &meta, &clips, Some(&custom)).unwrap();

        assert_eq!(report.subtitle_count, 0, "没有字幕就不该有 subtitles 文件");
        let copied = std::fs::read(&report.cover_path).unwrap();
        let original = std::fs::read(&custom).unwrap();
        assert_eq!(copied, original, "自定义封面应当原样复制");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
