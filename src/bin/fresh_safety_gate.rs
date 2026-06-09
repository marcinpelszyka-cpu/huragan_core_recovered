#[path = "../analytics.rs"]
mod analytics;
use analytics::*;
use base64::Engine;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

const DEFAULT_SHADOW_GATE: &str = "datasets/fresh_shadow_gate_signals.jsonl";
const DEFAULT_BUNDLER: &str = "datasets/fresh_bundle_risk_signals.jsonl";
const DEFAULT_SNIPER: &str = "datasets/sniper_follow_signals.jsonl";
const DEFAULT_FORWARD: &str = "datasets/fresh_forward_outcomes.jsonl";
const DEFAULT_OUT_SAFETY: &str = "datasets/fresh_safety_signals.jsonl";
const DEFAULT_OUT_INSIDER: &str = "datasets/fresh_insider_risk_signals.jsonl";
const DEFAULT_OUT_GATE: &str = "datasets/fresh_selection_gate_v1.jsonl";
const DEFAULT_SUMMARY: &str = "datasets/fresh_selection_gate_v1_summary.json";
const DEFAULT_REPORT: &str = "datasets/fresh_selection_gate_v1_report.md";
const DEFAULT_DIAGNOSTICS_JSON: &str = "datasets/fresh_selection_gate_v1_diagnostics.json";
const DEFAULT_DIAGNOSTICS_MD: &str = "datasets/fresh_selection_gate_v1_diagnostics.md";

