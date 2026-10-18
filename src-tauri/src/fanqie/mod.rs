//! 番茄短剧（fqnovel）在线片库。
//!
//! 两个公开页面接口：
//!
//! 1. **分类落地页**：按上线时间倒序，带 `next_offset` / `session_id`，
//!    可以一直往下翻 —— 对应界面里的「最新上线」。
//! 2. **新剧货架**：约 20 条一页，同样带 `next_offset` / `session_id` ——
//!    对应「新剧货架」，也可以接着翻。
//!
//! 两个接口都不需要登录，也不带任何用户标识：只读、匿名。
//!
//! ## 封面为什么要过一道本地转换
//!
//! 分类接口返回的封面是 **HEIC**（Android App 能直接显示），WebView2 不认，
//! 直接塞进 `<img>` 就是一块空白。所以封面走 [`cover_jpeg`]：下载后交给
//! 内置 ffmpeg 转成 JPEG 落盘缓存，下次直接复用。转化只做一次。
//! 新剧货架的封面本来就是 JPEG，前端拿到原地址直接用，不会走到这里。
//!
//! ## 播放为什么要在本地解密
//!
//! 官网只公开前 50 集左右，而且实测大量集数的公开 MP4 直接 404，覆盖不了整部剧。
//! 要看到后面的集数只能走 App 的 `video_model` 流：CENC-AES-CTR 加密的 HEVC MP4。
//! `encrypt_info.spade_a` 能派生出 16 字节 AES key，交给 ffmpeg 的
//! `-decryption_key` 直接解密；再转成 H.264 落盘缓存 —— WebView2 对 HEVC 的
//! 支持取决于系统组件，H.264 最保险。走 [`prepare_video`]。
//!
//! ## 批量下载为什么不转码
//!
//! 在线播放要「点开就能播」，才值得转 H.264。批量下载不一样：源片落盘后还要走
//! 整剧处理，其中「整集去重」本来就要完整编码一次 —— 下载时再转一遍是纯重复劳动，
//! 还是整部剧最慢的一环。所以 [`download_series`] 默认 `-c:v copy` 只解密，
//! 几秒就能落盘，唯一的一次编码留给去重；copy 解不开时自动退回转码兜底。

use crate::error::{AppError, AppResult};
use crate::models::{
    FanqieCategoryPage, FanqieDownloadReport, FanqieDownloadRequest, FanqieEpisode, FanqiePlayInfo,
    FanqiePreparedVideo, FanqieSelectItems, FanqieSelectorItem, FanqieSelectorRow,
    FanqieSeriesDetail, FanqieVideoInfo, OnlineDrama,
};
use crate::proc;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;
use tauri::Manager;

const BASE: &str = "https://api5-normal-sinfonlineb.fqnovel.com";
const COMMON_QUERY: &str = "aid=8662&app_name=novelread&channel=update_64&version_code=73532&update_version_code=73532&manifest_version_code=73532&version_name=7.3.5.32&device_platform=android&os=android&os_version=16&os_api=36&device_brand=Redmi&device_type=25053RT47C&resolution=1280*2772&dpi=520&language=zh&ac=wifi&ssmix=a";
const UA: &str = "com.dragon.read/7.3.5.32 (Linux; U; Android 16; zh_CN; 25053RT47C)";

/// 播放 / 详情接口要的是 App（phoenix）身份，和列表接口的 UA 不同。
const PHOENIX_UA: &str = "com.phoenix.read/73532 (Linux; U; Android 16; zh_CN; 25053RT47C; Build/BP2A.250605.031.A3; Cronet/TTNetVersion:04657795 2026-01-23 QuicVersion:c67e9834 2025-09-08)";

/// 官网。App 的播放流是 CENC 加密的 HEVC，官网 player 页里嵌的却是明文 MP4，
/// 所以「能看」这件事走官网这一层。
const SITE: &str = "https://hongguoduanju.com";
const WEB_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36";

fn client() -> AppResult<&'static reqwest::Client> {
    // 列表和封面会并发发请求，客户端只建一次（TLS 配置不便宜）。
    static CLIENT: std::sync::OnceLock<Result<reqwest::Client, String>> =
        std::sync::OnceLock::new();
    let built = CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(25))
            .user_agent(UA)
            .build()
            .map_err(|e| e.to_string())
    });
    built
        .as_ref()
        .map_err(|e| AppError::Msg(format!("建不了 HTTP 客户端：{e}")))
}

// ---------------- 响应结构（内部） ----------------
//
// 一律给默认值：接口字段经常缺席（比如某些条目没有 tag_info），
// 少一个字段不该让整页失败。

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    #[serde(default)]
    code: i64,
    #[serde(default)]
    message: String,
    data: Option<T>,
}

#[derive(Debug, Deserialize)]
struct CategoryData {
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    next_offset: i64,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    video_data: Vec<RawItem>,
}

#[derive(Debug, Deserialize)]
struct ChangeData {
    cell_view: Option<CellView>,
    #[serde(default)]
    has_more: bool,
    #[serde(default)]
    next_offset: i64,
    #[serde(default)]
    session_id: String,
}

#[derive(Debug, Deserialize)]
struct CellView {
    #[serde(default)]
    video_data: Vec<RawItem>,
}

#[derive(Debug, Deserialize)]
struct SelectorData {
    #[serde(default)]
    selector_rows: Vec<RawSelectorRow>,
}

#[derive(Debug, Deserialize)]
struct RawSelectorRow {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    row_name: String,
    #[serde(default)]
    items: Vec<RawSelectorItem>,
}

#[derive(Debug, Deserialize)]
struct RawSelectorItem {
    #[serde(default)]
    selector_item_id: String,
    #[serde(default)]
    show_name: String,
}

#[derive(Debug, Deserialize, Default)]
struct SubTitle {
    #[serde(default)]
    content: String,
    #[serde(default)]
    data_type: i64,
}

#[derive(Debug, Deserialize, Default)]
struct TagInfo {
    #[serde(default)]
    text: String,
}

#[derive(Debug, Deserialize, Default)]
struct RawItem {
    #[serde(default)]
    vid: String,
    #[serde(default)]
    series_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    video_desc: String,
    #[serde(default)]
    cover: String,
    /// 秒
    #[serde(default)]
    duration: i64,
    #[serde(default)]
    play_cnt: i64,
    #[serde(default)]
    score: String,
    #[serde(default)]
    episode_cnt: i64,
    #[serde(default)]
    sub_title_list: Vec<SubTitle>,
    #[serde(default)]
    tag_info: Option<TagInfo>,
    #[serde(default)]
    copyright: String,
}

/// 统一的「取数据 + 查错误码」。两个接口外壳形状一致。
async fn fetch<T: DeserializeOwned>(req: reqwest::RequestBuilder) -> AppResult<T> {
    let resp = req
        .send()
        .await
        .map_err(|e| AppError::Msg(format!("请求番茄接口失败：{e}")))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| AppError::Msg(format!("读取番茄接口响应失败：{e}")))?;
    if !status.is_success() {
        return Err(AppError::Msg(format!("番茄接口返回 HTTP {status}")));
    }
    let env: Envelope<T> = serde_json::from_str(&text)
        .map_err(|e| AppError::Msg(format!("番茄接口数据解析失败：{e}")))?;
    if env.code != 0 {
        let detail = if env.message.trim().is_empty() {
            format!("code {}", env.code)
        } else {
            format!("{}（code {}）", env.message.trim(), env.code)
        };
        return Err(AppError::Msg(format!("番茄接口报错：{detail}")));
    }
    env.data
        .ok_or_else(|| AppError::Msg("番茄接口没有返回数据".into()))
}

/// 把原始条目压成前端能直接用的稳定形状。
fn convert(raw: &RawItem) -> OnlineDrama {
    // 热度优先用接口给的「1578万」原文 —— play_cnt 经常是单片而不是整部的。
    let heat = raw
        .sub_title_list
        .iter()
        .find(|s| s.data_type == 27)
        .map(|s| s.content.trim().to_string())
        .unwrap_or_default();

    let mut tags: Vec<String> = Vec::new();
    if let Some(tag) = &raw.tag_info {
        let text = tag.text.trim();
        if !text.is_empty() {
            tags.push(text.to_string());
        }
    }
    for s in &raw.sub_title_list {
        let text = s.content.trim();
        if s.data_type == 3 && !text.is_empty() && !tags.iter().any(|t| t == text) {
            tags.push(text.to_string());
        }
    }

    OnlineDrama {
        vid: raw.vid.trim().to_string(),
        series_id: raw.series_id.trim().to_string(),
        title: raw.title.trim().to_string(),
        desc: raw.video_desc.trim().to_string(),
        cover: raw.cover.trim().to_string(),
        duration_sec: raw.duration.max(0),
        play_count: raw.play_cnt.max(0),
        heat,
        score: raw.score.trim().to_string(),
        episode_count: raw.episode_cnt.max(0),
        tags,
        copyright: raw.copyright.trim().to_string(),
        is_new: raw
            .tag_info
            .as_ref()
            .map(|t| t.text.trim() == "新剧")
            .unwrap_or(false),
    }
}

// ---------------- 列表与筛选 ----------------

/// 体裁决定场景：真人剧 default、漫剧 comic_series、AI 剧 ai_series。
fn scene_for(genre: &str) -> &'static str {
    match genre {
        "comic_series" => "comic_series",
        "ai_series" => "ai_series",
        _ => "default",
    }
}

/// 排序没传时统一按「最新上架」；场景由体裁决定。
fn normalize_select_items(mut items: FanqieSelectItems) -> (FanqieSelectItems, &'static str) {
    if items.sort.is_empty() {
        items.sort.push("online_time".to_string());
    }
    let scene = scene_for(items.genre.first().map(String::as_str).unwrap_or(""));
    (items, scene)
}

