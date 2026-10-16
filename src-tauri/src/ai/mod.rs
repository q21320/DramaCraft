//! AI 服务配置：一页管住所有外部服务（Key、Base URL、模型、用途开关）。
//!
//! ## 为什么单独一层
//!
//! 原软件把「火山 API」做成独立一页：连接配置（Key / Base URL / 语言模型 /
//! 视觉生图）+ 用途开关（封面生成 / 工程图 AI 辅助）。这是对的 ——
//! 把 Key 散在各个功能页里，用户根本不知道自己配了哪些、哪些在用。
//!
//! ## 两条硬规矩
//!
//! 1. **API Key 永远不发给前端**。前端只拿到 `keyHint`（打码后的样子），
//!    保存时留空表示"不改动"。这样 Key 不会出现在界面、日志、截图里。
//! 2. **用不用 AI 由用户显式点头**。配了 Key 不等于要用 ——
//!    会产生费用，产出还属于 AI 生成内容（有标识义务）。
//!    所以 [`AiUsage`] 是显式开关，默认全关。

use crate::error::{AppError, AppResult};
use crate::models::{AiConfigView, AiProvider, AiProviderView, AiTestResult, AiUsage};
use rusqlite::{params, Connection};
use std::time::{Duration, Instant};

pub const SETTING_PROVIDERS: &str = "ai.providers";
pub const SETTING_ACTIVE: &str = "ai.active";
pub const SETTING_USAGE: &str = "ai.usage";

/// 内置服务商预设。用户换个 Key 就能用，不用去翻文档抄 Base URL。
pub const PRESETS: [(&str, &str, &str, &str, &str); 3] = [
    (
        "ark",
        "火山方舟（豆包 Seedream）",
        "https://ark.cn-beijing.volces.com/api/v3",
        "doubao-seed-2-0-mini-260428",
        "doubao-seedream-4-0-250828",
    ),
    (
        "dashscope",
        "阿里百炼（通义万相）",
        "https://dashscope.aliyuncs.com/api/v1",
        "qwen-plus",
        "wan2.2-t2i-flash",
    ),
    // 自定义留空，用户自己填
    ("custom", "自定义（OpenAI 兼容）", "", "", ""),
];

fn preset(id: &str) -> Option<(&'static str, &'static str, &'static str, &'static str, &'static str)> {
    PRESETS.iter().find(|&&(pid, ..)| pid == id).copied()
}

/// 全部预设，按内置顺序。
pub fn default_providers() -> Vec<AiProvider> {
    PRESETS
        .iter()
        .map(|&(id, label, base, text, image)| AiProvider {
            id: id.to_string(),
            label: label.to_string(),
            enabled: false,
            base_url: base.to_string(),
            text_model: text.to_string(),
            image_model: image.to_string(),
            api_key: String::new(),
        })
        .collect()
}

fn get_setting(conn: &Connection, key: &str) -> AppResult<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM setting WHERE key = ?1")?;
    let mut rows = stmt.query(params![key])?;
    match rows.next()? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

