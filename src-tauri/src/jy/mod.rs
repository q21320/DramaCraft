//! 剪映草稿导出：把切片与字幕映射成一条剪映时间轴，交给开源库生成草稿。
//!
//! 为什么依赖 [pyJianYingDraft](https://github.com/GuanYixuan/pyJianYingDraft)（Apache-2.0）
//! 而不是自己拼 `draft_content.json`：草稿格式是剪映的私有格式且随版本变化，
//! 自己维护一份模板等于给自己挖坑。让专门跟这个格式的库去做，我们只负责
//! **把项目数据映射成草稿结构** —— 这部分是纯逻辑，可以测干净。
//!
//! 协议与编码/工作目录/超时处理，统一复用 [`crate::proc`]。

use crate::error::{AppError, AppResult};
use crate::models::Cue;
use crate::proc;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

pub const DEFAULT_WIDTH: u32 = 1080;
pub const DEFAULT_HEIGHT: u32 = 1920;
pub const DEFAULT_FPS: u32 = 30;
pub const DEFAULT_TIMEOUT_SECS: u64 = 300;

pub fn default_timeout() -> Duration {
    Duration::from_secs(DEFAULT_TIMEOUT_SECS)
}

/// 草稿里的一段视频
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftClip {
    pub path: String,
    pub duration_ms: i64,
}

