#[path = "../analytics.rs"]
mod analytics;
use analytics::*;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

const DEFAULT_SNIPER: &str = "datasets/sniper_follow_signals.jsonl";
const DEFAULT_BUNDLER: &str = "datasets/fresh_bundle_risk_signals.jsonl";
const DEFAULT_FORWARD: &str = "datasets/fresh_forward_outcomes.jsonl";
const DEFAULT_OUT: &str = "datasets/fresh_shadow_gate_signals.jsonl";
const DEFAULT_REPORT: &str = "datasets/fresh_shadow_gate_report.md";

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

fn decision_for(sniper: &Value, bundler: &Value) -> (&'static str, &'static str) {
    let sig = sniper.get("signal");
    let sniper_passed = boolv(sniper.get("passed"))
        || strv(sniper, "signal") == "FOLLOW_SHADOW"
        || sig.and_then(|v| v.as_bool()).unwrap_or(false);
    let good_snipers = i64v(
        sniper.get("good_sniper_count"),
        i64v(sniper.get("good_flip_sniper_count"), 0),
    );
    let good_buy_sol = f64v(
        sniper.get("good_sniper_buy_sol"),
        f64v(
            sniper.get("good_flip_sniper_buy_sol"),
            f64v(sniper.get("total_good_sniper_buy_sol"), 0.0),
        ),
    );
    let cls = if strv(bundler, "bundle_classification").is_empty() {
        "UNKNOWN"
    } else {
        strv(bundler, "bundle_classification")
    };
    let risk = f64v(bundler.get("risk_score"), 0.0);
    let follow = f64v(bundler.get("follow_score"), 0.0);
    let shared = i64v(bundler.get("shared_mother_count"), 0);
    let toxic = cls == "DEV_SNIPER_SUSPECT" || risk >= 70.0 || repeated_bad_mother(bundler);
    let strong = sniper_passed && good_snipers >= 2 && good_buy_sol >= 0.03;
    if toxic {
        return ("AVOID_DEV_CLUSTER", "high_risk_or_repeated_bad_mother");
    }
    if strong && follow >= 65.0 && risk < 45.0 {
        return (
            "FOLLOW_SHADOW_STRONG",
            "sniper_signal_plus_calibrated_low_risk_follow",
        );
    }
    if strong && follow >= 45.0 && risk < 60.0 {
        return (
            "FOLLOW_SHADOW_CANDIDATE",
            "sniper_signal_plus_moderate_follow_score",
        );
    }
    if shared >= 2 && risk < 60.0 {
        return (
            "UNKNOWN_WAIT",
            "shared_mother_cluster_needs_more_outcome_validation",
        );
    }
    ("UNKNOWN_WAIT", "insufficient_combined_signal")
}

fn decision_v2(v1: &str, forward: &Value) -> (String, String) {
    let evaluated = boolv(forward.get("evaluated"));
    let label = if strv(forward, "outcome_label").is_empty() {
        "not_evaluated"
    } else {
        strv(forward, "outcome_label")
    };
    let pnl30 = f64v(forward.get("pnl_30s_pct"), 0.0);
    let pnl60 = f64v(forward.get("pnl_60s_pct"), 0.0);
    let sell_flow = f64v(forward.get("sell_flow_ratio_60s"), 0.0);
    if matches!(
        label,
        "hard_dump_30s" | "hard_dump_60s" | "rug_or_liquidity_collapse"
    ) {
        return (
            "AVOID_FORWARD_DUMP".into(),
            format!("forward_outcome={label}"),
        );
    }
    if sell_flow >= 0.80 {
        return (
            "AVOID_FORWARD_DUMP".into(),
            format!("high_sell_flow_60s={sell_flow:.2}"),
        );
    }
    if label == "insufficient_price_data" || !evaluated {
        if v1.starts_with("FOLLOW_SHADOW") {
            return (
                format!("{v1}_V1_FALLBACK"),
                "forward_data_insufficient".into(),
            );
        }
        return (v1.into(), "forward_data_insufficient".into());
    }
    if v1 == "FOLLOW_SHADOW_STRONG" {
        if pnl30 >= -10.0 && pnl60 >= -20.0 && sell_flow < 0.70 {
            return (
                "FOLLOW_SHADOW_STRONG_V2".into(),
                format!("forward_confirmed:pnl_30s={pnl30:.1}%_pnl_60s={pnl60:.1}%"),
            );
        }
        return (
            "FOLLOW_SHADOW_STRONG_V1_DEMOTED".into(),
            format!("forward_weak:pnl_30s={pnl30:.1}%_pnl_60s={pnl60:.1}%"),
        );
    }
    if v1 == "FOLLOW_SHADOW_CANDIDATE" {
        if pnl30 >= -10.0 && pnl60 >= -20.0 {
            return (
                "FOLLOW_SHADOW_CANDIDATE_V2".into(),
                format!("forward_confirmed:pnl_30s={pnl30:.1}%_pnl_60s={pnl60:.1}%"),
            );
        }
        return (
            "FOLLOW_SHADOW_CANDIDATE_V1_DEMOTED".into(),
            format!("forward_weak:pnl_30s={pnl30:.1}%_pnl_60s={pnl60:.1}%"),
        );
    }
    (v1.into(), "no_forward_change".into())
}