#[derive(Debug, Clone, Default)]
struct RpcMintInfo {
    mint_authority_active: Option<bool>,
    freeze_authority_active: Option<bool>,
    supply_raw: Option<f64>,
    decimals: Option<u8>,
    source: &'static str,
    error: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct HolderStats {
    top_5_ex_pool_pct: Option<f64>,
    top_10_ex_pool_pct: Option<f64>,
    top_5_raw: f64,
    top_10_raw: f64,
    excluded_pool_accounts: usize,
    holder_count_seen: usize,
    source: &'static str,
    error: Option<String>,
}

fn load_dotenv_value(key: &str) -> String {
    if let Ok(v) = std::env::var(key) {
        if !v.is_empty() {
            return v;
        }
    }
    let Ok(text) = std::fs::read_to_string(".env") else {
        return String::new();
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || !line.contains('=') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() == key {
            return v.trim().trim_matches('"').trim_matches('\'').to_string();
        }
    }
    String::new()
}

fn sanitize_error(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || " .,:;_=-/()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .chars()
        .take(180)
        .collect()
}

async fn rpc_call(
    client: &reqwest::Client,
    rpc_url: &str,
    method: &str,
    params: Value,
) -> anyhow::Result<Value> {
    let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
    let resp: Value = client
        .post(rpc_url)
        .json(&body)
        .send()
        .await?
        .json()
        .await?;
    if let Some(err) = resp.get("error") {
        anyhow::bail!("rpc_error:{}", sanitize_error(&err.to_string()));
    }
    Ok(resp.get("result").cloned().unwrap_or(Value::Null))
}

fn parse_authority_from_json_parsed(result: &Value) -> Option<RpcMintInfo> {
    let info = result
        .get("value")?
        .get("data")?
        .get("parsed")?
        .get("info")?;
    let mint_auth = info
        .get("mintAuthority")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let freeze_auth = info
        .get("freezeAuthority")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let supply = f64v(info.get("supply"), 0.0);
    let decimals = i64v(info.get("decimals"), -1);
    Some(RpcMintInfo {
        mint_authority_active: Some(!mint_auth.is_empty()),
        freeze_authority_active: Some(!freeze_auth.is_empty()),
        supply_raw: if supply > 0.0 { Some(supply) } else { None },
        decimals: if decimals >= 0 {
            Some(decimals as u8)
        } else {
            None
        },
        source: "rpc_jsonParsed",
        error: None,
    })
}

fn parse_authority_from_base64(result: &Value) -> Option<RpcMintInfo> {
    let arr = result.get("value")?.get("data")?.as_array()?;
    let encoded = arr.first()?.as_str()?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    if bytes.len() < 82 {
        return None;
    }
    let mint_tag = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    let supply = u64::from_le_bytes(bytes[36..44].try_into().ok()?);
    let decimals = bytes[44];
    let freeze_tag = u32::from_le_bytes(bytes[46..50].try_into().ok()?);
    Some(RpcMintInfo {
        mint_authority_active: Some(mint_tag != 0),
        freeze_authority_active: Some(freeze_tag != 0),
        supply_raw: Some(supply as f64),
        decimals: Some(decimals),
        source: "rpc_base64",
        error: None,
    })
}

async fn fetch_mint_info(client: &reqwest::Client, rpc_url: &str, mint: &str) -> RpcMintInfo {
    match rpc_call(
        client,
        rpc_url,
        "getAccountInfo",
        json!([mint, {"encoding":"jsonParsed"}]),
    )
    .await
    {
        Ok(result) => {
            if let Some(info) = parse_authority_from_json_parsed(&result) {
                return info;
            }
        }
        Err(e) => {
            return RpcMintInfo {
                source: "rpc",
                error: Some(sanitize_error(&e.to_string())),
                ..Default::default()
            }
        }
    }
    match rpc_call(
        client,
        rpc_url,
        "getAccountInfo",
        json!([mint, {"encoding":"base64"}]),
    )
    .await
    {
        Ok(result) => parse_authority_from_base64(&result).unwrap_or(RpcMintInfo {
            source: "rpc",
            error: Some("mint_parse_failed".into()),
            ..Default::default()
        }),
        Err(e) => RpcMintInfo {
            source: "rpc",
            error: Some(sanitize_error(&e.to_string())),
            ..Default::default()
        },
    }
}

async fn fetch_supply_raw(client: &reqwest::Client, rpc_url: &str, mint: &str) -> Option<f64> {
    let Ok(result) = rpc_call(client, rpc_url, "getTokenSupply", json!([mint])).await else {
        return None;
    };
    let val = result.get("value")?;
    let amount = f64v(val.get("amount"), 0.0);
    if amount > 0.0 {
        Some(amount)
    } else {
        None
    }
}

fn pool_exclusion_accounts(rows: &[&Value]) -> HashSet<String> {
    let mut out = HashSet::new();
    let keys = [
        "pool_base_token_account",
        "pool_quote_token_account",
        "pool_token_account",
        "pool_coin_token_account",
        "pool_pc_token_account",
        "base_vault",
        "quote_vault",
        "coin_vault",
        "pc_vault",
    ];
    for r in rows {
        for k in keys {
            let v = strv(r, k);
            if !v.is_empty() {
                out.insert(v.to_string());
            }
        }
    }
    out
}

async fn fetch_holder_stats(
    client: &reqwest::Client,
    rpc_url: &str,
    mint: &str,
    supply_raw: Option<f64>,
    excluded: &HashSet<String>,
) -> HolderStats {
    let supply = if let Some(s) = supply_raw {
        s
    } else {
        fetch_supply_raw(client, rpc_url, mint).await.unwrap_or(0.0)
    };
    if supply <= 0.0 {
        return HolderStats {
            source: "rpc",
            error: Some("supply_unavailable".into()),
            ..Default::default()
        };
    }
    let result = match rpc_call(client, rpc_url, "getTokenLargestAccounts", json!([mint])).await {
        Ok(v) => v,
        Err(e) => {
            return HolderStats {
                source: "rpc",
                error: Some(sanitize_error(&e.to_string())),
                ..Default::default()
            }
        }
    };
    let Some(arr) = result.get("value").and_then(|v| v.as_array()) else {
        return HolderStats {
            source: "rpc",
            error: Some("largest_accounts_missing".into()),
            ..Default::default()
        };
    };
    let mut raw_amounts = Vec::new();
    let mut excluded_count = 0;
    for h in arr {
        let address = strv(h, "address");
        if excluded.contains(address) {
            excluded_count += 1;
            continue;
        }
        let amount = f64v(h.get("amount"), 0.0);
        if amount > 0.0 {
            raw_amounts.push(amount);
        }
    }
    let top5: f64 = raw_amounts.iter().take(5).sum();
    let top10: f64 = raw_amounts.iter().take(10).sum();
    HolderStats {
        top_5_ex_pool_pct: Some(top5 / supply * 100.0),
        top_10_ex_pool_pct: Some(top10 / supply * 100.0),
        top_5_raw: top5,
        top_10_raw: top10,
        excluded_pool_accounts: excluded_count,
        holder_count_seen: arr.len(),
        source: "rpc",
        error: None,
    }
}

fn authority_state(active: Option<bool>) -> &'static str {
    match active {
        Some(true) => "active",
        Some(false) => "revoked",
        None => "unknown",
    }
}

fn repeated_bad_mother(bundler: &Value) -> bool {
    bundler
        .get("top_mother_wallets")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter().any(|m| {
                let bad = i64v(m.get("bad_count"), 0);
                let good = i64v(m.get("good_count"), 0);
                bad >= 2 && bad >= good
            })
        })
        .unwrap_or(false)
}

