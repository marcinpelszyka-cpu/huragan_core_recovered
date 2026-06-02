use crate::engine::{MigrationTarget, QuoteAsset};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const PUMP_AMM_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
const DEBUG_UNPARSED_PATH: &str = "helius_unparsed_transactions.jsonl";
const BUY_SAMPLE_PATH: &str = "helius_buy_samples.jsonl";

pub async fn run_helius_log_scout(tx: mpsc::Sender<MigrationTarget>) -> anyhow::Result<()> {
    if std::env::var("HELIUS_MIGRATION_ENABLED").unwrap_or_else(|_| "true".into()) != "true" {
        return Ok(());
    }
    let ws_url = std::env::var("RPC_WS_URL")?;
    let rpc_url = std::env::var("RPC_URL")?;
    let client = reqwest::Client::new();
    let buy_capture_enabled = env_bool("HELIUS_BUY_CAPTURE_ENABLED", false);
    let buy_capture_limit = env_u64("HELIUS_BUY_CAPTURE_MAX_PER_RUN", 25);
    let mut seen_migrations: HashSet<String> = HashSet::new();
    let mut seen_buys: HashSet<String> = HashSet::new();
    let mut buy_samples_written = 0u64;

    loop {
        match connect_async(&ws_url).await {
            Ok((mut ws, _)) => {
                let sub = serde_json::json!({
                    "jsonrpc":"2.0",
                    "id":1,
                    "method":"logsSubscribe",
                    "params":[{"mentions":[PUMP_AMM_PROGRAM]},{"commitment":"processed"}]
                });
                ws.send(Message::Text(sub.to_string().into())).await?;
                println!("📡 [HELIUS] subscribed Pump AMM logs");

                while let Some(msg) = ws.next().await {
                    let text = match msg {
                        Ok(Message::Text(t)) => t.to_string(),
                        Ok(_) => continue,
                        Err(e) => {
                            eprintln!("⚠️ [HELIUS] websocket drop: {e}");
                            break;
                        }
                    };
                    let Ok(v) = serde_json::from_str::<Value>(&text) else {
                        continue;
                    };
                    let Some(signature) = extract_signature(&v) else {
                        continue;
                    };
                    let logs = extract_log_messages(&v);
                    if buy_capture_enabled
                        && buy_samples_written < buy_capture_limit
                        && looks_like_buy_log(&logs)
                        && seen_buys.insert(signature.clone())
                    {
                        match fetch_transaction_with_retry(&client, &rpc_url, &signature, "json")
                            .await
                        {
                            Ok(tx_json) => {
                                if capture_pump_amm_buy_sample(&signature, &tx_json) {
                                    buy_samples_written += 1;
                                } else {
                                    write_unparsed(&signature, "buy_capture_no_pump_ix", &tx_json);
                                }
                            }
                            Err(e) => {
                                let debug = serde_json::json!({"signature": signature, "reason": format!("buy_get_transaction_failed:{e}")});
                                let _ = crate::state::append_jsonl(DEBUG_UNPARSED_PATH, &debug);
                            }
                        }
                        if seen_buys.len() > 20_000 {
                            seen_buys.clear();
                        }
                    }

                    if !looks_like_create_pool_log(&logs) {
                        continue;
                    }
                    if !seen_migrations.insert(signature.clone()) {
                        continue;
                    }
                    if seen_migrations.len() > 20_000 {
                        seen_migrations.clear();
                    }

                    match fetch_transaction_with_retry(&client, &rpc_url, &signature, "jsonParsed")
                        .await
                    {
                        Ok(tx_json) => match parse_pump_amm_transaction(&signature, &tx_json) {
                            Some(target) => {
                                println!(
                                    "🎯 [HELIUS_MIGRATION] mint={} pool={} quote_asset={} base={} quote={} quote_vault={} coin_vault={}",
                                    target.mint,
                                    target.pool_state,
                                    QuoteAsset::from_mint(&target.quote_asset_mint).symbol(),
                                    target.base_mint,
                                    target.quote_mint,
                                    target.pool_base_token_account,
                                    target.pool_quote_token_account
                                );
                                let _ = tx.send(target).await;
                            }
                            None => {
                                write_unparsed(&signature, "parse_no_target", &tx_json);
                            }
                        },
                        Err(e) => {
                            let debug = serde_json::json!({"signature": signature, "reason": format!("get_transaction_failed:{e}")});
                            let _ = crate::state::append_jsonl(DEBUG_UNPARSED_PATH, &debug);
                        }
                    }
                }
            }
            Err(e) => eprintln!("helius reconnect: {e}"),
        }
        sleep(Duration::from_secs(2)).await;
    }
}

fn extract_signature(v: &Value) -> Option<String> {
    v.get("params")?
        .get("result")?
        .get("value")?
        .get("signature")?
        .as_str()
        .map(ToString::to_string)
}

