use crate::engine::MigrationTarget;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

pub async fn run_pumpportal_scout(tx: mpsc::Sender<MigrationTarget>) -> anyhow::Result<()> {
    let enabled = std::env::var("PUMPPORTAL_ENABLED").unwrap_or_else(|_| "false".into()) == "true";
    if !enabled {
        return Ok(());
    }
    let method =
        std::env::var("PUMPPORTAL_STREAM_METHOD").unwrap_or_else(|_| "subscribeMigration".into());
    loop {
        match connect_async("wss://pumpportal.fun/api/data").await {
            Ok((mut ws, _)) => {
                let msg = serde_json::json!({ "method": method });
                ws.send(Message::Text(msg.to_string().into())).await?;
                while let Some(msg) = ws.next().await {
                    let text = match msg {
                        Ok(Message::Text(t)) => t.to_string(),
                        Ok(_) => continue,
                        Err(_) => break,
                    };
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        if let Some(target) = parse_pumpportal_event(&v) {
                            let _ = tx.send(target).await;
                        }
                    }
                }
            }
            Err(e) => eprintln!("pumpportal reconnect: {e}"),
        }
        sleep(Duration::from_secs(3)).await;
    }
}

fn parse_pumpportal_event(v: &Value) -> Option<MigrationTarget> {
    let mint = v.get("mint")?.as_str()?.to_string();
    let tx_type = v.get("txType").and_then(|x| x.as_str()).unwrap_or("");
    if tx_type == "create" {
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
        ..Default::default()
    })
}