fn forward_dump(forward: &Value) -> bool {
    matches!(
        strv(forward, "outcome_label"),
        "hard_dump_30s" | "hard_dump_60s" | "rug_or_liquidity_collapse"
    ) || f64v(forward.get("sell_flow_ratio_60s"), 0.0) >= 0.80
}

fn v2_follow_signal(gate: &Value) -> bool {
    matches!(
        strv(gate, "decision"),
        "FOLLOW_SHADOW_STRONG_V2" | "FOLLOW_SHADOW_CANDIDATE_V2"
    )
}

fn score_bucket(v: f64) -> &'static str {
    if v < 20.0 {
        "00-20"
    } else if v < 45.0 {
        "20-45"
    } else if v < 60.0 {
        "45-60"
    } else if v < 65.0 {
        "60-65"
    } else if v < 80.0 {
        "65-80"
    } else {
        "80-100"
    }
}

fn forward_win(forward: &Value) -> bool {
    matches!(
        strv(forward, "outcome_label"),
        "forward_win_30s" | "forward_win_60s"
    )
}

fn holder_metrics_present(holders: &HolderStats) -> bool {
    holders.top_5_ex_pool_pct.is_some() && holders.top_10_ex_pool_pct.is_some()
}

fn authority_verified(mint_info: &RpcMintInfo) -> bool {
    mint_info.mint_authority_active == Some(false)
        && mint_info.freeze_authority_active == Some(false)
}

fn holder_ok_at(holders: &HolderStats, top5_max: f64, top10_max: f64) -> bool {
    holders
        .top_5_ex_pool_pct
        .map(|v| v <= top5_max)
        .unwrap_or(false)
        && holders
            .top_10_ex_pool_pct
            .map(|v| v <= top10_max)
            .unwrap_or(false)
}

fn safety_verified(mint_info: &RpcMintInfo, holders: &HolderStats) -> bool {
    authority_verified(mint_info)
        && holder_metrics_present(holders)
        && holder_ok_at(holders, 30.0, 45.0)
}

fn decide(
    gate: &Value,
    bundler: &Value,
    forward: &Value,
    mint_info: &RpcMintInfo,
    holders: &HolderStats,
) -> (String, String) {
    if mint_info.freeze_authority_active == Some(true) {
        return (
            "AVOID_FREEZE_AUTHORITY".into(),
            "freeze_authority_active".into(),
        );
    }
    if mint_info.mint_authority_active == Some(true) {
        return (
            "AVOID_MINT_AUTHORITY".into(),
            "mint_authority_active".into(),
        );
    }
    if holders.top_5_ex_pool_pct.map(|v| v > 30.0).unwrap_or(false) {
        return (
            "AVOID_SUPPLY_CONCENTRATION".into(),
            format!(
                "top_5_holders_ex_pool_pct={:.2}",
                holders.top_5_ex_pool_pct.unwrap_or(0.0)
            ),
        );
    }
    if holders
        .top_10_ex_pool_pct
        .map(|v| v > 45.0)
        .unwrap_or(false)
    {
        return (
            "AVOID_SUPPLY_CONCENTRATION".into(),
            format!(
                "top_10_holders_ex_pool_pct={:.2}",
                holders.top_10_ex_pool_pct.unwrap_or(0.0)
            ),
        );
    }
    if repeated_bad_mother(bundler) {
        return (
            "AVOID_SHARED_MOTHER_DUMP".into(),
            "repeated_bad_mother".into(),
        );
    }
    let shared = i64v(bundler.get("shared_mother_count"), 0);
    let risk = f64v(bundler.get("risk_score"), 0.0);
    let follow = f64v(bundler.get("follow_score"), 0.0);
    if shared >= 3 && risk >= 60.0 {
        return (
            "AVOID_INSIDER_CLUSTER".into(),
            format!("shared_mother_count={shared}:risk_score={risk:.1}"),
        );
    }
    if forward_dump(forward) {
        return (
            "AVOID_FORWARD_DUMP".into(),
            format!(
                "forward_outcome={}:sell_flow_60s={:.2}",
                strv(forward, "outcome_label"),
                f64v(forward.get("sell_flow_ratio_60s"), 0.0)
            ),
        );
    }

    if !authority_verified(mint_info) || !holder_metrics_present(holders) {
        return (
            "UNKNOWN_WAIT".into(),
            format!(
                "unverified_safety:authority={}:{}/holder_metrics_present={}",
                authority_state(mint_info.mint_authority_active),
                authority_state(mint_info.freeze_authority_active),
                holder_metrics_present(holders)
            ),
        );
    }

    if !holder_ok_at(holders, 30.0, 45.0) {
        return (
            "UNKNOWN_WAIT".into(),
            "holder_metrics_borderline_without_hard_reject".into(),
        );
    }

    if v2_follow_signal(gate) && risk < 45.0 {
        return (
            "FOLLOW_CANDIDATE".into(),
            "v2_follow_signal_plus_verified_safety".into(),
        );
    }
    if follow >= 65.0 && risk < 45.0 && shared <= 1 && holder_ok_at(holders, 20.0, 35.0) {
        return (
            "WATCHLIST_CANDIDATE_STRONG".into(),
            format!(
                "strong_watchlist:follow={follow:.1}:risk={risk:.1}:shared_mother_count={shared}"
            ),
        );
    }
    if follow >= 45.0 && risk < 60.0 && shared <= 2 && holder_ok_at(holders, 25.0, 40.0) {
        return (
            "WATCHLIST_CANDIDATE".into(),
            format!("watchlist:follow={follow:.1}:risk={risk:.1}:shared_mother_count={shared}"),
        );
    }
    if safety_verified(mint_info, holders) {
        return (
            "SAFE_TO_WATCH".into(),
            "verified_safety_pass_without_follow_edge".into(),
        );
    }
    (
        "UNKNOWN_WAIT".into(),
        "insufficient_safety_or_follow_confirmation".into(),
    )
}

