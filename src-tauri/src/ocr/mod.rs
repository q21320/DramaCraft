//! OCR 硬字幕识别：把烧在画面里的字幕读出来，变成可编辑的字幕轴。
//!
//! 场景：短剧素材大量带硬字幕。要改字幕、要去字幕、要做多语言版本，
//! 第一步都得先知道「原来写了什么、什么时候写的」。
//!
//! ## 流程
//!
//! 1. ffmpeg 按固定间隔抽帧，**默认只裁画面底部一条**（硬字幕基本都在底部）——
//!    既快，又能少认画面里的招牌、水印、弹幕；
//! 2. 每帧交给 RapidOCR（Apache-2.0，本地 ONNX 推理，不联网）；
//! 3. 把逐帧结果**合并成字幕区间**：相邻帧文本相同就延长时间，不同就断开。
//!
//! 第 3 步是纯函数，OCR 抖动、漏识别、一闪而过的误识别这些边界都在单元测试里。

use crate::error::{AppError, AppResult};
use crate::models::{Cue, OcrFrame, OcrOptions};
use crate::proc;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub const DEFAULT_TIMEOUT_SECS: u64 = 3600;

/// 默认选项：2 帧/秒，只看底部 30%。
pub fn default_options() -> OcrOptions {
    OcrOptions {
        sample_fps: 2.0,
        bottom_ratio: 0.30,
        min_confidence: 0.50,
        merge_threshold: 0.85,
        min_duration_ms: 400,
        max_frames: 4000,
    }
}

pub fn normalize_options(raw: &OcrOptions) -> OcrOptions {
    let finite = |v: f64, lo: f64, hi: f64, fallback: f64| {
        if v.is_finite() {
            v.clamp(lo, hi)
        } else {
            fallback
        }
    };
    OcrOptions {
        sample_fps: finite(raw.sample_fps, 0.1, 10.0, 2.0),
        bottom_ratio: finite(raw.bottom_ratio, 0.05, 1.0, 0.30),
        min_confidence: finite(raw.min_confidence, 0.0, 0.99, 0.50),
        merge_threshold: finite(raw.merge_threshold, 0.5, 1.0, 0.85),
        min_duration_ms: raw.min_duration_ms.clamp(0, 10_000),
        max_frames: raw.max_frames.clamp(1, 20_000),
    }
}

pub fn default_timeout() -> Duration {
    Duration::from_secs(DEFAULT_TIMEOUT_SECS)
}

/// 检查指定 Python 是否具备 rapidocr_onnxruntime。
pub fn status(python: &str) -> crate::models::RuntimeStatus {
    use crate::models::RuntimeStatus;

    let python = python.trim();
    if python.is_empty() {
        return RuntimeStatus {
            available: false,
            python: String::new(),
            detail: "未指定 Python 运行时，硬字幕识别不可用。请在「设置」里选择 python.exe".into(),
        };
    }
    let exe = Path::new(python);
    if !exe.is_file() {
        return RuntimeStatus {
            available: false,
            python: python.to_string(),
            detail: "指定的 Python 路径不存在，请到「设置」重新选择".into(),
        };
    }

    let mut cmd = proc::python_command(exe);
    cmd.arg("-c")
        .arg("import rapidocr_onnxruntime; print('ok')");

    match proc::no_window(&mut cmd).output() {
        Ok(out) if out.status.success() => RuntimeStatus {
            available: true,
            python: python.to_string(),
            detail: "rapidocr_onnxruntime 就绪（本地 ONNX 推理，不联网）".into(),
        },
        Ok(_) => RuntimeStatus {
            available: false,
            python: python.to_string(),
            detail: "该 Python 里没有安装 rapidocr_onnxruntime，可执行：pip install rapidocr_onnxruntime"
                .into(),
        },
        Err(err) => RuntimeStatus {
            available: false,
            python: python.to_string(),
            detail: format!("无法执行该 Python：{err}"),
        },
    }
}