fn merge(
    sniper_by: &HashMap<String, Value>,
    bundler_by: &HashMap<String, Value>,
    forward_by: &HashMap<String, Value>,
) -> Vec<Value> {
    let mut mints: Vec<String> = sniper_by.keys().chain(bundler_by.keys()).cloned().collect();
    mints.sort();
    mints.dedup();
    mints.into_iter().map(|mint| {
        let sniper = sniper_by.get(&mint).unwrap_or(&Value::Null);
        let bundler = bundler_by.get(&mint).unwrap_or(&Value::Null);
        let forward = forward_by.get(&mint).unwrap_or(&Value::Null);
        let (v1, v1_reason) = decision_for(sniper, bundler);
        let (decision, reason) = decision_v2(v1, forward);
        json!({
            "mint": mint,
            "v1_decision": v1,
            "decision": decision,
            "v1_reason": v1_reason,
            "reason": reason,
            "forward_confirmed": boolv(forward.get("evaluated")),
            "forward_outcome_label": if strv(forward, "outcome_label").is_empty() { "not_evaluated" } else { strv(forward, "outcome_label") },
            "pnl_30s_pct": f64v(forward.get("pnl_30s_pct"), 0.0),
            "pnl_60s_pct": f64v(forward.get("pnl_60s_pct"), 0.0),
            "sell_flow_ratio_60s": f64v(forward.get("sell_flow_ratio_60s"), 0.0),
            "live_allowed": false,
            "sniper_signal": if strv(sniper, "signal").is_empty() { "NO_SIGNAL" } else { strv(sniper, "signal") },
            "sniper_passed": boolv(sniper.get("passed")) || strv(sniper, "signal") == "FOLLOW_SHADOW" || sniper.get("signal").and_then(|v| v.as_bool()).unwrap_or(false),
            "good_sniper_count": i64v(sniper.get("good_sniper_count"), 0),
            "good_flip_sniper_count": i64v(sniper.get("good_flip_sniper_count"), 0),
            "good_sniper_buy_sol": f64v(sniper.get("good_sniper_buy_sol"), 0.0),
            "good_flip_sniper_buy_sol": f64v(sniper.get("good_flip_sniper_buy_sol"), 0.0),
            "bundle_classification": if strv(bundler, "bundle_classification").is_empty() { "UNKNOWN" } else { strv(bundler, "bundle_classification") },
            "early_buyer_count": i64v(bundler.get("early_buyer_count"), 0),
            "shared_mother_count": i64v(bundler.get("shared_mother_count"), 0),
            "top_mother_wallets": bundler.get("top_mother_wallets").cloned().unwrap_or_else(|| json!([])),
            "bundle_score": f64v(bundler.get("bundle_score"), 0.0),
            "mother_score": f64v(bundler.get("mother_score"), 0.0),
            "risk_score": f64v(bundler.get("risk_score"), 0.0),
            "follow_score": f64v(bundler.get("follow_score"), 0.0),
        })
    }).collect()
}

