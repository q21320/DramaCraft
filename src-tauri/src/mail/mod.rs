//! 完成通知邮件：队列跑完后发一封信。
//!
//! ## 为什么走 SMTP
//!
//! 用户手里的邮箱五花八门（QQ、163、Gmail、企业邮箱），SMTP 是唯一
//! 都支持的口子，不用绑某一家服务商，也不需要再申请额外的推送 Key。
//!
//! ## 授权码的存放
//!
//! 和 AI Key 一样，明文存在本地 SQLite 的 `setting` 表里，界面上明说。
//! **授权码永远不会返回给前端** —— 前端只拿到打码后的提示。
//!
//! ## 什么时候发
//!
//! 只在**整个队列结束时**发一封（`start_queue` 的收尾），不是每个任务一封。
//! 没有单独的开关：发件邮箱、授权码、SMTP 填全了就发，没填全就静默跳过；
//! 这一次一个任务都没跑，也不打扰。

use crate::db;
use crate::error::{AppError, AppResult};
use crate::models::{EmailConfigView, EmailTestResult, QueueReport};
use crate::pipeline::stage_label;
use lettre::message::{header::ContentType, Mailbox};
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tauri::Manager;

pub const SETTING_CONFIG: &str = "email.config";
pub const DEFAULT_PORT: u16 = 465;
const SEND_TIMEOUT_SECS: u64 = 25;

// ---------------- 配置 ----------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmailConfig {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    /// 收件人；空 = 发给发件邮箱自己
    pub to: String,
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: DEFAULT_PORT,
            username: String::new(),
            password: String::new(),
            to: String::new(),
        }
    }
}

pub fn load_config(conn: &Connection) -> AppResult<EmailConfig> {
    let raw = db::get_setting(conn, SETTING_CONFIG)?;
    Ok(raw
        .and_then(|text| serde_json::from_str::<EmailConfig>(&text).ok())
        .unwrap_or_default())
}

/// 保存配置。`password` 传 `None` 表示「不改动已存的授权码」，
/// 传 `Some("")` 才是显式清空 —— 否则改个收件人就会把授权码弄丢。
/// 允许保存不完整的配置（先填一半），
/// 但**只有填全了才会在队列结束时真正发信**。
pub fn save_config(
    conn: &Connection,
    host: &str,
    port: u16,
    username: &str,
    password: Option<&str>,
    to: &str,
) -> AppResult<EmailConfig> {
    let mut cfg = load_config(conn)?;
    cfg.host = host.trim().to_string();
    cfg.port = if port == 0 { DEFAULT_PORT } else { port };
    cfg.username = username.trim().to_string();
    if let Some(pw) = password {
        cfg.password = pw.trim().to_string();
    }
    // 收件人留空 = 发给自己；这样只填发件邮箱也能用
    cfg.to = if to.trim().is_empty() {
        cfg.username.clone()
    } else {
        to.trim().to_string()
    };

    if !cfg.username.is_empty() && cfg.username.parse::<Mailbox>().is_err() {
        return Err(AppError::Msg(format!(
            "发件邮箱格式不对：{}",
            cfg.username
        )));
    }
    for addr in recipients(&cfg) {
        if addr.parse::<Mailbox>().is_err() {
            return Err(AppError::Msg(format!("收件邮箱格式不对：{addr}")));
        }
    }

    let text = serde_json::to_string(&cfg)
        .map_err(|e| AppError::Msg(format!("邮件配置存不下来：{e}")))?;
    db::set_setting(conn, SETTING_CONFIG, &text)?;
    Ok(cfg)
}

fn missing_fields(cfg: &EmailConfig) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if cfg.host.trim().is_empty() {
        missing.push("SMTP 服务器");
    }
    if cfg.username.trim().is_empty() {
        missing.push("发件邮箱");
    }
    if cfg.password.trim().is_empty() {
        missing.push("授权码 / 密码");
    }
    if effective_to(cfg).is_empty() {
        missing.push("收件邮箱");
    }
    missing
}

fn effective_to(cfg: &EmailConfig) -> String {
    if cfg.to.trim().is_empty() {
        cfg.username.trim().to_string()
    } else {
        cfg.to.trim().to_string()
    }
}

