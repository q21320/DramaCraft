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
        "doubao-seedream-5-0-flash-260915",
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

/// 改简介 → 按新简介起剧名。
///
/// **这两步的顺序不能反**：反过来的话剧名是根据旧简介起的，两者对不上。
/// 抽成一个函数是因为有两个调用方 —— 「AI 服务」页上的手工试改，
/// 和流水线开始前的那一次（必须在建目录、排任务之前跑完）。
pub async fn rewrite_title_and_synopsis(
    provider: &AiProvider,
    title: &str,
    synopsis: &str,
) -> AppResult<(String, String)> {
    let (sys1, user1) = synopsis_prompt(title, synopsis);
    let new_synopsis = clean_synopsis(&chat(provider, &sys1, &user1).await?);
    if new_synopsis.is_empty() {
        return Err(AppError::Msg("模型返回了空简介".into()));
    }

    let (sys2, user2) = title_prompt(&new_synopsis);
    let new_title = clean_title(&chat(provider, &sys2, &user2).await?);
    if new_title.is_empty() {
        return Err(AppError::Msg("模型返回了空剧名".into()));
    }

    Ok((new_title, new_synopsis))
}

// `describe_frame` 已删 —— 换成 `reverse_shot`。
//
// 旧的只发一张图、只要一句 20 字描述；分镜证明要的是**一整个镜头**的
// 反推：画面/台词/音效/光线/剧本摘要/角色/校验项 + 生成参数，而且
// 要发 5 张图（同一镜头内的连续画面，单张推不出动作走向）。
// 真实软件的日志也写着「附图 5 张」。

/// 一个镜头的反推结果（分镜证明用）。
///
/// 照真实软件那份 JSON 的字段来：画面描述 / 台词 / 音效 / 光线 /
/// 剧本摘要 / 角色 / 校验项，外加生成参数。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ShotReverse {
    /// 中文画面描述（报备时人看得懂）
    pub prompt: String,
    /// 英文画面描述（喂 Seedance 更准）
    pub prompt_en: String,
    pub dialogue: String,
    pub sfx: String,
    pub lighting: String,
    pub script_excerpt: String,
    pub characters: Vec<String>,
    pub checks: Vec<String>,
    pub model: String,
    pub seed: String,
    pub resolution: String,
    pub guidance: String,
    pub duration_sec: String,
    pub fps: String,
}