fn build_rows_for_mint(
    mint: &str,
    gate: &Value,
    bundler: &Value,
    sniper: &Value,
    forward: &Value,
    mint_info: &RpcMintInfo,
    holders: &HolderStats,
) -> (Value, Value, Value) {
    let (decision, reason) = decide(gate, bundler, forward, mint_info, holders);
    let safety = json!({
        "mint": mint,
        "mint_authority_state": authority_state(mint_info.mint_authority_active),
        "freeze_authority_state": authority_state(mint_info.freeze_authority_active),
        "mint_supply_raw": mint_info.supply_raw,
        "mint_decimals": mint_info.decimals,
        "authority_source": mint_info.source,
        "authority_error": mint_info.error,
        "top_5_holders_ex_pool_pct": holders.top_5_ex_pool_pct,
        "top_10_holders_ex_pool_pct": holders.top_10_ex_pool_pct,
        "top_5_holders_ex_pool_raw": holders.top_5_raw,
        "top_10_holders_ex_pool_raw": holders.top_10_raw,
        "holder_count_seen": holders.holder_count_seen,
        "excluded_pool_accounts": holders.excluded_pool_accounts,
        "holder_source": holders.source,
        "holder_error": holders.error,
        "lp_status": "not_applicable_or_unknown",
        "live_allowed": false,
    });
    let insider = json!({
        "mint": mint,
        "shared_mother_count": i64v(bundler.get("shared_mother_count"), 0),
        "early_buyer_count": i64v(bundler.get("early_buyer_count"), 0),
        "risk_score": f64v(bundler.get("risk_score"), 0.0),
        "follow_score": f64v(bundler.get("follow_score"), 0.0),
        "bundle_classification": if strv(bundler, "bundle_classification").is_empty() { "UNKNOWN" } else { strv(bundler, "bundle_classification") },
        "repeated_bad_mother": repeated_bad_mother(bundler),
        "top_mother_wallets": bundler.get("top_mother_wallets").cloned().unwrap_or_else(|| json!([])),
        "forward_outcome_label": if strv(forward, "outcome_label").is_empty() { "not_evaluated" } else { strv(forward, "outcome_label") },
        "sell_flow_ratio_60s": f64v(forward.get("sell_flow_ratio_60s"), 0.0),
        "forward_dump_risk": forward_dump(forward),
        "live_allowed": false,
    });
    let gate_row = json!({
        "mint": mint,
        "decision": decision,
        "reason": reason,
        "live_allowed": false,
        "fresh_shadow_decision": if strv(gate, "decision").is_empty() { "UNKNOWN_WAIT" } else { strv(gate, "decision") },
        "sniper_signal": if strv(sniper, "signal").is_empty() { "NO_SIGNAL" } else { strv(sniper, "signal") },
        "bundle_classification": if strv(bundler, "bundle_classification").is_empty() { "UNKNOWN" } else { strv(bundler, "bundle_classification") },
        "risk_score": f64v(bundler.get("risk_score"), 0.0),
        "follow_score": f64v(bundler.get("follow_score"), 0.0),
        "shared_mother_count": i64v(bundler.get("shared_mother_count"), 0),
        "repeated_bad_mother": repeated_bad_mother(bundler),
        "forward_outcome_label": if strv(forward, "outcome_label").is_empty() { "not_evaluated" } else { strv(forward, "outcome_label") },
        "sell_flow_ratio_60s": f64v(forward.get("sell_flow_ratio_60s"), 0.0),
        "mint_authority_state": safety["mint_authority_state"].clone(),
        "freeze_authority_state": safety["freeze_authority_state"].clone(),
        "top_5_holders_ex_pool_pct": holders.top_5_ex_pool_pct,
        "top_10_holders_ex_pool_pct": holders.top_10_ex_pool_pct,
        "authority_verified": authority_verified(mint_info),
        "holder_metrics_present": holder_metrics_present(holders),
        "safety_verified": safety_verified(mint_info, holders),
        "tier": decision,
        "follow_score_bucket": score_bucket(f64v(bundler.get("follow_score"), 0.0)),
        "risk_score_bucket": score_bucket(f64v(bundler.get("risk_score"), 0.0)),
        "forward_win": forward_win(forward),
        "forward_dump_or_rug": forward_dump(forward),
        "lp_status": "not_applicable_or_unknown",
    });
    (safety, insider, gate_row)
}