/// 收件人支持多个，用逗号 / 分号 / 换行分开。
fn recipients(cfg: &EmailConfig) -> Vec<String> {
    effective_to(cfg)
        .split([',', '，', ';', '；', '\n'])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn is_ready(cfg: &EmailConfig) -> bool {
    missing_fields(cfg).is_empty()
}

/// 打码后的授权码。前端只能看到这个。
pub fn mask_secret(secret: &str) -> String {
    let trimmed = secret.trim();
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

/// 给前端看的视图：打码 + 缺哪几项。
/// 没有开关 —— 缺项就是「还没配好」，填全后自动生效。
pub fn view(cfg: &EmailConfig) -> EmailConfigView {
    let mut warnings = Vec::new();
    let missing = missing_fields(cfg);
    if !missing.is_empty() {
        warnings.push(format!(
            "还没配全（{}），填全之前不会发通知",
            missing.join("、")
        ));
    }
    warnings.push(
        "授权码以明文存在本地数据库里。QQ / 163 要填「授权码」，不是登录密码。".to_string(),
    );
    EmailConfigView {
        host: cfg.host.clone(),
        port: cfg.port,
        username: cfg.username.clone(),
        to: effective_to(cfg),
        has_password: !cfg.password.trim().is_empty(),
        password_hint: mask_secret(&cfg.password),
        warnings,
    }
}

// ---------------- 发信 ----------------

/// 465 走隐式 SSL（QQ / 163 / Gmail 的默认口），其他端口走 STARTTLS。
fn transport(cfg: &EmailConfig) -> AppResult<AsyncSmtpTransport<Tokio1Executor>> {
    let host = cfg.host.trim();
    if host.is_empty() {
        return Err(AppError::Msg("没填 SMTP 服务器".into()));
    }
    let creds = Credentials::new(
        cfg.username.trim().to_string(),
        cfg.password.trim().to_string(),
    );
    let builder = if cfg.port == 465 {
        let tls = TlsParameters::new(host.to_string())
            .map_err(|e| AppError::Msg(format!("SSL 初始化失败：{e}")))?;
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(host)
            .port(cfg.port)
            .tls(Tls::Wrapper(tls))
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(host)
            .map_err(|e| AppError::Msg(format!("SMTP 初始化失败：{e}")))?
            .port(cfg.port)
    };
    Ok(builder
        .credentials(creds)
        .timeout(Some(Duration::from_secs(SEND_TIMEOUT_SECS)))
        .build())
}

async fn send(cfg: &EmailConfig, subject: &str, body: &str) -> AppResult<()> {
    let from: Mailbox = cfg
        .username
        .trim()
        .parse()
        .map_err(|_| AppError::Msg(format!("发件邮箱格式不对：{}", cfg.username)))?;
    let mut builder = Message::builder()
        .from(from)
        .subject(subject)
        .header(ContentType::TEXT_PLAIN);
    let to = recipients(cfg);
    if to.is_empty() {
        return Err(AppError::Msg("没填收件邮箱".into()));
    }
    for addr in &to {
        let mailbox: Mailbox = addr
            .parse()
            .map_err(|_| AppError::Msg(format!("收件邮箱格式不对：{addr}")))?;
        builder = builder.to(mailbox);
    }
    let message = builder
        .body(body.to_string())
        .map_err(|e| AppError::Msg(format!("邮件内容组装失败：{e}")))?;
    transport(cfg)?
        .send(message)
        .await
        .map_err(|e| AppError::Msg(format!("SMTP 发送失败：{e}")))?;
    Ok(())
}

/// 发一封测试信。专门用来验证配置，不用真跑一遍队列。
pub async fn test(cfg: &EmailConfig) -> AppResult<String> {
    let missing = missing_fields(cfg);
    if !missing.is_empty() {
        return Err(AppError::Msg(format!(
            "还差：{}",
            missing.join("、")
        )));
    }
    let to = recipients(cfg).join("、");
    let started = Instant::now();
    send(
        cfg,
        "DramaDist 测试邮件",
        "收到这封邮件，说明完成通知已经配好了。\n\n队列跑完后会自动发一封结果邮件。\n\n— DramaDist",
    )
    .await?;
    Ok(format!(
        "已发送到 {to}，用了 {:.1} 秒；没收到就翻一下垃圾箱",
        started.elapsed().as_secs_f32()
    ))
}

/// 队列结束时调用。没配全 / 没跑任务都返回 `Ok(None)`，不报错。
///
/// **按「剧」写，不按任务写。**
///
/// 用户的原话：「哪一部剧成功了 哪一部剧失败了」。原来的正文只写
/// 「成功 12，失败 0」—— 一部剧拆开 32 个任务后变一串名字，看信不知道哪部成了。
pub async fn send_completion(
    app: &tauri::AppHandle,
    report: &QueueReport,
) -> AppResult<Option<String>> {
    let (cfg, dramas) = {
        let state = app.state::<crate::AppState>();
        let conn = state
            .db
            .lock()
            .map_err(|_| AppError::Msg("数据库锁被污染，请重启应用".into()))?;
        (load_config(&conn)?, collect_dramas(&conn, report))
    };
    if !is_ready(&cfg) || report.processed <= 0 {
        return Ok(None);
    }

    let ok = dramas.iter().filter(|d| d.status == "done").count();
    let bad = dramas.len() - ok;

    let to = recipients(&cfg).join("、");
    let subject = if dramas.is_empty() {
        // 手动排的任务，没有流水线
        if report.failed > 0 {
            format!(
                "DramaDist 处理完成：成功 {}，失败 {}",
                report.done, report.failed
            )
        } else {
            format!("DramaDist 处理完成：{} 个任务全部成功", report.done)
        }
    } else if bad == 0 {
        format!("DramaDist 处理完成：{ok} 部剧全部成功")
    } else {
        format!("DramaDist 处理完成：{ok} 部成功，{bad} 部没成")
    };

    let mut body = String::new();
    if !dramas.is_empty() {
        body.push_str(&format!(
            "共 {} 部剧：成功 {ok} 部，没成 {bad} 部\n",
            dramas.len()
        ));
    } else {
        body.push_str(&format!(
            "任务：成功 {}，失败 {}，剩余 {}\n",
            report.done, report.failed, report.remaining
        ));
    }

    // ---- 需要手动做的：放最前面 ----
    //
    // 用户的原话：「批量操作的，所以告诉他都有哪些」。邮件发到手机上，
    // 看完就知道要开剪映把哪几部剧的草稿拷进去截图 —— 这是收到信后
    // 第一件要动手的事，所以排在【成功的剧】前面。
    //
    // **所有剧都列，一部不漏。** 用户明确要求「没勾也提醒」：
    // 勾了且出了草稿 → 告诉他去哪拷、拷几集；
    // 勾了但一个都没出 → 这属于没成，已经在【没成的剧】里了；
    // 没勾 → 单独一句「这部剧没生成剪映草稿，需要你手动处理」。
    let want: Vec<&DramaOutcome> = dramas
        .iter()
        .filter(|d| d.status == "done" && (!d.draft_checked || d.draft_episodes > 0))
        .collect();
    if !want.is_empty() {
        body.push_str("\n需要你手动做：\n");
        for (i, d) in want.iter().enumerate() {
            if d.draft_episodes > 0 {
                body.push_str(&format!(
                    "  {}. {}（{} 集）—— 把草稿文件夹拷到剪映的草稿目录后截图\n",
                    i + 1,
                    d.draft_name,
                    d.draft_episodes
                ));
            } else {
                body.push_str(&format!(
                    "  {}. {} —— 这部剧没生成剪映草稿，需要你手动处理\n",
                    i + 1,
                    d.name
                ));
            }
        }
    }

    // ---- 成功的剧：一行一部 ----
    let good: Vec<&DramaOutcome> = dramas.iter().filter(|d| d.status == "done").collect();
    if !good.is_empty() {
        body.push_str("\n成功的剧：\n");
        for d in &good {
            body.push_str(&format!("✔ {}（{} 集）\n", d.name, d.episodes));
        }
    }

    // ---- 没成的剧：名字 + 卡在哪一步 + 为什么 ----
    let mut bad_list: Vec<&DramaOutcome> = dramas.iter().filter(|d| d.status != "done").collect();
    bad_list.sort_by_key(|d| d.status.as_str());
    if !bad_list.is_empty() {
        body.push_str("\n没成的剧：\n");
        for d in &bad_list {
            body.push_str(&format!(
                "✘ {}（{} 集）｜{}{}：\n",
                d.name,
                d.episodes,
                status_label(&d.status),
                if d.stage.is_empty() {
                    String::new()
                } else {
                    format!("在「{}」", stage_label(&d.stage))
                }
            ));
            body.push_str(&format!("   结论：{}\n", d.message));
            for line in d.reasons.iter().take(6) {
                body.push_str(&format!("   - {line}\n"));
            }
            if d.reasons.len() > 6 {
                body.push_str("   - …（其余见应用内日志）\n");
            }
        }
    }

    // ---- 手动任务（不属于任何剧）的失败 ----
    if report.run_ids.is_empty() && !report.failures.is_empty() {
        body.push_str("\n失败的任务：\n");
        for f in report.failures.iter().rev().take(10).collect::<Vec<_>>().into_iter().rev() {
            body.push_str(&format!(
                "- #{} {}：{}\n",
                f.job_id,
                crate::queue::kind_label(&f.kind),
                one_line(&f.reason, 160)
            ));
        }
        if report.failures.len() > 10 {
            body.push_str("…（其余见应用内日志）\n");
        }
    }

    body.push_str(&format!(
        "\n时间：{}\n",
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    ));
    body.push_str("\n— DramaDist");

    send(&cfg, &subject, &body).await?;
    Ok(Some(to))
}

/// 邮件里一部剧的结论。
struct DramaOutcome {
    name: String,
    episodes: i64,
    /// done | failed | canceled | skipped | pending | running
    status: String,
    /// 卡在哪一步（中文名）
    stage: String,
    message: String,
    /// 具体失败原因（日志里那几行），已去掉前缀
    reasons: Vec<String>,
    /// 剪映里显示的名字（草稿文件夹名）；没草稿就是空串
    draft_name: String,
    /// 真出了几个草稿；0 = 没勾草稿或一个都没成
    draft_episodes: i64,
    /// 这次运行勾了剪映草稿这一步吗
    draft_checked: bool,
}

/// 把本次运行涉及到的剧连同失败原因收集起来。
///
/// 顺带把「任务 → 剧」的对应关系算好：一个任务算在哪部剧头上，
/// 就靠 `pipeline_drama_id`；算不上的都是手动任务。
fn collect_dramas(conn: &Connection, report: &QueueReport) -> Vec<DramaOutcome> {
    let mut out = Vec::new();
    for run_id in &report.run_ids {
        let dramas = match db::list_pipeline_dramas(conn, *run_id) {
            Ok(d) => d,
            Err(_) => continue,
        };
        // 这次运行勾了哪几步 —— 用来判断「本该跑完的步骤是不是真跑了」
        let stages = run_stages(conn, *run_id);
        for d in dramas {
            let mut reasons: Vec<String> = Vec::new();
            let mut status = d.status.clone();
            let mut message = d.message.clone();
            // 剪映草稿：剪映里看到的文件夹名 + 实际出了几个
            let (draft_name, draft_episodes, draft_checked) = draft_info(conn, d.id);

            if status == "done" && !stages.is_empty() {
                // **「完成」也得核一遍。**
                //
                // 剧目状态是队列按「这台剧的任务都处置完了」推出来的，
                // 它不知道「这一步本来就没排任务」。踩过：没印封面图、
                // 没配模板，那几步排任务时直接跳过了 —— 任务表干净，
                // 剧目写「全部步骤已完成」，邮件也就报成成功。
                // 实际交付少了封面或成本报告。
                let not_run = missing_stages(conn, *run_id, d.id, &stages);
                if !not_run.is_empty() {
                    status = "incomplete".to_string();
                    message = format!("少了 {} 个步骤", not_run.len());
                    reasons.push(format!("这几步没跑成：{}", not_run.join("、")));
                }
            }

            if status != "done" {
                if let Ok(lines) = db::list_drama_error_logs(conn, *run_id, d.id) {
                    for line in lines {
                        // 日志里是「✗ 第03集.mp4 整集去重：…」，保留原文更好对账
                        let flat = one_line(&line, 200);
                        if !flat.trim().is_empty() && !reasons.contains(&flat) {
                            reasons.push(flat);
                        }
                    }
                }
                // 日志里没有？（比如整部没轮到）就把结论那一句当原因
                if reasons.is_empty() && !message.trim().is_empty() {
                    reasons.push(one_line(&message, 200));
                }
            }
            out.push(DramaOutcome {
                name: d.name,
                episodes: d.episode_count,
                status,
                stage: stage_label(&d.stage),
                message: one_line(&message, 200),
                reasons,
                draft_name,
                draft_episodes,
                draft_checked,
            });
        }
    }
    out
}

/// 这部剧的剪映草稿：`(剪映里显示的名字, 真出了几个, 勾了没有)`。
///
/// **名字取草稿任务的参数，不取剧名。** 草稿文件夹名 = 剪映里显示的名字，
/// 用户去剪映里找的就是它；而剧名可能带 AI 改过的后缀或脏字符。
///
/// 个数不是去数任务（一个任务做全部集），而是**去磁盘上数文件夹**：
/// 任务标 done 也不等于 4 集都写成了，只有 `draft_content.json` 真在的才算。
fn draft_info(conn: &Connection, drama_id: i64) -> (String, i64, bool) {
    let jobs = db::jobs_of_kind_for_drama(conn, drama_id, "draft").unwrap_or_default();
    // **一条任务都没有 = 这一步没排上。**
    //
    // 草稿默认不勾，没勾时队列压根不排这条任务 —— 于是「没产出」
    // 和「没跑」在库里长得一样，只能看这条任务存不存在来分辨。
    // 用户要求：没勾也要在邮件里点出来，否则批量跑十几部剧时，
    // 哪一部忘了勾得自己回去对。
    if jobs.is_empty() {
        return (String::new(), 0, false);
    }
    // 只看真跑完的那条；失败/撤销的当没有草稿（但算是勾了）
    let Some((_, payload)) = jobs.iter().find(|(s, _)| s == "done") else {
        return (String::new(), 0, true);
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
        return (String::new(), 0, true);
    };
    let name = v
        .get("draftName")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let root = v
        .get("draftRoot")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if root.is_empty() {
        return (name, 0, true);
    }

    // 数 `draftRoot` 下真有 `draft_content.json` 的子目录。
    // `_字幕/` 之类以 _ 开头的中间文件不算。
    let mut episodes = 0i64;
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let p = entry.path();
            if !p.is_dir() {
                continue;
            }
            let folder = p
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if folder.starts_with('_') {
                continue;
            }
            if p.join("draft_content.json").is_file() {
                episodes += 1;
            }
        }
    }
    (name, episodes, true)
}