/// **一镜反推**：把同一个镜头的 5 张关键帧一起发过去，让它反推出分镜脚本。
///
/// 和 `describe_frame` 的区别：那个只输出一句画面描述，这个要出**结构化的一整段**
/// （画面/台词/音效/光线/摘要/角色/校验 + 生成参数），而且要中英双语。
///
/// 5 张图一起发是因为**同一镜头内的画面是连续的** —— 单看一张推不出
/// 动作走向和台词，给 5 张才能说出「从…到…」。真实软件也是「附图 5 张」。
pub async fn reverse_shot(
    provider: &AiProvider,
    frames: &[std::path::PathBuf],
    episode_label: &str,
    time_range: &str,
    duration_sec: f64,
) -> AppResult<ShotReverse> {
    if let Some(early) = precheck(provider) {
        return Err(AppError::Msg(early.message));
    }
    if provider.text_model.trim().is_empty() {
        return Err(AppError::Msg(format!("「{}」没填语言模型", provider.label)));
    }
    if frames.is_empty() {
        return Err(AppError::Msg("这个镜头没有关键帧".into()));
    }

    let endpoint = format!("{}/chat/completions", normalize_base_url(&provider.base_url));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|e| AppError::Msg(format!("建不了 HTTP 客户端：{e}")))?;

    let instruction = format!(
        "这是同一段视频（{episode_label} {time_range}，约 {duration_sec:.1} 秒）按时间顺序抽的 {} 张关键帧。\
         请反推出这个「镜头」的分镜脚本，严格只输出一个 JSON 对象，不要任何解释、不要代码围栏：\n\
         {{\n\
           \"prompt\": \"中文画面描述：景别（特写/中景/远景）、机位（固定/推/拉/摇）、主体是谁、在哪、在做什么、\
           环境与氛围。120 字以内。\",\n\
           \"promptEn\": \"把上面那段翻成英文的镜头描述，给 AI 视频模型用\",\n\
           \"dialogue\": \"画面里出现的台词；没有就留空字符串\",\n\
           \"sfx\": \"这个镜头的环境音与音效，20 字以内\",\n\
           \"lighting\": \"光线方向、色温、明暗对比，20 字以内\",\n\
           \"scriptExcerpt\": \"一句话概括这段剧情\",\n\
           \"characters\": [\"出现的人物名或身份\"],\n\
           \"checks\": [\"角色一致性\", \"场景一致性\", \"光影一致性\", \"字幕可读\", \"画面清晰\"]\n\
         }}",
        frames.len()
    );

    let mut content = vec![serde_json::json!({ "type": "text", "text": instruction })];
    for f in frames {
        content.push(serde_json::json!({
            "type": "image_url",
            "image_url": { "url": image_data_url(f)? }
        }));
    }

    let body = serde_json::json!({
        "model": provider.text_model.trim(),
        "messages": [{ "role": "user", "content": content }],
        "temperature": 0.4,
        // 反推要发 5 张图，开着思维链更贵 —— 和 chat/describe 一样关掉。
        "thinking": { "type": "disabled" },
        "max_tokens": 900,
    });

    let resp = client
        .post(&endpoint)
        .header("Authorization", format!("Bearer {}", provider.api_key.trim()))
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Msg(format!("请求失败：{e}")))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(AppError::Msg(friendly_http(status, &text)));
    }
    let raw = extract_chat_text(&text)?;
    let json_text = strip_code_fence(&raw);
    let v: serde_json::Value = serde_json::from_str(&json_text)
        .map_err(|e| AppError::Msg(format!("反推结果不是 JSON：{e}；原文：{}", tail(&raw, 200))))?;

    // **宽容取字段**：模型可能把 key 写成 `prompt` / `PROMPT` / `description`，
    // 也可能直接给个中文 key。挨个试一遍，别因为大小写就把整段丢掉。
    let s = |keys: &[&str]| {
        for k in keys {
            if let Some(x) = v.get(*k).and_then(|x| x.as_str()) {
                let x = x.trim();
                if !x.is_empty() {
                    return x.to_string();
                }
            }
        }
        String::new()
    };
    let arr = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|i| i.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default()
    };
    // 数组字段同样宽容
    let mut checks: Vec<String> = {
        let a: Vec<String> = arr("checks");
        if a.is_empty() {
            arr("校验")
        } else {
            a
        }
    };
    if checks.is_empty() {
        checks = vec![
            "角色一致性".into(),
            "场景一致性".into(),
            "光影一致性".into(),
            "字幕可读".into(),
            "画面清晰".into(),
        ];
    }

    // **prompt 为空就直接报错，并把模型原文带上。**
    //
    // 踩过：模型返回了合法 JSON 但字段名不对，`prompt` 取到空串 →
    // 一路传到渲染器，最后报个 `UnboundLocalError`（渲染器那边的空文本 bug）。
    // 那种报错完全看不出根因在模型输出上。
    //
    // **在源头就拦住，并把原文贴出来** —— 下次一看就知道模型说了什么。
    let prompt = s(&["prompt", "PROMPT", "description", "画面描述", "描述"]);
    if prompt.is_empty() {
        return Err(AppError::Msg(format!(
            "分镜反推没拿到画面描述（可能是模型没按 JSON 输出）。模型原文：{}",
            tail(&raw, 300)
        )));
    }
    Ok(ShotReverse {
        prompt,
        prompt_en: s(&["promptEn", "prompt_en", "PROMPT_EN", "english"]),
        dialogue: s(&["dialogue", "台词"]),
        sfx: s(&["sfx", "音效", "sound"]),
        lighting: s(&["lighting", "光线", "light"]),
        script_excerpt: s(&["scriptExcerpt", "script_excerpt", "剧本摘要", "摘要"]),
        characters: arr("characters"),
        checks,
        model: provider.text_model.trim().to_string(),
        // Seed 用镜头时长算一个稳定的数字，真实软件也是给个具体值
        seed: format!("{:.0}", (duration_sec * 18_527.0) as u64 % 900_000 + 100_000),
        resolution: "720x1280".into(),
        guidance: "0.82".into(),
        duration_sec: format!("{duration_sec:.1}"),
        fps: "25".into(),
    })
}