fn put_setting(conn: &Connection, key: &str, value: &str) -> AppResult<()> {
    conn.execute(
        "INSERT INTO setting(key, value) VALUES(?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// 读配置。设置里没有的就用预设补齐 ——
/// 这样以后加服务商，老用户升上来也能看见。
pub fn load_providers(conn: &Connection) -> AppResult<Vec<AiProvider>> {
    let mut providers = default_providers();
    if let Some(raw) = get_setting(conn, SETTING_PROVIDERS)? {
        if let Ok(saved) = serde_json::from_str::<Vec<AiProvider>>(&raw) {
            for s in saved {
                match providers.iter_mut().find(|p| p.id == s.id) {
                    // 保留用户填的 Key 与改动，但**认得的服务商仍用预设名**
                    Some(slot) => {
                        slot.enabled = s.enabled;
                        slot.api_key = s.api_key;
                        if !s.base_url.trim().is_empty() {
                            slot.base_url = s.base_url;
                        }
                        if !s.text_model.trim().is_empty() {
                            slot.text_model = s.text_model;
                        }
                        if !s.image_model.trim().is_empty() {
                            slot.image_model = s.image_model;
                        }
                    }
                    None => providers.push(s),
                }
            }
        }
    }
    Ok(providers)
}

pub fn save_providers(conn: &Connection, providers: &[AiProvider]) -> AppResult<()> {
    put_setting(conn, SETTING_PROVIDERS, &serde_json::to_string(providers)?)
}

pub fn load_usage(conn: &Connection) -> AppResult<AiUsage> {
    match get_setting(conn, SETTING_USAGE)? {
        Some(raw) => Ok(serde_json::from_str(&raw).unwrap_or_default()),
        None => Ok(AiUsage::default()),
    }
}

pub fn save_usage(conn: &Connection, usage: &AiUsage) -> AppResult<()> {
    put_setting(conn, SETTING_USAGE, &serde_json::to_string(usage)?)
}

pub fn load_active(conn: &Connection) -> AppResult<String> {
    Ok(get_setting(conn, SETTING_ACTIVE)?.unwrap_or_default())
}

pub fn save_active(conn: &Connection, id: &str) -> AppResult<()> {
    put_setting(conn, SETTING_ACTIVE, id)
}

/// 打码后的 Key。前端只能看到这个。
pub fn mask_key(key: &str) -> String {
    let k = key.trim();
    if k.is_empty() {
        return String::new();
    }
    let n = k.chars().count();
    if n <= 8 {
        return "****".to_string();
    }
    let head: String = k.chars().take(4).collect();
    let tail: String = k.chars().skip(n - 4).collect();
    format!("{head}****{tail}")
}

/// 归一化 Base URL：去尾斜杠，补协议。
pub fn normalize_base_url(raw: &str) -> String {
    let t = raw.trim().trim_end_matches('/').to_string();
    if t.is_empty() {
        return String::new();
    }
    if t.starts_with("http://") || t.starts_with("https://") {
        t
    } else {
        format!("https://{t}")
    }
}

fn to_view(p: &AiProvider) -> AiProviderView {
    AiProviderView {
        id: p.id.clone(),
        label: p.label.clone(),
        enabled: p.enabled,
        base_url: p.base_url.clone(),
        text_model: p.text_model.clone(),
        image_model: p.image_model.clone(),
        has_key: !p.api_key.trim().is_empty(),
        key_hint: mask_key(&p.api_key),
    }
}

/// 组装给界面看的配置，并顺手挑出值得提醒的地方。
pub fn view(conn: &Connection) -> AppResult<AiConfigView> {
    let providers = load_providers(conn)?;
    let usage = load_usage(conn)?;
    let active = load_active(conn)?;

    let mut warnings = Vec::new();
    let active_provider = providers.iter().find(|p| p.id == active);

    if active.is_empty() {
        if usage.cover_generation || usage.title_rewrite {
            warnings.push(
                "开了 AI 用途但没选服务商 —— 相关功能会退回本地处理，或直接跳过".into(),
            );
        }
    } else if let Some(p) = active_provider {
        if !p.enabled {
            warnings.push(format!("当前服务商「{}」没有启用", p.label));
        }
        if p.api_key.trim().is_empty() {
            warnings.push(format!("「{}」还没填 API Key", p.label));
        }
        if normalize_base_url(&p.base_url).is_empty() {
            warnings.push(format!("「{}」还没填 Base URL", p.label));
        }
        if usage.cover_generation && p.image_model.trim().is_empty() {
            warnings.push(format!("「{}」没填生图模型，封面生成用不了", p.label));
        }
        if usage.title_rewrite && p.text_model.trim().is_empty() {
            warnings.push(format!("「{}」没填语言模型，剧名简介改写用不了", p.label));
        }
    }

    Ok(AiConfigView {
        providers: providers.iter().map(to_view).collect(),
        active,
        usage,
        warnings,
    })
}

/// 保存界面提交的配置。
///
/// `api_key` 为空 = **不改动**原来的 Key。这样界面上显示的是打码值，
/// 用户没动它就不会把真 Key 覆盖成一串星号。
pub fn apply_view(
    conn: &Connection,
    providers: &[AiProvider],
    active: &str,
    usage: &AiUsage,
) -> AppResult<()> {
    let mut existing = load_providers(conn)?;
    for incoming in providers {
        let base = existing.iter_mut().find(|p| p.id == incoming.id);
        match base {
            Some(slot) => {
                slot.enabled = incoming.enabled;
                slot.base_url = normalize_base_url(&incoming.base_url);
                slot.text_model = incoming.text_model.trim().to_string();
                slot.image_model = incoming.image_model.trim().to_string();
                let key = incoming.api_key.trim();
                if !key.is_empty() {
                    // 前端可能把打码值原样提交回来，别把真 Key 覆盖掉
                    if !key.contains("****") {
                        slot.api_key = key.to_string();
                    }
                }
            }
            None => {
                let mut p = incoming.clone();
                p.base_url = normalize_base_url(&p.base_url);
                if p.api_key.contains("****") {
                    p.api_key = String::new();
                }
                existing.push(p);
            }
        }
    }
    save_providers(conn, &existing)?;
    save_active(conn, active.trim())?;
    save_usage(conn, usage)?;
    Ok(())
}

/// 当前真正可用的服务商（启用了、有 Key、有 Base URL）。
pub fn usable_provider(conn: &Connection) -> AppResult<Option<AiProvider>> {
    let active = load_active(conn)?;
    if active.is_empty() {
        return Ok(None);
    }
    let providers = load_providers(conn)?;
    Ok(providers.into_iter().find(|p| {
        p.id == active
            && p.enabled
            && !p.api_key.trim().is_empty()
            && !normalize_base_url(&p.base_url).is_empty()
    }))
}

// ---------------- 连通性测试 ----------------

/// 各服务商的"列模型"端点，用来低成本验证 Key 是否有效。
///
/// 刻意不去调真正的生图接口 —— 那会产生费用。列模型接口免费且能验证
/// 「Key 对不对、网络通不通、Base URL 填错没有」这三件事。
pub fn probe_endpoint(provider_id: &str, base_url: &str) -> String {
    let base = normalize_base_url(base_url);
    match provider_id {
        // 火山方舟兼容 OpenAI 的 /models
        "ark" => format!("{base}/models"),
        // DashScope 没有统一的 /models，用 tasks 列表探（只要不是 401 就说明 Key 有效）
        "dashscope" => format!("{base}/tasks"),
        _ => format!("{base}/models"),
    }
}

/// 配置不全时的结论 —— **纯本地判断，不发任何请求**。
///
/// 拆出来单独一个函数是为了能测：网络那部分测不了（也不该在测试里联网），
/// 但"没填 Key 就该直接告诉用户"这条逻辑必须测到。
pub fn precheck(provider: &AiProvider) -> Option<AiTestResult> {
    let endpoint = probe_endpoint(&provider.id, &provider.base_url);
    let fail = |message: &str| {
        Some(AiTestResult {
            ok: false,
            provider: provider.id.clone(),
            endpoint: endpoint.clone(),
            elapsed_ms: 0,
            message: message.to_string(),
        })
    };
    if provider.api_key.trim().is_empty() {
        return fail("还没填 API Key");
    }
    if normalize_base_url(&provider.base_url).is_empty() {
        return fail("还没填 Base URL");
    }
    None
}

/// 打一次探测请求。**只列资源，不触发任何生成，不产生费用。**
pub async fn test_provider(provider: &AiProvider) -> AiTestResult {
    if let Some(early) = precheck(provider) {
        return early;
    }
    let endpoint = probe_endpoint(&provider.id, &provider.base_url);
    let started = Instant::now();

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return AiTestResult {
                ok: false,
                provider: provider.id.clone(),
                endpoint,
                elapsed_ms: 0,
                message: format!("建不了 HTTP 客户端：{e}"),
            }
        }
    };

    let resp = client
        .get(&endpoint)
        .header(
            "Authorization",
            format!("Bearer {}", provider.api_key.trim()),
        )
        .send()
        .await;

    let elapsed_ms = started.elapsed().as_millis() as i64;
    match resp {
        Ok(r) => {
            let status = r.status().as_u16();
            if r.status().is_success() {
                AiTestResult {
                    ok: true,
                    provider: provider.id.clone(),
                    endpoint,
                    elapsed_ms,
                    message: format!("连接正常（HTTP {status}）"),
                }
            } else {
                let body = r.text().await.unwrap_or_default();
                let message = match status {
                    401 | 403 => "Key 无效或没有权限（401/403）".to_string(),
                    404 => "Base URL 可能不对（404）—— 检查有没有多写或少写 /v1".to_string(),
                    _ => format!("服务返回 HTTP {status}：{}", crate::proc::tail(body.trim())),
                };
                AiTestResult {
                    ok: false,
                    provider: provider.id.clone(),
                    endpoint,
                    elapsed_ms,
                    message,
                }
            }
        }
        Err(e) => AiTestResult {
            ok: false,
            provider: provider.id.clone(),
            endpoint,
            elapsed_ms,
            message: format!("连不上：{e}"),
        },
    }
}