fn make_summary(
    gate_rows: &[Value],
    safety_rows: &[Value],
    insider_rows: &[Value],
    rpc_enabled: bool,
) -> Value {
    json!({
        "rows": gate_rows.len(),
        "decisions": counter_json(gate_rows.iter().map(|r| strv(r, "decision").to_string())),
        "mint_authority_states": counter_json(safety_rows.iter().map(|r| strv(r, "mint_authority_state").to_string())),
        "freeze_authority_states": counter_json(safety_rows.iter().map(|r| strv(r, "freeze_authority_state").to_string())),
        "bundle_classes": counter_json(insider_rows.iter().map(|r| strv(r, "bundle_classification").to_string())),
        "rpc_enabled": rpc_enabled,
        "live_allowed": false,
    })
}

fn write_report(path: &str, summary: &Value, gate_rows: &[Value]) -> anyhow::Result<()> {
    let mut out = String::new();
    out.push_str(
        "# Fresh Safety + Insider Gate v1\n\nShadow-only selection gate. No live permission.\n\n",
    );
    out.push_str(&format!(
        "- rows: {}\n- live_allowed: false\n- rpc_enabled: {}\n\n",
        summary["rows"], summary["rpc_enabled"]
    ));
    out.push_str("## Decisions\n\n| Decision | Count |\n|---|---:|\n");
    if let Some(obj) = summary.get("decisions").and_then(|v| v.as_object()) {
        let mut items: Vec<_> = obj.iter().collect();
        items.sort_by_key(|(k, _)| *k);
        for (k, v) in items {
            out.push_str(&format!("| {k} | {v} |\n"));
        }
    }
    out.push_str("\n## Top candidate/watch rows\n\n| Mint | Decision | Risk | Follow | Shadow | Reason |\n|---|---|---:|---:|---|---|\n");
    for r in gate_rows
        .iter()
        .filter(|r| {
            matches!(
                strv(r, "decision"),
                "FOLLOW_CANDIDATE"
                    | "SAFE_TO_WATCH"
                    | "WATCHLIST_CANDIDATE"
                    | "WATCHLIST_CANDIDATE_STRONG"
            )
        })
        .take(80)
    {
        out.push_str(&format!(
            "| {}... | {} | {:.1} | {:.1} | {} | {} |\n",
            &strv(r, "mint")[..strv(r, "mint").len().min(12)],
            strv(r, "decision"),
            f64v(r.get("risk_score"), 0.0),
            f64v(r.get("follow_score"), 0.0),
            strv(r, "fresh_shadow_decision"),
            strv(r, "reason")
        ));
    }
    std::fs::create_dir_all(
        std::path::Path::new(path)
            .parent()
            .unwrap_or(std::path::Path::new(".")),
    )?;
    std::fs::write(path, out)?;
    Ok(())
}

fn rate(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        ((n as f64 / d as f64) * 10_000.0).round() / 10_000.0
    }
}