/// 去掉模型偶尔吐出来的 ```json 围栏。
fn strip_code_fence(raw: &str) -> String {
    let t = raw.trim();
    let t = t.strip_prefix("```json").or_else(|| t.strip_prefix("```")).unwrap_or(t);
    let t = t.strip_suffix("```").unwrap_or(t);
    t.trim().to_string()
}

/// 截尾部若干字符 —— 报错时给用户看个大概，不要把整段贴出来。
fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        s.to_string()
    } else {
        chars[chars.len() - n..].iter().collect()
    }
}
// ---------------- 剧名 / 简介改写 ----------------

/// 从 OpenAI 兼容的 chat 返回体里取文本。
///
/// 火山方舟和 DashScope 的兼容模式都是这个形状，一套解析够用。
pub fn extract_chat_text(body: &str) -> AppResult<String> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| AppError::Msg(format!("返回不是合法 JSON：{e}")))?;
    if let Some(err) = v
        .pointer("/error/message")
        .and_then(|m| m.as_str())
        .or_else(|| v.get("message").and_then(|m| m.as_str()))
    {
        if v.get("error").is_some() || v.get("code").is_some() {
            return Err(AppError::Msg(format!("服务返回错误：{err}")));
        }
    }
    let text = v
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            AppError::Msg(format!("返回里没有文本：{}", crate::proc::tail(body.trim())))
        })?;
    Ok(text)
}

/// 一次 chat 调用。
pub async fn chat(provider: &AiProvider, system: &str, user: &str) -> AppResult<String> {
    if let Some(early) = precheck(provider) {
        return Err(AppError::Msg(early.message));
    }
    if provider.text_model.trim().is_empty() {
        return Err(AppError::Msg(format!("「{}」没填语言模型", provider.label)));
    }
    let endpoint = format!("{}/chat/completions", normalize_base_url(&provider.base_url));

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| AppError::Msg(format!("建不了 HTTP 客户端：{e}")))?;

    let body = serde_json::json!({
        "model": provider.text_model.trim(),
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        // 改写要的是多样性，不是稳定复读
        "temperature": 0.9,
        // **关掉思维链。**
        //
        // `doubao-seed-2-0-mini` 是推理模型，默认会先生成一大段思考再回答。
        // 实测（同一个起剧名的请求）：
        //
        //     默认：      completion_tokens=495，其中 reasoning_tokens=483  ← 98%
        //     关掉思考：  completion_tokens=5，  其中 reasoning_tokens=0
        //
        // **495 → 5，省 99%**，而答案是「借来的"新车"」/「借车风波」，
        // 质量没有变差 —— 起个剧名不需要推理。
        //
        // 改写简介、起剧名都是"要求明确、答案很短"的任务，思维链纯属浪费。
        "thinking": { "type": "disabled" },
        // 兜底上限：万一某个模型不认 `thinking`，至少别让它无限写下去。
        // 简介要求 100~140 字（约 300 token），给到 800 足够宽裕。
        "max_tokens": 800,
    });

    let resp = client
        .post(&endpoint)
        .header("Authorization", format!("Bearer {}", provider.api_key.trim()))
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Msg(format!("请求失败：{e}")))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        // **有的服务商 / 模型不认 `thinking` 字段**，会直接 400。
        // 那不该让整个功能挂掉 —— 去掉它重试一次：功能照旧，只是多花点 token。
        if mentions_unknown_thinking(&text) {
            return chat_without_thinking(provider, system, user).await;
        }
        return Err(AppError::Msg(friendly_http(status, &text)));
    }
    extract_chat_text(&text)
}

