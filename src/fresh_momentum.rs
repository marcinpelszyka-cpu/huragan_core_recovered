use crate::state::append_jsonl;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

#[derive(Debug)]
struct TrackedFreshToken {
    mint: String,
    name: String,
    symbol: String,
    creator: String,
    created_at: Instant,
    entry_market_cap_sol: f64,
    current_market_cap_sol: f64,
    max_market_cap_sol: f64,
    min_market_cap_sol: f64,
    buy_count: u64,
    sell_count: u64,
    buy_volume_sol: f64,
    sell_volume_sol: f64,
    buyers: HashSet<String>,
    sellers: HashSet<String>,
    snapshots_done: HashSet<u64>,
    first_trade_seen: bool,
}

impl TrackedFreshToken {
    fn label_for(&self, final_label: bool) -> String {
        let net = self.buy_volume_sol - self.sell_volume_sol;
        let mc_change = if self.entry_market_cap_sol > 0.0 {
            (self.current_market_cap_sol / self.entry_market_cap_sol - 1.0) * 100.0
        } else {
            0.0
        };
        if !final_label {
            "tracking_snapshot".into()
        } else if self.buy_count + self.sell_count == 0 {
            "no_trade_data".into()
        } else if self.current_market_cap_sol <= self.entry_market_cap_sol * 0.5 || net < -0.5 {
            "rug_60s".into()
        } else if mc_change >= 100.0 {
            "moonshot_100k_or_2x".into()
        } else if mc_change >= 50.0 {
            "pump_40k_or_50pct".into()
        } else {
            "flat".into()
        }
    }

    fn snapshot(&self, age: u64, final_label: bool) -> Value {
        let net = self.buy_volume_sol - self.sell_volume_sol;
        let mc_change = if self.entry_market_cap_sol > 0.0 {
            (self.current_market_cap_sol / self.entry_market_cap_sol - 1.0) * 100.0
        } else {
            0.0
        };
        let max_dd = if self.max_market_cap_sol > 0.0 {
            (self.min_market_cap_sol / self.max_market_cap_sol - 1.0) * 100.0
        } else {
            0.0
        };
        let momentum =
            mc_change >= 2.0 && self.buy_count >= 2 && net >= 0.01 && self.buyers.len() >= 2;
        let label = self.label_for(final_label);
        let trade_stream_missing = self.buy_count + self.sell_count == 0;
        json!({
            "captured_at_utc": chrono::Utc::now().to_rfc3339(),
            "mint": self.mint,
            "age_secs": age,
            "name": self.name,
            "symbol": self.symbol,
            "creator": self.creator,
            "entry_market_cap_sol": self.entry_market_cap_sol,
            "current_market_cap_sol": self.current_market_cap_sol,
            "max_market_cap_sol": self.max_market_cap_sol,
            "min_market_cap_sol": self.min_market_cap_sol,
            "buy_count": self.buy_count,
            "sell_count": self.sell_count,
            "buy_volume_sol": self.buy_volume_sol,
            "sell_volume_sol": self.sell_volume_sol,
            "unique_buyers": self.buyers.len(),
            "unique_sellers": self.sellers.len(),
            "net_flow_sol": net,
            "max_drawdown_pct": max_dd,
            "momentum_passed": momentum,
            "label": label,
            "exit_label": label,
            "trade_stream_missing": trade_stream_missing,
            "first_trade_seen": self.first_trade_seen,
        })
    }
}

pub async fn run_fresh_momentum_daemon() -> anyhow::Result<()> {
    let min_mc = env_f64("FRESH_MIN_MARKET_CAP_SOL", 25.0);
    let min_fee = env_f64("FRESH_MIN_FEE_SOL", 0.2);
    let max_fee = env_f64("FRESH_MAX_FEE_SOL", 1.3);
    let max_active = env_usize("FRESH_MAX_ACTIVE_TRACKED", 50);
    let track_secs = env_u64("FRESH_TRACK_SECS", 300);
    let snapshot_secs = std::env::var("FRESH_SNAPSHOT_SECS")
        .unwrap_or_else(|_| "10,30,60,120,300".into())
        .split(',')
        .filter_map(|s| s.trim().parse::<u64>().ok())
        .collect::<Vec<_>>();
    let subscribe_trades =
        std::env::var("FRESH_SUBSCRIBE_TOKEN_TRADES").unwrap_or_else(|_| "true".into()) == "true";
    let api_key = std::env::var("PUMPPORTAL_API_KEY").unwrap_or_default();

    loop {
        let url = if api_key.is_empty() {
            "wss://pumpportal.fun/api/data".to_string()
        } else {
            format!("wss://pumpportal.fun/api/data?api-key={api_key}")
        };
        match connect_async(&url).await {
            Ok((mut ws, _)) => {
                ws.send(Message::Text(
                    json!({"method":"subscribeNewToken"}).to_string().into(),
                ))
                .await?;
                let mut active: HashMap<String, TrackedFreshToken> = HashMap::new();
                while let Some(msg) = ws.next().await {
                    let text = match msg {
                        Ok(Message::Text(t)) => t.to_string(),
                        Ok(_) => continue,
                        Err(_) => break,
                    };
                    if let Ok(v) = serde_json::from_str::<Value>(&text) {
                        handle_event(
                            &mut ws,
                            &mut active,
                            &snapshot_secs,
                            track_secs,
                            max_active,
                            min_mc,
                            min_fee,
                            max_fee,
                            subscribe_trades,
                            &v,
                        )
                        .await?;
                    }
                    flush_snapshots(
                        &mut ws,
                        &mut active,
                        &snapshot_secs,
                        track_secs,
                        subscribe_trades,
                    )
                    .await?;
                }
            }
            Err(e) => eprintln!("fresh reconnect: {e}"),
        }
        sleep(Duration::from_secs(3)).await;
    }
}