fn diagnostics(gate_rows: &[Value]) -> Value {
    let mut by_decision: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for r in gate_rows {
        by_decision
            .entry(strv(r, "decision").to_string())
            .or_default()
            .push(r);
    }
    let mut decision_stats = serde_json::Map::new();
    for (decision, rows) in by_decision {
        let total = rows.len();
        let wins = rows.iter().filter(|r| boolv(r.get("forward_win"))).count();
        let dumps = rows
            .iter()
            .filter(|r| boolv(r.get("forward_dump_or_rug")))
            .count();
        decision_stats.insert(decision, json!({
            "count": total,
            "forward_win": wins,
            "forward_dump_or_rug": dumps,
            "win_rate": rate(wins, total),
            "dump_or_rug_rate": rate(dumps, total),
            "follow_score_buckets": counter_json(rows.iter().map(|r| strv(r, "follow_score_bucket").to_string())),
            "risk_score_buckets": counter_json(rows.iter().map(|r| strv(r, "risk_score_bucket").to_string())),
            "forward_labels": counter_json(rows.iter().map(|r| strv(r, "forward_outcome_label").to_string())),
        }));
    }
    let top_rows = |decision: &str, limit: usize| -> Vec<Value> {
        let mut rows: Vec<Value> = gate_rows
            .iter()
            .filter(|r| strv(r, "decision") == decision)
            .cloned()
            .collect();
        rows.sort_by(|a, b| {
            let af = f64v(a.get("follow_score"), 0.0);
            let bf = f64v(b.get("follow_score"), 0.0);
            let ar = f64v(a.get("risk_score"), 0.0);
            let br = f64v(b.get("risk_score"), 0.0);
            bf.partial_cmp(&af)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| ar.partial_cmp(&br).unwrap_or(std::cmp::Ordering::Equal))
        });
        rows.into_iter().take(limit).map(|r| json!({
            "mint": strv(&r, "mint"),
            "decision": strv(&r, "decision"),
            "follow_score": f64v(r.get("follow_score"), 0.0),
            "risk_score": f64v(r.get("risk_score"), 0.0),
            "shared_mother_count": i64v(r.get("shared_mother_count"), 0),
            "top_5_holders_ex_pool_pct": r.get("top_5_holders_ex_pool_pct").cloned().unwrap_or(Value::Null),
            "top_10_holders_ex_pool_pct": r.get("top_10_holders_ex_pool_pct").cloned().unwrap_or(Value::Null),
            "forward_outcome_label": strv(&r, "forward_outcome_label"),
            "reason": strv(&r, "reason"),
        })).collect()
    };
    json!({
        "rows": gate_rows.len(),
        "decision_stats": decision_stats,
        "top_watchlist_candidate_strong": top_rows("WATCHLIST_CANDIDATE_STRONG", 25),
        "top_watchlist_candidate": top_rows("WATCHLIST_CANDIDATE", 25),
        "top_safe_to_watch": top_rows("SAFE_TO_WATCH", 25),
        "live_allowed": false,
    })
}

fn write_diagnostics_md(path: &str, diag: &Value) -> anyhow::Result<()> {
    let mut out = String::new();
    out.push_str(
        "# Fresh Selection Gate v1 Diagnostics

Shadow-only diagnostics. No live permission.

",
    );
    out.push_str(&format!(
        "- rows: {}
- live_allowed: false

",
        diag["rows"]
    ));
    out.push_str(
        "## Decision stats

| Decision | Count | Win rate | Dump/rug rate |
|---|---:|---:|---:|
",
    );
    if let Some(stats) = diag.get("decision_stats").and_then(|v| v.as_object()) {
        let mut items: Vec<_> = stats.iter().collect();
        items.sort_by_key(|(k, _)| *k);
        for (decision, s) in items {
            out.push_str(&format!(
                "| {decision} | {} | {:.2}% | {:.2}% |
",
                s["count"],
                f64v(s.get("win_rate"), 0.0) * 100.0,
                f64v(s.get("dump_or_rug_rate"), 0.0) * 100.0
            ));
        }
    }
    for (title, key) in [
        (
            "Top WATCHLIST_CANDIDATE_STRONG",
            "top_watchlist_candidate_strong",
        ),
        ("Top WATCHLIST_CANDIDATE", "top_watchlist_candidate"),
        ("Top SAFE_TO_WATCH", "top_safe_to_watch"),
    ] {
        out.push_str(&format!(
            "
## {title}

| Mint | Follow | Risk | Shared | Top5 | Top10 | Forward | Reason |
|---|---:|---:|---:|---:|---:|---|---|
"
        ));
        if let Some(rows) = diag.get(key).and_then(|v| v.as_array()) {
            for r in rows {
                let mint = strv(r, "mint");
                out.push_str(&format!(
                    "| {}... | {:.1} | {:.1} | {} | {:.2} | {:.2} | {} | {} |
",
                    &mint[..mint.len().min(12)],
                    f64v(r.get("follow_score"), 0.0),
                    f64v(r.get("risk_score"), 0.0),
                    i64v(r.get("shared_mother_count"), 0),
                    f64v(r.get("top_5_holders_ex_pool_pct"), 0.0),
                    f64v(r.get("top_10_holders_ex_pool_pct"), 0.0),
                    strv(r, "forward_outcome_label"),
                    strv(r, "reason"),
                ));
            }
        }
    }
    std::fs::create_dir_all(
        std::path::Path::new(path)
            .parent()
            .unwrap_or(std::path::Path::new(".")),
    )?;
    std::fs::write(path, out)?;
    Ok(())
}