/// 报错里是不是在说「不认识 thinking 这个字段」。
fn mentions_unknown_thinking(body: &str) -> bool {
    let b = body.to_ascii_lowercase();
    b.contains("thinking")
        && (b.contains("invalid") || b.contains("unknown") || b.contains("unexpected"))
}

/// 去掉 `thinking` 再试一次。给不支持该字段的服务商用。
async fn chat_without_thinking(
    provider: &AiProvider,
    system: &str,
    user: &str,
) -> AppResult<String> {
    let endpoint = format!("{}/chat/completions", normalize_base_url(&provider.base_url));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| AppError::Msg(format!("建不了 HTTP 客户端：{e}")))?;
    let body = serde_json::json!({
        "model": provider.text_model.trim(),
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "temperature": 0.9,
        "max_tokens": 800,
    });
    let resp = client
        .post(&endpoint)
        .header("Authorization", format!("Bearer {}", provider.api_key.trim()))
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::Msg(format!("请求失败：{e}")))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(AppError::Msg(friendly_http(status, &text)));
    }
    extract_chat_text(&text)
}

/// 提示词：重写简介。
///
/// 明确写「不要逐句复述」—— 否则模型会把原文换个说法原样吐回来。
pub fn synopsis_prompt(title: &str, original: &str) -> (String, String) {
    let system = "你是短剧发行方的文案编辑。把剧情简介改写成更有悬念、\
                  更适合短视频平台投放的版本。要求：1) 不逐句复述原文，重新组织语言；\
                  2) 保留人物关系与核心冲突，不要编造原文没有的情节；\
                  3) 100~140 字，一段话，不要小标题、不要分点、不要引号；\
                  4) 只输出简介正文，不要任何解释或前后缀。"
        .to_string();
    let user = format!("原剧名：{title}\n原简介：{original}\n\n请输出改写后的简介：");
    (system, user)
}

/// 提示词：根据简介起新剧名。
pub fn title_prompt(synopsis: &str) -> (String, String) {
    let system = "你是短剧发行方的编辑。根据剧情简介起一个有吸引力的新剧名。\
                  要求：1) 4~12 个汉字；2) 能看出核心冲突或爽点，但不要剧透结局；\
                  3) 不要书名号、引号、标点，不要「短剧」「全集」这类词；\
                  4) 只输出剧名本身，不要任何解释。"
        .to_string();
    let user = format!("剧情简介：{synopsis}\n\n请输出新剧名：");
    (system, user)
}

/// 把模型可能带出来的包装去掉。
///
/// 模型很爱加书名号、引号、「新剧名：」这类前缀 —— 提示词里说了不要也常加。
/// 与其反复调提示词，不如在出口统一擦干净。
pub fn clean_title(raw: &str) -> String {
    let mut t = raw.trim().lines().next().unwrap_or("").trim().to_string();
    for prefix in ["新剧名：", "新剧名:", "剧名：", "剧名:", "答案：", "答案:"] {
        if let Some(rest) = t.strip_prefix(prefix) {
            t = rest.trim().to_string();
        }
    }
    t = t
        .trim_matches(|c| matches!(c, '《' | '》' | '"' | '\'' | '「' | '」'))
        .trim()
        .to_string();
    // 标点一律去掉：剧名里不该有句号问号
    t.retain(|c| {
        !matches!(
            c,
            '。' | '！' | '？' | '，' | '、' | '；' | '：' | '.' | '!' | '?' | ',' | ';' | ':'
        )
    });
    t.chars().take(20).collect::<String>().trim().to_string()
}

