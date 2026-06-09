#[path = "../analytics.rs"]
mod analytics;
use analytics::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;

const DEFAULT_GATE: &str = "datasets/fresh_shadow_gate_signals.jsonl";
const DEFAULT_EVENTS: &str = "datasets/sniper_trade_events.jsonl";
const DEFAULT_BUNDLER: &str = "datasets/fresh_bundle_risk_signals.jsonl";
const DEFAULT_SNIPER: &str = "datasets/sniper_follow_signals.jsonl";
const DEFAULT_OUT: &str = "datasets/fresh_forward_outcomes.jsonl";
const DEFAULT_REPORT: &str = "datasets/fresh_forward_outcome_report.md";
const DEFAULT_SUMMARY: &str = "datasets/fresh_forward_outcome_summary.json";

fn event_time(e: &Value) -> i64 {
    i64v(
        e.get("timestamp")
            .or_else(|| e.get("block_time"))
            .or_else(|| e.get("blockTime")),
        0,
    )
}

fn event_age(e: &Value, signal_time: i64) -> i64 {
    if e.get("age_secs").is_some() {
        return i64v(e.get("age_secs"), 0);
    }
    let t = event_time(e);
    if t > 0 && signal_time > 0 {
        (t - signal_time).max(0)
    } else {
        0
    }
}

fn implied_price(e: &Value) -> f64 {
    let quote = f64v(
        e.get("quote_delta_sol")
            .or_else(|| e.get("buy_sol"))
            .or_else(|| e.get("sell_sol")),
        0.0,
    );
    let token = f64v(
        e.get("token_delta_raw")
            .or_else(|| e.get("token_amount"))
            .or_else(|| e.get("amount")),
        0.0,
    );
    if quote > 0.0 && token > 0.0 {
        quote / token
    } else {
        0.0
    }
}

fn weighted_buy_price(events: &[Value], signal_time: i64, entry_window_secs: i64) -> (f64, usize) {
    let mut quote = 0.0;
    let mut token = 0.0;
    let mut count = 0;
    for e in events {
        if strv(e, "side") == "buy" && event_age(e, signal_time) <= entry_window_secs {
            let q = f64v(e.get("quote_delta_sol"), 0.0);
            let t = f64v(e.get("token_delta_raw"), 0.0);
            if q > 0.0 && t > 0.0 {
                quote += q;
                token += t;
                count += 1;
            }
        }
    }
    if quote > 0.0 && token > 0.0 {
        (quote / token, count)
    } else {
        (0.0, count)
    }
}

fn last_price_in_window(events: &[Value], signal_time: i64, window_secs: i64) -> (f64, String) {
    let mut best: Option<(i64, i64, f64, String)> = None;
    for e in events {
        let age = event_age(e, signal_time);
        if age < 0 || age > window_secs {
            continue;
        }
        let p = implied_price(e);
        if p <= 0.0 {
            continue;
        }
        let row = (age, event_time(e), p, strv(e, "side").to_string());
        if best
            .as_ref()
            .map(|b| (row.0, row.1) > (b.0, b.1))
            .unwrap_or(true)
        {
            best = Some(row);
        }
    }
    best.map(|x| (x.2, x.3)).unwrap_or((0.0, String::new()))
}

fn sell_flow_ratio(events: &[Value], signal_time: i64, window_secs: i64) -> (f64, f64, f64) {
    let mut buys = 0.0;
    let mut sells = 0.0;
    for e in events {
        let age = event_age(e, signal_time);
        if age < 0 || age > window_secs {
            continue;
        }
        let q = f64v(e.get("quote_delta_sol"), 0.0);
        match strv(e, "side") {
            "buy" => buys += q,
            "sell" => sells += q,
            _ => {}
        }
    }
    let denom = (buys + sells).max(1e-12);
    (sells / denom, buys, sells)
}

fn label_from_pnl(
    pnl30: Option<f64>,
    pnl60: Option<f64>,
    sell_ratio60: f64,
    event_count: usize,
) -> &'static str {
    if event_count == 0 {
        return "no_trade_data";
    }
    let vals: Vec<f64> = [pnl30, pnl60].into_iter().flatten().collect();
    if vals.is_empty() {
        return "insufficient_price_data";
    }
    let worst = vals.iter().copied().fold(f64::INFINITY, f64::min);
    let best = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if worst <= -80.0 || (worst <= -60.0 && sell_ratio60 >= 0.8) {
        return "rug_or_liquidity_collapse";
    }
    if pnl30.map(|v| v <= -40.0).unwrap_or(false) {
        return "hard_dump_30s";
    }
    if pnl60.map(|v| v <= -40.0).unwrap_or(false) {
        return "hard_dump_60s";
    }
    if pnl30.map(|v| v >= 25.0).unwrap_or(false) {
        return "forward_win_30s";
    }
    if pnl60.map(|v| v >= 25.0).unwrap_or(false) {
        return "forward_win_60s";
    }
    if worst > -20.0 && best < 25.0 {
        return "flat_or_noise";
    }
    "insufficient_price_data"
}