/// 分类落地页（默认按上线时间倒序，可带筛选、可翻页）。
pub async fn category(
    offset: i64,
    limit: i64,
    session_id: &str,
    select_items: FanqieSelectItems,
) -> AppResult<FanqieCategoryPage> {
    let offset = offset.max(0);
    let limit = limit.clamp(1, 30);
    let (select_items, req_scene) = normalize_select_items(select_items);

    let body = serde_json::json!({
        "filter_ids": "",
        "req_scene": req_scene,
        "offset": offset,
        "need_selector_panel": false,
        "limit": limit,
        "select_items": select_items,
        "session_id": session_id,
        "req_type": "only_content",
        "client_req_type": 3,
    });

    let url = format!("{BASE}/reading/distribution/category/landpage/v/?{COMMON_QUERY}");
    let data: CategoryData = fetch(
        client()?
            .post(url)
            .header("content-type", "application/json; charset=utf-8")
            .json(&body),
    )
    .await?;

    Ok(FanqieCategoryPage {
        items: data.video_data.iter().map(convert).collect(),
        has_more: data.has_more,
        next_offset: data.next_offset,
        session_id: data.session_id,
    })
}

/// 筛选面板：按当前体裁拉取主题 / 设定 / 背景等可选项。
/// 面板 ID 会随体裁变化，界面切换体裁后要重新调用一次。
pub async fn selector_rows(genre: &str) -> AppResult<Vec<FanqieSelectorRow>> {
    let genre = genre.trim();
    let select_items = FanqieSelectItems {
        genre: if genre.is_empty() {
            Vec::new()
        } else {
            vec![genre.to_string()]
        },
        sort: vec!["online_time".to_string()],
        ..Default::default()
    };
    let body = serde_json::json!({
        "filter_ids": "",
        "req_scene": scene_for(genre),
        "offset": 0,
        "need_selector_panel": true,
        "limit": 1,
        "select_items": select_items,
        "session_id": "",
        "req_type": "default",
        "client_req_type": 3,
    });
    let url = format!("{BASE}/reading/distribution/category/landpage/v/?{COMMON_QUERY}");
    let data: SelectorData = fetch(
        client()?
            .post(url)
            .header("content-type", "application/json; charset=utf-8")
            .json(&body),
    )
    .await?;

    Ok(data
        .selector_rows
        .into_iter()
        .filter_map(|row| {
            let key = row.kind.trim().to_string();
            if key.is_empty() {
                return None;
            }
            let items: Vec<FanqieSelectorItem> = row
                .items
                .into_iter()
                .filter(|i| {
                    !i.selector_item_id.trim().is_empty() && !i.show_name.trim().is_empty()
                })
                .map(|i| FanqieSelectorItem {
                    id: i.selector_item_id.trim().to_string(),
                    name: i.show_name.trim().to_string(),
                })
                .collect();
            if items.is_empty() {
                return None;
            }
            Some(FanqieSelectorRow {
                label: row.row_name.trim().to_string(),
                key,
                items,
            })
        })
        .collect())
}

/// 新剧货架：一页约 20 条，按 `next_offset` / `session_id` 接着翻。
pub async fn new_shelf(offset: i64, session_id: &str) -> AppResult<FanqieCategoryPage> {
    let offset = offset.max(0);
    let url = format!(
        "{BASE}/reading/bookapi/bookmall/cell/change/v1/?{COMMON_QUERY}\
         &change_type=1&selected_items=firstonlinetime_new&client_req_type=1\
         &cell_id=7431550523368554558&limit=20&offset={offset}&cell_gender=2"
    );
    // session_id 由接口下发，这里按 query 参数安全编码，避免直接拼串。
    let mut url =
        reqwest::Url::parse(&url).map_err(|e| AppError::Msg(format!("新剧货架地址不合法：{e}")))?;
    url.query_pairs_mut().append_pair("session_id", session_id);

    let data: ChangeData = fetch(client()?.get(url)).await?;
    let items = data.cell_view.map(|c| c.video_data).unwrap_or_default();
    Ok(FanqieCategoryPage {
        items: items.iter().map(convert).collect(),
        has_more: data.has_more,
        next_offset: data.next_offset,
        session_id: data.session_id,
    })
}

// ---------------- 详情与播放（视频详情页） ----------------

/// 接口只要求设备号是纯数字，并不校验真实性：按时间戳搅一个即可。
fn random_digits(len: usize) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let mut x = nanos ^ SEQ.fetch_add(0x9e37_79b9_7f4a_7c15, Ordering::Relaxed);
    let mut out = String::with_capacity(len);
    for i in 0..len {
        // xorshift64：够随机，也不值得为此加一个 rand 依赖
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let digit = if i == 0 { 1 + (x % 9) } else { x % 10 };
        out.push(char::from(b'0' + digit as u8));
    }
    out
}

fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// 前端会把路由参数直接传进来，只接受纯数字 ID，避免拼出奇怪的 URL。
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_digit())
}

/// 播放 / 详情接口专用的请求头。
fn phoenix(req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    req.header("user-agent", PHOENIX_UA)
        .header("content-type", "application/json; charset=utf-8")
        .header("X-XS-From-Web", "0")
        .header("Sdk-Version", "2")
        .header("X-SS-DP", "8662")
        .header("X-Tt-Store-Region", "cn")
}

// ---------------- SSR 页面解析 ----------------
//
// 官网 player 页把数据直接嵌在 HTML 的 JSON 里，没有签名。字段值会带
// `\u002F` 这类转义，所以取出来之后统一走一次反转义。

fn vid_list_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r#""vid_list"\s*:\s*\[([^\]]*)\]"#).expect("vid_list regex"))
}

fn main_url_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r#""(?:main_url|contentUrl)"\s*:\s*"([^"]+)""#).expect("main_url regex")
    })
}

/// `[?&]Range=...`：参考实现会把这个参数剥掉，让 CDN 回整段而不是分片。
fn range_param_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"([?&])Range=[^&]*").expect("Range regex"))
}

/// 反转义 JSON 字符串字面量（`\u002F`、`\"`、`&amp;` 等）。
fn unescape_json_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('u') => {
                let mut hex = String::with_capacity(4);
                for _ in 0..4 {
                    if let Some(h) = chars.next() {
                        hex.push(h);
                    }
                }
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(ch) => out.push(ch),
                    None => {
                        out.push_str("\\u");
                        out.push_str(&hex);
                    }
                }
            }
            Some('/') => out.push('/'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out.replace("&amp;", "&")
}

/// 从 SSR HTML 里按 key 取一个字符串字段。
fn pick_json_string(html: &str, key: &str) -> Option<String> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r#""([a-z_]+)"\s*:\s*"((?:[^"\\]|\\.)*)""#).expect("json string regex")
    });
    for cap in re.captures_iter(html) {
        if &cap[1] == key {
            return Some(unescape_json_string(&cap[2]));
        }
    }
    None
}

/// 视频直链：反转义后去掉 Range 参数，保证 CDN 回整段。
fn normalize_media_url(raw: &str) -> String {
    let text = unescape_json_string(raw);
    let text = range_param_re().replace_all(&text, "$1");
    // 被剥掉的参数可能留下 `?&` 或 `&&`，清一下
    let text = text.replace("?&", "?").replace("&&", "&");
    text.trim_end_matches(&['?', '&', ' '][..]).to_string()
}

/// `/video/.../360p.mp4` 里的清晰度；认不出就当 360p。
fn quality_from_url(url: &str) -> String {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"/(\d{3,4})p\.mp4").expect("quality regex"));
    re.captures(url)
        .map(|c| format!("{}p", &c[1]))
        .unwrap_or_else(|| "360p".into())
}

/// 从 `/player/{sid}/{vid}` 页面里抠出 main_url。
fn parse_play_url(html: &str) -> Option<String> {
    let raw = main_url_re().captures(html)?.get(1)?.as_str();
    let url = normalize_media_url(raw);
    if url.starts_with("https://") || url.starts_with("http://") {
        Some(url)
    } else {
        None
    }
}

/// 从官网剧集页解析分集列表（App 详情接口不可用时的兜底）。
fn parse_series_page(html: &str, series_id: &str) -> Option<FanqieSeriesDetail> {
    let raw_list = vid_list_re().captures(html)?.get(1)?.as_str();
    let mut episodes = Vec::new();
    for part in raw_list.split(',') {
        let vid = part.trim().trim_matches('"').trim();
        if !valid_id(vid) {
            continue;
        }
        let index = episodes.len() as i64 + 1;
        episodes.push(FanqieEpisode {
            vid: vid.to_string(),
            index,
            title: format!("第 {index} 集"),
            duration_sec: 0,
        });
    }
    if episodes.is_empty() {
        return None;
    }
    Some(FanqieSeriesDetail {
        series_id: series_id.to_string(),
        title: pick_json_string(html, "series_name").unwrap_or_else(|| "未命名短剧".into()),
        intro: pick_json_string(html, "series_intro").unwrap_or_default(),
        cover: pick_json_string(html, "series_cover").unwrap_or_default(),
        episode_count: episodes.len() as i64,
        episodes,
        source: "web".into(),
    })
}

/// 从 App 详情接口的 `data` 里解析整部剧。
///
/// 形状是 `{ "<series_id>": { "video_data": { ... } } }`，key 会变，所以按值找。
fn parse_app_series(data: &serde_json::Value, series_id: &str) -> Option<FanqieSeriesDetail> {
    let vd = data
        .as_object()?
        .values()
        .find_map(|entry| entry.get("video_data"))?;

    let intro = vd
        .get("series_intro")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();

    let mut episodes = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (pos, item) in vd
        .get("video_list")
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let vid = item.get("vid").and_then(|v| v.as_str()).unwrap_or("").trim();
        if !valid_id(vid) || !seen.insert(vid.to_string()) {
            continue;
        }
        let index = item
            .get("vid_index")
            .and_then(|v| v.as_i64())
            .filter(|i| *i > 0)
            .unwrap_or(pos as i64 + 1);
        // 接口偶尔把整部剧的简介塞进每集 title，超过一行就没意义了
        let raw_title = item.get("title").and_then(|v| v.as_str()).unwrap_or("").trim();
        let title = if raw_title.is_empty()
            || raw_title == intro
            || raw_title.chars().count() > 30
        {
            format!("第 {index} 集")
        } else {
            raw_title.to_string()
        };
        episodes.push(FanqieEpisode {
            vid: vid.to_string(),
            index,
            title,
            duration_sec: item.get("duration").and_then(|v| v.as_i64()).unwrap_or(0).max(0),
        });
    }
    episodes.sort_by_key(|e| e.index);
    if episodes.is_empty() {
        return None;
    }

    let episode_count = vd
        .get("episode_cnt")
        .and_then(|v| v.as_i64())
        .filter(|n| *n > 0)
        .unwrap_or(episodes.len() as i64);

    Some(FanqieSeriesDetail {
        series_id: vd
            .get("series_id")
            .and_then(|v| v.as_str())
            .unwrap_or(series_id)
            .to_string(),
        title: vd
            .get("series_title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string(),
        intro,
        cover: vd
            .get("series_cover")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string(),
        episode_count,
        episodes,
        source: "app".into(),
    })
}