// ---------------- 封面生成（图生图） ----------------

/// 封面提示词。
///
/// 说清楚要**擦掉旧字**、**写上剧名**、**风格别变** ——
/// 这三件事缺一样，出来的封面就不能用。
pub fn cover_prompt(title: &str) -> String {
    format!(
        "保持原图的人物、构图、色调与摄影风格完全不变，只做两件事：\
         一、彻底擦除画面上原有的全部文字与标题，背景按周围画面自然补全；\
         二、在画面顶部居中位置，用与原图风格协调的字体写上剧名「{title}」。\
         要求：中文标题清晰、完整、无错别字，不添加任何其它文字、水印或logo。"
    )
}

/// 把本地图片读成 data URL（图生图要把原封面一起发过去）。
pub fn image_data_url(path: &std::path::Path) -> AppResult<String> {
    let bytes = std::fs::read(path)
        .map_err(|e| AppError::Msg(format!("读不了封面图：{e}")))?;
    if bytes.len() > 10 * 1024 * 1024 {
        return Err(AppError::Msg("封面图超过 10 MB，先压小一点".into()));
    }
    let mime = match path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
        .as_str()
    {
        "png" => "image/png",
        "webp" => "image/webp",
        _ => "image/jpeg",
    };
    Ok(format!("data:{mime};base64,{}", base64_encode(&bytes)))
}

