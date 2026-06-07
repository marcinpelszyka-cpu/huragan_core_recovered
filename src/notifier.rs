use std::env;
use std::time::Duration;

const TELEGRAM_MAX_MESSAGE_LEN: usize = 3900;

pub async fn send_telegram_alert(message: impl Into<String>) {
    let token = env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default();
    let chat_id = env::var("TELEGRAM_CHAT_ID").unwrap_or_default();
    if token.is_empty() || chat_id.is_empty() {
        return;
    }
    let message = truncate_message(&message.into());
    let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(8))
        .build()
    {
        Ok(client) => client,
        Err(_) => return,
    };
    let _ = client
        .post(url)
        .json(&serde_json::json!({
            "chat_id": chat_id,
            "text": message,
            "disable_web_page_preview": true
        }))
        .send()
        .await;
}

fn truncate_message(message: &str) -> String {
    if message.chars().count() <= TELEGRAM_MAX_MESSAGE_LEN {
        return message.to_string();
    }
    let mut out: String = message.chars().take(TELEGRAM_MAX_MESSAGE_LEN).collect();
    out.push_str("\n…truncated");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telegram_message_truncates_long_text() {
        let msg = "x".repeat(TELEGRAM_MAX_MESSAGE_LEN + 100);
        let out = truncate_message(&msg);
        assert!(out.contains("truncated"));
        assert!(out.chars().count() <= TELEGRAM_MAX_MESSAGE_LEN + 16);
    }

    #[test]
    fn telegram_message_keeps_short_text() {
        assert_eq!(truncate_message("hello"), "hello");
    }
}