// ---------------- 文本处理（纯函数） ----------------

/// 归一化：转小写、去掉空白与常见标点，只留下真正的内容字符。
///
/// OCR 对同一句的标点、空格经常前后不一致，直接比较会把同一句判成两句。
pub fn normalize_text(raw: &str) -> String {
    raw.chars()
        .filter(|c| {
            !c.is_whitespace()
                && !matches!(
                    c,
                    '，' | '。'
                        | '、'
                        | '；'
                        | '：'
                        | '？'
                        | '！'
                        | '「'
                        | '」'
                        | '『'
                        | '』'
                        | '（'
                        | '）'
                        | '《'
                        | '》'
                        | '“'
                        | '”'
                        | '‘'
                        | '’'
                        | ','
                        | '.'
                        | ';'
                        | ':'
                        | '?'
                        | '!'
                        | '"'
                        | '\''
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '-'
                        | '_'
                        | '~'
                        | '·'
                )
        })
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// 编辑距离（只算字符级增删改）。
pub fn levenshtein(a: &[char], b: &[char]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// 文本相似度 0..=1。两边都归一化后为空则视为 1.0（都"没有内容"）。
pub fn similarity_ratio(a: &str, b: &str) -> f64 {
    let na = normalize_text(a);
    let nb = normalize_text(b);
    if na.is_empty() && nb.is_empty() {
        return 1.0;
    }
    if na.is_empty() || nb.is_empty() {
        return 0.0;
    }
    if na == nb {
        return 1.0;
    }
    let ca: Vec<char> = na.chars().collect();
    let cb: Vec<char> = nb.chars().collect();
    let distance = levenshtein(&ca, &cb);
    let longest = ca.len().max(cb.len()) as f64;
    1.0 - (distance as f64 / longest)
}

/// 合并中的一条候选字幕
struct Pending {
    start_ms: i64,
    end_ms: i64,
    /// 这一句在各帧上读到的所有版本，最后取最长的那条
    texts: Vec<String>,
}

impl Pending {
    fn best_text(&self) -> String {
        self.texts
            .iter()
            .max_by_key(|t| normalize_text(t).chars().count())
            .cloned()
            .unwrap_or_default()
    }
}

/// 把逐帧 OCR 结果合并成字幕区间。
///
/// 规则：
/// - 空文本 → 视为「这一帧没有字幕」，断开当前句；
/// - 与上一帧相似度 ≥ `threshold` → 延长时间（OCR 抖动不会切断同一句）；
/// - 否则断开，另起一句；
/// - 最终短于 `min_duration_ms` 的丢弃（一闪而过的误识别）；
/// - 每句取**归一化后最长**的那个版本作为文本（OCR 常有帧漏字，最长的通常最完整）。
pub fn merge_frames(
    frames: &[OcrFrame],
    sample_interval_ms: i64,
    min_duration_ms: i64,
    threshold: f64,
) -> Vec<Cue> {
    let interval = sample_interval_ms.max(1);
    let mut out: Vec<Cue> = Vec::new();
    let mut pending: Option<Pending> = None;

    let flush = |pending: &mut Option<Pending>, out: &mut Vec<Cue>| {
        if let Some(p) = pending.take() {
            if p.end_ms - p.start_ms >= min_duration_ms {
                let text = p.best_text();
                if !normalize_text(&text).is_empty() {
                    out.push(Cue {
                        id: 0,
                        media_id: 0,
                        index_no: 0,
                        start_ms: p.start_ms,
                        end_ms: p.end_ms,
                        text,
                    });
                }
            }
        }
    };

    for frame in frames {
        let text = frame.text.trim();
        if text.is_empty() {
            flush(&mut pending, &mut out);
            continue;
        }

        match pending.as_mut() {
            Some(p) if similarity_ratio(p.best_text().as_str(), text) >= threshold => {
                p.end_ms = frame.time_ms + interval;
                p.texts.push(text.to_string());
            }
            _ => {
                flush(&mut pending, &mut out);
                pending = Some(Pending {
                    start_ms: frame.time_ms,
                    end_ms: frame.time_ms + interval,
                    texts: vec![text.to_string()],
                });
            }
        }
    }
    flush(&mut pending, &mut out);

    out.sort_by_key(|c| (c.start_ms, c.end_ms));
    for (i, cue) in out.iter_mut().enumerate() {
        cue.index_no = i as i64 + 1;
    }
    out
}

// ---------------- 抽帧 ----------------

/// 按固定间隔抽帧到 `out_dir`，返回按文件名顺序排列的帧路径。
///
/// 时间戳由序号推算：第 i 帧（从 0 开始）对应 `i * 1000 / fps` 毫秒。
pub fn extract_frames(
    ffmpeg: &Path,
    input: &Path,
    out_dir: &Path,
    opts: &OcrOptions,
) -> AppResult<Vec<PathBuf>> {
    if !input.is_file() {
        return Err(AppError::Msg(format!(
            "素材文件不存在：{}",
            input.to_string_lossy()
        )));
    }
    let o = normalize_options(opts);
    std::fs::create_dir_all(out_dir)?;

    // 清掉上一次的残留，避免混入旧帧
    if let Ok(entries) = std::fs::read_dir(out_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "jpg").unwrap_or(false) {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    let filter = if o.bottom_ratio >= 0.999 {
        format!("fps={:.4}", o.sample_fps)
    } else {
        format!(
            "fps={:.4},crop=w=iw:h=trunc(ih*{:.4}/2)*2:x=0:y=trunc(ih*{:.4}/2)*2",
            o.sample_fps,
            o.bottom_ratio,
            1.0 - o.bottom_ratio
        )
    };

    let pattern = out_dir.join("%06d.jpg");
    let output = crate::proc::no_window(
        Command::new(ffmpeg)
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-y")
            .arg("-i")
            .arg(input.as_os_str())
            .arg("-an")
            .arg("-sn")
            .arg("-vf")
            .arg(&filter)
            .arg("-frames:v")
            .arg(o.max_frames.to_string())
            .arg("-q:v")
            .arg("3")
            .arg(pattern.as_os_str()),
    )
    .output()
    .map_err(|e| AppError::Process(format!("无法启动 ffmpeg：{e}")))?;

    if !output.status.success() {
        return Err(AppError::Process(format!(
            "抽帧失败（退出码 {:?}）：{}",
            output.status.code(),
            proc::tail(&String::from_utf8_lossy(&output.stderr))
        )));
    }

    let mut frames: Vec<PathBuf> = std::fs::read_dir(out_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "jpg").unwrap_or(false))
        .collect();
    frames.sort();

    if frames.is_empty() {
        return Err(AppError::Process(
            "没有抽到任何帧，素材可能损坏或过短".into(),
        ));
    }
    Ok(frames)
}

// ---------------- OCR 调用 ----------------

/// 帧文件清单 → 带时间戳的请求项
pub fn frame_requests(frames: &[PathBuf], sample_fps: f64) -> Vec<serde_json::Value> {
    let fps = if sample_fps.is_finite() && sample_fps > 0.0 {
        sample_fps
    } else {
        1.0
    };
    frames
        .iter()
        .enumerate()
        .map(|(i, path)| {
            let time_ms = ((i as f64) * 1000.0 / fps).round() as i64;
            serde_json::json!({ "path": path.to_string_lossy(), "time_ms": time_ms })
        })
        .collect()
}

/// 调 worker 识别。返回逐帧文本。
pub fn run_worker(
    python: &Path,
    worker: &Path,
    frames: &[PathBuf],
    opts: &OcrOptions,
    timeout: Duration,
) -> AppResult<(Vec<OcrFrame>, String)> {
    let o = normalize_options(opts);
    let request = serde_json::json!({
        "frames": frame_requests(frames, o.sample_fps),
        "min_confidence": o.min_confidence,
    });

    let value = proc::run_json_worker(python, worker, &request, timeout)?;

    let engine = value
        .get("engine")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    let parsed: Vec<OcrFrame> = value
        .get("frames")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|e| AppError::Process(format!("OCR 应答结构不符合约定：{e}")))?
        .unwrap_or_default();

    Ok((parsed, engine))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(time_ms: i64, text: &str) -> OcrFrame {
        OcrFrame {
            time_ms,
            text: text.to_string(),
        }
    }

    // ---------------- 归一化与相似度 ----------------

    #[test]
    fn normalize_strips_punctuation_and_case() {
        assert_eq!(normalize_text("你好，世界！"), "你好世界");
        assert_eq!(normalize_text("  Hello, World.  "), "helloworld");
        assert_eq!(normalize_text("A-B_C"), "abc");
        assert_eq!(normalize_text(""), "");
    }

    #[test]
    fn levenshtein_basics() {
        let a: Vec<char> = "kitten".chars().collect();
        let b: Vec<char> = "sitting".chars().collect();
        assert_eq!(levenshtein(&a, &b), 3);
        assert_eq!(levenshtein(&[], &b), 7);
        assert_eq!(levenshtein(&a, &[]), 6);
        assert_eq!(levenshtein(&a, &a), 0);
    }

    #[test]
    fn similarity_handles_jitter_and_empty() {
        // 只差标点 → 完全一致
        assert_eq!(similarity_ratio("你好，世界", "你好世界"), 1.0);
        // OCR 少认一个字的抖动
        let jitter = similarity_ratio("今天天气真不错", "今天天气不错");
        assert!(jitter > 0.8 && jitter < 1.0, "实际 {jitter}");
        // 完全不同的两句
        assert!(similarity_ratio("你好世界", "完全不同") < 0.5);
        // 两边都空 → 视为相同；只有一边空 → 0
        assert_eq!(similarity_ratio("", "   "), 1.0);
        assert_eq!(similarity_ratio("", "abc"), 0.0);
    }

    // ---------------- 合并逻辑 ----------------

    #[test]
    fn merges_repeated_frames_into_one_cue() {
        let frames = vec![
            frame(0, "第一句"),
            frame(500, "第一句"),
            frame(1000, "第一句"),
        ];
        let cues = merge_frames(&frames, 500, 400, 0.85);
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start_ms, 0);
        assert_eq!(cues[0].end_ms, 1500, "最后一帧要算上它自己的时长");
        assert_eq!(cues[0].text, "第一句");
        assert_eq!(cues[0].index_no, 1);
    }

    #[test]
    fn jitter_does_not_split_one_line() {
        let frames = vec![
            frame(0, "今天天气真不错"),
            frame(500, "今天天气不错"),   // 漏了一个字
            frame(1000, "今天天气真不错啊"),
        ];
        let cues = merge_frames(&frames, 500, 400, 0.85);
        assert_eq!(cues.len(), 1, "抖动不该把同一句切成多段");
        // 取归一化后最长的版本
        assert_eq!(cues[0].text, "今天天气真不错啊");
    }

    #[test]
    fn different_lines_split_into_separate_cues() {
        let frames = vec![
            frame(0, "第一句台词"),
            frame(500, "第一句台词"),
            frame(1000, "第二句台词"),
            frame(1500, "第二句台词"),
        ];
        let cues = merge_frames(&frames, 500, 400, 0.85);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "第一句台词");
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (0, 1000));
        assert_eq!(cues[1].text, "第二句台词");
        assert_eq!((cues[1].start_ms, cues[1].end_ms), (1000, 2000));
        assert_eq!(cues[1].index_no, 2);
    }

    #[test]
    fn blank_frames_break_the_current_line() {
        let frames = vec![
            frame(0, "第一句"),
            frame(500, "第一句"),
            frame(1000, ""),
            frame(1500, "第一句"),
            frame(2000, "第一句"),
        ];
        let cues = merge_frames(&frames, 500, 400, 0.85);
        assert_eq!(cues.len(), 2, "中间没有字幕的帧应该断开");
        assert_eq!(cues[0].end_ms, 1000);
        assert_eq!(cues[1].start_ms, 1500);
    }

    #[test]
    fn flashes_shorter_than_minimum_are_dropped() {
        let frames = vec![
            frame(0, "正常的一句"),
            frame(500, "正常的一句"),
            frame(1000, "一闪而过"),
            frame(1500, "另一句正常的"),
            frame(2000, "另一句正常的"),
        ];
        // 单帧只覆盖 500ms，min_duration 设 600 就该被丢掉
        let cues = merge_frames(&frames, 500, 600, 0.85);
        assert_eq!(cues.len(), 2);
        assert!(cues.iter().all(|c| !c.text.contains("一闪而过")));
    }

    #[test]
    fn empty_and_single_inputs_are_handled() {
        assert!(merge_frames(&[], 500, 400, 0.85).is_empty());
        // 全空帧
        let blanks = vec![frame(0, ""), frame(500, "   ")];
        assert!(merge_frames(&blanks, 500, 400, 0.85).is_empty());
        // 只有一帧但足够长 → 保留
        let one = vec![frame(0, "单独一句")];
        assert_eq!(merge_frames(&one, 1000, 400, 0.85).len(), 1);
        // 只有一帧但太短 → 丢掉
        assert!(merge_frames(&one, 100, 400, 0.85).is_empty());
    }

    #[test]
    fn results_are_sorted_and_renumbered() {
        let frames = vec![
            frame(2000, "后面"),
            frame(2500, "后面"),
            frame(0, "前面"),
            frame(500, "前面"),
        ];
        let cues = merge_frames(&frames, 500, 400, 0.85);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "前面");
        assert_eq!(cues[1].text, "后面");
        assert_eq!(cues[0].index_no, 1);
        assert_eq!(cues[1].index_no, 2);
    }

    #[test]
    fn very_different_neighbour_is_never_merged() {
        let frames = vec![frame(0, "完全不一样的一句话"), frame(500, "另一个主题的内容")];
        let cues = merge_frames(&frames, 500, 100, 0.85);
        assert_eq!(cues.len(), 2);
    }

    // ---------------- 选项与请求 ----------------

    #[test]
    fn option_normalization_clamps_and_recovers_from_nan() {
        let wild = OcrOptions {
            sample_fps: f64::NAN,
            bottom_ratio: 5.0,
            min_confidence: -3.0,
            merge_threshold: 99.0,
            min_duration_ms: -100,
            max_frames: 0,
        };
        let o = normalize_options(&wild);
        assert_eq!(o.sample_fps, 2.0, "NaN 回落到默认");
        assert_eq!(o.bottom_ratio, 1.0);
        assert_eq!(o.min_confidence, 0.0);
        assert_eq!(o.merge_threshold, 1.0);
        assert_eq!(o.min_duration_ms, 0);
        assert_eq!(o.max_frames, 1);
    }

    #[test]
    fn frame_requests_carry_rising_timestamps() {
        let frames: Vec<PathBuf> = (0..4)
            .map(|i| PathBuf::from(format!("E:/tmp/{i:06}.jpg")))
            .collect();
        let reqs = frame_requests(&frames, 2.0);
        assert_eq!(reqs.len(), 4);
        assert_eq!(reqs[0]["time_ms"], 0);
        assert_eq!(reqs[1]["time_ms"], 500);
        assert_eq!(reqs[3]["time_ms"], 1500);
    }

    #[test]
    fn missing_input_is_reported_before_spawning() {
        let err = extract_frames(
            Path::new("Z:/nope/ffmpeg.exe"),
            Path::new("Z:/nope/missing.mp4"),
            &std::env::temp_dir().join("dramadist_ocr_none"),
            &default_options(),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("素材文件不存在"), "实际：{err}");
    }

    #[test]
    fn status_without_python_is_unavailable_without_panicking() {
        let s = status("   ");
        assert!(!s.available);
        assert!(!s.detail.is_empty());
        let s = status("Z:/nope/python.exe");
        assert!(!s.available);
        assert!(s.detail.contains("不存在"), "实际：{}", s.detail);
    }

    // ---------------- 端到端（真实 ffmpeg + 装了 rapidocr 的 Python） ----------------

    fn env_path(key: &str) -> Option<PathBuf> {
        let raw = std::env::var(key).ok()?;
        let path = PathBuf::from(raw);
        path.is_file().then_some(path)
    }

    #[test]
    fn end_to_end_reads_burned_in_subtitles() {
        let (Some(ffmpeg), Some(python)) = (
            env_path("DRAMADIST_TEST_FFMPEG"),
            env_path("DRAMADIST_TEST_OCR_PYTHON"),
        ) else {
            eprintln!("跳过：需要同时设置 DRAMADIST_TEST_FFMPEG 与 DRAMADIST_TEST_OCR_PYTHON");
            return;
        };

        let worker = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("resources")
            .join("ocr_worker.py");
        assert!(worker.is_file(), "找不到 ocr_worker.py");

        let dir = std::env::temp_dir().join("dramadist_ocr_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // 造一段 6 秒视频：黑底白字，前 3 秒 HELLO 123，后 3 秒 WORLD 456，文字压在底部
        let video = dir.join("hard_sub.mp4");
        let font = "C\\:/Windows/Fonts/arial.ttf";
        let filter = format!(
            "drawtext=fontfile='{font}':text='HELLO 123':fontcolor=white:fontsize=48:\
             x=(w-text_w)/2:y=h*0.75:enable='between(t,0,2.9)',\
             drawtext=fontfile='{font}':text='WORLD 456':fontcolor=white:fontsize=48:\
             x=(w-text_w)/2:y=h*0.75:enable='between(t,3.0,5.9)'"
        );

        let mut args: Vec<String> = vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-y".into(),
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            "color=c=black:s=640x360:d=6".into(),
            "-vf".into(),
            filter,
            "-t".into(),
            "6".into(),
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "ultrafast".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
            video.to_string_lossy().to_string(),
        ];
        args.shrink_to_fit();
        let status = crate::proc::no_window(Command::new(&ffmpeg).args(&args))
            .status()
            .expect("无法启动 ffmpeg");
        assert!(status.success(), "生成带硬字幕的测试视频失败");

        let opts = default_options();
        let frames_dir = dir.join("frames");
        let frames = extract_frames(&ffmpeg, &video, &frames_dir, &opts).expect("抽帧失败");
        assert!(
            frames.len() >= 8,
            "6 秒 @2fps 应当抽到 12 帧左右，实际 {}",
            frames.len()
        );

        let (recognized, engine) =
            run_worker(&python, &worker, &frames, &opts, Duration::from_secs(900)).expect("OCR 失败");
        assert_eq!(recognized.len(), frames.len());
        assert!(!engine.is_empty());

        let interval = (1000.0 / opts.sample_fps).round() as i64;
        let cues = merge_frames(
            &recognized,
            interval,
            opts.min_duration_ms,
            opts.merge_threshold,
        );
        let texts: Vec<&str> = cues.iter().map(|c| c.text.as_str()).collect();
        assert!(
            cues.len() >= 2,
            "应当至少拼出两句，实际 {} 句：{texts:?}",
            cues.len()
        );

        let joined: String = cues.iter().map(|c| normalize_text(&c.text)).collect();
        assert!(joined.contains("hello"), "没识别到 HELLO，实际：{joined}");
        assert!(joined.contains("123"), "没识别到 123，实际：{joined}");
        assert!(joined.contains("world"), "没识别到 WORLD，实际：{joined}");
        assert!(joined.contains("456"), "没识别到 456，实际：{joined}");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