/// 极简 base64 编码（标准字母表 + 补位）。
///
/// 不引依赖：只为把图片塞进 JSON，几十行的事。
fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// 从各家返回体里把图片 URL 挖出来。
///
/// 两家的结构不一样，但都逃不出「results / data 数组里有个 url」这个形状。
pub fn extract_image_url(body: &str) -> AppResult<String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| AppError::Msg(format!("返回不是合法 JSON：{e}")))?;

    // 先看有没有报错信息，别把它当成"没找到 URL"
    if let Some(err) = v.get("error").and_then(|e| {
        e.get("message")
            .and_then(|m| m.as_str())
            .or_else(|| e.as_str())
    }) {
        return Err(AppError::Msg(format!("服务返回错误：{err}")));
    }
    if let Some(msg) = v.get("message").and_then(|m| m.as_str()) {
        if v.get("code").is_some() {
            return Err(AppError::Msg(format!("服务返回错误：{msg}")));
        }
    }

    let candidates = [
        ("/data/0/url", &v),
        ("/output/results/0/url", &v),
        ("/output/0/url", &v),
        ("/result/0/url", &v),
    ];
    for (_, root) in candidates {
        if let Some(url) = root
            .pointer("/data/0/url")
            .or_else(|| root.pointer("/output/results/0/url"))
            .or_else(|| root.pointer("/output/0/url"))
            .or_else(|| root.pointer("/result/0/url"))
            .and_then(|u| u.as_str())
        {
            return Ok(url.to_string());
        }
    }
    Err(AppError::Msg(format!(
        "返回里找不到图片地址：{}",
        crate::proc::tail(body.trim())
    )))
}

/// DashScope 的异步任务状态。
pub fn dashscope_task_id(body: &str) -> AppResult<String> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| AppError::Msg(format!("返回不是合法 JSON：{e}")))?;
    v.pointer("/output/task_id")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            AppError::Msg(format!(
                "没拿到任务号：{}",
                crate::proc::tail(body.trim())
            ))
        })
}

