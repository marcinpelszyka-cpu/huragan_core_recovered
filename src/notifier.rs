use std::env;

pub async fn send_telegram_alert(message: impl Into<String>) {
    let token = env::var("TELEGRAM_BOT_TOKEN").unwrap_or_default();
    let chat_id = env::var("TELEGRAM_CHAT_ID").unwrap_or_default();
    if token.is_empty() || chat_id.is_empty() {
        return;
    }
    let message = message.into();
    tokio::spawn(async move {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", token);
        let _ = reqwest::Client::new()
            .post(url)
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "text": message,
                "parse_mode": "Markdown"
            }))
            .send()
            .await;
    });
}