/// 这次运行勾选了哪几步（存的是 JSON 数组字符串，坏了的当没有）。
fn run_stages(conn: &Connection, run_id: i64) -> Vec<String> {
    match db::get_pipeline_run(conn, run_id) {
        Ok(Some(run)) => serde_json::from_str::<Vec<String>>(&run.stages).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// 本该在这部剧上跑的步骤里，没真正做完的那些。
///
/// 三种都算没做完：
///
/// 1. **一个任务都没排上** —— 勾了却没排；（缺简介、缺封面图之类主动跳过的除外，
///    那些日志里有 warn，这里不重复算）
/// 2. **任务标了 done，但磁盘上的产出不在** —— 用户的原话：
///    「没有日志但是有资源的文件呢」。任务表是队列自己的记账，它可能记成
///    成功而文件已经被删、被手动移走、或者杀进程时只落了个半个文件。
///    所以这里要**真的去看文件**。
/// 3. **任务失败或被撤销** —— 队列已经把它标成 failed / canceled，
///    剧目却还是 done 的（大多是旧数据、或者中途被强行改过）。
fn missing_stages(conn: &Connection, run_id: i64, drama_id: i64, stages: &[String]) -> Vec<String> {
    let errored = db::list_drama_error_logs(conn, run_id, drama_id).unwrap_or_default();
    let mut missing = Vec::new();
    for stage in stages {
        if stage == "rewrite" || stage == "regroup" {
            continue;
        }
        let label = stage_label(stage);
        let kind = crate::pipeline::stage_to_job_kind(stage);
        match db::jobs_of_kind_for_drama(conn, drama_id, kind) {
            Ok(jobs) if !jobs.is_empty() => {
                if let Some(reason) = stage_problem(&jobs) {
                    missing.push(format!("{label}（{reason}）"));
                }
            }
            _ => {
                // 跳过的那几步都写过日志，日志里提到了就不算「搞丢了」
                if errored.iter().any(|l| l.contains(&label)) {
                    continue;
                }
                missing.push(format!("{label}（没排上任务）"));
            }
        }
    }
    missing
}

/// 一步里每个任务的 `(状态, 参数)`。
type KindJobs = Vec<(String, String)>;

/// 一步的结论：`None` = 这个任务真的落盘了；`Some(原因)` = 没做完。
///
/// 只看「有条子标成 done」是不够的 —— 得按参数里的路径去磁盘上点一遍。
fn stage_problem(jobs: &KindJobs) -> Option<String> {
    let total = jobs.len();
    let not_done = jobs.iter().filter(|(s, _)| s != "done").count();
    if not_done > 0 {
        return Some(format!("{not_done}/{total} 个没跑完"));
    }
    let missing = jobs
        .iter()
        .filter(|(_, payload)| !payload_landed(payload))
        .count();
    if missing > 0 {
        return Some(format!("产出少了 {missing}/{total} 份"));
    }
    None
}

/// 任务参数里写的产物，磁盘上到底有没有。
///
/// 每类任务的「产物是什么」不一样：
/// - 去重 / 去字幕：一个文件
/// - 切片：一个目录下的分片
/// - 分镜 / 成本 / 封面：一个文件或一组图
/// - 剪映草稿：草稿目录（再严一点就是里面的 `draft_content.json`）
///
/// **路径解析不出来就当它落地了** —— 这一步不是校验器，
/// 不能因为换了字段名就把整部剧错报成失败。
fn payload_landed(payload: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(payload) else {
        return true;
    };
    let path_of = |key: &str| -> Option<String> {
        v.get(key)
            .and_then(|x| x.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };

    // 单文件产出
    for key in ["outPath", "outDir", "draftRoot"] {
        if let Some(p) = path_of(key) {
            return exists_with_content(std::path::Path::new(&p));
        }
    }
    // 一组路径（去字幕产物路径等）
    if let Some(arr) = v.get("quzimuPaths").and_then(|x| x.as_array()) {
        let paths: Vec<&str> = arr.iter().filter_map(|x| x.as_str()).collect();
        if !paths.is_empty() {
            return paths
                .iter()
                .all(|p| exists_with_content(std::path::Path::new(p)));
        }
    }
    true
}

/// 路径存在且有内容。
///
/// **空目录不算**（切片切出 0 片就是空目录），
/// **小得离谱的文件也不算**（踩过：ffmpeg 只写出 321 字节的空容器，
/// 状态还是 ok、文件也在，实际什么都没切出来）。
fn exists_with_content(path: &std::path::Path) -> bool {
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.is_dir() {
            // 目录：里面得真有点东西
            return std::fs::read_dir(path)
                .map(|mut it| it.next().is_some())
                .unwrap_or(false);
        }
        return meta.len() > 1024;
    }
    false
}

/// 一长串东西塞进邮件会撑破版面，截断并标清楚。
fn one_line(text: &str, max: usize) -> String {
    let flat = text.replace(['\r', '\n'], " ").trim().to_string();
    let mut chars: Vec<char> = flat.chars().collect();
    if chars.len() <= max {
        return flat;
    }
    chars.truncate(max);
    format!("{}…", chars.into_iter().collect::<String>())
}

fn status_label(status: &str) -> &'static str {
    match status {
        "done" => "完成",
        "failed" => "失败",
        "incomplete" => "步骤不全",
        "canceled" => "中途停止",
        "skipped" => "没轮到",
        "pending" | "running" => "没跑完",
        _ => "未知",
    }
}

/// 把发信结果记到日志里 —— 发失败时用户至少能在「日志」页看到原因。
pub fn log_result(app: &tauri::AppHandle, result: &AppResult<Option<String>>) {
    let state = app.state::<crate::AppState>();
    let Ok(conn) = state.db.lock() else {
        return;
    };
    match result {
        Ok(Some(to)) => {
            db::append_log(&conn, None, None, "info", &format!("完成通知邮件已发送到 {to}"));
        }
        // **跳过要说明原因。**
        //
        // 踩过：`send_completion` 的 `Ok(None)` 是「没配全 / 没任务，静默跳过」，
        // 这里原本什么都不记 —— 用户填了一半设置，跑完以为邮件发了，
        // 实际上被静默跳过了，日志里一个字都没有。
        //
        // 现在把「缺哪几项」写进日志，用户一看就知道去补什么。
        Ok(None) => {
            let msg = match load_config(&conn) {
                Ok(cfg) => {
                    let miss = missing_fields(&cfg);
                    if !miss.is_empty() {
                        format!(
                            "完成通知邮件没发 —— 设置里还缺 {}（填全后自动发）",
                            miss.join("、")
                        )
                    } else {
                        "完成通知邮件：这次没有可通知的任务，跳过".to_string()
                    }
                }
                Err(e) => format!("完成通知邮件：读不了邮件设置（{e}），跳过"),
            };
            db::append_log(&conn, None, None, "info", &msg);
        }
        Err(e) => {
            db::append_log(
                &conn,
                None,
                None,
                "warn",
                &format!("完成通知邮件没发出去：{e}"),
            );
        }
    }
}

/// 命令层的测试结果包装。
pub async fn test_result(cfg: &EmailConfig) -> EmailTestResult {
    match test(cfg).await {
        Ok(detail) => EmailTestResult { ok: true, detail },
        Err(e) => EmailTestResult {
            ok: false,
            detail: e.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        db::migrate_for_tests(&c).unwrap();
        c
    }

    #[test]
    fn defaults_are_blank_but_use_port_465() {
        let c = conn();
        let cfg = load_config(&c).unwrap();
        assert_eq!(cfg.port, 465);
        assert!(cfg.host.is_empty());
        assert!(!is_ready(&cfg), "什么都没填时不该发通知");
    }

    #[test]
    fn blank_password_keeps_the_saved_one() {
        let c = conn();
        save_config(&c, "smtp.qq.com", 465, "a@qq.com", Some("auth-code"), "b@qq.com")
            .unwrap();
        let after = save_config(&c, "smtp.qq.com", 465, "a@qq.com", None, "b@qq.com")
            .unwrap();
        assert_eq!(after.password, "auth-code", "留空不该把授权码抹掉");
    }

    #[test]
    fn empty_recipient_falls_back_to_sender() {
        let c = conn();
        let cfg = save_config(
            &c,
            "smtp.qq.com",
            465,
            "a@qq.com",
            Some("auth-code"),
            "   ",
        )
        .unwrap();
        assert_eq!(cfg.to, "a@qq.com");
        assert_eq!(view(&cfg).to, "a@qq.com");
    }

    #[test]
    fn incomplete_config_is_allowed_but_does_not_notify() {
        let c = conn();
        // 只填了一半也能存；存完不是「就绪」状态，缺什么写进 warnings
        let cfg = save_config(&c, "smtp.qq.com", 465, "a@qq.com", None, "").unwrap();
        assert!(!is_ready(&cfg));
        assert!(view(&cfg)
            .warnings
            .iter()
            .any(|w| w.contains("授权码") && w.contains("不会发通知")));
    }

    #[test]
    fn filled_config_is_ready_without_an_enable_switch() {
        let c = conn();
        let cfg = save_config(
            &c,
            "smtp.qq.com",
            465,
            "a@qq.com",
            Some("auth-code"),
            "b@qq.com",
        )
        .unwrap();
        assert!(is_ready(&cfg), "填全了就该自动发，不需要开关");
        assert!(!view(&cfg).warnings.iter().any(|w| w.contains("还没配全")));
    }

    #[test]
    fn recipients_accept_comma_and_semicolon() {
        let mut cfg = EmailConfig::default();
        cfg.username = "a@qq.com".into();
        cfg.to = "b@qq.com, c@qq.com；d@qq.com".into();
        assert_eq!(
            recipients(&cfg),
            vec!["b@qq.com", "c@qq.com", "d@qq.com"]
        );
    }

    #[test]
    fn secret_is_masked_in_the_view() {
        let mut cfg = EmailConfig::default();
        cfg.username = "a@qq.com".into();
        cfg.password = "abcdefghijklmn".into();
        let json = serde_json::to_string(&view(&cfg)).unwrap();
        assert!(!json.contains("abcdefghijklmn"), "真授权码泄漏到前端了：{json}");
        assert!(json.contains("abc****klmn"));
    }

    #[test]
    fn mask_handles_short_secrets() {
        assert_eq!(mask_secret(""), "");
        assert_eq!(mask_secret("1234"), "****");
        assert_eq!(mask_secret("abcdefghijkl"), "abc****ijkl");
    }
}
