//! Optional Telegram alert module.
//!
//! Sends alerts when the scheduler encounters errors.
//! All functions are no-ops when bot_token/chat_id are not configured.

use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

/// Throttles repeated alerts by title. Same alert won't be sent more than once per cooldown.
pub struct AlertThrottle {
    last_sent: HashMap<String, Instant>,
    cooldown: Duration,
}

impl AlertThrottle {
    pub fn new(cooldown: Duration) -> Self {
        Self {
            last_sent: HashMap::new(),
            cooldown,
        }
    }

    fn should_send(&mut self, title: &str) -> bool {
        match self.last_sent.get(title) {
            Some(last) if last.elapsed() < self.cooldown => false,
            _ => {
                self.last_sent.insert(title.to_string(), Instant::now());
                true
            }
        }
    }

    /// Send a throttled alert. Silently skips if not configured or within cooldown.
    pub async fn send(
        &mut self,
        client: &reqwest::Client,
        bot_token: Option<&str>,
        chat_id: Option<&str>,
        title: &str,
        message: &str,
    ) {
        if !self.should_send(title) {
            debug!("Telegram alert throttled (cooldown): {}", title);
            return;
        }
        send_alert(client, bot_token, chat_id, title, message).await;
    }
}

/// Send alert to Telegram. No-op if bot_token or chat_id are not provided.
pub async fn send_alert(
    client: &reqwest::Client,
    bot_token: Option<&str>,
    chat_id: Option<&str>,
    title: &str,
    message: &str,
) {
    let (Some(token), Some(chat)) = (bot_token, chat_id) else {
        debug!("Telegram not configured, skipping alert: {}", title);
        return;
    };

    let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
    let text = format!(
        "\u{1f6a8} *{}*\n\n{}",
        escape_markdown(title),
        escape_markdown(message)
    );

    let result = client
        .post(&url)
        .json(&serde_json::json!({
            "chat_id": chat,
            "text": text,
            "parse_mode": "MarkdownV2"
        }))
        .send()
        .await;

    match result {
        Ok(resp) if resp.status().is_success() => {
            debug!("Telegram alert sent: {}", title);
        }
        Ok(resp) => {
            warn!(
                "Telegram API error: {} - {:?}",
                resp.status(),
                resp.text().await
            );
        }
        Err(e) => {
            warn!("Failed to send Telegram alert: {}", e);
        }
    }
}

fn escape_markdown(text: &str) -> String {
    let special_chars = [
        '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
    ];
    let mut result = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        if special_chars.contains(&c) {
            result.push('\\');
        }
        result.push(c);
    }
    result
}