/// 草稿时间轴上的一条字幕（已按拼接顺序偏移过）
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftCue {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// 发给 worker 的请求
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftRequest {
    pub draft_root: String,
    pub draft_name: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub clips: Vec<DraftClip>,
    pub cues: Vec<DraftCue>,
    pub font_size: f64,
}

/// worker 的应答
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftReport {
    pub draft_path: String,
    pub draft_name: String,
    pub clip_count: i64,
    pub cue_count: i64,
    pub duration_ms: i64,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// 把「每个切片各自的字幕」拼到草稿时间轴上。
///
/// 调用方负责先用 [`crate::subtitle::project_range`] 把整片字幕裁到每个切片的区间；
/// 这里只做三件事：**按顺序累加偏移**、把越界的时间夹回片段内、丢掉空文本。
pub fn assemble_timeline(clip_durations_ms: &[i64], cues_per_clip: &[Vec<Cue>]) -> Vec<DraftCue> {
    let mut out: Vec<DraftCue> = Vec::new();
    let mut offset = 0i64;

    for (index, raw_duration) in clip_durations_ms.iter().enumerate() {
        let duration = (*raw_duration).max(0);
        if let Some(cues) = cues_per_clip.get(index) {
            for cue in cues {
                let text = cue.text.trim();
                if text.is_empty() {
                    continue;
                }
                let start = cue.start_ms.clamp(0, duration);
                let end = cue.end_ms.clamp(0, duration);
                if end <= start {
                    continue;
                }
                out.push(DraftCue {
                    start_ms: offset + start,
                    end_ms: offset + end,
                    text: text.to_string(),
                });
            }
        }
        offset += duration;
    }

    out.sort_by_key(|c| (c.start_ms, c.end_ms));
    out
}

/// 草稿名要当目录名用，必须去掉 Windows 非法字符。
pub fn safe_draft_name(raw: &str) -> String {
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
        return "DramaDist_切片".to_string();
    }
    trimmed.chars().take(60).collect()
}

pub fn export_draft(
    python: &Path,
    worker: &Path,
    req: &DraftRequest,
    timeout: Duration,
) -> AppResult<DraftReport> {
    let value = proc::run_json_worker(python, worker, req, timeout)?;
    serde_json::from_value(value)
        .map_err(|e| AppError::Process(format!("草稿 worker 应答结构不符合约定：{e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(start: i64, end: i64, text: &str) -> Cue {
        Cue {
            id: 0,
            media_id: 1,
            index_no: 0,
            start_ms: start,
            end_ms: end,
            text: text.to_string(),
        }
    }

    #[test]
    fn timeline_offsets_cues_by_preceding_clip_lengths() {
        let durations = vec![3_000, 4_000, 2_500];
        let per_clip = vec![
            vec![cue(200, 2_500, "第一片字幕")],
            vec![cue(0, 1_000, "第二片甲"), cue(2_000, 3_500, "第二片乙")],
            vec![],
        ];
        let timeline = assemble_timeline(&durations, &per_clip);

        assert_eq!(timeline.len(), 3);
        // 第一片：原样
        assert_eq!((timeline[0].start_ms, timeline[0].end_ms), (200, 2_500));
        // 第二片：整体后移第一片长度 3000
        assert_eq!((timeline[1].start_ms, timeline[1].end_ms), (3_000, 4_000));
        assert_eq!((timeline[2].start_ms, timeline[2].end_ms), (5_000, 6_500));
        assert_eq!(timeline[2].text, "第二片乙");
    }

    #[test]
    fn timeline_clamps_to_clip_bounds_and_drops_degenerate() {
        let durations = vec![2_000];
        let per_clip = vec![vec![
            cue(-500, 800, "起点越界"),
            cue(1_800, 9_000, "终点越界"),
            cue(900, 900, "零长度"),
            cue(0, 100, "   "),
        ]];
        let timeline = assemble_timeline(&durations, &per_clip);
        assert_eq!(timeline.len(), 2, "零长度与空文本都要丢掉");
        assert_eq!((timeline[0].start_ms, timeline[0].end_ms), (0, 800));
        assert_eq!((timeline[1].start_ms, timeline[1].end_ms), (1_800, 2_000));
    }

    #[test]
    fn timeline_handles_missing_and_mismatched_inputs() {
        // cues_per_clip 比 durations 短：缺的部分当没有字幕，不 panic
        let timeline = assemble_timeline(&[1_000, 2_000], &[vec![cue(0, 500, "只有第一片")]]);
        assert_eq!(timeline.len(), 1);
        // 全空
        assert!(assemble_timeline(&[], &[]).is_empty());
        assert!(assemble_timeline(&[1_000], &[vec![]]).is_empty());
    }

    #[test]
    fn timeline_sorts_cues_that_arrive_out_of_order() {
        let durations = vec![5_000];
        let per_clip = vec![vec![cue(3_000, 4_000, "后"), cue(100, 900, "前")]];
        let timeline = assemble_timeline(&durations, &per_clip);
        assert_eq!(timeline[0].text, "前");
        assert_eq!(timeline[1].text, "后");
    }

    #[test]
    fn draft_name_is_filesystem_safe() {
        assert_eq!(safe_draft_name("都市爽剧A_切片"), "都市爽剧A_切片");
        // Windows 非法字符全部替换
        assert_eq!(safe_draft_name(r#"a/b\c:d*e?f"g<h>i|j"#), "a_b_c_d_e_f_g_h_i_j");
        // 控制字符
        assert_eq!(safe_draft_name("a\u{1}b"), "a_b");
        // 空 / 纯空白 / 纯点 → 兜底名
        assert_eq!(safe_draft_name("   "), "DramaDist_切片");
        assert_eq!(safe_draft_name("..."), "DramaDist_切片");
        // 超长截断
        assert_eq!(safe_draft_name(&"字".repeat(100)).chars().count(), 60);
    }

    // ---------------- 端到端（真实 ffmpeg + 装了 pyJianYingDraft 的 Python） ----------------

    fn env_path(key: &str) -> Option<std::path::PathBuf> {
        let raw = std::env::var(key).ok()?;
        let path = std::path::PathBuf::from(raw);
        path.is_file().then_some(path)
    }

    #[test]
    fn end_to_end_draft_export_with_real_library() {
        let (Some(ffmpeg), Some(python)) = (
            env_path("DRAMADIST_TEST_FFMPEG"),
            env_path("DRAMADIST_TEST_JY_PYTHON"),
        ) else {
            eprintln!("跳过：需要同时设置 DRAMADIST_TEST_FFMPEG 与 DRAMADIST_TEST_JY_PYTHON");
            return;
        };

        let worker = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("jy_worker.py");
        assert!(worker.is_file(), "找不到 jy_worker.py");

        let dir = std::env::temp_dir().join("dramadist_jy_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("clips")).unwrap();

        // 造两段 2 秒素材
        for (name, freq) in [("001.mp4", "440"), ("002.mp4", "660")] {
            let out = dir.join("clips").join(name);
            let status = crate::proc::no_window(std::process::Command::new(&ffmpeg).args([
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
            ]))
            .arg(&out)
            .status()
            .expect("无法启动 ffmpeg");
            assert!(status.success(), "生成测试片段失败：{name}");
        }

        let clips = vec![
            DraftClip {
                path: dir.join("clips").join("001.mp4").to_string_lossy().to_string(),
                duration_ms: 2_000,
            },
            DraftClip {
                path: dir.join("clips").join("002.mp4").to_string_lossy().to_string(),
                duration_ms: 2_000,
            },
        ];
        let request = DraftRequest {
            draft_root: dir.join("drafts").to_string_lossy().to_string(),
            draft_name: "端到端_测试草稿".into(),
            width: DEFAULT_WIDTH,
            height: DEFAULT_HEIGHT,
            fps: DEFAULT_FPS,
            clips,
            cues: vec![
                DraftCue {
                    start_ms: 100,
                    end_ms: 1_500,
                    text: "第一句".into(),
                },
                DraftCue {
                    start_ms: 2_100,
                    end_ms: 3_500,
                    text: "第二句".into(),
                },
            ],
            font_size: 8.0,
        };

        let report = export_draft(&python, &worker, &request, Duration::from_secs(180))
            .expect("草稿导出失败");
        assert_eq!(report.clip_count, 2);
        assert_eq!(report.cue_count, 2);
        assert_eq!(report.duration_ms, 4_000, "两段 2 秒拼起来应当是 4 秒");

        let draft_dir = std::path::Path::new(&report.draft_path);
        assert!(draft_dir.is_dir(), "草稿目录不存在：{}", report.draft_path);
        let content_path = draft_dir.join("draft_content.json");
        assert!(content_path.is_file(), "缺少 draft_content.json");
        assert!(
            draft_dir.join("draft_meta_info.json").is_file(),
            "缺少 draft_meta_info.json"
        );

        let content = std::fs::read_to_string(&content_path).unwrap();
        assert!(content.contains("第一句"), "草稿内容里没有第一句字幕");
        assert!(content.contains("第二句"), "草稿内容里没有第二句字幕");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