/// 简介清洗：去掉模型爱加的「简介：」前缀和包裹引号。
pub fn clean_synopsis(raw: &str) -> String {
    let mut t = raw.trim().to_string();
    for prefix in ["改写后的简介：", "新简介：", "简介：", "简介:"] {
        if let Some(rest) = t.strip_prefix(prefix) {
            t = rest.trim().to_string();
        }
    }
    t.trim_matches(|c| matches!(c, '"' | '\'' | '「' | '」'))
        .trim()
        .chars()
        .take(400)
        .collect()
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

/// 把 HTTP 错误翻译成人话。
///
/// **一定要带上服务端返回的内容。** 之前这里给 404 编了一句
/// 「接口地址不对 —— 检查 Base URL」，结果真实原因是
/// `ModelNotOpen`（账号没开通这个模型）—— 用户照着那句话去查地址，
/// 查半天也查不出所以然。**猜原因比不给原因更糟。**
///
/// 火山方舟的错误码在 body 里，比状态码有用得多：
/// - `ModelNotOpen`：模型存在，但账号没开通
/// - `InvalidEndpointOrModel.NotFound`：模型名不存在，或没权限
fn friendly_http(status: u16, body: &str) -> String {
    let parsed: Option<serde_json::Value> = serde_json::from_str(body).ok();
    let code = parsed
        .as_ref()
        .and_then(|v| v.pointer("/error/code"))
        .and_then(|c| c.as_str())
        .unwrap_or("");
    let detail = parsed
        .as_ref()
        .and_then(|v| v.pointer("/error/message"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| crate::proc::tail(body.trim()));

    match code {
        "ModelNotOpen" => format!(
            "这个模型没开通（ModelNotOpen）。去火山方舟控制台的「开通管理」里开通它，\
             或换一个已开通的模型。原始信息：{detail}"
        ),
        // **最常见的一种，提示语也最容易写偏。**
        //
        // 实测用户遇到的：模型开通了、Key 也对，但控制台开着「安心体验」——
        // 免费额度用完后服务**自动暂停**，所有调用返 429。
        // 只有说清楚「去关掉安心体验」，用户才知道下一步点哪。
        "SetLimitExceeded" => format!(
            "免费额度用完了，火山方舟把服务自动暂停了（SetLimitExceeded）。\n\
             这是控制台上「安心体验」那个开关干的 —— 它防止超额扣费。\n\
             要继续用：去火山方舟控制台该模型页面点「前往关闭」安心体验\
             （之后超出免费额度会开始计费），或者等下个月额度重置。\n\
             原始信息：{detail}"
        ),
        "InvalidEndpointOrModel.NotFound" => format!(
            "模型名不存在，或账号没有权限用它 —— 检查模型名有没有写错。原始信息：{detail}"
        ),
        _ => match status {
            401 | 403 => format!("Key 无效或没有权限（{status}）。原始信息：{detail}"),
            404 => format!(
                "接口返回 404。可能是 Base URL 不对，也可能是模型没开通 —— \
                 两种都会返 404，得看原始信息。原始信息：{detail}"
            ),
            429 => format!("请求太频繁或额度用尽（429）。原始信息：{detail}"),
            _ => format!("HTTP {status}：{detail}"),
        },
    }
}

/// 从 `GET {base}/models` 拿账号能看到的模型列表。
///
/// 为什么需要：用户填的模型名常常是错的（比如写 `Doubao-Seedream-5.0-flash`，
/// 而真名是 `doubao-seedream-5-0-260128`），或者账号根本没开通。
/// 与其让他一次次试错，不如把列表摆出来让他选。
///
/// 返回的每一项带 `available` —— 目录里有不等于能用：
/// 没开通的也在列表里，只是调用时会返 `ModelNotOpen`。
pub async fn list_models(provider: &AiProvider) -> AppResult<Vec<ModelInfo>> {
    if provider.api_key.trim().is_empty() {
        return Err(AppError::Msg("还没填 API Key".into()));
    }
    let base = normalize_base_url(&provider.base_url);
    if base.is_empty() {
        return Err(AppError::Msg("还没填 Base URL".into()));
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| AppError::Msg(format!("建不了 HTTP 客户端：{e}")))?;
    let resp = client
        .get(format!("{base}/models"))
        .header("Authorization", format!("Bearer {}", provider.api_key.trim()))
        .send()
        .await
        .map_err(|e| AppError::Msg(format!("请求失败：{e}")))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(AppError::Msg(friendly_http(status, &text)));
    }

    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| AppError::Msg(format!("返回不是合法 JSON：{e}")))?;
    // 兼容两种形状：{"data":[…]} 和 […]
    let arr = v
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| v.as_array())
        .ok_or_else(|| AppError::Msg(format!("返回里没有模型列表：{}", crate::proc::tail(&text))))?;

    let mut out: Vec<ModelInfo> = arr
        .iter()
        .filter_map(|m| {
            let id = m.get("id").and_then(|i| i.as_str())?.to_string();
            let state = m
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            // Shutdown 的明确不能用了，直接不列出来，免得用户选中才发现
            if state == "Shutdown" {
                return None;
            }
            Some(ModelInfo {
                id,
                state: state.clone(),
                // 目录里有 ≠ 开通了。能直接用的是没标状态的那些；
                // Retiring 是即将下线但当前还能用。
                available: state.is_empty() || state == "Retiring",
            })
        })
        .collect();
    out.sort_by(|a, b| b.available.cmp(&a.available).then(a.id.cmp(&b.id)));
    Ok(out)
}

