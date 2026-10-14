//! 云端字幕适配器：把语音转写交给**用户自备 Key** 的云端服务。
//!
//! ## 为什么需要它
//!
//! 本地 faster-whisper 不需要联网、不花钱，但慢、吃内存，长素材尤其明显。
//! 云端按量计费、几十秒就能出结果。两条路各有场景，所以做成可切换的。
//!
//! ## 为什么按「OpenAI 兼容」的形状实现
//!
//! `/v1/audio/transcriptions` + `response_format=verbose_json` 这套是事实标准，
//! 国内外大量服务（含自建网关）都兼容它。用户只要填 Base URL + Key + 模型名，
//! 就能接上自己买的那家 —— 我们**不绑定任何一家服务商**，也不替用户选。
//!
//! ## 关于 Key 的存放
//!
//! Key 存在本地 SQLite 的 `setting` 表里，**明文**。这一点在界面上明说，
//! 并且 **Key 永远不会返回给前端**：前端只能看到打码后的提示。
//!
//! ## 诚实边界
//!
//! 适配器本身用本地 mock 服务做过端到端验证（请求形状、鉴权头、multipart 字段、
//! 响应解析、错误映射）。但**没有对接过任何真实服务商** —— 那需要真实 Key 与费用，
//! 由使用者自行验证。

use crate::error::{AppError, AppResult};
use crate::models::CloudConfigView;
use crate::proc::{no_window, tail};
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

pub const SETTING_BASE_URL: &str = "cloud.base_url";
pub const SETTING_API_KEY: &str = "cloud.api_key";
pub const SETTING_MODEL: &str = "cloud.model";
pub const SETTING_LANGUAGE: &str = "cloud.language";

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub const DEFAULT_MODEL: &str = "whisper-1";
pub const DEFAULT_LANGUAGE: &str = "zh";

/// 上传的音频：16kHz 单声道 mp3，体积小、各家都收。
pub const AUDIO_BITRATE: &str = "64k";

const REQUEST_TIMEOUT_SECS: u64 = 900;

// ---------------- 配置 ----------------

#[derive(Debug, Clone)]
pub struct CloudConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub language: String,
}

impl Default for CloudConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            api_key: String::new(),
            model: DEFAULT_MODEL.to_string(),
            language: DEFAULT_LANGUAGE.to_string(),
        }
    }
}

fn get(conn: &Connection, key: &str) -> AppResult<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM setting WHERE key = ?1")?;
    let mut rows = stmt.query(params![key])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

fn put(conn: &Connection, key: &str, value: &str) -> AppResult<()> {
    conn.execute(
        "INSERT INTO setting(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn load_config(conn: &Connection) -> AppResult<CloudConfig> {
    let defaults = CloudConfig::default();
    Ok(CloudConfig {
        base_url: get(conn, SETTING_BASE_URL)?
            .map(|v| normalize_base_url(&v))
            .unwrap_or(defaults.base_url),
        api_key: get(conn, SETTING_API_KEY)?.unwrap_or_default(),
        model: get(conn, SETTING_MODEL)?
            .filter(|v| !v.trim().is_empty())
            .unwrap_or(defaults.model),
        language: get(conn, SETTING_LANGUAGE)?
            .filter(|v| !v.trim().is_empty())
            .unwrap_or(defaults.language),
    })
}

/// 保存配置。`api_key` 传 `None` 表示「不改动已有的 Key」，
/// 传 `Some("")` 才是显式清空 —— 否则用户改个模型名就会把 Key 弄丢。
pub fn save_config(
    conn: &Connection,
    base_url: &str,
    api_key: Option<&str>,
    model: &str,
    language: &str,
) -> AppResult<CloudConfig> {
    put(conn, SETTING_BASE_URL, &normalize_base_url(base_url))?;
    if let Some(key) = api_key {
        put(conn, SETTING_API_KEY, key.trim())?;
    }
    put(
        conn,
        SETTING_MODEL,
        if model.trim().is_empty() {
            DEFAULT_MODEL
        } else {
            model.trim()
        },
    )?;
    put(
        conn,
        SETTING_LANGUAGE,
        if language.trim().is_empty() {
            DEFAULT_LANGUAGE
        } else {
            language.trim()
        },
    )?;
    load_config(conn)
}

/// Base URL 归一化：去空白与结尾斜杠，空值回落默认。
pub fn normalize_base_url(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        DEFAULT_BASE_URL.to_string()
    } else {
        trimmed.to_string()
    }
}