// ---------------- 官网页面 ----------------

async fn fetch_web_html(url: &str) -> AppResult<String> {
    let resp = client()?
        .get(url)
        .header("user-agent", WEB_UA)
        .header("accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
        .header("accept-language", "zh-CN,zh;q=0.9")
        .header("referer", format!("{SITE}/"))
        .send()
        .await
        .map_err(|e| AppError::Msg(format!("请求官网页面失败：{e}")))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(AppError::Msg(format!("官网页面返回 HTTP {status}")));
    }
    resp.text()
        .await
        .map_err(|e| AppError::Msg(format!("读取官网页面失败：{e}")))
}

async fn web_series_detail(series_id: &str) -> AppResult<FanqieSeriesDetail> {
    let html = fetch_web_html(&format!("{SITE}/player/{series_id}")).await?;
    parse_series_page(&html, series_id)
        .ok_or_else(|| AppError::Msg("官网页面里没有分集列表".into()))
}

/// 整部剧的详情：先走 App 详情接口，失败再退回官网页面。
pub async fn series_detail(series_id: &str) -> AppResult<FanqieSeriesDetail> {
    let sid = series_id.trim();
    if !valid_id(sid) {
        return Err(AppError::Msg("剧集 ID 不合法".into()));
    }

    let url = format!(
        "{BASE}/novel/player/multi_video_detail/preload/v1/?{COMMON_QUERY}&device_id={}&iid={}&_rticket={}",
        random_digits(19),
        random_digits(19),
        now_millis()
    );
    let body = serde_json::json!({
        "biz_param": {
            "detail_page_version": 0,
            "disable_digg_stat": false,
            "need_all_video_definition": false,
            "need_mp4_align": false,
            "screen_width_px": "900",
            "source": 7,
            "use_os_player": false,
            "use_server_dns": false,
        },
        "dr_scene": "preload",
        "series_id": sid,
    });

    let app_result = async {
        let data: serde_json::Value =
            fetch(phoenix(client()?.post(url).json(&body))).await?;
        parse_app_series(&data, sid)
            .ok_or_else(|| AppError::Msg("App 详情里没有分集列表".into()))
    }
    .await;

    match app_result {
        Ok(detail) => Ok(detail),
        Err(app_err) => match web_series_detail(sid).await {
            Ok(detail) => Ok(detail),
            Err(web_err) => Err(AppError::Msg(format!(
                "拿不到剧集详情：App 接口失败（{app_err}）；官网兜底也失败（{web_err}）"
            ))),
        },
    }
}

/// 一集的可播放直链（官网兜底）。
///
/// 主链路是 [`prepare_video`]：App 流解密后本地播放，覆盖全部集数。
/// 但官网 player 页里的 `main_url` / `contentUrl` 是明文 MP4，
/// 在解密失败时可以应个急，所以保留：官网没给出时按公开路径拼一次并用 HEAD 验证。
pub async fn play_url(series_id: &str, vid: &str) -> AppResult<FanqiePlayInfo> {
    let sid = series_id.trim();
    let vid = vid.trim();
    if !valid_id(sid) || !valid_id(vid) {
        return Err(AppError::Msg("剧集 / 视频 ID 不合法".into()));
    }

    let page_error: String;
    match fetch_web_html(&format!("{SITE}/player/{sid}/{vid}")).await {
        Ok(html) => match parse_play_url(&html) {
            Some(url) => {
                return Ok(FanqiePlayInfo {
                    quality: quality_from_url(&url),
                    url,
                    source: "web".into(),
                })
            }
            None => page_error = "页面里没有 main_url".into(),
        },
        Err(e) => page_error = e.to_string(),
    }

    let url = format!("{SITE}/video/public/{vid}/360p.mp4");
    let resp = client()?
        .head(&url)
        .header("user-agent", WEB_UA)
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => Ok(FanqiePlayInfo {
            quality: quality_from_url(&url),
            url,
            source: "web-fallback".into(),
        }),
        Ok(r) => Err(AppError::Msg(format!(
            "官网没有可播放的直链（HTTP {}；{page_error}）",
            r.status()
        ))),
        Err(e) => Err(AppError::Msg(format!(
            "官网播放地址探测失败：{e}；{page_error}"
        ))),
    }
}

/// 拉一次 `video_model`：里面是整集的全部分辨率、编码和加密信息。
async fn fetch_video_model(vid: &str) -> AppResult<serde_json::Value> {
    let url = format!(
        "{BASE}/novel/player/video_model/v1/?{COMMON_QUERY}&device_id={}&iid={}&_rticket={}",
        random_digits(19),
        random_digits(19),
        now_millis()
    );
    let body = serde_json::json!({
        "video_id": vid,
        "content_type": 1,
        "biz_param": {
            "need_all_video_definition": true,
            "video_platform": 3,
        },
    });

    #[derive(Debug, Deserialize, Default)]
    struct VideoModelData {
        #[serde(default)]
        video_model: String,
    }

    let data: VideoModelData = fetch(phoenix(client()?.post(url).json(&body))).await?;
    if data.video_model.trim().is_empty() {
        return Err(AppError::Msg("视频详情里没有 video_model".into()));
    }
    serde_json::from_str(&data.video_model)
        .map_err(|e| AppError::Msg(format!("视频详情解析失败：{e}")))
}

/// 用户给的那个 `video_model` 接口：时长 + 全部清晰度 + 是否加密。
pub async fn video_info(vid: &str) -> AppResult<FanqieVideoInfo> {
    let vid = vid.trim();
    if !valid_id(vid) {
        return Err(AppError::Msg("视频 ID 不合法".into()));
    }

    let model = fetch_video_model(vid).await?;
    let duration_sec = model
        .get("video_duration")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    let mut qualities: Vec<String> = Vec::new();
    let mut encrypted = false;
    for item in video_list_items(&model) {
        if let Some(def) = item
            .get("video_meta")
            .and_then(|m| m.get("definition"))
            .and_then(|d| d.as_str())
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            if !qualities.iter().any(|q| q == def) {
                qualities.push(def.to_string());
            }
        }
        if item
            .get("encrypt_info")
            .and_then(|e| e.get("encrypt"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            encrypted = true;
        }
    }

    Ok(FanqieVideoInfo {
        vid: vid.to_string(),
        duration_sec,
        qualities,
        encrypted,
    })
}

// ---------------- App 流的解密播放 ----------------

/// `video_model.video_list` 里的一条流。
#[derive(Debug, Clone)]
struct ModelStream {
    url: String,
    definition: String,
    codec: String,
    encrypted: bool,
    /// CENC-AES-CTR 的 16 字节 key，由 `spade_a` 派生
    key: Option<[u8; 16]>,
}

