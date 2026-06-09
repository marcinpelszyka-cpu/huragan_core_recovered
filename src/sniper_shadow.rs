use crate::state::append_jsonl;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

fn parse_ws_json_value(text: &str) -> Result<Value, serde_json::Error> {
    match sonic_rs::from_str::<serde_json::Value>(text) {
        Ok(v) => Ok(v),
        Err(_) => serde_json::from_str::<Value>(text),
    }
}

const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";
const DEFAULT_OUT: &str = "sniper_follow_shadow.jsonl";

#[derive(Debug, Clone)]
struct FreshCandidate {
    mint: String,
    creator: String,
    name: String,
    symbol: String,
    entry_market_cap_sol: f64,
    first_seen_unix: i64,
}

pub async fn run_sniper_shadow_daemon() -> anyhow::Result<()> {
    let rpc_url = std::env::var("RPC_URL")?;
    let api_key = std::env::var("PUMPPORTAL_API_KEY").unwrap_or_default();
    let out_path = std::env::var("SNIPER_SHADOW_OUT").unwrap_or_else(|_| DEFAULT_OUT.into());
    let target_mc_sol = env_f64("SNIPER_SHADOW_TARGET_MC_SOL", 25.0);
    let mc_tolerance_sol = env_f64("SNIPER_SHADOW_MC_TOLERANCE_SOL", 25.0);
    let delay_secs = env_u64("SNIPER_SHADOW_GTFA_DELAY_SECS", 12);
    let max_age_secs = env_i64("SNIPER_SHADOW_MAX_AGE_SECS", 60);
    let client = reqwest::Client::new();

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
                println!("🔎 [SNIPER_SHADOW] subscribed new token stream");
                let mut seen: HashSet<String> = HashSet::new();
                while let Some(msg) = ws.next().await {
                    let text = match msg {
                        Ok(Message::Text(t)) => t.to_string(),
                        Ok(_) => continue,
                        Err(e) => {
                            eprintln!("⚠️ [SNIPER_SHADOW] websocket drop: {e}");
                            break;
                        }
                    };
                    let Ok(v) = parse_ws_json_value(&text) else {
                        continue;
                    };
                    let Some(candidate) = parse_new_token_candidate(&v) else {
                        continue;
                    };
                    if !seen.insert(candidate.mint.clone()) {
                        continue;
                    }
                    if !mc_in_shadow_range(
                        candidate.entry_market_cap_sol,
                        target_mc_sol,
                        mc_tolerance_sol,
                    ) {
                        continue;
                    }
                    let rpc_url = rpc_url.clone();
                    let out_path = out_path.clone();
                    let client = client.clone();
                    tokio::spawn(async move {
                        sleep(Duration::from_secs(delay_secs)).await;
                        match build_shadow_signal(&client, &rpc_url, candidate, max_age_secs).await {
                            Ok(row) => {
                                let _ = append_jsonl(out_path, &row);
                            }
                            Err(e) => {
                                let err = json!({
                                    "captured_at_utc": chrono::Utc::now().to_rfc3339(),
                                    "dataset": "sniper_follow_shadow_error",
                                    "reason": sanitize(&e.to_string()),
                                });
                                let _ = append_jsonl("sniper_follow_shadow_errors.jsonl", &err);
                            }
                        }
                    });
                    if seen.len() > 50_000 {
                        seen.clear();
                    }
                }
            }
            Err(e) => eprintln!("sniper shadow reconnect: {e}"),
        }
        sleep(Duration::from_secs(3)).await;
    }
}

fn parse_new_token_candidate(v: &Value) -> Option<FreshCandidate> {
    if v.get("txType").and_then(|x| x.as_str()) != Some("create") {
        return None;
    }
    let mint = v.get("mint")?.as_str()?.to_string();
    let quote_mint = v
        .get("quoteMint")
        .or_else(|| v.get("quote_mint"))
        .and_then(|x| x.as_str())
        .unwrap_or(WSOL_MINT);
    if quote_mint != WSOL_MINT {
        return None;
    }
    Some(FreshCandidate {
        mint,
        creator: v
            .get("traderPublicKey")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        name: v
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        symbol: v
            .get("symbol")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        entry_market_cap_sol: v
            .get("marketCapSol")
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0),
        first_seen_unix: chrono::Utc::now().timestamp(),
    })
}

fn mc_in_shadow_range(mc: f64, target: f64, tolerance: f64) -> bool {
    if mc <= 0.0 {
        return false;
    }
    let low = (target - tolerance).max(0.0);
    let high = target + tolerance;
    mc >= low && mc <= high
}