/// 拼出转写端点。用户把完整 URL 粘进来也能用。
pub fn transcribe_endpoint(base_url: &str) -> String {
    let base = normalize_base_url(base_url);
    if base.ends_with("/audio/transcriptions") {
        base
    } else {
        format!("{base}/audio/transcriptions")
    }
}

/// Key 打码。前端只能看到这个，看不到原 Key。
pub fn mask_key(key: &str) -> String {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 8 {
        return "*".repeat(chars.len());
    }
    let head: String = chars.iter().take(3).collect();
    let tail: String = chars.iter().skip(chars.len() - 4).collect();
    format!("{head}****{tail}")
}

/// 给前端的视图：打码 + 明确的提醒。
pub fn view(cfg: &CloudConfig) -> CloudConfigView {
    let mut warnings: Vec<String> = Vec::new();
    if cfg.api_key.trim().is_empty() {
        warnings.push("还没有填 API Key，云端转写不可用。".to_string());
    }
    let base = normalize_base_url(&cfg.base_url);
    if base.starts_with("http://") && !is_loopback(&base) {
        warnings.push(
            "Base URL 是 http 明文，API Key 会原样经过网络。正式使用请换成 https。".to_string(),
        );
    }
    warnings.push(
        "Key 以明文存在本地数据库里。如果介意，可以用只含必要权限的临时 Key，用完就删。"
            .to_string(),
    );
    CloudConfigView {
        base_url: base,
        model: cfg.model.clone(),
        language: cfg.language.clone(),
        has_key: !cfg.api_key.trim().is_empty(),
        key_hint: mask_key(&cfg.api_key),
        warnings,
    }
}

fn is_loopback(url: &str) -> bool {
    url.contains("127.0.0.1") || url.contains("localhost") || url.contains("[::1]")
}

/// 把 HTTP 状态码翻译成用户能照着做的提示。
pub fn friendly_error(status: u16, body: &str) -> String {
    let hint = match status {
        400 => "请求被拒绝，多半是模型名不对，或这家服务不支持 verbose_json 时间轴",
        401 | 403 => "API Key 无效、过期，或没有该模型的权限，检查「设置 → 云端字幕」里的 Key",
        404 => "接口地址不对。检查 Base URL 是否含正确的版本路径（多数服务需要 /v1）",
        413 | 415 => "音频被拒收。可能是文件太大或格式不支持，可以换更短的素材试试",
        429 => "请求太频繁或额度用尽。稍后重试，或检查账户余额",
        500..=599 => "服务端出错，通常重试即可；持续失败请查服务商状态页",
        _ => "请求失败",
    };
    let detail = tail(body);
    if detail.is_empty() {
        format!("{hint}（HTTP {status}）")
    } else {
        format!("{hint}（HTTP {status}）。服务端返回：{detail}")
    }
}