async fn handle_event(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    active: &mut HashMap<String, TrackedFreshToken>,
    _snapshot_secs: &[u64],
    _track_secs: u64,
    max_active: usize,
    min_mc: f64,
    min_fee: f64,
    max_fee: f64,
    subscribe_trades: bool,
    v: &Value,
) -> anyhow::Result<()> {
    let tx_type = v.get("txType").and_then(|x| x.as_str()).unwrap_or("");
    let mint = v
        .get("mint")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if mint.is_empty() {
        return Ok(());
    }
    if tx_type == "create" {
        let mc = v
            .get("marketCapSol")
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0);
        let fee = v.get("solAmount").and_then(|x| x.as_f64()).unwrap_or(0.0);
        if mc >= min_mc && fee >= min_fee && fee <= max_fee {
            if active.len() >= max_active {
                let skipped = json!({"mint":mint,"exit_label":"skipped_capacity","label":"skipped_capacity","captured_at_utc":chrono::Utc::now().to_rfc3339()});
                append_jsonl("fresh_lifecycle_snapshots.jsonl", &skipped)?;
                append_jsonl("fresh_lifecycle_v2_snapshots.jsonl", &skipped)?;
                return Ok(());
            }
            append_jsonl("fresh_momentum_candidates.jsonl", v)?;
            append_jsonl("fresh_lifecycle_v2_candidates.jsonl", v)?;
            active.insert(
                mint.clone(),
                TrackedFreshToken {
                    mint: mint.clone(),
                    name: v.get("name").and_then(|x| x.as_str()).unwrap_or("").into(),
                    symbol: v
                        .get("symbol")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .into(),
                    creator: v
                        .get("traderPublicKey")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .into(),
                    created_at: Instant::now(),
                    entry_market_cap_sol: mc,
                    current_market_cap_sol: mc,
                    max_market_cap_sol: mc,
                    min_market_cap_sol: mc,
                    buy_count: 0,
                    sell_count: 0,
                    buy_volume_sol: 0.0,
                    sell_volume_sol: 0.0,
                    buyers: HashSet::new(),
                    sellers: HashSet::new(),
                    snapshots_done: HashSet::new(),
                    first_trade_seen: false,
                },
            );
            if subscribe_trades {
                ws.send(Message::Text(
                    json!({"method":"subscribeTokenTrade","keys":[mint]})
                        .to_string()
                        .into(),
                ))
                .await?;
            }
        }
    } else if let Some(t) = active.get_mut(&mint) {
        let mc = v
            .get("marketCapSol")
            .and_then(|x| x.as_f64())
            .unwrap_or(t.current_market_cap_sol);
        t.current_market_cap_sol = mc;
        t.max_market_cap_sol = t.max_market_cap_sol.max(mc);
        t.min_market_cap_sol = t.min_market_cap_sol.min(mc);
        let sol = v.get("solAmount").and_then(|x| x.as_f64()).unwrap_or(0.0);
        let wallet = v
            .get("traderPublicKey")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if tx_type == "buy" {
            t.first_trade_seen = true;
            t.buy_count += 1;
            t.buy_volume_sol += sol;
            if !wallet.is_empty() {
                t.buyers.insert(wallet);
            }
        } else if tx_type == "sell" {
            t.first_trade_seen = true;
            t.sell_count += 1;
            t.sell_volume_sol += sol;
            if !wallet.is_empty() {
                t.sellers.insert(wallet);
            }
        }
    }
    Ok(())
}

async fn flush_snapshots(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    active: &mut HashMap<String, TrackedFreshToken>,
    snapshot_secs: &[u64],
    track_secs: u64,
    subscribe_trades: bool,
) -> anyhow::Result<()> {
    let mut done = Vec::new();
    for (mint, t) in active.iter_mut() {
        let age = t.created_at.elapsed().as_secs();
        for s in snapshot_secs {
            if age >= *s && !t.snapshots_done.contains(s) {
                let snapshot = t.snapshot(*s, *s >= track_secs);
                append_jsonl("fresh_lifecycle_snapshots.jsonl", &snapshot)?;
                append_jsonl("fresh_lifecycle_v2_snapshots.jsonl", &snapshot)?;
                t.snapshots_done.insert(*s);
            }
        }
        if age >= track_secs {
            done.push(mint.clone());
        }
    }
    for mint in done {
        active.remove(&mint);
        if subscribe_trades {
            ws.send(Message::Text(
                json!({"method":"unsubscribeTokenTrade","keys":[mint]})
                    .to_string()
                    .into(),
            ))
            .await?;
        }
    }
    Ok(())
}

fn env_f64(key: &str, default: f64) -> f64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
