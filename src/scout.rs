use crate::engine::MigrationTarget;
use crate::state::append_jsonl;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::collections::HashSet;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const PUMPPORTAL_EVENTS_PATH: &str = "pumpportal_migration_events.jsonl";

pub async fn run_pumpportal_scout(tx: mpsc::Sender<MigrationTarget>) -> anyhow::Result<()> {
    let enabled = std::env::var("PUMPPORTAL_ENABLED").unwrap_or_else(|_| "false".into()) == "true";
    if !enabled {
        return Ok(());
    }
    let method =
        std::env::var("PUMPPORTAL_STREAM_METHOD").unwrap_or_else(|_| "subscribeMigration".into());
    let api_key = std::env::var("PUMPPORTAL_API_KEY").unwrap_or_default();
    let mut reconnect_delay_secs = env_u64("PUMPPORTAL_RECONNECT_INITIAL_SECS", 3).max(1);
    let reconnect_max_secs =
        env_u64("PUMPPORTAL_RECONNECT_MAX_SECS", 120).max(reconnect_delay_secs);
    let mut seen: HashSet<String> = HashSet::new();
    loop {
        let url = pumpportal_ws_url(&api_key);
        match connect_async(&url).await {
            Ok((mut ws, _)) => {
                reconnect_delay_secs = env_u64("PUMPPORTAL_RECONNECT_INITIAL_SECS", 3).max(1);
                let msg = serde_json::json!({ "method": method });
                ws.send(Message::Text(msg.to_string().into())).await?;
                println!("📡 [PUMPPORTAL] subscribed {method}");
                while let Some(msg) = ws.next().await {
                    let text = match msg {
                        Ok(Message::Text(t)) => t.to_string(),
                        Ok(_) => continue,
                        Err(e) => {
                            eprintln!("⚠️ [PUMPPORTAL] websocket drop: {e}");
                            break;
                        }
                    };
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        record_pumpportal_event(&v);
                        if let Some(target) = parse_pumpportal_event(&v) {
                            let dedupe_key = if target.migration_signature.is_empty() {
                                format!("mint:{}", target.mint)
                            } else {
                                format!("sig:{}", target.migration_signature)
                            };
                            if seen.insert(dedupe_key) {
                                println!(
                                    "🎯 [PUMPPORTAL_MIGRATION] mint={} sig={} mc_sol={}",
                                    target.mint, target.migration_signature, target.market_cap_sol
                                );
                                let _ = tx.send(target).await;
                            }
                            if seen.len() > 20_000 {
                                seen.clear();
                            }
                        }
                    }
                }
            }
            Err(e) => eprintln!("pumpportal reconnect: backing off {reconnect_delay_secs}s: {e}"),
        }
        sleep(Duration::from_secs(reconnect_delay_secs)).await;
        reconnect_delay_secs = reconnect_delay_secs
            .saturating_mul(2)
            .min(reconnect_max_secs);
    }
}

fn parse_pumpportal_event(v: &Value) -> Option<MigrationTarget> {
    let mint = v.get("mint")?.as_str()?.to_string();
    let tx_type = v.get("txType").and_then(|x| x.as_str()).unwrap_or("");
    if tx_type == "create" {
        return None;
    }
    if tx_type != "migrate" && tx_type != "migration" {
        return None;
    }
    Some(MigrationTarget {
        mint,
        name: v
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("PUMPPORTAL")
            .to_string(),
        symbol: v
            .get("symbol")
            .and_then(|x| x.as_str())
            .unwrap_or("AMM")
            .to_string(),
        source: "pumpportal_migration".to_string(),
        market_cap_sol: v
            .get("marketCapSol")
            .and_then(|x| x.as_f64())
            .unwrap_or_default(),
        v_sol_in_bonding_curve: v
            .get("vSolInBondingCurve")
            .and_then(|x| x.as_f64())
            .unwrap_or_default(),
        migration_signature: v
            .get("signature")
            .or_else(|| v.get("txSignature"))
            .or_else(|| v.get("transactionSignature"))
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        ..Default::default()
    })
}

fn pumpportal_ws_url(api_key: &str) -> String {
    if api_key.trim().is_empty() {
        "wss://pumpportal.fun/api/data".to_string()
    } else {
        format!("wss://pumpportal.fun/api/data?api-key={api_key}")
    }
}

fn record_pumpportal_event(v: &Value) {
    let tx_type = v.get("txType").and_then(|x| x.as_str()).unwrap_or("");
    if tx_type != "migrate" && tx_type != "migration" {
        return;
    }
    let compact = serde_json::json!({
        "captured_at_utc": chrono::Utc::now().to_rfc3339(),
        "source": "pumpportal_migration",
        "txType": tx_type,
        "mint": v.get("mint"),
        "signature": v.get("signature").or_else(|| v.get("txSignature")).or_else(|| v.get("transactionSignature")),
        "marketCapSol": v.get("marketCapSol"),
        "raw": v,
    });
    let _ = append_jsonl(PUMPPORTAL_EVENTS_PATH, &compact);
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pumpportal_url_uses_api_key_when_present() {
        assert_eq!(
            pumpportal_ws_url(""),
            "wss://pumpportal.fun/api/data".to_string()
        );
        assert_eq!(
            pumpportal_ws_url("abc"),
            "wss://pumpportal.fun/api/data?api-key=abc".to_string()
        );
    }

    #[test]
    fn migration_event_parses_as_shadow_signal_only() {
        let v = serde_json::json!({
            "txType": "migrate",
            "mint": "Mint111",
            "signature": "Sig111",
            "marketCapSol": 42.0,
            "vSolInBondingCurve": 30.0,
            "name": "Token",
            "symbol": "TOK"
        });
        let target = parse_pumpportal_event(&v).unwrap();
        assert_eq!(target.mint, "Mint111");
        assert_eq!(target.migration_signature, "Sig111");
        assert_eq!(target.source, "pumpportal_migration");
        assert!(!target.is_amm());
    }

    #[test]
    fn ignores_non_migration_events() {
        let create = serde_json::json!({"txType":"create","mint":"Mint111"});
        let buy = serde_json::json!({"txType":"buy","mint":"Mint111"});
        assert!(parse_pumpportal_event(&create).is_none());
        assert!(parse_pumpportal_event(&buy).is_none());
    }
}