fn extract_log_messages(v: &Value) -> Vec<String> {
    v.get("params")
        .and_then(|p| p.get("result"))
        .and_then(|r| r.get("value"))
        .and_then(|val| val.get("logs"))
        .and_then(|logs| logs.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn looks_like_create_pool_log(logs: &[String]) -> bool {
    logs.iter().any(|l| {
        let lower = l.to_ascii_lowercase();
        (lower.contains("create") || lower.contains("initialize")) && lower.contains("pool")
    })
}

fn looks_like_buy_log(logs: &[String]) -> bool {
    logs.iter().any(|l| l.contains("Instruction: Buy"))
}

async fn fetch_transaction_with_retry(
    client: &reqwest::Client,
    rpc_url: &str,
    signature: &str,
    encoding: &str,
) -> anyhow::Result<Value> {
    let attempts = std::env::var("HELIUS_GET_TX_RETRIES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8u64);
    let delay_ms = std::env::var("HELIUS_GET_TX_RETRY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(400u64);
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 0..attempts {
        match fetch_transaction(client, rpc_url, signature, encoding).await {
            Ok(v) => return Ok(v),
            Err(e) => {
                let msg = e.to_string();
                let retryable = msg.contains("transaction_not_available")
                    || msg.contains("rate limited")
                    || msg.contains("-32429");
                last_err = Some(e);
                if !retryable || attempt + 1 >= attempts {
                    break;
                }
                sleep(Duration::from_millis(delay_ms.saturating_mul(attempt + 1))).await;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("get_transaction_failed")))
}

async fn fetch_transaction(
    client: &reqwest::Client,
    rpc_url: &str,
    signature: &str,
    encoding: &str,
) -> anyhow::Result<Value> {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getTransaction",
        "params": [
            signature,
            {
                "encoding": encoding,
                "commitment": "confirmed",
                "maxSupportedTransactionVersion": 0
            }
        ]
    });
    let resp: Value = client
        .post(rpc_url)
        .json(&body)
        .send()
        .await?
        .json()
        .await?;
    if let Some(err) = resp.get("error") {
        anyhow::bail!("rpc_error:{err}");
    }
    let result = resp.get("result").cloned().unwrap_or(Value::Null);
    if result.is_null() {
        anyhow::bail!("transaction_not_available");
    }
    Ok(result)
}

fn capture_pump_amm_buy_sample(signature: &str, tx: &Value) -> bool {
    let expanded_accounts = expanded_account_metas(tx);
    let pump_ixs = find_pump_amm_ixs(tx, &expanded_accounts);
    if pump_ixs.is_empty() {
        return false;
    }
    let pre_token_balances = token_balance_snapshots(tx, &expanded_accounts, "preTokenBalances");
    let post_token_balances = token_balance_snapshots(tx, &expanded_accounts, "postTokenBalances");
    let token_deltas = token_balance_deltas(&pre_token_balances, &post_token_balances);
    let log_messages = tx_log_messages(tx);
    let instructions: Vec<Value> = pump_ixs
        .into_iter()
        .map(|ix| {
            let resolved_accounts: Vec<Value> = ix
                .account_indices
                .iter()
                .map(|idx| {
                    expanded_accounts
                        .get(*idx)
                        .map(|a| {
                            serde_json::json!({
                                "index": idx,
                                "pubkey": a.pubkey.clone(),
                                "source": a.source.clone(),
                                "signer": a.signer,
                                "writable": a.writable,
                            })
                        })
                        .unwrap_or_else(|| serde_json::json!({"index": idx, "missing": true}))
                })
                .collect();
            let data_analysis = analyze_instruction_data(&ix.data_base58, &token_deltas);
            serde_json::json!({
                "location": ix.location.clone(),
                "index": ix.index,
                "program_id": ix.program_id.clone(),
                "data_base58": ix.data_base58.clone(),
                "account_indices": ix.account_indices.clone(),
                "accounts": resolved_accounts,
                "account_count": resolved_accounts.len(),
                "data_analysis": data_analysis,
            })
        })
        .collect();
    let sample = serde_json::json!({
        "signature": signature,
        "slot": tx.get("slot"),
        "blockTime": tx.get("blockTime"),
        "expanded_accounts": expanded_accounts,
        "pump_amm_instruction_count": instructions.len(),
        "pump_amm_instructions": instructions,
        "pre_token_balances": pre_token_balances,
        "post_token_balances": post_token_balances,
        "token_deltas": token_deltas,
        "logMessages": log_messages,
    });
    let _ = crate::state::append_jsonl(BUY_SAMPLE_PATH, &sample);
    true
}

fn parse_pump_amm_transaction(signature: &str, tx: &Value) -> Option<MigrationTarget> {
    if !looks_like_create_pool_log(&tx_log_messages(tx)) {
        return None;
    }
    let keys = account_keys(tx);
    if keys.is_empty() {
        return None;
    }
    let pump_ix_accounts = find_pump_create_pool_ix(tx, &keys)?;
    let pool_state = pump_ix_accounts.first()?.clone();
    let token_balances = post_token_balances_owned(tx, &keys);

    // Pool vaults must be token accounts owned by the Pump AMM pool state.
    // Do not pick the largest postTokenBalance globally: that can be a trader,
    // fee-recipient, or other auxiliary account touched by MigrateV2.
    let Some((quote, coin)) = resolve_pool_vaults(&pool_state, &token_balances) else {
        write_unparsed(signature, "pool_vault_resolution_failed", tx);
        return None;
    };
    if quote.amount == 0 || coin.amount == 0 {
        return None;
    }

    if pool_state.is_empty() || pool_state == quote.account || pool_state == coin.account {
        return None;
    }

    let creator = infer_creator(tx, &keys).unwrap_or_default();
    let block_time = tx
        .get("blockTime")
        .and_then(|v| v.as_i64())
        .unwrap_or_default();

    Some(MigrationTarget {
        mint: coin.mint.clone(),
        name: "HELIUS_MIGRATION".into(),
        symbol: "AMM".into(),
        source: "helius_migration".into(),
        pool_state,
        base_mint: quote.mint.clone(),
        quote_mint: coin.mint.clone(),
        quote_asset_mint: quote.mint.clone(),
        pool_base_token_account: quote.account.clone(),
        pool_quote_token_account: coin.account.clone(),
        creator,
        migration_signature: signature.to_string(),
        migration_block_time: block_time,
        ..Default::default()
    })
}

#[derive(Debug, Clone)]
struct TokenBalance {
    account: String,
    mint: String,
    amount: u64,
    owner: String,
}

#[derive(Debug, Clone, Serialize)]
struct ExpandedAccountMeta {
    pubkey: String,
    source: String,
    signer: bool,
    writable: bool,
}

#[derive(Debug, Clone)]
struct PumpAmmInstruction {
    location: String,
    index: usize,
    program_id: String,
    data_base58: String,
    account_indices: Vec<usize>,
}

#[derive(Debug, Clone, Serialize)]
struct TokenBalanceSnapshot {
    account_index: usize,
    account: String,
    mint: String,
    owner: String,
    amount: u64,
}

#[derive(Debug, Clone, Serialize)]
struct TokenBalanceDelta {
    account_index: usize,
    account: String,
    mint: String,
    owner: String,
    pre_amount: u64,
    post_amount: u64,
    delta: i128,
}

fn account_keys(tx: &Value) -> Vec<String> {
    tx.pointer("/transaction/message/accountKeys")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|k| {
                    if let Some(s) = k.as_str() {
                        Some(s.to_string())
                    } else {
                        k.get("pubkey")
                            .and_then(|p| p.as_str())
                            .map(ToString::to_string)
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn expanded_account_metas(tx: &Value) -> Vec<ExpandedAccountMeta> {
    let account_keys = tx
        .pointer("/transaction/message/accountKeys")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let static_count = account_keys.len();
    let num_required_signatures = tx
        .pointer("/transaction/message/header/numRequiredSignatures")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let num_readonly_signed = tx
        .pointer("/transaction/message/header/numReadonlySignedAccounts")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let num_readonly_unsigned = tx
        .pointer("/transaction/message/header/numReadonlyUnsignedAccounts")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    let mut out = Vec::with_capacity(static_count + 16);
    for (idx, key) in account_keys.iter().enumerate() {
        let pubkey = account_key_pubkey(key).unwrap_or_default();
        let signer = key
            .get("signer")
            .and_then(|v| v.as_bool())
            .unwrap_or(idx < num_required_signatures);
        let writable = key
            .get("writable")
            .and_then(|v| v.as_bool())
            .unwrap_or_else(|| {
                if idx < num_required_signatures {
                    idx < num_required_signatures.saturating_sub(num_readonly_signed)
                } else {
                    idx < static_count.saturating_sub(num_readonly_unsigned)
                }
            });
        out.push(ExpandedAccountMeta {
            pubkey,
            source: "static".to_string(),
            signer,
            writable,
        });
    }

    if let Some(writable) = tx
        .pointer("/meta/loadedAddresses/writable")
        .and_then(|v| v.as_array())
    {
        for key in writable {
            if let Some(pubkey) = key.as_str() {
                out.push(ExpandedAccountMeta {
                    pubkey: pubkey.to_string(),
                    source: "loaded_writable".to_string(),
                    signer: false,
                    writable: true,
                });
            }
        }
    }
    if let Some(readonly) = tx
        .pointer("/meta/loadedAddresses/readonly")
        .and_then(|v| v.as_array())
    {
        for key in readonly {
            if let Some(pubkey) = key.as_str() {
                out.push(ExpandedAccountMeta {
                    pubkey: pubkey.to_string(),
                    source: "loaded_readonly".to_string(),
                    signer: false,
                    writable: false,
                });
            }
        }
    }
    out
}

fn account_key_pubkey(key: &Value) -> Option<String> {
    if let Some(s) = key.as_str() {
        Some(s.to_string())
    } else {
        key.get("pubkey")
            .and_then(|p| p.as_str())
            .map(ToString::to_string)
    }
}

fn find_pump_amm_ixs(tx: &Value, accounts: &[ExpandedAccountMeta]) -> Vec<PumpAmmInstruction> {
    let mut out = Vec::new();
    if let Some(instructions) = tx
        .pointer("/transaction/message/instructions")
        .and_then(|v| v.as_array())
    {
        collect_pump_amm_ixs_in(instructions, accounts, "top".to_string(), &mut out);
    }
    if let Some(groups) = tx
        .pointer("/meta/innerInstructions")
        .and_then(|v| v.as_array())
    {
        for group in groups {
            let parent_index = group
                .get("index")
                .and_then(|v| v.as_u64())
                .unwrap_or_default();
            if let Some(instructions) = group.get("instructions").and_then(|v| v.as_array()) {
                collect_pump_amm_ixs_in(
                    instructions,
                    accounts,
                    format!("inner:{parent_index}"),
                    &mut out,
                );
            }
        }
    }
    out
}

fn collect_pump_amm_ixs_in(
    instructions: &[Value],
    accounts: &[ExpandedAccountMeta],
    location: String,
    out: &mut Vec<PumpAmmInstruction>,
) {
    for (index, ix) in instructions.iter().enumerate() {
        let Some(program_id) = ix_program_id(ix, accounts) else {
            continue;
        };
        if program_id != PUMP_AMM_PROGRAM {
            continue;
        }
        let data_base58 = ix
            .get("data")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let account_indices = ix_account_indices(ix, accounts);
        out.push(PumpAmmInstruction {
            location: location.clone(),
            index,
            program_id,
            data_base58,
            account_indices,
        });
    }
}

fn ix_program_id(ix: &Value, accounts: &[ExpandedAccountMeta]) -> Option<String> {
    ix.get("programId")
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
        .or_else(|| {
            ix.get("programIdIndex")
                .and_then(|v| v.as_u64())
                .and_then(|idx| accounts.get(idx as usize).map(|a| a.pubkey.clone()))
        })
}

fn ix_account_indices(ix: &Value, accounts: &[ExpandedAccountMeta]) -> Vec<usize> {
    ix.get("accounts")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    if let Some(idx) = a.as_u64() {
                        Some(idx as usize)
                    } else {
                        let pubkey = a.as_str()?;
                        accounts.iter().position(|meta| meta.pubkey == pubkey)
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn token_balance_snapshots(
    tx: &Value,
    accounts: &[ExpandedAccountMeta],
    field: &str,
) -> Vec<TokenBalanceSnapshot> {
    tx.pointer(&format!("/meta/{field}"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|b| {
                    let account_index = b.get("accountIndex")?.as_u64()? as usize;
                    let account = accounts
                        .get(account_index)
                        .map(|a| a.pubkey.clone())
                        .unwrap_or_default();
                    let mint = b.get("mint")?.as_str()?.to_string();
                    let owner = b
                        .get("owner")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let amount = b.pointer("/uiTokenAmount/amount")?.as_str()?.parse().ok()?;
                    Some(TokenBalanceSnapshot {
                        account_index,
                        account,
                        mint,
                        owner,
                        amount,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn token_balance_deltas(
    pre: &[TokenBalanceSnapshot],
    post: &[TokenBalanceSnapshot],
) -> Vec<TokenBalanceDelta> {
    let mut rows: HashMap<(usize, String), TokenBalanceDelta> = HashMap::new();
    for b in pre {
        rows.insert(
            (b.account_index, b.mint.clone()),
            TokenBalanceDelta {
                account_index: b.account_index,
                account: b.account.clone(),
                mint: b.mint.clone(),
                owner: b.owner.clone(),
                pre_amount: b.amount,
                post_amount: 0,
                delta: -(b.amount as i128),
            },
        );
    }
    for b in post {
        rows.entry((b.account_index, b.mint.clone()))
            .and_modify(|row| {
                row.account = if row.account.is_empty() {
                    b.account.clone()
                } else {
                    row.account.clone()
                };
                row.owner = if row.owner.is_empty() {
                    b.owner.clone()
                } else {
                    row.owner.clone()
                };
                row.post_amount = b.amount;
                row.delta = b.amount as i128 - row.pre_amount as i128;
            })
            .or_insert_with(|| TokenBalanceDelta {
                account_index: b.account_index,
                account: b.account.clone(),
                mint: b.mint.clone(),
                owner: b.owner.clone(),
                pre_amount: 0,
                post_amount: b.amount,
                delta: b.amount as i128,
            });
    }
    let mut out: Vec<TokenBalanceDelta> = rows.into_values().filter(|row| row.delta != 0).collect();
    out.sort_by_key(|row| row.account_index);
    out
}

fn analyze_instruction_data(data_base58: &str, token_deltas: &[TokenBalanceDelta]) -> Value {
    let expected_buy = anchor_discriminator("global:buy");
    let expected_buy_hex = bytes_to_hex(&expected_buy);
    let Ok(bytes) = bs58::decode(data_base58).into_vec() else {
        return serde_json::json!({
            "decode_error": "invalid_base58",
            "expected_global_buy_discriminator_hex": expected_buy_hex,
        });
    };
    let discriminator = if bytes.len() >= 8 {
        bytes[..8].to_vec()
    } else {
        bytes.clone()
    };
    let discriminator_hex = bytes_to_hex(&discriminator);
    let payload = if bytes.len() > 8 { &bytes[8..] } else { &[] };
    let u64_fields: Vec<Value> = payload
        .chunks_exact(8)
        .enumerate()
        .map(|(field_index, chunk)| {
            let value = u64::from_le_bytes(chunk.try_into().expect("u64 chunk"));
            serde_json::json!({
                "field_index": field_index,
                "payload_offset": field_index * 8,
                "value": value,
                "matches": amount_match_guesses(value as u128, token_deltas),
            })
        })
        .collect();
    let u128_fields: Vec<Value> = payload
        .chunks_exact(16)
        .enumerate()
        .map(|(field_index, chunk)| {
            let value = u128::from_le_bytes(chunk.try_into().expect("u128 chunk"));
            serde_json::json!({
                "field_index": field_index,
                "payload_offset": field_index * 16,
                "value": value.to_string(),
                "matches": amount_match_guesses(value, token_deltas),
            })
        })
        .collect();
    serde_json::json!({
        "byte_len": bytes.len(),
        "discriminator_hex": discriminator_hex,
        "expected_global_buy_discriminator_hex": expected_buy_hex,
        "matches_global_buy_discriminator": discriminator == expected_buy,
        "u64_le_fields": u64_fields,
        "u128_le_fields": u128_fields,
    })
}

fn amount_match_guesses(value: u128, token_deltas: &[TokenBalanceDelta]) -> Vec<Value> {
    token_deltas
        .iter()
        .filter_map(|delta| {
            if delta.delta.unsigned_abs() != value {
                return None;
            }
            let role_guess = match (
                QuoteAsset::from_mint(&delta.mint),
                delta.delta.is_positive(),
            ) {
                (QuoteAsset::Wsol, false) => "matches_spend_lamports_or_max_quote_in",
                (QuoteAsset::Wsol, true) => "matches_pool_quote_in_or_refund",
                (QuoteAsset::Usdc, false) => "matches_quote_spend_or_max_quote_in",
                (QuoteAsset::Usdc, true) => "matches_pool_quote_in_or_refund",
                (QuoteAsset::Unsupported, true) => "matches_expected_or_actual_tokens_out",
                (QuoteAsset::Unsupported, false) => "matches_token_debit",
            };
            Some(serde_json::json!({
                "role_guess": role_guess,
                "account_index": delta.account_index,
                "account": delta.account.clone(),
                "mint": delta.mint.clone(),
                "owner": delta.owner.clone(),
                "delta": delta.delta.to_string(),
            }))
        })
        .collect()
}

fn anchor_discriminator(name: &str) -> [u8; 8] {
    let hash = solana_sdk::hash::hash(name.as_bytes());
    let mut out = [0u8; 8];
    out.copy_from_slice(&hash.as_ref()[..8]);
    out
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn env_bool(key: &str, default: bool) -> bool {
    std::env::var(key).map(|v| v == "true").unwrap_or(default)
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn find_pump_create_pool_ix(tx: &Value, keys: &[String]) -> Option<Vec<String>> {
    if let Some(instructions) = tx
        .pointer("/transaction/message/instructions")
        .and_then(|v| v.as_array())
    {
        if let Some(accounts) = find_pump_ix_accounts_in(instructions, keys) {
            return Some(accounts);
        }
    }

    let inner = tx.pointer("/meta/innerInstructions")?.as_array()?;
    for group in inner {
        let Some(instructions) = group.get("instructions").and_then(|v| v.as_array()) else {
            continue;
        };
        if let Some(accounts) = find_pump_ix_accounts_in(instructions, keys) {
            return Some(accounts);
        }
    }
    None
}

fn find_pump_ix_accounts_in(instructions: &[Value], keys: &[String]) -> Option<Vec<String>> {
    for ix in instructions {
        let program = ix.get("programId").and_then(|v| v.as_str()).or_else(|| {
            ix.get("programIdIndex")
                .and_then(|v| v.as_u64())
                .and_then(|i| keys.get(i as usize).map(String::as_str))
        });
        if program != Some(PUMP_AMM_PROGRAM) {
            continue;
        }
        let accounts = ix.get("accounts")?.as_array()?;
        let out: Vec<String> = accounts
            .iter()
            .filter_map(|a| {
                if let Some(s) = a.as_str() {
                    Some(s.to_string())
                } else {
                    a.as_u64().and_then(|i| keys.get(i as usize).cloned())
                }
            })
            .collect();
        if out.len() >= 4 {
            return Some(out);
        }
    }
    None
}

fn tx_log_messages(tx: &Value) -> Vec<String> {
    tx.pointer("/meta/logMessages")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|x| x.as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn post_token_balances_owned(tx: &Value, keys: &[String]) -> Vec<TokenBalance> {
    tx.pointer("/meta/postTokenBalances")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|b| {
                    let idx = b.get("accountIndex")?.as_u64()? as usize;
                    let account = keys.get(idx)?.clone();
                    let mint = b.get("mint")?.as_str()?.to_string();
                    let amount = b.pointer("/uiTokenAmount/amount")?.as_str()?.parse().ok()?;
                    let owner = b
                        .get("owner")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    Some(TokenBalance {
                        account,
                        mint,
                        amount,
                        owner,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn resolve_pool_vaults(
    pool_state: &str,
    balances: &[TokenBalance],
) -> Option<(TokenBalance, TokenBalance)> {
    let mut pool_owned: Vec<TokenBalance> = balances
        .iter()
        .filter(|b| b.owner == pool_state && b.amount > 0)
        .cloned()
        .collect();
    if pool_owned.len() < 2 {
        return None;
    }

    pool_owned.sort_by_key(|b| std::cmp::Reverse(b.amount));

    let quote = pool_owned
        .iter()
        .filter(|b| QuoteAsset::from_mint(&b.mint).is_supported())
        .max_by_key(|b| b.amount)?
        .clone();
    let coin = pool_owned
        .iter()
        .filter(|b| !QuoteAsset::from_mint(&b.mint).is_supported())
        .max_by_key(|b| b.amount)?
        .clone();
    Some((quote, coin))
}

fn infer_creator(tx: &Value, keys: &[String]) -> Option<String> {
    tx.pointer("/transaction/message/accountKeys")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|k| {
                let signer = k.get("signer").and_then(|s| s.as_bool()).unwrap_or(false);
                let writable = k.get("writable").and_then(|s| s.as_bool()).unwrap_or(false);
                if signer && writable {
                    k.get("pubkey")
                        .and_then(|p| p.as_str())
                        .map(ToString::to_string)
                } else {
                    None
                }
            })
        })
        .or_else(|| keys.first().cloned())
}

fn write_unparsed(signature: &str, reason: &str, tx: &Value) {
    let compact = serde_json::json!({
        "signature": signature,
        "reason": reason,
        "slot": tx.get("slot"),
        "blockTime": tx.get("blockTime"),
        "logMessages": tx.pointer("/meta/logMessages"),
    });
    let _ = crate::state::append_jsonl(DEBUG_UNPARSED_PATH, &compact);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{USDC_MINT, WSOL_MINT};

    #[test]
    fn raw_log_without_enrichment_does_not_create_target() {
        let v = serde_json::json!({"params":{"result":{"value":{"signature":"abc"}}}});
        assert!(parse_pump_amm_transaction("abc", &v).is_none());
    }

    #[test]
    fn swap_log_is_not_create_pool() {
        let logs = vec!["Program log: Instruction: Buy".to_string()];
        assert!(!looks_like_create_pool_log(&logs));
    }

    #[test]
    fn expanded_accounts_append_loaded_addresses_in_runtime_order() {
        let tx = serde_json::json!({
            "transaction": {"message": {
                "header": {
                    "numRequiredSignatures": 1,
                    "numReadonlySignedAccounts": 0,
                    "numReadonlyUnsignedAccounts": 1
                },
                "accountKeys": ["Signer", "WritableStatic", PUMP_AMM_PROGRAM]
            }},
            "meta": {
                "loadedAddresses": {
                    "writable": ["LoadedWritable"],
                    "readonly": ["LoadedReadonly"]
                }
            }
        });
        let accounts = expanded_account_metas(&tx);
        assert_eq!(accounts.len(), 5);
        assert_eq!(accounts[0].pubkey, "Signer");
        assert!(accounts[0].signer);
        assert!(accounts[0].writable);
        assert_eq!(accounts[2].pubkey, PUMP_AMM_PROGRAM);
        assert!(!accounts[2].writable);
        assert_eq!(accounts[3].pubkey, "LoadedWritable");
        assert!(accounts[3].writable);
        assert_eq!(accounts[4].pubkey, "LoadedReadonly");
        assert!(!accounts[4].writable);
    }

    #[test]
    fn raw_buy_fixture_extracts_27_accounts_and_data() {
        let mut data = anchor_discriminator("global:buy").to_vec();
        data.extend_from_slice(&7_000_000u64.to_le_bytes());
        data.extend_from_slice(&3_000_000u64.to_le_bytes());
        let data_base58 = bs58::encode(data).into_string();
        let mut static_keys: Vec<Value> = (0..23)
            .map(|i| Value::String(format!("Static{i:02}")))
            .collect();
        static_keys.push(Value::String(PUMP_AMM_PROGRAM.to_string()));
        let account_indices: Vec<usize> = (0..23).chain([24usize, 25, 26, 27]).collect();
        let tx = serde_json::json!({
            "transaction": {"message": {
                "header": {
                    "numRequiredSignatures": 1,
                    "numReadonlySignedAccounts": 0,
                    "numReadonlyUnsignedAccounts": 1
                },
                "accountKeys": static_keys,
                "instructions": [{
                    "programIdIndex": 23,
                    "accounts": account_indices,
                    "data": data_base58
                }]
            }},
            "meta": {
                "loadedAddresses": {
                    "writable": ["LoadedWritableA", "LoadedWritableB"],
                    "readonly": ["LoadedReadonlyA", "LoadedReadonlyB"]
                },
                "logMessages": ["Program log: Instruction: Buy"]
            }
        });
        let expanded = expanded_account_metas(&tx);
        let ixs = find_pump_amm_ixs(&tx, &expanded);
        assert_eq!(ixs.len(), 1);
        assert_eq!(ixs[0].account_indices.len(), 27);
        assert_eq!(ixs[0].data_base58, data_base58);
        assert_eq!(expanded[24].pubkey, "LoadedWritableA");
        assert_eq!(expanded[27].pubkey, "LoadedReadonlyB");
    }

    #[test]
    fn decoder_matches_global_buy_and_token_deltas() {
        let token_mint = "TokenMint111111111111111111111111111111111111";
        let mut data = anchor_discriminator("global:buy").to_vec();
        data.extend_from_slice(&7_000_000u64.to_le_bytes());
        data.extend_from_slice(&3_000_000u64.to_le_bytes());
        let data_base58 = bs58::encode(data).into_string();
        let deltas = vec![
            TokenBalanceDelta {
                account_index: 1,
                account: "UserToken".into(),
                mint: token_mint.into(),
                owner: "User".into(),
                pre_amount: 0,
                post_amount: 7_000_000,
                delta: 7_000_000,
            },
            TokenBalanceDelta {
                account_index: 2,
                account: "UserWsol".into(),
                mint: WSOL_MINT.into(),
                owner: "User".into(),
                pre_amount: 10_000_000,
                post_amount: 7_000_000,
                delta: -3_000_000,
            },
        ];
        let analysis = analyze_instruction_data(&data_base58, &deltas);
        assert_eq!(
            analysis["matches_global_buy_discriminator"].as_bool(),
            Some(true)
        );
        assert_eq!(
            analysis["u64_le_fields"][0]["value"].as_u64(),
            Some(7_000_000)
        );
        assert_eq!(
            analysis["u64_le_fields"][1]["value"].as_u64(),
            Some(3_000_000)
        );
        assert_eq!(
            analysis["u64_le_fields"][0]["matches"][0]["role_guess"].as_str(),
            Some("matches_expected_or_actual_tokens_out")
        );
        assert_eq!(
            analysis["u64_le_fields"][1]["matches"][0]["role_guess"].as_str(),
            Some("matches_spend_lamports_or_max_quote_in")
        );
    }

    #[test]
    fn parsed_pump_amm_tx_creates_target() {
        let token = "Tok11111111111111111111111111111111111111111";
        let pool = "Pool1111111111111111111111111111111111111111";
        let wsol_ata = "WsolAta1111111111111111111111111111111111111";
        let token_ata = "TokAta11111111111111111111111111111111111111";
        let tx = serde_json::json!({
            "slot": 1,
            "blockTime": 1710000000,
            "transaction": {"message": {
                "accountKeys": [
                    {"pubkey":"Creator111111111111111111111111111111111111", "signer":true, "writable":true},
                    {"pubkey":pool, "signer":false, "writable":true},
                    {"pubkey":wsol_ata, "signer":false, "writable":true},
                    {"pubkey":token_ata, "signer":false, "writable":true},
                    {"pubkey":WSOL_MINT, "signer":false, "writable":false},
                    {"pubkey":token, "signer":false, "writable":false},
                    {"pubkey":PUMP_AMM_PROGRAM, "signer":false, "writable":false}
                ],
                "instructions": [{"programId":PUMP_AMM_PROGRAM, "accounts":[pool,wsol_ata,token_ata,WSOL_MINT,token]}]
            }},
            "meta": {"logMessages":["Program log: Instruction: CreatePool"], "postTokenBalances": [
                {"accountIndex":2, "owner":pool, "mint":WSOL_MINT, "uiTokenAmount":{"amount":"89000000000"}},
                {"accountIndex":3, "owner":pool, "mint":token, "uiTokenAmount":{"amount":"1000000000000"}}
            ]}
        });
        let target = parse_pump_amm_transaction("sig", &tx).unwrap();
        assert_eq!(target.mint, token);
        assert_eq!(target.pool_state, pool);
        assert_eq!(target.pool_base_token_account, wsol_ata);
        assert_eq!(target.pool_quote_token_account, token_ata);
        assert_eq!(target.quote_asset_mint, WSOL_MINT);
        assert!(target.is_amm());
    }

    #[test]
    fn parsed_usdc_pool_tx_uses_usdc_as_quote_asset() {
        let token = "Tok11111111111111111111111111111111111111111";
        let pool = "Pool1111111111111111111111111111111111111111";
        let usdc_ata = "UsdcAta1111111111111111111111111111111111111";
        let token_ata = "TokAta11111111111111111111111111111111111111";
        let tx = serde_json::json!({
            "slot": 1,
            "blockTime": 1710000000,
            "transaction": {"message": {
                "accountKeys": [
                    {"pubkey":"Creator111111111111111111111111111111111111", "signer":true, "writable":true},
                    {"pubkey":pool, "signer":false, "writable":true},
                    {"pubkey":usdc_ata, "signer":false, "writable":true},
                    {"pubkey":token_ata, "signer":false, "writable":true},
                    {"pubkey":USDC_MINT, "signer":false, "writable":false},
                    {"pubkey":token, "signer":false, "writable":false},
                    {"pubkey":PUMP_AMM_PROGRAM, "signer":false, "writable":false}
                ],
                "instructions": [{"programId":PUMP_AMM_PROGRAM, "accounts":[pool,usdc_ata,token_ata,USDC_MINT,token]}]
            }},
            "meta": {"logMessages":["Program log: Instruction: CreatePool"], "postTokenBalances": [
                {"accountIndex":2, "owner":pool, "mint":USDC_MINT, "uiTokenAmount":{"amount":"89000000"}},
                {"accountIndex":3, "owner":pool, "mint":token, "uiTokenAmount":{"amount":"1000000000000"}}
            ]}
        });
        let target = parse_pump_amm_transaction("sig", &tx).unwrap();
        assert_eq!(target.mint, token);
        assert_eq!(target.pool_state, pool);
        assert_eq!(target.base_mint, USDC_MINT);
        assert_eq!(target.quote_asset_mint, USDC_MINT);
        assert_eq!(target.pool_base_token_account, usdc_ata);
        assert_eq!(target.pool_quote_token_account, token_ata);
        assert!(target.is_amm());
    }

    #[test]
    fn parsed_inner_create_pool_uses_pool_owned_vaults_not_largest_balances() {
        let token = "Tok11111111111111111111111111111111111111111";
        let pool = "Pool1111111111111111111111111111111111111111";
        let wsol_vault = "WsolVault11111111111111111111111111111111111";
        let token_vault = "TokenVault1111111111111111111111111111111111";
        let user_wsol = "UserWsol111111111111111111111111111111111111";
        let user_token = "UserToken11111111111111111111111111111111111";
        let tx = serde_json::json!({
            "slot": 1,
            "blockTime": 1710000000,
            "transaction": {"message": {
                "accountKeys": [
                    {"pubkey":"Creator111111111111111111111111111111111111", "signer":true, "writable":true},
                    {"pubkey":pool, "signer":false, "writable":true},
                    {"pubkey":wsol_vault, "signer":false, "writable":true},
                    {"pubkey":token_vault, "signer":false, "writable":true},
                    {"pubkey":user_wsol, "signer":false, "writable":true},
                    {"pubkey":user_token, "signer":false, "writable":true},
                    {"pubkey":WSOL_MINT, "signer":false, "writable":false},
                    {"pubkey":token, "signer":false, "writable":false},
                    {"pubkey":PUMP_AMM_PROGRAM, "signer":false, "writable":false}
                ],
                "instructions": [{"programId":"Other111111111111111111111111111111111111111", "accounts":[pool]}]
            }},
            "meta": {
                "logMessages":["Program log: Instruction: MigrateV2", "Program log: Instruction: CreatePool"],
                "innerInstructions": [{
                    "index": 0,
                    "instructions": [{"programId":PUMP_AMM_PROGRAM, "accounts":[pool,wsol_vault,token_vault,WSOL_MINT,token]}]
                }],
                "postTokenBalances": [
                    {"accountIndex":2, "owner":pool, "mint":WSOL_MINT, "uiTokenAmount":{"amount":"89000000000"}},
                    {"accountIndex":3, "owner":pool, "mint":token, "uiTokenAmount":{"amount":"1000000000000"}},
                    {"accountIndex":4, "owner":"Trader1111111111111111111111111111111111111", "mint":WSOL_MINT, "uiTokenAmount":{"amount":"530000000000"}},
                    {"accountIndex":5, "owner":"Trader1111111111111111111111111111111111111", "mint":token, "uiTokenAmount":{"amount":"999999999999999"}}
                ]
            }
        });
        let target = parse_pump_amm_transaction("sig", &tx).unwrap();
        assert_eq!(target.pool_state, pool);
        assert_eq!(target.pool_base_token_account, wsol_vault);
        assert_eq!(target.pool_quote_token_account, token_vault);
    }
}