fn write_report(path: &str, rows: &[Value]) -> anyhow::Result<()> {
    let mut decisions: BTreeMap<String, usize> = BTreeMap::new();
    let mut classes: BTreeMap<String, usize> = BTreeMap::new();
    for r in rows {
        *decisions
            .entry(strv(r, "decision").to_string())
            .or_default() += 1;
        *classes
            .entry(strv(r, "bundle_classification").to_string())
            .or_default() += 1;
    }
    let mut s = String::new();
    s.push_str("# Fresh Shadow Gate Report — V2 Forward-Confirmed\n\nShadow-only combined decision from sniper-follow + bundler + forward outcomes.\\n\n");
    s.push_str(&format!(
        "- mints: {}\n- live_allowed: false for all rows\n\n",
        rows.len()
    ));
    s.push_str("## V2 Decisions\n\n| Decision | Count |\n|---|---:|\n");
    for (k, v) in decisions {
        s.push_str(&format!("| {k} | {v} |\n"));
    }
    s.push_str("\n## Bundle classes\n\n| Class | Count |\n|---|---:|\n");
    for (k, v) in classes {
        s.push_str(&format!("| {k} | {v} |\n"));
    }
    std::fs::create_dir_all(
        std::path::Path::new(path)
            .parent()
            .unwrap_or(std::path::Path::new(".")),
    )?;
    std::fs::write(path, s)?;
    Ok(())
}

fn run(args: &[String]) -> anyhow::Result<Value> {
    let sniper = latest_by_mint(&read_jsonl(&arg_value(args, "--sniper", DEFAULT_SNIPER)));
    let bundler = latest_by_mint(&read_jsonl(&arg_value(args, "--bundler", DEFAULT_BUNDLER)));
    let forward_path = arg_value(args, "--forward", DEFAULT_FORWARD);
    let forward = latest_by_mint(&read_jsonl(&forward_path));
    let out = arg_value(args, "--out", DEFAULT_OUT);
    let report = arg_value(args, "--report", DEFAULT_REPORT);
    let rows = merge(&sniper, &bundler, &forward);
    write_jsonl(&out, &rows)?;
    write_report(&report, &rows)?;
    Ok(
        json!({"mints": rows.len(), "decisions": counter_json(rows.iter().map(|r| strv(r, "decision").to_string())), "out": out, "report": report, "live_allowed": false}),
    )
}

fn self_test() {
    let mut sniper = HashMap::new();
    sniper.insert("M1".to_string(), json!({"mint":"M1","signal":"FOLLOW_SHADOW","passed":true,"good_sniper_count":2,"good_sniper_buy_sol":0.04}));
    let mut bundler = HashMap::new();
    bundler.insert("M1".to_string(), json!({"mint":"M1","bundle_classification":"GOOD_SNIPER_CLUSTER","risk_score":0,"follow_score":70,"early_buyer_count":3,"shared_mother_count":0}));
    let rows = merge(&sniper, &bundler, &HashMap::new());
    assert_eq!(strv(&rows[0], "v1_decision"), "FOLLOW_SHADOW_STRONG");
    assert_eq!(
        strv(&rows[0], "decision"),
        "FOLLOW_SHADOW_STRONG_V1_FALLBACK"
    );
    let mut fwd = HashMap::new();
    fwd.insert("M1".into(), json!({"evaluated":true,"outcome_label":"forward_win_30s","pnl_30s_pct":73.0,"pnl_60s_pct":51.0,"sell_flow_ratio_60s":0.39}));
    let rows = merge(&sniper, &bundler, &fwd);
    assert_eq!(strv(&rows[0], "decision"), "FOLLOW_SHADOW_STRONG_V2");
    fwd.insert("M1".into(), json!({"evaluated":true,"outcome_label":"hard_dump_60s","pnl_30s_pct":-50.0,"pnl_60s_pct":-80.0,"sell_flow_ratio_60s":0.95}));
    let rows = merge(&sniper, &bundler, &fwd);
    assert_eq!(strv(&rows[0], "decision"), "AVOID_FORWARD_DUMP");
    bundler.insert("M1".into(), json!({"mint":"M1","bundle_classification":"DEV_SNIPER_SUSPECT","risk_score":80,"follow_score":70}));
    let rows = merge(&sniper, &bundler, &HashMap::new());
    assert_eq!(strv(&rows[0], "v1_decision"), "AVOID_DEV_CLUSTER");
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

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if has_flag(&args, "--self-test") {
        self_test();
        return Ok(());
    }
    println!("{}", serde_json::to_string_pretty(&run(&args)?)?);
    Ok(())
}
