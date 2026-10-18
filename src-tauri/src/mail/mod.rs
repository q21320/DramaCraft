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
pub async fn send_completion(
    app: &tauri::AppHandle,
    report: &QueueReport,
) -> AppResult<Option<String>> {
    let cfg = {
        let state = app.state::<crate::AppState>();
        let conn = state
            .db
            .lock()
            .map_err(|_| AppError::Msg("数据库锁被污染，请重启应用".into()))?;
        load_config(&conn)?
    };
    if !is_ready(&cfg) || report.processed <= 0 {
        return Ok(None);
    }

    let to = recipients(&cfg).join("、");
    let subject = if report.failed > 0 {
        format!(
            "DramaDist 处理完成：成功 {}，失败 {}",
            report.done, report.failed
        )
    } else {
        format!("DramaDist 处理完成：{} 个任务全部成功", report.done)
    };

    let mut body = format!(
        "队列已结束。\n\n成功：{}\n失败：{}\n剩余：{}\n结束原因：{}\n时间：{}\n",
        report.done,
        report.failed,
        report.remaining,
        report.stopped_because,
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
    );
    let failures: Vec<&String> = report
        .messages
        .iter()
        .filter(|m| m.contains("失败 ——"))
        .collect();
    if !failures.is_empty() {
        body.push_str("\n失败的任务：\n");
        for line in failures.iter().take(20) {
            body.push_str(&format!("- {line}\n"));
        }
        if failures.len() > 20 {
            body.push_str("…（其余见应用内日志）\n");
        }
    }
    body.push_str("\n— DramaDist");

    send(&cfg, &subject, &body).await?;
    Ok(Some(to))
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