fn evaluate_signal(
    gate: &Value,
    events: &[Value],
    bundler: &Value,
    sniper: &Value,
    entry_window_secs: i64,
) -> Value {
    let mint = strv(gate, "mint");
    let decision = strv(gate, "decision");
    let mut out = json!({
        "mint": mint,
        "decision": if decision.is_empty() { "UNKNOWN_WAIT" } else { decision },
        "live_allowed": false,
        "bundle_classification": if !strv(gate, "bundle_classification").is_empty() { strv(gate, "bundle_classification") } else if !strv(bundler, "bundle_classification").is_empty() { strv(bundler, "bundle_classification") } else { "UNKNOWN" },
        "risk_score": f64v(gate.get("risk_score"), f64v(bundler.get("risk_score"), 0.0)),
        "follow_score": f64v(gate.get("follow_score"), f64v(bundler.get("follow_score"), 0.0)),
        "good_sniper_count": i64v(gate.get("good_sniper_count"), i64v(sniper.get("good_sniper_count"), 0)),
        "good_flip_sniper_count": i64v(gate.get("good_flip_sniper_count"), i64v(sniper.get("good_flip_sniper_count"), 0)),
    });
    if decision != "FOLLOW_SHADOW_STRONG" && decision != "FOLLOW_SHADOW_CANDIDATE" {
        out.as_object_mut().unwrap().extend(
            json!({"outcome_label":"not_evaluated","evaluated":false})
                .as_object()
                .unwrap()
                .clone(),
        );
        return out;
    }
    if events.is_empty() {
        out.as_object_mut().unwrap().extend(
            json!({"outcome_label":"no_trade_data","evaluated":true,"event_count":0})
                .as_object()
                .unwrap()
                .clone(),
        );
        return out;
    }
    let mut ev = events.to_vec();
    ev.sort_by_key(|e| (event_time(e), strv(e, "signature").to_string()));
    let signal_time = ev
        .iter()
        .map(event_time)
        .filter(|t| *t > 0)
        .min()
        .unwrap_or(0);
    let (entry_price, entry_trades) = weighted_buy_price(&ev, signal_time, entry_window_secs);
    let (p30, side30) = last_price_in_window(&ev, signal_time, 30);
    let (p60, side60) = last_price_in_window(&ev, signal_time, 60);
    let (sell_ratio60, buy_sol60, sell_sol60) = sell_flow_ratio(&ev, signal_time, 60);
    let pnl30 = if entry_price > 0.0 && p30 > 0.0 {
        Some((p30 - entry_price) / entry_price * 100.0)
    } else {
        None
    };
    let pnl60 = if entry_price > 0.0 && p60 > 0.0 {
        Some((p60 - entry_price) / entry_price * 100.0)
    } else {
        None
    };
    let label = if entry_price > 0.0 {
        label_from_pnl(pnl30, pnl60, sell_ratio60, ev.len())
    } else {
        "insufficient_price_data"
    };
    out.as_object_mut().unwrap().extend(
        json!({
            "evaluated": true,
            "outcome_label": label,
            "signal_time": signal_time,
            "event_count": ev.len(),
            "entry_trade_count": entry_trades,
            "entry_price_proxy": entry_price,
            "price_30s": p30,
            "price_60s": p60,
            "price_30s_side": side30,
            "price_60s_side": side60,
            "pnl_30s_pct": pnl30.map(|v| (v * 1_000_000.0).round() / 1_000_000.0),
            "pnl_60s_pct": pnl60.map(|v| (v * 1_000_000.0).round() / 1_000_000.0),
            "buy_sol_60s": (buy_sol60 * 1_000_000_000_000.0).round() / 1_000_000_000_000.0,
            "sell_sol_60s": (sell_sol60 * 1_000_000_000_000.0).round() / 1_000_000_000_000.0,
            "sell_flow_ratio_60s": (sell_ratio60 * 1_000_000.0).round() / 1_000_000.0,
        })
        .as_object()
        .unwrap()
        .clone(),
    );
    out
}

fn summarize(rows: &[Value]) -> Value {
    let labels = counter_json(rows.iter().map(|r| strv(r, "outcome_label").to_string()));
    json!({"rows": rows.len(), "evaluated": rows.iter().filter(|r| boolv(r.get("evaluated"))).count(), "labels": labels, "live_allowed": false})
}