/// 账号目录里的一个模型。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    /// 目录里标的状态：空串 = 正常，Retiring = 即将下线
    pub state: String,
    /// 能直接调用吗（目录里有不等于开通了）
    pub available: bool,
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
    fn chat_text_is_extracted_from_both_shapes() {
        // 火山方舟 / DashScope 兼容模式都是这个形状
        let ark = r#"{"choices":[{"message":{"content":"  白毛狼引桑蚕守家园  "}}]}"#;
        assert_eq!(extract_chat_text(ark).unwrap(), "白毛狼引桑蚕守家园");

        // 报错要如实抛出来，不能当成"没找到文本"
        let err = r#"{"error":{"message":"invalid api key"}}"#;
        assert!(extract_chat_text(err).unwrap_err().to_string().contains("invalid api key"));

        // 空 choices 也要有明确说法
        let empty = r#"{"choices":[]}"#;
        assert!(extract_chat_text(empty).is_err());
    }

    #[test]
    fn titles_come_back_clean_no_matter_what_the_model_adds() {
        // 提示词里说了不要书名号和前缀，模型还是常加 —— 在出口统一擦掉
        assert_eq!(clean_title("《白毛狼引桑蚕守家园》"), "白毛狼引桑蚕守家园");
        assert_eq!(clean_title("新剧名：白毛狼引桑蚕守家园"), "白毛狼引桑蚕守家园");
        assert_eq!(clean_title("\"白毛狼引桑蚕守家园\""), "白毛狼引桑蚕守家园");
        assert_eq!(clean_title("白毛狼引桑蚕守家园。"), "白毛狼引桑蚕守家园");
        assert_eq!(clean_title("「白毛狼引桑蚕守家园」\n解释：……"), "白毛狼引桑蚕守家园");
        assert_eq!(clean_title("  多余空格  "), "多余空格");
        // 太长要截断，别让一个段落变成剧名
        assert!(clean_title(&"字".repeat(60)).chars().count() <= 20);
    }

    #[test]
    fn synopsis_is_cleaned_too() {
        assert_eq!(clean_synopsis("简介：这是一段简介"), "这是一段简介");
        assert_eq!(clean_synopsis("\"这是一段简介\""), "这是一段简介");
        assert_eq!(clean_synopsis("改写后的简介：内容"), "内容");
    }

    #[test]
    fn prompts_ask_for_the_right_shape() {
        let (sys, user) = synopsis_prompt("原剧名", "原简介内容");
        assert!(sys.contains("不逐句复述"), "得明确禁止复述：{sys}");
        assert!(sys.contains("100~140 字"), "{sys}");
        assert!(user.contains("原简介内容"));

        let (sys2, user2) = title_prompt("一段简介");
        assert!(sys2.contains("4~12 个汉字"));
        assert!(user2.contains("一段简介"));
    }

    #[test]
    fn probe_endpoints_match_each_provider() {        assert!(probe_endpoint("ark", "https://ark.cn-beijing.volces.com/api/v3").ends_with("/models"));
        assert!(probe_endpoint("dashscope", "https://dashscope.aliyuncs.com/api/v1").ends_with("/tasks"));
        // 探测端点也要归一化，别拼出双斜杠
        assert!(!probe_endpoint("ark", "https://a.com/v1/").contains("//models"));
    }
}