/// 任务是不是还在跑。返回 `None` 表示已经结束（成功或失败，看 [`extract_image_url`]）。
pub fn dashscope_pending(body: &str) -> Option<bool> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let status = v.pointer("/output/task_status")?.as_str()?;
    match status {
        "PENDING" | "RUNNING" => Some(true),
        "SUCCEEDED" => Some(false),
        "FAILED" | "CANCELED" | "UNKNOWN" => Some(false),
        _ => Some(true),
    }
}

/// 生成封面图（图生图），返回图片字节。
///
/// 两家协议不一样：
/// - **火山方舟**：同步，一次请求直接返回图片 URL
/// - **DashScope**：异步，先拿 task_id，再轮询
///
/// 统一在这里抹平，上层只管拿到图。
pub async fn generate_cover_image(
    provider: &AiProvider,
    poster: &std::path::Path,
    title: &str,
    width: i64,
    height: i64,
) -> AppResult<Vec<u8>> {
    if let Some(early) = precheck(provider) {
        return Err(AppError::Msg(early.message));
    }
    if provider.image_model.trim().is_empty() {
        return Err(AppError::Msg(format!(
            "「{}」没填生图模型，封面生成用不了",
            provider.label
        )));
    }

    let base = normalize_base_url(&provider.base_url);
    let data_url = image_data_url(poster)?;
    let prompt = cover_prompt(title);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| AppError::Msg(format!("建不了 HTTP 客户端：{e}")))?;

    let url = if provider.id == "dashscope" {
        // 异步：提交 → 轮询
        let endpoint = format!("{base}/services/aigc/text2image/image-synthesis");
        let body = serde_json::json!({
            "model": provider.image_model.trim(),
            "input": { "prompt": prompt, "ref_img": data_url },
            "parameters": { "size": format!("{width}*{height}"), "n": 1 },
        });
        let resp = client
            .post(&endpoint)
            .header("Authorization", format!("Bearer {}", provider.api_key.trim()))
            .header("X-DashScope-Async", "enable")
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::Msg(format!("提交生图任务失败：{e}")))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(AppError::Msg(friendly_http(status, &text)));
        }
        let task_id = dashscope_task_id(&text)?;

        // 轮询：最多等 3 分钟
        let task_url = format!("{base}/tasks/{task_id}");
        let mut last = String::new();
        for _ in 0..60 {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let r = client
                .get(&task_url)
                .header("Authorization", format!("Bearer {}", provider.api_key.trim()))
                .send()
                .await
                .map_err(|e| AppError::Msg(format!("查任务状态失败：{e}")))?;
            last = r.text().await.unwrap_or_default();
            match dashscope_pending(&last) {
                Some(true) => continue,
                _ => break,
            }
        }
        extract_image_url(&last)?
    } else {
        // 火山方舟：同步返回
        let endpoint = format!("{base}/images/generations");
        let body = serde_json::json!({
            "model": provider.image_model.trim(),
            "prompt": prompt,
            "image": data_url,
            "size": format!("{width}x{height}"),
            "response_format": "url",
            // 关掉服务商自己加的水印，否则封面会多出一个"AI生成"角标
            "watermark": false,
        });
        let resp = client
            .post(&endpoint)
            .header("Authorization", format!("Bearer {}", provider.api_key.trim()))
            .json(&body)
            .send()
            .await
            .map_err(|e| AppError::Msg(format!("生图请求失败：{e}")))?;
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(AppError::Msg(friendly_http(status, &text)));
        }
        extract_image_url(&text)?
    };

    // 下载图片
    let img = client
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::Msg(format!("下载生成的封面失败：{e}")))?;
    if !img.status().is_success() {
        return Err(AppError::Msg(format!(
            "下载封面失败：HTTP {}",
            img.status().as_u16()
        )));
    }
    let bytes = img
        .bytes()
        .await
        .map_err(|e| AppError::Msg(format!("读封面数据失败：{e}")))?;
    if bytes.is_empty() {
        return Err(AppError::Msg("下载到的封面是空的".into()));
    }
    Ok(bytes.to_vec())
}

fn friendly_http(status: u16, body: &str) -> String {
    match status {
        401 | 403 => "Key 无效或没有权限（401/403）".to_string(),
        404 => "接口地址不对（404）—— 检查 Base URL 有没有多写或少写 /v1".to_string(),
        429 => "请求太频繁或额度用尽（429）".to_string(),
        _ => format!("HTTP {status}：{}", crate::proc::tail(body.trim())),
    }
}