fn write_report(path: &str, rows: &[Value], summary: &Value) -> anyhow::Result<()> {
    let mut by_decision: BTreeMap<String, usize> = BTreeMap::new();
    for r in rows {
        *by_decision
            .entry(strv(r, "decision").to_string())
            .or_default() += 1;
    }
    let mut out = String::new();
    out.push_str("# Fresh Forward Outcome Report\n\nShadow-only forward labels for fresh shadow gate decisions.\n\n");
    out.push_str(&format!(
        "- rows: {}\n- evaluated: {}\n- live_allowed: false\n\n",
        summary["rows"], summary["evaluated"]
    ));
    out.push_str("## Decisions\n\n| Decision | Count |\n|---|---:|\n");
    for (k, v) in by_decision {
        out.push_str(&format!("| {k} | {v} |\n"));
    }
    std::fs::create_dir_all(
        std::path::Path::new(path)
            .parent()
            .unwrap_or(std::path::Path::new(".")),
    )?;
    std::fs::write(path, out)?;
    Ok(())
}

fn run(args: &[String]) -> anyhow::Result<Value> {
    let gate_path = arg_value(args, "--gate", DEFAULT_GATE);
    let events_path = arg_value(args, "--events", DEFAULT_EVENTS);
    let bundler_path = arg_value(args, "--bundler", DEFAULT_BUNDLER);
    let sniper_path = arg_value(args, "--sniper", DEFAULT_SNIPER);
    let out_path = arg_value(args, "--out", DEFAULT_OUT);
    let report_path = arg_value(args, "--report", DEFAULT_REPORT);
    let summary_path = arg_value(args, "--summary", DEFAULT_SUMMARY);
    let entry_window_secs: i64 = arg_value(args, "--entry-window-secs", "10")
        .parse()
        .unwrap_or(10);
    let gate = read_jsonl(&gate_path);
    let events_by = group_by_mint(&read_jsonl(&events_path));
    let bundler = latest_by_mint(&read_jsonl(&bundler_path));
    let sniper = latest_by_mint(&read_jsonl(&sniper_path));
    let rows: Vec<Value> = gate
        .iter()
        .map(|g| {
            let mint = strv(g, "mint");
            evaluate_signal(
                g,
                events_by.get(mint).map(|v| v.as_slice()).unwrap_or(&[]),
                bundler.get(mint).unwrap_or(&Value::Null),
                sniper.get(mint).unwrap_or(&Value::Null),
                entry_window_secs,
            )
        })
        .collect();
    let summary = summarize(&rows);
    write_jsonl(&out_path, &rows)?;
    write_json(&summary_path, &summary)?;
    write_report(&report_path, &rows, &summary)?;
    Ok(
        json!({"rows": rows.len(), "evaluated": summary["evaluated"], "labels": summary["labels"], "out": out_path, "report": report_path, "summary": summary_path, "live_allowed": false}),
    )
}

fn self_test() {
    let events = vec![
        json!({"mint":"M1","timestamp":100,"age_secs":0,"side":"buy","quote_delta_sol":1.0,"token_delta_raw":100.0}),
        json!({"mint":"M1","timestamp":130,"age_secs":30,"side":"buy","quote_delta_sol":1.5,"token_delta_raw":100.0}),
    ];
    let row = evaluate_signal(
        &json!({"mint":"M1","decision":"FOLLOW_SHADOW_STRONG"}),
        &events,
        &Value::Null,
        &Value::Null,
        10,
    );
    assert_eq!(strv(&row, "outcome_label"), "forward_win_30s");
    let skip = evaluate_signal(
        &json!({"mint":"M2","decision":"UNKNOWN_WAIT"}),
        &[],
        &Value::Null,
        &Value::Null,
        10,
    );
    assert_eq!(strv(&skip, "outcome_label"), "not_evaluated");
    println!("SELF_TEST_OK");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_forward_win() {
        let events = vec![
            json!({"mint":"M1","timestamp":100,"age_secs":0,"side":"buy","quote_delta_sol":1.0,"token_delta_raw":100.0}),
            json!({"mint":"M1","timestamp":130,"age_secs":30,"side":"buy","quote_delta_sol":1.5,"token_delta_raw":100.0}),
        ];
        let row = evaluate_signal(
            &json!({"mint":"M1","decision":"FOLLOW_SHADOW_STRONG"}),
            &events,
            &Value::Null,
            &Value::Null,
            10,
        );
        assert_eq!(strv(&row, "outcome_label"), "forward_win_30s");
    }

    #[test]
    fn skips_non_follow_decisions() {
        let row = evaluate_signal(
            &json!({"mint":"M1","decision":"UNKNOWN_WAIT"}),
            &[],
            &Value::Null,
            &Value::Null,
            10,
        );
        assert_eq!(strv(&row, "outcome_label"), "not_evaluated");
    }
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if has_flag(&args, "--self-test") {
        self_test();
        return Ok(());
    }
    println!("{}", serde_json::to_string_pretty(&run(&args)?)?);
    Ok(())
}