async fn run(args: &[String]) -> anyhow::Result<Value> {
    let shadow_path = arg_value(args, "--shadow-gate", DEFAULT_SHADOW_GATE);
    let bundler_path = arg_value(args, "--bundler", DEFAULT_BUNDLER);
    let sniper_path = arg_value(args, "--sniper", DEFAULT_SNIPER);
    let forward_path = arg_value(args, "--forward", DEFAULT_FORWARD);
    let out_safety = arg_value(args, "--out-safety", DEFAULT_OUT_SAFETY);
    let out_insider = arg_value(args, "--out-insider", DEFAULT_OUT_INSIDER);
    let out_gate = arg_value(args, "--out", DEFAULT_OUT_GATE);
    let summary_path = arg_value(args, "--summary", DEFAULT_SUMMARY);
    let report_path = arg_value(args, "--report", DEFAULT_REPORT);
    let diagnostics_json_path = arg_value(args, "--diagnostics-json", DEFAULT_DIAGNOSTICS_JSON);
    let diagnostics_md_path = arg_value(args, "--diagnostics-md", DEFAULT_DIAGNOSTICS_MD);
    let limit_mints: usize = arg_value(args, "--limit-mints", "0").parse().unwrap_or(0);
    let dry_run = has_flag(args, "--dry-run");
    let no_rpc = has_flag(args, "--no-rpc") || dry_run;
    let rpc_url = if no_rpc {
        String::new()
    } else {
        load_dotenv_value("RPC_URL")
    };
    let rpc_enabled = !no_rpc && !rpc_url.is_empty();

    let shadow = latest_by_mint(&read_jsonl(&shadow_path));
    let bundler = latest_by_mint(&read_jsonl(&bundler_path));
    let sniper = latest_by_mint(&read_jsonl(&sniper_path));
    let forward = latest_by_mint(&read_jsonl(&forward_path));
    let mut mints: Vec<String> = shadow
        .keys()
        .chain(bundler.keys())
        .chain(sniper.keys())
        .chain(forward.keys())
        .cloned()
        .collect();
    mints.sort();
    mints.dedup();
    if limit_mints > 0 && mints.len() > limit_mints {
        mints.truncate(limit_mints);
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let mut safety_rows = Vec::new();
    let mut insider_rows = Vec::new();
    let mut gate_rows = Vec::new();

    for mint in &mints {
        let gate = shadow.get(mint).unwrap_or(&Value::Null);
        let b = bundler.get(mint).unwrap_or(&Value::Null);
        let s = sniper.get(mint).unwrap_or(&Value::Null);
        let f = forward.get(mint).unwrap_or(&Value::Null);
        let related = [gate, b, s, f];
        let excluded = pool_exclusion_accounts(&related);
        let mint_info = if rpc_enabled {
            fetch_mint_info(&client, &rpc_url, mint).await
        } else {
            RpcMintInfo {
                source: "not_requested",
                ..Default::default()
            }
        };
        let holders = if rpc_enabled {
            fetch_holder_stats(&client, &rpc_url, mint, mint_info.supply_raw, &excluded).await
        } else {
            HolderStats {
                source: "not_requested",
                ..Default::default()
            }
        };
        let (safety, insider, gate_row) =
            build_rows_for_mint(mint, gate, b, s, f, &mint_info, &holders);
        safety_rows.push(safety);
        insider_rows.push(insider);
        gate_rows.push(gate_row);
    }

    let summary = make_summary(&gate_rows, &safety_rows, &insider_rows, rpc_enabled);
    if !dry_run {
        write_jsonl(&out_safety, &safety_rows)?;
        write_jsonl(&out_insider, &insider_rows)?;
        write_jsonl(&out_gate, &gate_rows)?;
        write_json(&summary_path, &summary)?;
        write_report(&report_path, &summary, &gate_rows)?;
        let diag = diagnostics(&gate_rows);
        write_json(&diagnostics_json_path, &diag)?;
        write_diagnostics_md(&diagnostics_md_path, &diag)?;
    }
    Ok(
        json!({"rows": gate_rows.len(), "decisions": summary["decisions"].clone(), "rpc_enabled": rpc_enabled, "dry_run": dry_run, "out": if dry_run { "DRY_RUN" } else { &out_gate }, "summary": if dry_run { "DRY_RUN" } else { &summary_path },
        "diagnostics": if dry_run { "DRY_RUN" } else { &diagnostics_json_path }, "live_allowed": false}),
    )
}

fn self_test() {
    let mut info = RpcMintInfo {
        mint_authority_active: Some(false),
        freeze_authority_active: Some(false),
        supply_raw: Some(100.0),
        source: "test",
        ..Default::default()
    };
    let mut holders = HolderStats {
        top_5_ex_pool_pct: Some(10.0),
        top_10_ex_pool_pct: Some(20.0),
        source: "test",
        ..Default::default()
    };
    let gate = json!({"mint":"M","decision":"FOLLOW_SHADOW_STRONG_V2"});
    let bundler = json!({"mint":"M","risk_score":20,"follow_score":80,"shared_mother_count":0});
    let forward = json!({"mint":"M","outcome_label":"forward_win_30s","sell_flow_ratio_60s":0.2});
    assert_eq!(
        decide(&gate, &bundler, &forward, &info, &holders).0,
        "FOLLOW_CANDIDATE"
    );
    info.freeze_authority_active = Some(true);
    assert_eq!(
        decide(&gate, &bundler, &forward, &info, &holders).0,
        "AVOID_FREEZE_AUTHORITY"
    );
    info.freeze_authority_active = Some(false);
    info.mint_authority_active = Some(true);
    assert_eq!(
        decide(&gate, &bundler, &forward, &info, &holders).0,
        "AVOID_MINT_AUTHORITY"
    );
    info.mint_authority_active = Some(false);
    holders.top_5_ex_pool_pct = Some(31.0);
    assert_eq!(
        decide(&gate, &bundler, &forward, &info, &holders).0,
        "AVOID_SUPPLY_CONCENTRATION"
    );
    holders.top_5_ex_pool_pct = Some(10.0);
    holders.top_10_ex_pool_pct = Some(46.0);
    assert_eq!(
        decide(&gate, &bundler, &forward, &info, &holders).0,
        "AVOID_SUPPLY_CONCENTRATION"
    );
    holders.top_10_ex_pool_pct = Some(20.0);
    let bad_mother = json!({"top_mother_wallets":[{"bad_count":2,"good_count":0}],"risk_score":20});
    assert_eq!(
        decide(&gate, &bad_mother, &forward, &info, &holders).0,
        "AVOID_SHARED_MOTHER_DUMP"
    );
    let insider = json!({"shared_mother_count":3,"risk_score":60});
    assert_eq!(
        decide(&gate, &insider, &forward, &info, &holders).0,
        "AVOID_INSIDER_CLUSTER"
    );
    let dump = json!({"outcome_label":"hard_dump_60s","sell_flow_ratio_60s":0.1});
    assert_eq!(
        decide(&gate, &bundler, &dump, &info, &holders).0,
        "AVOID_FORWARD_DUMP"
    );
    let (_, _, row) = build_rows_for_mint(
        "M",
        &gate,
        &bundler,
        &Value::Null,
        &forward,
        &info,
        &holders,
    );
    assert_eq!(boolv(row.get("live_allowed")), false);
    assert!(boolv(row.get("authority_verified")));
    assert!(boolv(row.get("holder_metrics_present")));
    assert!(boolv(row.get("safety_verified")));
    let diag = diagnostics(&[row.clone()]);
    assert_eq!(diag["rows"], 1);
    assert!(diag["decision_stats"]
        .as_object()
        .unwrap()
        .contains_key("FOLLOW_CANDIDATE"));
    let excl_rows = vec![json!({"pool_base_token_account":"POOL"})];
    let refs: Vec<&Value> = excl_rows.iter().collect();
    assert!(pool_exclusion_accounts(&refs).contains("POOL"));
    println!("SELF_TEST_OK");
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn self_test_path() {
        self_test();
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if has_flag(&args, "--self-test") {
        self_test();
        return Ok(());
    }
    println!("{}", serde_json::to_string_pretty(&run(&args).await?)?);
    Ok(())
}