async fn build_shadow_signal(
    client: &reqwest::Client,
    rpc_url: &str,
    candidate: FreshCandidate,
    max_age_secs: i64,
) -> anyhow::Result<Value> {
    let txs = gtfa_fetch(client, rpc_url, &candidate.mint, candidate.first_seen_unix - 5, candidate.first_seen_unix + max_age_secs).await?;
    let events = extract_trade_events(&candidate.mint, candidate.first_seen_unix, &txs);
    let early = events
        .iter()
        .filter(|e| e.age_secs <= 10 && e.side == "buy" && e.quote_delta_sol >= 0.01)
        .collect::<Vec<_>>();
    let wallets = early
        .iter()
        .filter_map(|e| (!e.owner.is_empty()).then_some(e.owner.clone()))
        .collect::<HashSet<_>>();
    let total_buy_sol = early.iter().map(|e| e.quote_delta_sol).sum::<f64>();
    let bought = early.iter().map(|e| e.token_delta_raw).sum::<u64>();
    let early_wallets = wallets.iter().cloned().collect::<HashSet<_>>();
    let sold = events
        .iter()
        .filter(|e| e.age_secs <= 10 && e.side == "sell" && early_wallets.contains(&e.owner))
        .map(|e| e.token_delta_raw)
        .sum::<u64>();
    let hold_ratio = if bought > 0 {
        (bought.saturating_sub(sold)) as f64 / bought as f64
    } else {
        0.0
    };
    let passed = wallets.len() >= 2 && total_buy_sol >= 0.03 && hold_ratio >= 0.75;
    Ok(json!({
        "captured_at_utc": chrono::Utc::now().to_rfc3339(),
        "dataset": "sniper_follow_shadow",
        "mint": candidate.mint,
        "creator": candidate.creator,
        "name": candidate.name,
        "symbol": candidate.symbol,
        "entry_market_cap_sol": candidate.entry_market_cap_sol,
        "age_limit_secs": max_age_secs,
        "early_window_secs": 10,
        "trade_event_count": events.len(),
        "early_sniper_wallet_count": wallets.len(),
        "early_sniper_buy_sol": total_buy_sol,
        "early_sniper_hold_ratio_10s": hold_ratio,
        "signal": if passed { "FOLLOW_SHADOW" } else { "NO_SIGNAL" },
        "passed": passed,
        "live_allowed": false,
        "source": "pumpportal_newtoken_plus_helius_gtfa",
    }))
}

async fn gtfa_fetch(
    client: &reqwest::Client,
    rpc_url: &str,
    address: &str,
    start_time: i64,
    end_time: i64,
) -> anyhow::Result<Vec<Value>> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTransactionsForAddress",
        "params": [address, {
            "transactionDetails": "full",
            "sortOrder": "asc",
            "limit": 100,
            "filters": {
                "status": "succeeded",
                "blockTime": {"gte": start_time, "lte": end_time}
            }
        }]
    });
    let resp: Value = client.post(rpc_url).json(&body).send().await?.json().await?;
    if let Some(err) = resp.get("error") {
        anyhow::bail!("rpc_error:{}", sanitize(&err.to_string()));
    }
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    let data = if let Some(arr) = result.as_array() {
        arr.clone()
    } else {
        result
            .get("data")
            .and_then(|x| x.as_array())
            .cloned()
            .unwrap_or_default()
    };
    Ok(data)
}

#[derive(Debug)]
struct ShadowTradeEvent {
    owner: String,
    side: &'static str,
    age_secs: i64,
    token_delta_raw: u64,
    quote_delta_sol: f64,
}

fn extract_trade_events(mint: &str, first_seen_unix: i64, rows: &[Value]) -> Vec<ShadowTradeEvent> {
    let mut out = Vec::new();
    for row in rows {
        let tx = unwrap_tx(row);
        let block_time = row
            .get("blockTime")
            .or_else(|| tx.get("blockTime"))
            .and_then(|x| x.as_i64())
            .unwrap_or(first_seen_unix);
        let keys = account_keys(&tx);
        let signer = primary_signer(&keys);
        let native_delta = native_sol_delta_for(&tx, &keys, &signer).abs();
        let pre = token_balance_map(&tx, "preTokenBalances", &keys);
        let post = token_balance_map(&tx, "postTokenBalances", &keys);
        let mut all_keys = pre.keys().cloned().collect::<HashSet<_>>();
        all_keys.extend(post.keys().cloned());
        for key in all_keys {
            let Some((_, bal_mint, owner)) = parse_balance_key(&key) else {
                continue;
            };
            if bal_mint != mint {
                continue;
            }
            let before = *pre.get(&key).unwrap_or(&0);
            let after = *post.get(&key).unwrap_or(&0);
            if before == after {
                continue;
            }
            if !owner.is_empty() && !signer.is_empty() && owner != signer {
                continue;
            }
            let side = if after > before { "buy" } else { "sell" };
            out.push(ShadowTradeEvent {
                owner: if owner.is_empty() { signer.clone() } else { owner },
                side,
                age_secs: (block_time - first_seen_unix).max(0),
                token_delta_raw: after.abs_diff(before),
                quote_delta_sol: native_delta,
            });
        }
    }
    out
}