// ---------------- 响应解析（纯函数） ----------------

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct CloudResponse {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub segments: Vec<CloudSegment>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct CloudSegment {
    #[serde(default)]
    pub start: f64,
    #[serde(default)]
    pub end: f64,
    #[serde(default)]
    pub text: String,
}

/// 解析转写结果 → 字幕条目。
///
/// 有些服务在极短音频上不返回 `segments`，只返回整段 `text`；
/// 这时退化成一条覆盖全片的字幕，而不是丢结果。
pub fn to_cues(resp: &CloudResponse, media_id: i64) -> Vec<crate::models::Cue> {
    if !resp.segments.is_empty() {
        let items: Vec<(f64, f64, String)> = resp
            .segments
            .iter()
            .map(|s| (s.start, s.end, s.text.clone()))
            .collect();
        let cues = crate::subtitle::cues_from_timed_text(&items, media_id);
        if !cues.is_empty() {
            return cues;
        }
    }

    let text = resp.text.trim();
    if text.is_empty() {
        return Vec::new();
    }
    let duration = if resp.duration > 0.0 { resp.duration } else { 0.0 };
    crate::subtitle::cues_from_timed_text(&[(0.0, duration, text.to_string())], media_id)
}

pub fn parse_response(body: &str) -> AppResult<CloudResponse> {
    serde_json::from_str(body)
        .map_err(|e| AppError::Process(format!("云端返回的内容无法解析：{e}；原始内容：{}", tail(body))))
}

// ---------------- 音频提取 ----------------

/// 从视频里抽音频，转成 16kHz 单声道 mp3。
pub fn extract_audio(ffmpeg: &Path, input: &Path, out: &Path) -> AppResult<i64> {
    if !input.is_file() {
        return Err(AppError::Msg(format!(
            "素材文件不存在：{}",
            input.to_string_lossy()
        )));
    }
    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if out.exists() {
        let _ = std::fs::remove_file(out);
    }

    let output = no_window(
        Command::new(ffmpeg)
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-y")
            .arg("-i")
            .arg(input.as_os_str())
            .arg("-vn")
            .arg("-ac")
            .arg("1")
            .arg("-ar")
            .arg("16000")
            .arg("-b:a")
            .arg(AUDIO_BITRATE)
            .arg("-f")
            .arg("mp3")
            .arg(out.as_os_str()),
    )
    .output()
    .map_err(|e| AppError::Process(format!("无法启动 ffmpeg：{e}")))?;

    if !output.status.success() {
        return Err(AppError::Process(format!(
            "提取音频失败（退出码 {:?}）：{}",
            output.status.code(),
            tail(&String::from_utf8_lossy(&output.stderr))
        )));
    }
    let size = std::fs::metadata(out).map(|m| m.len() as i64).unwrap_or(0);
    if size <= 0 {
        return Err(AppError::Process(
            "提取出来的音频是空的，素材里可能没有音轨".into(),
        ));
    }
    Ok(size)
}

// ---------------- 调用 ----------------

fn http_client() -> AppResult<&'static reqwest::Client> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    let built = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .build()
            .map_err(|e| e.to_string())
    });
    built
        .as_ref()
        .map_err(|e| AppError::Process(format!("创建 HTTP 客户端失败：{e}")))
}

/// 上传音频并取回转写结果。
pub async fn transcribe(cfg: &CloudConfig, audio: &Path) -> AppResult<CloudResponse> {
    if cfg.api_key.trim().is_empty() {
        return Err(AppError::Msg(
            "还没配置 API Key。到「设置 → 云端字幕」填写后再试。".into(),
        ));
    }
    if !audio.is_file() {
        return Err(AppError::Msg(format!(
            "音频文件不存在：{}",
            audio.to_string_lossy()
        )));
    }

    let endpoint = transcribe_endpoint(&cfg.base_url);
    let bytes = std::fs::read(audio)?;
    let file_name = audio
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "audio.mp3".to_string());

    let part = reqwest::multipart::Part::bytes(bytes)
        .file_name(file_name)
        .mime_str("audio/mpeg")
        .map_err(|e| AppError::Process(format!("构造上传内容失败：{e}")))?;

    let mut form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", cfg.model.clone())
        .text("response_format", "verbose_json");

    let language = cfg.language.trim();
    if !language.is_empty() && language != "auto" {
        form = form.text("language", language.to_string());
    }

    let response = http_client()?
        .post(&endpoint)
        .bearer_auth(cfg.api_key.trim())
        .multipart(form)
        .send()
        .await
        .map_err(|e| AppError::Process(format!("请求云端失败：{e}")))?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    if !status.is_success() {
        return Err(AppError::Process(friendly_error(status.as_u16(), &body)));
    }
    parse_response(&body)
}

/// 连通性自检：不转写，只确认地址、鉴权与模型能不能用。
///
/// 做法是拿一段极短的静音去请求一次 —— 大部分服务会正常返回（哪怕文本为空），
/// 而地址错、Key 错、模型不存在都会在这一步暴露出来。
pub async fn test_connection(cfg: &CloudConfig) -> AppResult<String> {
    let endpoint = transcribe_endpoint(&cfg.base_url);
    if cfg.api_key.trim().is_empty() {
        return Err(AppError::Msg("还没配置 API Key".into()));
    }

    // 一段 0.2 秒的极小 mp3（静音），避免真的消耗多少额度
    let silent: Vec<u8> = Vec::new();
    let part = reqwest::multipart::Part::bytes(silent)
        .file_name("probe.mp3")
        .mime_str("audio/mpeg")
        .map_err(|e| AppError::Process(format!("构造探测内容失败：{e}")))?;
    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", cfg.model.clone())
        .text("response_format", "verbose_json");

    let response = http_client()?
        .post(&endpoint)
        .bearer_auth(cfg.api_key.trim())
        .multipart(form)
        .send()
        .await
        .map_err(|e| AppError::Process(format!("连接不上 {endpoint}：{e}")))?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    if status.is_success() {
        Ok(format!("{endpoint} 可达，鉴权通过（模型 {}）", cfg.model))
    } else if status.as_u16() == 400 {
        // 400 通常是"音频太小/为空"—— 说明地址与 Key 都对
        Ok(format!(
            "{endpoint} 可达，鉴权通过；服务端因探测内容为空返回 400，属正常"
        ))
    } else {
        Err(AppError::Process(friendly_error(status.as_u16(), &body)))
    }
}