/// 标准 base64 解码（兼容 URL-safe 字母表，忽略空白与 padding）。
fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn sextet(c: u8) -> Option<u8> {
        match c {
            b'A'..=b'Z' => Some(c - b'A'),
            b'a'..=b'z' => Some(c - b'a' + 26),
            b'0'..=b'9' => Some(c - b'0' + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for b in input.bytes() {
        if b == b'=' {
            break;
        }
        if b.is_ascii_whitespace() {
            continue;
        }
        acc = (acc << 6) | sextet(b)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// `encrypt_info.spade_a` → 16 字节 AES key。
///
/// 算法与开源实现 hongguo-downloader 一致：先解一层 base64，再按前三个字节
/// 异或出来的长度截取一段字节流做滚动异或，最后从明文里取出 32 个 hex 字符。
/// 解不开就返回 `None`，调用方会跳过这条流。
fn derive_spade_key(spade_a: &str) -> Option<[u8; 16]> {
    let buf = base64_decode(spade_a)?;
    if buf.len() < 3 {
        return None;
    }
    let key0 = buf[0] ^ buf[1] ^ buf[2];
    let w22 = buf.len() as i32 - key0 as i32 + 0x2f;
    // 原始实现里的 x27 = key0 - 0x30，要求至少为 2
    if key0 < 0x32 || w22 < 2 || w22 > buf.len() as i32 - 1 {
        return None;
    }

    let mut x21 = buf[1..1 + w22 as usize].to_vec();
    let (mut w11, mut w12) = (0x55u8, 0xfau8);
    let w10 = 0xebu8;
    for i in 0..x21.len() {
        let w13 = x21[i];
        let pc = (i as u32).count_ones() as u8;
        let w14 = if i & 1 == 0 { w12 } else { w11 };
        if i & 1 == 1 {
            w11 = w13;
        } else {
            w12 = w13;
        }
        x21[i] = w10.wrapping_sub(pc).wrapping_add(w14 ^ w13);
    }

    let c0 = x21[0];
    let hval = match c0 {
        0x30..=0x39 => c0 - 0x30,
        0x61..=0x7a => c0 - 0x57,
        _ => return None,
    };
    let w9 = w22 - hval as i32;
    if w9 < 2 {
        return None;
    }
    let text = std::str::from_utf8(&x21[1..w9 as usize]).ok()?;
    if text.len() != 32 {
        return None;
    }

    let mut key = [0u8; 16];
    for (i, chunk) in text.as_bytes().chunks(2).enumerate() {
        key[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(key)
}

/// `video_list` 在接口里有两种形状：数组，或 `video_1` / `video_5` 这类对象。
fn video_list_items(model: &serde_json::Value) -> Vec<&serde_json::Value> {
    match model.get("video_list") {
        Some(serde_json::Value::Array(list)) => list.iter().collect(),
        Some(serde_json::Value::Object(map)) => map.values().collect(),
        _ => Vec::new(),
    }
}

/// 把 `video_list` 里的一条地址变成能直接 GET 的 https 链接。
///
/// 接口偶尔会把地址整个换成 **base64 字面量**（`aHR0cHM6...`），而且这种流
/// 不带 `video_meta` / `encrypt_info`。实测解出来是明文 HEVC，能正常下载；
/// 只认 `https://` 开头会把这一集直接丢掉 —— 《雨夜双星念》就因此缺了 3 集。
fn stream_url(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.starts_with("https://") {
        return Some(raw.to_string());
    }
    // 明文 http 不安全，也不该混进播放链路
    if raw.starts_with("http://") || raw.is_empty() {
        return None;
    }
    let decoded = String::from_utf8(base64_decode(raw)?).ok()?;
    let decoded = decoded.trim();
    decoded.starts_with("https://").then(|| decoded.to_string())
}

/// 把 `video_list` 摊平成可下载的流。
fn model_streams(model: &serde_json::Value) -> Vec<ModelStream> {
    let mut out = Vec::new();
    for item in video_list_items(model) {
        let meta = item.get("video_meta");
        let definition = meta
            .and_then(|m| m.get("definition"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let codec = meta
            .and_then(|m| m.get("codec_type"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();

        let enc = item.get("encrypt_info");
        let encrypted = enc
            .and_then(|e| e.get("encrypt"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let key = enc
            .and_then(|e| e.get("spade_a"))
            .and_then(|v| v.as_str())
            .and_then(derive_spade_key);
        // 加密但 key 派生失败的流，下载了也解不开
        if encrypted && key.is_none() {
            continue;
        }

        // main 域挂了还有 backup 域 —— 两条都留成候选，下载时按顺序试。
        // 以前一个 item 只取一条，正好碰上 main 超时就得整集重来。
        for field in ["main_url", "backup_url"] {
            let Some(url) = item
                .get(field)
                .and_then(|v| v.as_str())
                .and_then(stream_url)
            else {
                continue;
            };
            if out.iter().any(|s: &ModelStream| s.url == url) {
                continue;
            }
            out.push(ModelStream {
                url,
                definition: definition.clone(),
                codec: codec.clone(),
                encrypted,
                key,
            });
        }
    }
    out
}

/// 编码优先级：ffmpeg 能直接解 bytevc1 / hvc1 / h264；
/// bytevc2 是私有变体，只能补 fourcc 后碰运气，排最后。
fn codec_rank(codec: &str) -> u8 {
    let c = codec.to_ascii_lowercase();
    if c.contains("bytevc1") || c.contains("hvc1") || c.contains("h265") || c.contains("hevc") {
        0
    } else if c.contains("h264") || c.contains("avc") {
        1
    } else if c.contains("bytevc2") || c.contains("bvc2") {
        3
    } else {
        2
    }
}

/// 清晰度偏好：默认 480p —— 竖屏短剧够清楚，文件也只有十几 MB。
fn definition_rank(definition: &str) -> u8 {
    match definition.to_ascii_lowercase().as_str() {
        "480p" => 0,
        "360p" => 1,
        "540p" => 2,
        "720p" => 3,
        "1080p" => 4,
        _ => 5,
    }
}

/// 按「编码可用性 → 清晰度偏好」排序，返回依次尝试的流下标。
fn pick_model_streams(streams: &[ModelStream], preferred: Option<&str>) -> Vec<usize> {
    let mut order: Vec<usize> = (0..streams.len()).collect();
    order.sort_by_key(|&i| {
        let s = &streams[i];
        let wanted = preferred
            .map(|p| s.definition.eq_ignore_ascii_case(p))
            .unwrap_or(false);
        let def = if wanted {
            0
        } else {
            1 + definition_rank(&s.definition)
        };
        (codec_rank(&s.codec), def)
    });
    order
}

fn is_nonempty(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.len() > 0)
        .unwrap_or(false)
}

/// 只保留 ASCII 字母数字，避免把接口里的字符串直接拼进文件名。
fn safe_name(raw: &str) -> String {
    let out: String = raw.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if out.is_empty() {
        "x".into()
    } else {
        out
    }
}

/// `stsd` 里的 sample entry 格式从 `bvc2` 改成 `hvc1`，让 ffmpeg 用 HEVC 解码器试一把。
///
/// 只改 sample entry 的 fourcc，不动 `mdat`，避免误伤视频数据。
fn patch_bvc2_fourcc(path: &std::path::Path) -> AppResult<bool> {
    let mut data = std::fs::read(path)?;
    let mut changed = false;
    let mut i = 0usize;
    while i + 20 <= data.len() {
        if &data[i..i + 4] == b"stsd" {
            // type(4) version+flags(4) entry_count(4) entry_size(4) format(4)
            let format = i + 16;
            if &data[format..format + 4] == b"bvc2" {
                data[format..format + 4].copy_from_slice(b"hvc1");
                changed = true;
            }
            i = format + 4;
        } else {
            i += 1;
        }
    }
    if changed {
        std::fs::write(path, data)?;
    }
    Ok(changed)
}

/// 在线播放用的编码参数，比 [`crate::media::H264Encoder::quality_args`] 低一档。
///
/// 导出切片要保画质；在线看一集 1-2 分钟，还要下载和落盘缓存，
/// CRF 26 / CQ 27 在手机上看着几乎没区别，体积却能小一半。
fn playback_quality_args(encoder: crate::media::H264Encoder) -> Vec<String> {
    use crate::media::H264Encoder;
    match encoder {
        H264Encoder::Nvenc => vec!["-preset".into(), "p4".into(), "-cq".into(), "27".into()],
        H264Encoder::Qsv => vec!["-global_quality".into(), "26".into()],
        H264Encoder::Amf => vec![
            "-quality".into(),
            "balanced".into(),
            "-rc".into(),
            "cqp".into(),
            "-qp_i".into(),
            "26".into(),
            "-qp_p".into(),
            "26".into(),
        ],
        H264Encoder::X264 => vec![
            "-preset".into(),
            "veryfast".into(),
            "-crf".into(),
            "26".into(),
        ],
    }
}

/// 用 ffmpeg 把下载好的加密分片解出来，写到 `out`。
///
/// `copy_video` = 只解密不重编码：几秒就能落盘，产物保持平台原编码（多为 HEVC）。
/// 这条是给「下载后还要整剧处理」准备的 —— 去重那一步本来就要完整编码一次，
/// 下载时再转一遍 H.264 纯属重复劳动。
///
/// `copy` 失败会自动退回转码：个别流（比如 bytevc2）ffmpeg 只认 fourcc 不识内容，
/// 不能让整集因为「想省一次编码」而下载失败。
async fn decrypt_part(
    part: &std::path::Path,
    out: &std::path::Path,
    ffmpeg: &std::path::Path,
    key: Option<String>,
    copy_video: bool,
) -> AppResult<()> {
    let ffmpeg = ffmpeg.to_path_buf();
    let part_for_task = part.to_path_buf();
    let out_for_task = out.to_path_buf();
    let output = tauri::async_runtime::spawn_blocking(move || {
        let mut cmd = Command::new(&ffmpeg);
        proc::no_window(&mut cmd);
        if let Some(key) = &key {
            // 必须在 -i 之前，ffmpeg 才会按 CENC 解密输入
            cmd.arg("-decryption_key").arg(key);
        }
        cmd.args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(&part_for_task)
            .args(["-map", "0:v:0", "-map", "0:a:0?"]);
        if copy_video {
            // 只解密、不重编码 —— 下载提速的关键
            cmd.args(["-c:v", "copy", "-c:a", "copy"]);
        } else {
            // 探测编码器要真编一帧（阻塞），只在真要转码时才做
            let encoder = crate::media::pick_h264_encoder(&ffmpeg);
            cmd.args(["-c:v", encoder.name()])
                .args(playback_quality_args(encoder))
                .args([
                    "-pix_fmt",
                    "yuv420p",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "128k",
                ]);
        }
        cmd.args(["-movflags", "+faststart"])
            .arg(&out_for_task)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.output()
    })
    .await
    .map_err(|e| AppError::Msg(format!("解密任务异常：{e}")))?
    .map_err(|e| AppError::Msg(format!("调用 ffmpeg 失败：{e}")))?;

    if !output.status.success() {
        let detail = proc::tail(&String::from_utf8_lossy(&output.stderr));
        return Err(AppError::Msg(format!(
            "{}失败：{detail}",
            if copy_video { "解密直存" } else { "解密 / 转码" }
        )));
    }
    if !is_nonempty(out) {
        let _ = std::fs::remove_file(out);
        return Err(AppError::Msg("解密失败：没有产出文件".into()));
    }
    Ok(())
}

/// 下载一条流并解密，写到 `out`。
///
/// 返回 true 表示只做了解密（原编码），false 表示转成了 H.264。
async fn fetch_and_decrypt(
    stream: &ModelStream,
    out: &std::path::Path,
    ffmpeg: &std::path::Path,
    copy_video: bool,
) -> AppResult<bool> {
    let resp = client()?
        .get(&stream.url)
        .timeout(Duration::from_secs(300))
        .send()
        .await
        .map_err(|e| AppError::Msg(format!("下载失败：{e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Msg(format!("下载失败：HTTP {}", resp.status())));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::Msg(format!("读取视频数据失败：{e}")))?;
    if bytes.is_empty() {
        return Err(AppError::Msg("下载到的视频是空的".into()));
    }

    let part = out.with_extension("part.mp4");
    std::fs::write(&part, &bytes)?;
    if codec_rank(&stream.codec) == 3 {
        if let Ok(true) = patch_bvc2_fourcc(&part) {
            // 补过 fourcc 再试，成功与否交给 ffmpeg 判断
        }
    }

    let key = if stream.encrypted {
        stream
            .key
            .map(|k| k.iter().map(|b| format!("{b:02x}")).collect::<String>())
    } else {
        None
    };
    // 先写临时文件，成功后再改名，中途失败不会留下“看起来有效”的缓存
    let tmp = out.with_extension("tmp.mp4");

    // 快速模式先试「只解密」；解不开再退回转码（少数私有编码需要重编码才能识别）
    let modes: &[bool] = if copy_video { &[true, false] } else { &[false] };
    let mut last_error = String::new();
    for &copy in modes {
        let _ = std::fs::remove_file(&tmp);
        match decrypt_part(&part, &tmp, ffmpeg, key.clone(), copy).await {
            Ok(()) => {
                if let Err(e) = std::fs::rename(&tmp, out) {
                    let _ = std::fs::remove_file(&tmp);
                    let _ = std::fs::remove_file(&part);
                    return Err(AppError::Msg(format!("写入视频缓存失败：{e}")));
                }
                // 加密源只是中间产物，成不成都删掉
                let _ = std::fs::remove_file(&part);
                return Ok(copy);
            }
            Err(e) => last_error = e.to_string(),
        }
    }

    let _ = std::fs::remove_file(&part);
    let _ = std::fs::remove_file(&tmp);
    Err(AppError::Msg(last_error))
}

/// 准备一集的本地文件：下载 App 流 → 派生 key → ffmpeg 解密。
///
/// `copy_video`：true = 只解密不转码（下载用，快，产物是平台原编码）；
/// false = 解密并转成 H.264（在线播放用，WebView2 直接能播）。
///
/// 官网只公开前 50 集且部分文件缺失，所以播放主链路走这里；
/// 前端失败时才退回 [`play_url`] 的官网明文流。
/// 同一条流失败会自动换下一条（比如 bytevc2 解不了就换 bytevc1）。
pub async fn prepare_video(
    app: &tauri::AppHandle,
    vid: &str,
    quality: Option<&str>,
    ffmpeg: Option<PathBuf>,
    copy_video: bool,
) -> AppResult<FanqiePreparedVideo> {
    let vid = vid.trim();
    if !valid_id(vid) {
        return Err(AppError::Msg("视频 ID 不合法".into()));
    }
    let ffmpeg = ffmpeg.ok_or_else(|| AppError::Msg("找不到 ffmpeg，无法解密视频".into()))?;

    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| AppError::Msg(format!("找不到缓存目录：{e}")))?
        .join("fanqie_videos");
    std::fs::create_dir_all(&dir)?;

    let q = quality.map(str::trim).filter(|q| !q.is_empty());
    // H.264 缓存：播放用；下载时也优先用它 —— 有就直接拷，连解密都省。
    let h264 = match q {
        Some(q) => dir.join(format!("{vid}_{}.mp4", safe_name(q))),
        None => dir.join(format!("{vid}.mp4")),
    };
    // 只解密不转码的缓存：平台原编码（多为 HEVC），给下载用。
    let raw = match q {
        Some(q) => dir.join(format!("{vid}_{}_raw.mp4", safe_name(q))),
        None => dir.join(format!("{vid}_raw.mp4")),
    };
    if is_nonempty(&h264) {
        return Ok(FanqiePreparedVideo {
            vid: vid.to_string(),
            path: h264.to_string_lossy().to_string(),
            // 缓存里没存元信息，清晰度交给前端用「本地缓存」显示
            quality: String::new(),
            codec: "h264".into(),
            duration_sec: 0.0,
            source: "cache".into(),
        });
    }
    if copy_video && is_nonempty(&raw) {
        return Ok(FanqiePreparedVideo {
            vid: vid.to_string(),
            path: raw.to_string_lossy().to_string(),
            quality: String::new(),
            codec: "source".into(),
            duration_sec: 0.0,
            source: "cache".into(),
        });
    }

    let model = fetch_video_model(vid).await?;
    let duration_sec = model
        .get("video_duration")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let streams = model_streams(&model);
    if streams.is_empty() {
        return Err(AppError::Msg("视频详情里没有可下载的流".into()));
    }

    let out = if copy_video { &raw } else { &h264 };
    let mut last_error = String::new();
    for idx in pick_model_streams(&streams, quality) {
        let stream = &streams[idx];
        match fetch_and_decrypt(stream, out, &ffmpeg, copy_video).await {
            Ok(copied) => {
                // 本想只解密、结果退回了转码：产物就是 H.264，
                // 挪回标准缓存名，下次播放直接命中，不用再转一遍
                let path = if copy_video && !copied {
                    if std::fs::rename(&raw, &h264).is_ok() {
                        h264.clone()
                    } else {
                        // 目标处可能有上次留下的空壳，Windows 下 rename 不覆盖；
                        // 还不行就直接用 raw —— 内容本身就是 H.264，播放不受影响
                        let _ = std::fs::remove_file(&h264);
                        if std::fs::rename(&raw, &h264).is_ok() {
                            h264.clone()
                        } else {
                            raw.clone()
                        }
                    }
                } else {
                    out.clone()
                };
                return Ok(FanqiePreparedVideo {
                    vid: vid.to_string(),
                    path: path.to_string_lossy().to_string(),
                    quality: stream.definition.clone(),
                    codec: if copy_video && copied {
                        "source".into()
                    } else {
                        "h264".into()
                    },
                    duration_sec,
                    source: "app".into(),
                });
            }
            Err(e) => {
                last_error = format!("{} {}（{}）", stream.definition, stream.codec, e);
            }
        }
    }

    Err(AppError::Msg(format!(
        "没有能解开的视频流：{last_error}"
    )))
}

// ---------------- 批量下载到流水线目录 ----------------

/// Windows 文件名里不能出现的字符统一换成下划线，并限制长度。
pub fn safe_file_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned
        .trim()
        .trim_end_matches(|c| c == '.' || c == ' ')
        .trim();
    let short: String = trimmed.chars().take(80).collect();
    if short.is_empty() {
        "未命名短剧".into()
    } else {
        short
    }
}

/// 数一个剧目录里平铺的 mp4（`二创/` 里的产出不算）。
pub fn count_episodes(dir: &Path) -> i64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| {
            let p = e.path();
            p.is_file()
                && p.extension()
                    .map(|x| x.eq_ignore_ascii_case("mp4"))
                    .unwrap_or(false)
        })
        .count() as i64
}

/// 一集最多试几次。
///
/// 番茄对连续请求限流时会直接给空流或换一种返回形态，隔一会儿再拉
/// 往往就正常了 —— 实测单独重跑那些「失败」的集全都能下，所以必须重试，
/// 否则用户看到的后果就是「缺了好几集」。
const DOWNLOAD_ATTEMPTS: usize = 3;
const DOWNLOAD_RETRY_WAIT_MS: u64 = 1500;

/// 一部剧内同时下载几集。
///
/// 每集都是「取流 → ffmpeg 解密转码 → 落盘」，串行时绝大部分时间在等 ffmpeg。
/// 3 路能明显缩短整部剧的等待；再往上会和转码抢 CPU（ffmpeg 本身吃满核），
/// 也更容易被番茄限流，反而更慢。
const DOWNLOAD_CONCURRENCY: usize = 3;

/// 把下载过程写进运行日志（`run_log`）。
///
/// 用户要的：「下载的情况也加进日志……保存路径、下载到第几集了」。
/// `artifact` 传本集的 mp4 路径，日志页那一行就能直接点开播 ——
/// 和流水线产物一样，「发生过什么」和「东西在哪」写在同一条记录里。
///
/// 日志写不进去不能影响下载：拿不到库就直接跳过。
fn log_download(app: &tauri::AppHandle, level: &str, message: &str, artifact: &str) {
    let Some(state) = app.try_state::<crate::AppState>() else {
        return;
    };
    let Ok(conn) = state.db.lock() else { return };
    crate::db::append_log_with_file(&conn, None, None, level, message, artifact);
}

/// 日志里报文件大小用。拿不到就算了，不能因为格式化把下载弄断。
fn size_label(path: &std::path::Path) -> String {
    match std::fs::metadata(path) {
        Ok(m) => format!("{:.1} MB", m.len() as f64 / 1024.0 / 1024.0),
        Err(_) => "大小未知".into(),
    }
}

/// 按文件头认图片格式。
///
/// 不靠 URL 后缀 —— 番茄图床的签名地址经常没有后缀，
/// 而且同一个地址也可能返回 HEIC。
fn image_kind(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some("jpg");
    }
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some("png");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        return Some("heic");
    }
    None
}

/// 封面只可能来自番茄自己的图床：App 详情给的是 `*.fqnovelpic.com`，
/// 官网页面给的是 `*.byteimg.com`。只放行 https + 这几个域名，
/// 避免这个命令变成「打任意内网地址」的入口。
fn is_fanqie_image_url(url: &reqwest::Url) -> bool {
    if url.scheme() != "https" {
        return false;
    }
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    ["fqnovelpic.com", "byteimg.com"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

/// 下载封面，落成素材包里的 `poster.jpg`。
///
/// JPEG 直接写；PNG / WEBP 按原格式落盘（流水线认扩展名，ffmpeg 也读得了）；
/// HEIC 必须过一道 ffmpeg 转 JPEG —— WebView 显示不了，用户也没法直接用。
async fn save_cover(
    url: &str,
    dir: &std::path::Path,
    ffmpeg: Option<PathBuf>,
) -> AppResult<std::path::PathBuf> {
    let parsed = reqwest::Url::parse(url).map_err(|_| AppError::Msg("封面地址不合法".into()))?;
    if !is_fanqie_image_url(&parsed) {
        return Err(AppError::Msg("只接受番茄图床的 https 封面地址".into()));
    }

    let resp = client()?
        .get(url)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| AppError::Msg(format!("封面下载失败：{e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Msg(format!("封面下载失败：HTTP {}", resp.status())));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::Msg(format!("读取封面失败：{e}")))?;
    if bytes.is_empty() {
        return Err(AppError::Msg("封面是空的".into()));
    }

    let kind = image_kind(&bytes);
    if let Some(ext @ ("jpg" | "png" | "webp")) = kind {
        let dest = dir.join(format!("poster.{ext}"));
        std::fs::write(&dest, &bytes)?;
        return Ok(dest);
    }

    // HEIC / 认不出的格式：交给 ffmpeg，它能按内容自动识别
    let ffmpeg = ffmpeg.ok_or_else(|| AppError::Msg("封面是 HEIC，需要 ffmpeg 转成 JPEG".into()))?;
    let src = dir.join("poster.src");
    let dest = dir.join("poster.jpg");
    std::fs::write(&src, &bytes)?;
    let src_task = src.clone();
    let dest_task = dest.clone();
    let output = tauri::async_runtime::spawn_blocking(move || {
        let mut cmd = Command::new(&ffmpeg);
        proc::no_window(&mut cmd);
        cmd.args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(&src_task)
            .args(["-frames:v", "1", "-q:v", "2"])
            .arg(&dest_task)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.output()
    })
    .await
    .map_err(|e| AppError::Msg(format!("封面转换任务异常：{e}")))?
    .map_err(|e| AppError::Msg(format!("调用 ffmpeg 失败：{e}")))?;
    let _ = std::fs::remove_file(&src);
    if !output.status.success() {
        let _ = std::fs::remove_file(&dest);
        return Err(AppError::Msg(format!(
            "封面转 JPEG 失败：{}",
            proc::tail(&String::from_utf8_lossy(&output.stderr))
        )));
    }
    if !is_nonempty(&dest) {
        return Err(AppError::Msg("封面转 JPEG 失败：没有产出文件".into()));
    }
    Ok(dest)
}

/// 把一部剧的全部分集下载到流水线源目录。
///
/// 目录结构按用户要的素材包来（也和 `pipeline::scan_source` 认的一致）：
///
/// ```text
/// <root>/<剧名>/
///     剧名.txt
///     intro.txt      ← 简介：首行剧名、空行、正文（pipeline 认它当简介）
///     poster.jpg     ← 封面（pipeline 认 poster / 封面）
///     第01集.mp4
///     第02集.mp4
///     二创/          ← 流水线产出（每部剧自己的）
/// ```
///
/// 每完成一集发一条 `fanqie:download`，前端用它显示进度。
///
/// `copy_video`：true = 只解密不转码（推荐）。下载阶段几乎不占 CPU，
/// 唯一的一次 H.264 编码留给整剧处理的「整集去重」—— 反正那一步本来就要完整编码。
pub async fn download_series(
    app: &tauri::AppHandle,
    req: &FanqieDownloadRequest,
    out_root: &std::path::Path,
    quality: Option<&str>,
    ffmpeg: Option<PathBuf>,
    copy_video: bool,
) -> AppResult<FanqieDownloadReport> {
    use tauri::Emitter;

    let sid = req.series_id.trim();
    if !valid_id(sid) {
        return Err(AppError::Msg("剧集 ID 不合法".into()));
    }
    if req.episodes.is_empty() {
        return Err(AppError::Msg("这部剧没有可下载的分集".into()));
    }
    let title = safe_file_name(if req.title.trim().is_empty() {
        "未命名短剧"
    } else {
        req.title.trim()
    });
    let dir = out_root.join(&title);
    std::fs::create_dir_all(&dir)?;
    // 产出目录先建出来：用户一眼就能看到「源片 + 二创」的结构
    std::fs::create_dir_all(dir.join(crate::pipeline::OUTPUT_DIR_NAME))?;

    // 素材包里先有剧名、简介、封面：流水线靠 `intro.txt` 拿剧情、
    // 靠 `poster.jpg` 做新封面。先写这两个文件，中途断了也不至于空白。
    std::fs::write(dir.join("剧名.txt"), &title)?;
    // 简介按用户素材包的叫法用 `intro.txt`；旧版本写的是 `简介.txt`，
    // 直接改名，别留两个简介互相打架。
    let intro_path = dir.join("intro.txt");
    let legacy_intro = dir.join("简介.txt");
    if !intro_path.is_file() && legacy_intro.is_file() {
        let _ = std::fs::rename(&legacy_intro, &intro_path);
    }
    // 用户可能改过简介，已经有内容就不覆盖。
    // 格式照素材包来：首行剧名、空行、正文 —— pipeline 会跳过首行。
    if !is_nonempty(&intro_path) {
        std::fs::write(&intro_path, format!("{title}\n\n{}", req.intro.trim()))?;
    }

    let mut episodes = req.episodes.clone();
    episodes.sort_by_key(|e| e.index);
    let total = episodes.len() as i64;

    log_download(
        app,
        "info",
        &format!(
            "下载《{title}》开始：共 {total} 集，保存到 {}（{}，已写 剧名.txt / intro.txt）",
            dir.to_string_lossy(),
            if copy_video {
                "只解密不转码，省下的一次编码留给整剧去重"
            } else {
                "解密并转成 H.264"
            }
        ),
        "",
    );

    // 封面：素材包叫 poster.jpg（pipeline 靠名字认）。
    // 已经有了就不动 —— 用户可能自己换过封面。
    let existing_poster = ["poster.jpg", "poster.jpeg", "poster.png", "poster.webp"]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| is_nonempty(p));
    match existing_poster {
        Some(p) => log_download(
            app,
            "info",
            &format!(
                "封面已存在，跳过：{}",
                p.file_name().unwrap_or_default().to_string_lossy()
            ),
            "",
        ),
        None if req.cover.trim().is_empty() => log_download(
            app,
            "warn",
            "没有拿到封面地址，这次不写 poster.jpg",
            "",
        ),
        None => match save_cover(req.cover.trim(), &dir, ffmpeg.clone()).await {
            Ok(p) => log_download(
                app,
                "ok",
                &format!(
                    "封面已保存：{}（{}）",
                    p.file_name().unwrap_or_default().to_string_lossy(),
                    size_label(&p)
                ),
                "",
            ),
            Err(e) => log_download(
                app,
                "warn",
                &format!("封面没下下来（不影响原片）：{e}"),
                "",
            ),
        },
    }

    // 一部剧内并行下载。串行时每集都要等完整个「取流 → ffmpeg 解密转码」，
    // 而这几步里 ffmpeg 并不会把网络和磁盘占满 —— 3 路同时跑才吃得下带宽和 CPU。
    let concurrency = DOWNLOAD_CONCURRENCY.min(episodes.len()).max(1);
    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));
    let done_downloaded = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let done_skipped = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let done_failed = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let mut tasks = tokio::task::JoinSet::new();

    for ep in episodes.iter() {
        let app = app.clone();
        let title = title.clone();
        let dir = dir.clone();
        let quality = quality.map(str::to_string);
        let ffmpeg = ffmpeg.clone();
        let sem = sem.clone();
        let done_downloaded = done_downloaded.clone();
        let done_skipped = done_skipped.clone();
        let done_failed = done_failed.clone();
        let sid = sid.to_string();
        let vid = ep.vid.clone();
        let index = ep.index.max(1);

        tasks.spawn(async move {
            use std::sync::atomic::Ordering;

            // 排队等一个并发名额；前面的集下完自动补位
            let _permit = sem.acquire_owned().await;

            let file = dir.join(format!("第{index:02}集.mp4"));
            // 旧版本落盘叫 `<剧名>-第01集.mp4`。已经下好的直接改名，
            // 不让用户为了一次命名调整把整部剧重下一遍。
            let legacy = dir.join(format!("{title}-第{index:02}集.mp4"));
            if !file.exists() && is_nonempty(&legacy) {
                let _ = std::fs::rename(&legacy, &file);
            }

            // 进度按「已处理完的集数」算：并发完成顺序是乱的，
            // 用它前端进度才单调，不会忽上忽下。
            let progress = || {
                done_downloaded.load(Ordering::Relaxed)
                    + done_skipped.load(Ordering::Relaxed)
                    + done_failed.load(Ordering::Relaxed)
            };
            let emit = |message: &str| {
                let _ = app.emit(
                    "fanqie:download",
                    serde_json::json!({
                        "seriesId": &sid,
                        "title": &title,
                        "index": progress(),
                        "episodeIndex": index,
                        "total": total,
                        "phase": "episode",
                        "message": message,
                        "downloaded": done_downloaded.load(Ordering::Relaxed),
                        "skipped": done_skipped.load(Ordering::Relaxed),
                        "failed": done_failed.load(Ordering::Relaxed),
                    }),
                );
            };

            if is_nonempty(&file) {
                done_skipped.fetch_add(1, Ordering::Relaxed);
                let message = format!("第 {index} 集已存在，跳过");
                log_download(
                    &app,
                    "info",
                    &format!("{message}（{}/{total}）：{}（{}）", progress(), file.file_name().unwrap_or_default().to_string_lossy(), size_label(&file)),
                    &file.to_string_lossy(),
                );
                emit(&message);
                return (index, None);
            }

            // 开始那一行也要写：并发时哪几集在跑，看日志最清楚。
            log_download(&app, "info", &format!("第 {index} 集开始下载…"), "");
            emit(&format!("第 {index} 集开始下载…"));

            let mut last_error = String::new();
            let mut ok = false;
            for attempt in 0..DOWNLOAD_ATTEMPTS {
                if attempt > 0 {
                    log_download(
                        &app,
                        "warn",
                        &format!("第 {index} 集第 {attempt} 次重试…"),
                        "",
                    );
                    tokio::time::sleep(Duration::from_millis(DOWNLOAD_RETRY_WAIT_MS)).await;
                }
                match prepare_video(
                    &app,
                    &vid,
                    quality.as_deref(),
                    ffmpeg.clone(),
                    copy_video,
                )
                .await
                {
                    Ok(prepared) => match std::fs::copy(&prepared.path, &file) {
                        Ok(_) => {
                            ok = true;
                            break;
                        }
                        // 磁盘写失败重试也没用，直接报；
                        // 清掉可能写了一半的文件，下次不会把它当成已下载
                        Err(e) => {
                            let _ = std::fs::remove_file(&file);
                            last_error = format!("拷贝失败（{e}）");
                            break;
                        }
                    },
                    Err(e) => last_error = e.to_string(),
                }
            }

            if ok {
                done_downloaded.fetch_add(1, Ordering::Relaxed);
                let message = format!("第 {index} 集完成");
                log_download(
                    &app,
                    "ok",
                    &format!("{message}（{}/{total}）→ {}（{}）", progress(), file.file_name().unwrap_or_default().to_string_lossy(), size_label(&file)),
                    &file.to_string_lossy(),
                );
                emit(&message);
                (index, None)
            } else {
                done_failed.fetch_add(1, Ordering::Relaxed);
                let message = format!("第 {index} 集：{last_error}");
                log_download(&app, "error", &format!("{message}（{}/{total}）", progress()), "");
                emit(&message);
                (index, Some(message))
            }
        });
    }

    // 等所有集跑完。失败按集号排回去 —— 并发完成顺序是乱的，
    // 报告/提示里按集数读起来才顺。
    let mut failures: Vec<(i64, String)> = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((index, Some(message))) => failures.push((index, message)),
            Ok((_, None)) => {}
            Err(e) => failures.push((0, format!("某集下载任务异常退出：{e}"))),
        }
    }
    failures.sort_by_key(|(index, _)| *index);
    let failed: Vec<String> = failures.into_iter().map(|(_, message)| message).collect();
    let downloaded = done_downloaded.load(std::sync::atomic::Ordering::Relaxed);
    let skipped = done_skipped.load(std::sync::atomic::Ordering::Relaxed);

    let _ = app.emit(
        "fanqie:download",
        serde_json::json!({
            "seriesId": sid,
            "title": title,
            "index": total,
            "episodeIndex": episodes.last().map(|e| e.index).unwrap_or(0),
            "total": total,
            "phase": "series",
            "message": format!("《{title}》下载完成"),
            "downloaded": downloaded,
            "skipped": skipped,
            "failed": failed.len(),
        }),
    );

    log_download(
        app,
        if failed.is_empty() { "ok" } else { "warn" },
        &format!(
            "《{title}》下载结束：新下 {downloaded} 集、跳过 {skipped} 集、失败 {} 集。保存路径：{}",
            failed.len(),
            dir.to_string_lossy()
        ),
        "",
    );

    Ok(FanqieDownloadReport {
        series_id: sid.to_string(),
        title,
        dir: dir.to_string_lossy().to_string(),
        downloaded,
        skipped,
        failed,
    })
}

// ---------------- 封面：HEIC → JPEG ----------------

/// FNV-1a。只用来把长 URL 变成稳定的文件名，不参与任何安全判断。
fn hash_url(url: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in url.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn is_heic(url: &str) -> bool {
    reqwest::Url::parse(url)
        .map(|u| u.path().to_ascii_lowercase().ends_with(".heic"))
        .unwrap_or(false)
}

/// 把非 HEIC 的封面原样返回；HEIC 的下载并转成 JPEG，返回本地缓存路径。
///
/// 只接受番茄图床的 https 地址 —— 这个命令会被前端用任意字符串调用，
/// 不能让它变成「读任意文件 / 打任意内网地址」的入口。
pub async fn cover_jpeg(
    app: &tauri::AppHandle,
    url: &str,
    ffmpeg: Option<PathBuf>,
) -> AppResult<String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| AppError::Msg("封面地址不合法".into()))?;
    if !is_fanqie_image_url(&parsed) {
        return Err(AppError::Msg("只接受番茄图床的 https 封面地址".into()));
    }
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    if !is_heic(url) {
        return Ok(url.to_string());
    }

    let dir = app
        .path()
        .app_cache_dir()
        .map_err(|e| AppError::Msg(format!("找不到缓存目录：{e}")))?
        .join("fanqie_covers");
    std::fs::create_dir_all(&dir)?;

    // 签名会随过期时间变，但图床路径不变 —— 按「域名 + 路径」缓存，
    // 同一张图换了签名也能命中，不会越存越多。
    let key = hash_url(&format!("{host}{}", parsed.path()));
    let jpg = dir.join(format!("{key}.jpg"));
    if jpg.is_file() {
        if let Ok(meta) = std::fs::metadata(&jpg) {
            if meta.len() > 0 {
                return Ok(jpg.to_string_lossy().to_string());
            }
        }
    }

    let ffmpeg = ffmpeg.ok_or_else(|| AppError::Msg("找不到 ffmpeg，无法转换 HEIC 封面".into()))?;

    let resp = client()?
        .get(url)
        .send()
        .await
        .map_err(|e| AppError::Msg(format!("下载封面失败：{e}")))?;
    if !resp.status().is_success() {
        return Err(AppError::Msg(format!(
            "下载封面失败：HTTP {}",
            resp.status()
        )));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::Msg(format!("读取封面数据失败：{e}")))?;
    if bytes.is_empty() {
        return Err(AppError::Msg("封面数据是空的".into()));
    }

    let src = dir.join(format!("{key}.heic"));
    std::fs::write(&src, &bytes)?;

    let src_for_task = src.clone();
    let jpg_for_task = jpg.clone();
    let output = tauri::async_runtime::spawn_blocking(move || {
        let mut cmd = Command::new(ffmpeg);
        proc::no_window(&mut cmd);
        cmd.args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(&src_for_task)
            .args(["-frames:v", "1", "-q:v", "3"])
            .arg(&jpg_for_task)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd.output()
    })
    .await
    .map_err(|e| AppError::Msg(format!("封面转换任务异常：{e}")))?;
    let output = output.map_err(|e| AppError::Msg(format!("调用 ffmpeg 失败：{e}")))?;

    // 源文件只是转换的中间产物，成不成都删掉
    let _ = std::fs::remove_file(&src);

    if !output.status.success() {
        let _ = std::fs::remove_file(&jpg);
        let detail = proc::tail(&String::from_utf8_lossy(&output.stderr));
        return Err(AppError::Msg(format!("封面转换失败：{detail}")));
    }
    if !jpg.is_file() {
        return Err(AppError::Msg("封面转换失败：没有产出文件".into()));
    }
    Ok(jpg.to_string_lossy().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> RawItem {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn convert_picks_heat_and_category_tags() {
        let raw = parse(
            r#"{
                "vid": "1", "series_id": "2", "title": " 暗夜 ",
                "video_desc": "简介", "cover": "https://x.heic",
                "duration": 2177, "play_cnt": 19445, "score": "8.0",
                "episode_cnt": 30,
                "sub_title_list": [
                    {"content": "1578万", "data_type": 27},
                    {"content": "悬疑", "data_type": 3}
                ]
            }"#,
        );
        let item = convert(&raw);
        assert_eq!(item.title, "暗夜");
        assert_eq!(item.heat, "1578万");
        assert_eq!(item.tags, vec!["悬疑".to_string()]);
        assert_eq!(item.duration_sec, 2177);
        assert_eq!(item.episode_count, 30);
        assert!(!item.is_new);
    }

    #[test]
    fn convert_puts_the_new_badge_first_and_dedupes_tags() {
        let raw = parse(
            r#"{
                "title": "新剧", "episode_cnt": 73,
                "tag_info": {"text": "新剧"},
                "sub_title_list": [
                    {"content": "爱情", "data_type": 3},
                    {"content": "爱情", "data_type": 3},
                    {"content": "6266万", "data_type": 27}
                ]
            }"#,
        );
        let item = convert(&raw);
        assert!(item.is_new);
        assert_eq!(item.tags, vec!["新剧".to_string(), "爱情".to_string()]);
    }

    #[test]
    fn missing_optional_fields_still_parse() {
        let raw = parse(r#"{"title": "只有标题"}"#);
        let item = convert(&raw);
        assert_eq!(item.title, "只有标题");
        assert_eq!(item.play_count, 0);
        assert!(item.heat.is_empty());
        assert!(item.tags.is_empty());
    }

    #[test]
    fn shelf_page_carries_cursor_and_items() {
        let env: Envelope<ChangeData> = serde_json::from_str(
            r#"{
                "code": 0, "message": "SUCCESS",
                "data": {
                    "cell_view": {"video_data": [{"vid": "v1", "title": "剧一"}]},
                    "has_more": true, "next_offset": 20, "session_id": "sid"
                }
            }"#,
        )
        .unwrap();
        let data = env.data.unwrap();
        assert!(data.has_more);
        assert_eq!(data.next_offset, 20);
        assert_eq!(data.session_id, "sid");
        let items = data.cell_view.map(|c| c.video_data).unwrap_or_default();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].vid, "v1");
    }

    #[test]
    fn shelf_page_without_cell_view_is_empty() {
        let env: Envelope<ChangeData> = serde_json::from_str(
            r#"{
                "code": 0, "message": "SUCCESS",
                "data": {"has_more": false, "next_offset": 0, "session_id": ""}
            }"#,
        )
        .unwrap();
        let data = env.data.unwrap();
        assert!(data.cell_view.is_none());
        assert!(!data.has_more);
    }

    #[test]
    fn select_items_defaults_to_online_time_and_scene() {
        let (items, scene) = normalize_select_items(FanqieSelectItems::default());
        assert_eq!(items.sort, vec!["online_time".to_string()]);
        assert_eq!(scene, "default");

        let (items, scene) = normalize_select_items(FanqieSelectItems {
            genre: vec!["comic_series".to_string()],
            ..Default::default()
        });
        assert_eq!(items.sort, vec!["online_time".to_string()]);
        assert_eq!(scene, "comic_series");
        assert_eq!(scene_for("ai_series"), "ai_series");
    }

    #[test]
    fn selector_panel_parses_rows_and_items() {
        let env: Envelope<SelectorData> = serde_json::from_str(
            r#"{
                "code": 0, "message": "SUCCESS",
                "data": {
                    "selector_rows": [
                        {"type": "genre", "row_name": "全部体裁",
                         "items": [{"selector_item_id": "short_play", "show_name": "真人剧"}]},
                        {"type": "empty", "row_name": "空", "items": []}
                    ]
                }
            }"#,
        )
        .unwrap();
        let rows = env.data.unwrap().selector_rows;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, "genre");
        assert_eq!(rows[0].row_name, "全部体裁");
        assert_eq!(rows[0].items[0].selector_item_id, "short_play");
        assert_eq!(rows[0].items[0].show_name, "真人剧");
    }

    #[test]
    fn heic_detection_ignores_query_string() {
        assert!(is_heic("https://a.fqnovelpic.com/x.heic?sign=1"));
        assert!(!is_heic("https://a.fqnovelpic.com/x.image?sign=1"));
        assert!(!is_heic("not a url"));
    }

    #[test]
    fn image_kind_reads_magic_not_extension() {
        // 番茄图床的签名地址常常没有后缀，格式只能靠文件头判断
        assert_eq!(image_kind(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10]), Some("jpg"));
        assert_eq!(
            image_kind(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00]),
            Some("png")
        );
        let webp = b"RIFF\x24\x00\x00\x00WEBPVP8 ".to_vec();
        assert_eq!(image_kind(&webp), Some("webp"));
        let mut heic = [0u8; 16];
        heic[4..8].copy_from_slice(b"ftyp");
        heic[8..12].copy_from_slice(b"heic");
        assert_eq!(image_kind(&heic), Some("heic"));
        // HTML 错误页 / 空响应不能被当成图片写进 poster.jpg
        assert_eq!(image_kind(b"<html>404</html>"), None);
        assert_eq!(image_kind(b""), None);
        // 不足 12 字节的 RIFF 不能越界
        assert_eq!(image_kind(b"RIFF"), None);
    }

    #[test]
    fn cover_hosts_are_limited_to_fanqie_cdn() {
        let ok = |u: &str| is_fanqie_image_url(&reqwest::Url::parse(u).unwrap());
        assert!(ok("https://p6-novel.byteimg.com/cover.image"));
        assert!(ok("https://a.fqnovelpic.com/x.heic"));
        // http、内网、伪装域名都不能放行
        assert!(!ok("http://p6-novel.byteimg.com/cover.image"));
        assert!(!ok("https://127.0.0.1/a.jpg"));
        assert!(!ok("https://evil.com.byteimg.com.attacker.net/a.jpg"));
        assert!(!ok("https://notbyteimg.com/a.jpg"));
    }

    #[test]
    fn cover_hash_is_stable_and_url_sensitive() {
        let a = hash_url("https://a/x.heic");
        assert_eq!(a, hash_url("https://a/x.heic"));
        assert_ne!(a, hash_url("https://a/y.heic"));
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn unescape_handles_unicode_slashes_and_html_amp() {
        assert_eq!(
            unescape_json_string(r"https:\u002F\u002Fa.com\u002Fv.mp4?a=1\u0026b=2&amp;c=3"),
            "https://a.com/v.mp4?a=1&b=2&c=3"
        );
        assert_eq!(unescape_json_string(r#"\"引号\""#), "\"引号\"");
    }

    #[test]
    fn media_url_drops_range_param() {
        assert_eq!(
            normalize_media_url("https://a/v.mp4?Range=0-99&x=1"),
            "https://a/v.mp4?x=1"
        );
        assert_eq!(
            normalize_media_url("https://a/v.mp4?Range=0-99"),
            "https://a/v.mp4"
        );
    }

    #[test]
    fn series_page_parses_episode_list_and_meta() {
        let html = r#"
            <script>window.__DATA__ = {
              "series_name":"测试剧",
              "series_intro":"一句简介",
              "series_cover":"https:\u002F\u002Fp6-novel.byteimg.com\u002Fcover.image",
              "vid_list":["7696498819780840473","7696498859899358233","bad"]
            };</script>
        "#;
        let detail = parse_series_page(html, "7696496958919167001").unwrap();
        assert_eq!(detail.title, "测试剧");
        assert_eq!(detail.intro, "一句简介");
        assert_eq!(detail.cover, "https://p6-novel.byteimg.com/cover.image");
        assert_eq!(detail.source, "web");
        assert_eq!(detail.episode_count, 2);
        assert_eq!(detail.episodes[0].vid, "7696498819780840473");
        assert_eq!(detail.episodes[1].index, 2);
        assert_eq!(detail.episodes[1].title, "第 2 集");
    }

    #[test]
    fn app_series_uses_vid_index_and_duration() {
        let data: serde_json::Value = serde_json::from_str(
            r#"{
                "7696496958919167001": {
                    "video_data": {
                        "series_id": "7696496958919167001",
                        "series_title": "测试剧",
                        "series_intro": "一句简介",
                        "series_cover": "https://a.heic",
                        "episode_cnt": 2,
                        "video_list": [
                            {"vid": "111", "vid_index": 1, "title": "一句简介", "duration": 66},
                            {"vid": "222", "vid_index": 2, "title": "", "duration": 70},
                            {"vid": "111", "vid_index": 1, "title": "", "duration": 66}
                        ]
                    }
                }
            }"#,
        )
        .unwrap();
        let detail = parse_app_series(&data, "7696496958919167001").unwrap();
        assert_eq!(detail.source, "app");
        assert_eq!(detail.episode_count, 2);
        assert_eq!(detail.episodes.len(), 2);
        // 简介被塞进 title 时换成集号
        assert_eq!(detail.episodes[0].title, "第 1 集");
        assert_eq!(detail.episodes[0].duration_sec, 66);
        assert_eq!(detail.episodes[1].title, "第 2 集");
    }

    #[test]
    fn play_page_extracts_plain_mp4_url() {
        let html = r#"{"main_url":"https:\u002F\u002Fhongguoduanju.com\u002Fvideo\u002Fpublic\u002F123\u002F360p.mp4?Range=0-1"}"#;
        assert_eq!(
            parse_play_url(html).unwrap(),
            "https://hongguoduanju.com/video/public/123/360p.mp4"
        );
        assert!(parse_play_url(r#"{"main_url":"javascript:alert(1)"}"#).is_none());
    }

    #[test]
    fn ids_must_be_pure_digits() {
        assert!(valid_id("7694898085759028248"));
        assert!(!valid_id(""));
        assert!(!valid_id("../etc"));
        assert!(!valid_id("12a"));
    }

    #[test]
    fn quality_comes_from_url() {
        assert_eq!(
            quality_from_url("https://a/video/public/123/720p.mp4"),
            "720p"
        );
        assert_eq!(quality_from_url("https://a/video/123.mp4"), "360p");
    }

    #[test]
    fn base64_decodes_standard_and_url_safe() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVsbG8").unwrap(), b"hello");
        assert_eq!(base64_decode("a-b_").unwrap(), base64_decode("a+b/").unwrap());
        assert!(base64_decode("!!!!").is_none());
    }

    #[test]
    fn spade_key_matches_known_vectors() {
        // 两串都是从 video_model 里实测拿到的，key 已用 ffmpeg 解密验证过
        let key = derive_spade_key("o7wt9Wq4J/Z3jj/EQYoM90aODsNGjRbxWrwk81m/F+9pkyGDgw==")
            .unwrap();
        assert_eq!(
            key.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "b307678b13f75e1c16078d36f7d457df"
        );

        let key = derive_spade_key("rLwiwFq4FMJauRTyXrkjwmiPEMJdixfBRrg88kKOE/VCvAqHhw==")
            .unwrap();
        assert_eq!(
            key.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "afab7c6e7424ec27a652228bb2ed8b73"
        );

        assert!(derive_spade_key("").is_none());
        assert!(derive_spade_key("AAAA").is_none());
    }

    fn stream_json_video_list() -> serde_json::Value {
        serde_json::json!({
            "video_duration": 66.0,
            "video_list": [
                {
                    "main_url": "https://cdn/a-480.mp4",
                    "video_meta": {"definition": "480p", "codec_type": "bytevc2"},
                    "encrypt_info": {"encrypt": true, "spade_a": "rLwiwFq4FMJauRTyXrkjwmiPEMJdixfBRrg88kKOE/VCvAqHhw=="}
                },
                {
                    "main_url": "https://cdn/a-360.mp4",
                    "video_meta": {"definition": "360p", "codec_type": "bytevc1"},
                    "encrypt_info": {"encrypt": true, "spade_a": "o7wt9Wq4J/Z3jj/EQYoM90aODsNGjRbxWrwk81m/F+9pkyGDgw=="}
                },
                {
                    "main_url": "http://insecure/x.mp4",
                    "video_meta": {"definition": "1080p", "codec_type": "bytevc1"}
                }
            ]
        })
    }

    #[test]
    fn model_streams_skip_http_and_keep_keys() {
        let streams = model_streams(&stream_json_video_list());
        assert_eq!(streams.len(), 2);
        assert_eq!(streams[0].definition, "480p");
        assert!(streams[0].encrypted);
        assert!(streams[0].key.is_some());
        assert_eq!(streams[1].codec, "bytevc1");
    }

    #[test]
    fn model_streams_accepts_base64_urls_and_keeps_backup() {
        // 接口偶尔把地址整串换成 base64，而且这种流不带 meta/encrypt。
        // `aHR0cHM6Ly9jZG4vYmFzZTY0Lm1wNA==` 解码就是 https://cdn/base64.mp4
        let model = serde_json::json!({
            "video_list": [
                {
                    "main_url": "aHR0cHM6Ly9jZG4vYmFzZTY0Lm1wNA==",
                    "backup_url": "https://cdn/backup.mp4"
                },
                { "main_url": "http://insecure/x.mp4" }
            ]
        });
        let streams = model_streams(&model);
        assert_eq!(streams.len(), 2, "base64 的 main 和 https 的 backup 都要留下");
        assert_eq!(streams[0].url, "https://cdn/base64.mp4");
        assert!(!streams[0].encrypted);
        assert_eq!(streams[1].url, "https://cdn/backup.mp4");
    }

    #[test]
    fn stream_order_prefers_decodable_codec_over_definition() {
        let streams = model_streams(&stream_json_video_list());
        let order = pick_model_streams(&streams, None);
        // 360p 的 bytevc1 排在 480p 的 bytevc2 前面
        assert_eq!(order, vec![1, 0]);

        let order = pick_model_streams(&streams, Some("480p"));
        // 用户点名 480p 也不会选 bytevc2，编码可用性优先
        assert_eq!(order, vec![1, 0]);
    }

    #[test]
    fn bvc2_fourcc_is_patched_in_stsd_only() {
        let dir = std::env::temp_dir().join(format!("dd-bvc2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sample.mp4");

        // stsd: size(4) type(4) version+flags(4) entry_count(4) entry_size(4) format(4)
        let mut data = Vec::new();
        data.extend_from_slice(&[0, 0, 0, 24]);
        data.extend_from_slice(b"stsd");
        data.extend_from_slice(&[0, 0, 0, 0]);
        data.extend_from_slice(&[0, 0, 0, 1]);
        data.extend_from_slice(&[0, 0, 0, 8]);
        data.extend_from_slice(b"bvc2");
        // mdat 里也有同样的字节，不能被误改
        data.extend_from_slice(b"bvc2");
        std::fs::write(&file, &data).unwrap();

        assert!(patch_bvc2_fourcc(&file).unwrap());
        let patched = std::fs::read(&file).unwrap();
        assert_eq!(&patched[20..24], b"hvc1");
        assert_eq!(&patched[24..28], b"bvc2");

        // 已经改过就不再报变更
        assert!(!patch_bvc2_fourcc(&file).unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn safe_file_name_replaces_windows_reserved_chars() {
        assert_eq!(safe_file_name(" 剧:名/第?集 "), "剧_名_第_集");
        assert_eq!(safe_file_name(""), "未命名短剧");
        assert_eq!(safe_file_name("..."), "未命名短剧");
        // 太长的名字要截短，否则整条路径会超出 Windows 限制
        let long = "剧".repeat(100);
        assert_eq!(safe_file_name(&long).chars().count(), 80);
    }
}