fn unwrap_tx(row: &Value) -> Value {
    row.get("nativeTransaction")
        .cloned()
        .unwrap_or_else(|| row.clone())
}

fn account_keys(tx: &Value) -> Vec<(String, bool)> {
    tx.get("transaction")
        .and_then(|t| t.get("message"))
        .and_then(|m| m.get("accountKeys"))
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .map(|k| {
                    if let Some(s) = k.as_str() {
                        (s.to_string(), false)
                    } else {
                        (
                            k.get("pubkey")
                                .or_else(|| k.get("account"))
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string(),
                            k.get("signer").and_then(|x| x.as_bool()).unwrap_or(false),
                        )
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn primary_signer(keys: &[(String, bool)]) -> String {
    keys.iter()
        .find(|(_, signer)| *signer)
        .or_else(|| keys.first())
        .map(|(k, _)| k.clone())
        .unwrap_or_default()
}

fn native_sol_delta_for(tx: &Value, keys: &[(String, bool)], pubkey: &str) -> f64 {
    let Some(idx) = keys.iter().position(|(k, _)| k == pubkey) else {
        return 0.0;
    };
    let meta = tx.get("meta").unwrap_or(&Value::Null);
    let pre = meta
        .get("preBalances")
        .and_then(|x| x.as_array())
        .and_then(|a| a.get(idx))
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let post = meta
        .get("postBalances")
        .and_then(|x| x.as_array())
        .and_then(|a| a.get(idx))
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    (post - pre) as f64 / 1e9
}

fn token_balance_map(tx: &Value, field: &str, keys: &[(String, bool)]) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    for b in tx
        .get("meta")
        .and_then(|m| m.get(field))
        .and_then(|x| x.as_array())
        .into_iter()
        .flatten()
    {
        let idx = b.get("accountIndex").and_then(|x| x.as_u64()).unwrap_or(u64::MAX) as usize;
        let account = keys.get(idx).map(|(k, _)| k.as_str()).unwrap_or("");
        let mint = b.get("mint").and_then(|x| x.as_str()).unwrap_or("");
        let owner = b.get("owner").and_then(|x| x.as_str()).unwrap_or("");
        let amount = b
            .get("uiTokenAmount")
            .and_then(|x| x.get("amount"))
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        out.insert(format!("{account}|{mint}|{owner}"), amount);
    }
    out
}

fn parse_balance_key(key: &str) -> Option<(String, String, String)> {
    let mut parts = key.splitn(3, '|');
    Some((
        parts.next()?.to_string(),
        parts.next()?.to_string(),
        parts.next()?.to_string(),
    ))
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || " .,:;_=-/()".contains(c) { c } else { '_' })
        .collect::<String>()
        .chars()
        .take(240)
        .collect()
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

fn env_i64(key: &str, default: i64) -> i64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mc_range_uses_target_and_tolerance() {
        assert!(mc_in_shadow_range(25.0, 25.0, 5.0));
        assert!(mc_in_shadow_range(20.0, 25.0, 5.0));
        assert!(!mc_in_shadow_range(19.9, 25.0, 5.0));
    }

    #[test]
    fn extracts_buy_from_token_balance_delta() {
        let row = json!({
            "blockTime": 100,
            "transaction": {"message": {"accountKeys": [{"pubkey":"Wallet","signer":true}, "Ata"]}},
            "meta": {
                "preBalances": [1000000000i64, 0],
                "postBalances": [990000000i64, 0],
                "preTokenBalances": [{"accountIndex":1,"mint":"Mint","owner":"Wallet","uiTokenAmount":{"amount":"0"}}],
                "postTokenBalances": [{"accountIndex":1,"mint":"Mint","owner":"Wallet","uiTokenAmount":{"amount":"100"}}]
            }
        });
        let events = extract_trade_events("Mint", 95, &[row]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].side, "buy");
        assert_eq!(events[0].age_secs, 5);
        assert_eq!(events[0].token_delta_raw, 100);
        assert!((events[0].quote_delta_sol - 0.01).abs() < 1e-12);
    }
}