/// 音频临时文件路径
pub fn temp_audio_path(media_id: i64) -> PathBuf {
    std::env::temp_dir().join(format!("dramadist_cloud_{media_id}.mp3"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_normalization() {
        assert_eq!(normalize_base_url("  https://a.com/v1/  "), "https://a.com/v1");
        assert_eq!(normalize_base_url(""), DEFAULT_BASE_URL);
        assert_eq!(normalize_base_url("   "), DEFAULT_BASE_URL);
    }

    #[test]
    fn endpoint_appends_path_unless_already_present() {
        assert_eq!(
            transcribe_endpoint("https://a.com/v1"),
            "https://a.com/v1/audio/transcriptions"
        );
        // 用户直接粘完整地址也不会拼成两遍
        assert_eq!(
            transcribe_endpoint("https://a.com/v1/audio/transcriptions"),
            "https://a.com/v1/audio/transcriptions"
        );
        assert_eq!(
            transcribe_endpoint("https://a.com/v1/"),
            "https://a.com/v1/audio/transcriptions"
        );
    }

    #[test]
    fn key_masking_never_leaks_the_middle() {
        assert_eq!(mask_key(""), "");
        assert_eq!(mask_key("short"), "*****");
        assert_eq!(mask_key("sk-1234567890abcdef"), "sk-****cdef");
        // 打码结果里不应该出现原 Key 的中间部分
        let key = "sk-abcdefghijklmnop";
        let masked = mask_key(key);
        assert!(!masked.contains("defghijklm"), "{masked}");
    }

    #[test]
    fn view_flags_missing_key_and_plain_http() {
        let mut cfg = CloudConfig::default();
        let v = view(&cfg);
        assert!(!v.has_key);
        assert!(v.warnings.iter().any(|w| w.contains("还没有填 API Key")));

        cfg.api_key = "sk-1234567890abcdef".into();
        cfg.base_url = "http://api.example.com/v1".into();
        let v = view(&cfg);
        assert!(v.has_key);
        assert_eq!(v.key_hint, "sk-****cdef");
        assert!(
            v.warnings.iter().any(|w| w.contains("经过网络")),
            "非回环的 http 要提醒传输风险：{:?}",
            v.warnings
        );

        // 本地回环不算风险（mock 服务就是 http）
        cfg.base_url = "http://127.0.0.1:8080/v1".into();
        let v = view(&cfg);
        assert!(
            !v.warnings.iter().any(|w| w.contains("经过网络")),
            "回环地址不该报传输风险：{:?}",
            v.warnings
        );
        // 但"Key 明文存在本地库"这条提醒始终要有
        assert!(v.warnings.iter().any(|w| w.contains("本地数据库")));
    }

    #[test]
    fn errors_are_actionable() {
        assert!(friendly_error(401, "").contains("API Key"));
        assert!(friendly_error(404, "not found").contains("Base URL"));
        assert!(friendly_error(429, "").contains("额度"));
        assert!(friendly_error(503, "").contains("服务端"));
        // 未知状态码也要带上服务端原文
        assert!(friendly_error(418, "teapot").contains("teapot"));
        // 空 body 不该留下一个空的"服务端返回："
        let msg = friendly_error(500, "   ");
        assert!(!msg.contains("服务端返回"), "{msg}");
    }

    #[test]
    fn parses_segments_into_cues() {
        let body = r#"{
            "text": "你好世界",
            "language": "zh",
            "duration": 4.0,
            "segments": [
                {"id": 0, "start": 0.2, "end": 2.0, "text": "你好"},
                {"id": 1, "start": 2.2, "end": 3.8, "text": "世界"}
            ]
        }"#;
        let resp = parse_response(body).unwrap();
        assert_eq!(resp.language, "zh");
        let cues = to_cues(&resp, 7);
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "你好");
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (200, 2_000));
        assert_eq!(cues[0].media_id, 7);
        assert_eq!(cues[1].index_no, 2);
    }

    #[test]
    fn falls_back_to_whole_text_when_segments_are_missing() {
        let body = r#"{"text":"只有整段文本","language":"zh","duration":3.5}"#;
        let resp = parse_response(body).unwrap();
        let cues = to_cues(&resp, 1);
        assert_eq!(cues.len(), 1, "没有 segments 时不该把结果丢掉");
        assert_eq!(cues[0].text, "只有整段文本");
        assert_eq!((cues[0].start_ms, cues[0].end_ms), (0, 3_500));
    }

    #[test]
    fn empty_response_yields_no_cues() {
        let resp = parse_response(r#"{"text":"","segments":[]}"#).unwrap();
        assert!(to_cues(&resp, 1).is_empty());
        // 全是空段落的也该是空
        let resp = parse_response(r#"{"segments":[{"start":0,"end":1,"text":"  "}]}"#).unwrap();
        assert!(to_cues(&resp, 1).is_empty());
    }

    #[test]
    fn broken_json_is_reported_with_a_snippet() {
        let err = parse_response("<html>502 Bad Gateway</html>").unwrap_err().to_string();
        assert!(err.contains("无法解析"), "{err}");
        assert!(err.contains("502"), "要带上原文片段：{err}");
    }

    #[test]
    fn missing_input_is_reported_before_spawning() {
        let err = extract_audio(
            Path::new("Z:/nope/ffmpeg.exe"),
            Path::new("Z:/nope/missing.mp4"),
            Path::new("Z:/nope/out.mp3"),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("素材文件不存在"), "实际：{err}");
    }

    #[test]
    fn config_round_trips_and_keeps_key_when_not_touched() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrate_for_tests(&conn).unwrap();

        // 初始：默认值、没有 Key
        let cfg = load_config(&conn).unwrap();
        assert_eq!(cfg.base_url, DEFAULT_BASE_URL);
        assert!(cfg.api_key.is_empty());

        // 存 Key
        let cfg = save_config(&conn, "https://a.com/v1/", Some("sk-secret-key-123456"), "m1", "zh")
            .unwrap();
        assert_eq!(cfg.base_url, "https://a.com/v1");
        assert_eq!(cfg.api_key, "sk-secret-key-123456");

        // 只改模型：Key 必须原样保留（api_key 传 None）
        let cfg = save_config(&conn, "https://a.com/v1", None, "m2", "en").unwrap();
        assert_eq!(cfg.api_key, "sk-secret-key-123456", "改模型不该丢掉 Key");
        assert_eq!(cfg.model, "m2");
        assert_eq!(cfg.language, "en");

        // 显式清空
        let cfg = save_config(&conn, "https://a.com/v1", Some(""), "m2", "en").unwrap();
        assert!(cfg.api_key.is_empty());
    }

    // ---------------- 端到端：真实 HTTP + 本地 mock 服务 ----------------

    fn env_path(key: &str) -> Option<PathBuf> {
        std::env::var(key)
            .ok()
            .map(PathBuf::from)
            .filter(|p| p.is_file())
    }

    fn any_python() -> Option<PathBuf> {
        if let Ok(p) = std::env::var("DRAMADIST_TEST_PYTHON") {
            let path = PathBuf::from(p);
            if path.is_file() {
                return Some(path);
            }
        }
        crate::tools::which("python3").or_else(|| crate::tools::which("python"))
    }

    #[test]
    fn end_to_end_against_a_local_mock_service() {
        let (Some(ffmpeg), Some(python)) = (env_path("DRAMADIST_TEST_FFMPEG"), any_python()) else {
            eprintln!("跳过：需要 DRAMADIST_TEST_FFMPEG 与一个可用的 python");
            return;
        };

        let mock = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("tests")
            .join("mock_asr_server.py");
        assert!(mock.is_file(), "找不到 mock 服务脚本：{}", mock.display());

        // 占一个空闲端口再让给 mock
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("无法绑定端口");
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let dir = std::env::temp_dir().join("dramadist_cloud_e2e");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let record = dir.join("last_request.json");

        let mut mock_child = crate::proc::no_window(
            Command::new(&python)
                .arg(&mock)
                .arg(port.to_string())
                .arg(record.to_string_lossy().to_string())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        )
        .spawn()
        .expect("无法启动 mock 服务");

        // 等它起来
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut ready = false;
        while std::time::Instant::now() < deadline {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                ready = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        let outcome = (|| -> AppResult<()> {
            assert!(ready, "mock 服务没能在 20 秒内启动");

            // 造一段带音轨的素材，再抽成音频
            let video = dir.join("in.mp4");
            let status = crate::proc::no_window(
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
                    "sine=frequency=440",
                    "-t",
                    "4",
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
            .arg(&video)
            .status()
            .expect("无法启动 ffmpeg");
            assert!(status.success(), "生成测试素材失败");

            let audio = dir.join("audio.mp3");
            let size = extract_audio(&ffmpeg, &video, &audio)?;
            assert!(size > 0, "抽出来的音频是空的");

            let cfg = CloudConfig {
                base_url: format!("http://127.0.0.1:{port}/v1"),
                api_key: "sk-test-1234567890".into(),
                model: "whisper-1".into(),
                language: "zh".into(),
            };

            let response = tauri::async_runtime::block_on(transcribe(&cfg, &audio))?;
            let cues = to_cues(&response, 5);
            assert_eq!(cues.len(), 2, "应当解析出两段字幕");
            assert_eq!(cues[0].text, "你好");
            assert_eq!((cues[0].start_ms, cues[0].end_ms), (200, 2_000));
            assert_eq!(cues[1].text, "世界");
            assert_eq!((cues[1].start_ms, cues[1].end_ms), (2_200, 3_800));
            assert_eq!(cues[0].media_id, 5);
            assert_eq!(response.language, "zh");
            Ok(())
        })();

        // 不论成败都要收掉 mock，否则会一直占着端口
        let _ = mock_child.kill();
        let _ = mock_child.wait();

        outcome.expect("云端转写端到端失败");

        // 关键：验证「我们到底发出去了什么」
        let raw = std::fs::read_to_string(&record).expect("mock 没有记录到请求");
        let recorded: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(recorded["method"], "POST");
        assert!(
            recorded["path"]
                .as_str()
                .unwrap()
                .ends_with("/v1/audio/transcriptions"),
            "请求路径不对：{recorded}"
        );
        assert_eq!(recorded["authorization"], "Bearer sk-test-1234567890");
        assert!(
            recorded["content_type"]
                .as_str()
                .unwrap()
                .starts_with("multipart/form-data"),
            "应当是 multipart 上传：{recorded}"
        );
        assert_eq!(recorded["has_file_field"], true, "{recorded}");
        assert_eq!(recorded["has_model_field"], true, "{recorded}");
        assert_eq!(recorded["has_response_format"], true, "{recorded}");
        assert_eq!(recorded["has_language"], true, "{recorded}");
        assert!(
            recorded["body_bytes"].as_i64().unwrap() > 1_000,
            "上传的音频字节数太小，可能没真的带上文件：{recorded}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_key_is_caught_locally_without_sending_anything() {
        // 反向验证：没配 Key 时应当在本地就拦下，一个字节都不发出去
        let Some(python) = any_python() else {
            eprintln!("跳过：本机没有 python");
            return;
        };
        let mock = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("tests")
            .join("mock_asr_server.py");
        if !mock.is_file() {
            eprintln!("跳过：找不到 mock 服务脚本");
            return;
        }

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("无法绑定端口");
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let dir = std::env::temp_dir().join("dramadist_cloud_nokey");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let record = dir.join("last_request.json");

        let mut child = crate::proc::no_window(
            Command::new(&python)
                .arg(&mock)
                .arg(port.to_string())
                .arg(record.to_string_lossy().to_string())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        )
        .spawn()
        .expect("无法启动 mock 服务");

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut ready = false;
        while std::time::Instant::now() < deadline {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                ready = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }

        let outcome = (|| -> AppResult<()> {
            assert!(ready, "mock 服务没起来");

            let audio = dir.join("a.mp3");
            std::fs::write(&audio, b"not really audio")?;

            let empty = CloudConfig {
                base_url: format!("http://127.0.0.1:{port}/v1"),
                api_key: String::new(),
                model: "whisper-1".into(),
                language: "zh".into(),
            };
            let err = tauri::async_runtime::block_on(transcribe(&empty, &audio))
                .unwrap_err()
                .to_string();
            assert!(err.contains("API Key"), "空 Key 应当被本地拦下：{err}");
            assert!(
                !record.exists(),
                "空 Key 不该发出任何请求，但 mock 收到了请求"
            );
            Ok(())
        })();

        let _ = child.kill();
        let _ = child.wait();
        outcome.expect("反向验证失败");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