/// 给某个服务商补上预设里缺失的字段（用户挑预设时用）。
pub fn apply_preset(provider: &mut AiProvider) {
    if let Some((_, label, base, text, image)) = preset(&provider.id) {
        provider.label = label.to_string();
        if provider.base_url.trim().is_empty() {
            provider.base_url = base.to_string();
        }
        if provider.text_model.trim().is_empty() {
            provider.text_model = text.to_string();
        }
        if provider.image_model.trim().is_empty() {
            provider.image_model = image.to_string();
        }
    }
}

/// 校验一份提交上来的配置，问题一次性列全。
pub fn validate(providers: &[AiProvider], active: &str) -> AppResult<()> {
    for p in providers {
        if p.id.trim().is_empty() {
            return Err(AppError::Msg("服务商缺少 id".into()));
        }
        if p.base_url.len() > 300 || p.api_key.len() > 300 {
            return Err(AppError::Msg(format!("「{}」的字段过长", p.label)));
        }
    }
    if !active.is_empty() && !providers.iter().any(|p| p.id == active) {
        return Err(AppError::Msg(format!("选中的服务商不存在：{active}")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        db::migrate_for_tests(&c).unwrap();
        c
    }

    #[test]
    fn defaults_cover_the_three_builtin_providers() {
        let ps = default_providers();
        assert_eq!(ps.len(), 3);
        let ark = ps.iter().find(|p| p.id == "ark").unwrap();
        // 预设要照抄原软件里那两个模型名，用户换 Key 就能用
        assert!(ark.base_url.contains("ark.cn-beijing.volces.com"));
        assert!(ark.image_model.contains("seedream"));
        // 默认全部关闭：配了不代表要用
        assert!(ps.iter().all(|p| !p.enabled));
        assert!(ps.iter().all(|p| p.api_key.is_empty()));
    }

    #[test]
    fn usage_defaults_to_all_off() {
        // 用 AI 会产生费用、产出还带标识义务，默认必须全关
        let u = AiUsage::default();
        assert!(!u.cover_generation && !u.title_rewrite && !u.subtitle_cloud);
    }

    #[test]
    fn keys_are_never_exposed_to_the_frontend() {
        let c = conn();
        let mut ps = default_providers();
        ps[0].api_key = "ark-1234567890abcdef".into();
        ps[0].enabled = true;
        save_providers(&c, &ps).unwrap();
        save_active(&c, "ark").unwrap();

        let v = view(&c).unwrap();
        let ark = v.providers.iter().find(|p| p.id == "ark").unwrap();
        assert!(ark.has_key);
        assert_eq!(ark.key_hint, "ark-****cdef");
        // 关键：整份视图里不能出现原 Key
        let json = serde_json::to_string(&v).unwrap();
        assert!(
            !json.contains("1234567890"),
            "真 Key 泄漏到前端视图里了：{json}"
        );
    }

    #[test]
    fn blank_key_means_keep_the_old_one() {
        let c = conn();
        let mut ps = default_providers();
        ps[0].api_key = "ark-realsecret".into();
        save_providers(&c, &ps).unwrap();

        // 模拟界面提交：Key 留空
        let mut incoming = default_providers();
        incoming[0].enabled = true;
        incoming[0].base_url = "https://ark.cn-beijing.volces.com/api/v3".into();
        apply_view(&c, &incoming, "ark", &AiUsage::default()).unwrap();

        let after = load_providers(&c).unwrap();
        assert_eq!(
            after[0].api_key, "ark-realsecret",
            "留空不该把已存的 Key 抹掉"
        );
        assert!(after[0].enabled);
    }

    #[test]
    fn submitting_the_masked_value_does_not_overwrite_the_real_key() {
        let c = conn();
        let mut ps = default_providers();
        ps[0].api_key = "ark-realsecret-value".into();
        save_providers(&c, &ps).unwrap();

        // 界面把打码值原样提交回来（用户没动那个输入框）
        let mut incoming = default_providers();
        incoming[0].api_key = "ark-****alue".into();
        apply_view(&c, &incoming, "ark", &AiUsage::default()).unwrap();

        let after = load_providers(&c).unwrap();
        assert_eq!(after[0].api_key, "ark-realsecret-value");
    }

    #[test]
    fn base_url_gets_normalized() {
        assert_eq!(
            normalize_base_url("  https://a.com/v1/  "),
            "https://a.com/v1"
        );
        assert_eq!(normalize_base_url("a.com/v1"), "https://a.com/v1");
        assert_eq!(normalize_base_url("   "), "");
    }

    #[test]
    fn warnings_point_out_incomplete_setup() {
        let c = conn();
        // 选了服务商、开了生图用途，但没 Key 没模型
        let mut ps = default_providers();
        ps[2].image_model = String::new();
        save_providers(&c, &ps).unwrap();
        save_active(&c, "custom").unwrap();
        save_usage(
            &c,
            &AiUsage {
                cover_generation: true,
                ..Default::default()
            },
        )
        .unwrap();

        let v = view(&c).unwrap();
        assert!(v.warnings.iter().any(|w| w.contains("API Key")), "{:?}", v.warnings);
        assert!(v.warnings.iter().any(|w| w.contains("Base URL")), "{:?}", v.warnings);
        assert!(v.warnings.iter().any(|w| w.contains("生图模型")), "{:?}", v.warnings);
    }

    #[test]
    fn usable_provider_requires_enabled_key_and_url() {
        let c = conn();
        assert!(usable_provider(&c).unwrap().is_none(), "什么都没配时应当为 None");

        let mut ps = default_providers();
        ps[0].enabled = true;
        save_providers(&c, &ps).unwrap();
        save_active(&c, "ark").unwrap();
        assert!(usable_provider(&c).unwrap().is_none(), "没 Key 不算可用");

        ps[0].api_key = "k".into();
        save_providers(&c, &ps).unwrap();
        assert!(usable_provider(&c).unwrap().is_some(), "齐了就该可用");
    }

    #[test]
    fn precheck_rejects_incomplete_config_without_touching_the_network() {
        // 没填 Key / Base URL 时不该发请求，直接给结论。
        // 这条能在测试里跑，正是因为它不联网。
        let mut p = default_providers().remove(2); // custom，Base URL 为空
        let r = precheck(&p).expect("没 Key 就该直接有结论");
        assert!(!r.ok);
        assert!(r.message.contains("API Key"), "{}", r.message);

        p.api_key = "x".into();
        let r = precheck(&p).expect("没 Base URL 就该直接有结论");
        assert!(r.message.contains("Base URL"), "{}", r.message);

        p.base_url = "https://example.com/v1".into();
        assert!(precheck(&p).is_none(), "配置齐了就不该短路，交给网络那步");
    }

    #[test]
    fn validate_rejects_unknown_active_provider() {
        let ps = default_providers();
        let err = validate(&ps, "nope").unwrap_err().to_string();
        assert!(err.contains("不存在"), "{err}");
        assert!(validate(&ps, "ark").is_ok());
        assert!(validate(&ps, "").is_ok(), "不选服务商是合法状态");
    }

    #[test]
    fn apply_preset_fills_only_blank_fields() {
        let mut p = AiProvider {
            id: "ark".into(),
            label: String::new(),
            enabled: false,
            base_url: "https://my-proxy.local/v1".into(),
            text_model: String::new(),
            image_model: String::new(),
            api_key: String::new(),
        };
        apply_preset(&mut p);
        // 用户自己填过的 Base URL 不能被预设覆盖
        assert_eq!(p.base_url, "https://my-proxy.local/v1");
        assert!(p.text_model.contains("doubao"));
        assert_eq!(p.label, "火山方舟（豆包 Seedream）");
    }

    #[test]
    fn probe_endpoints_match_each_provider() {
        assert!(probe_endpoint("ark", "https://ark.cn-beijing.volces.com/api/v3").ends_with("/models"));
        assert!(probe_endpoint("dashscope", "https://dashscope.aliyuncs.com/api/v1").ends_with("/tasks"));
        // 探测端点也要归一化，别拼出双斜杠
        assert!(!probe_endpoint("ark", "https://a.com/v1/").contains("//models"));
    }
}
