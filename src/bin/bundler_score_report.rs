#[path = "../analytics.rs"]
mod analytics;
use analytics::*;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

const DEFAULT_SIGNALS: &str = "datasets/fresh_bundle_risk_signals.jsonl";
const DEFAULT_EDGES: &str = "datasets/bundler_wallet_edges.jsonl";
const DEFAULT_STATE: &str = "state.jsonl";
const DEFAULT_FORWARD: &str = "datasets/fresh_forward_outcomes.jsonl";
const DEFAULT_REPORT: &str = "datasets/bundler_score_calibration_report.md";
const DEFAULT_SUMMARY: &str = "datasets/bundler_score_calibration_summary.json";

fn load_outcomes(state_path: &str, forward_path: &str) -> HashMap<String, Value> {
    let mut latest = HashMap::new();
    for r in read_jsonl(state_path) {
        let mint = strv(&r, "mint");
        if !mint.is_empty() {
            latest.insert(mint.to_string(), r);
        }
    }
    let mut out = HashMap::new();
    for (mint, r) in latest {
        let reason = if !strv(&r, "exit_reason").is_empty() {
            strv(&r, "exit_reason")
        } else {
            strv(&r, "live_exit_reason")
        };
        let status = strv(&r, "status");
        let pnl = f64v(r.get("realized_pnl_sol"), f64v(r.get("net_pnl_sol"), 0.0));
        let bad = status == "unrecoverable_dust_or_rug"
            || matches!(reason, "hard_stop" | "rug_guard" | "price_unavailable")
            || reason.contains("dust_or_rug")
            || pnl < -0.0005;
        let good = pnl > 0.00005;
        out.insert(mint, json!({"bad":bad,"good":good,"pnl":pnl,"exit_reason":reason,"status":status,"source":"state"}));
    }
    for r in read_jsonl(forward_path) {
        let mint = strv(&r, "mint");
        if mint.is_empty() || out.contains_key(mint) {
            continue;
        }
        let label = strv(&r, "outcome_label");
        if matches!(
            label,
            "no_trade_data" | "insufficient_price_data" | "not_evaluated" | ""
        ) {
            continue;
        }
        let pnl = f64v(r.get("pnl_60s_pct"), f64v(r.get("pnl_30s_pct"), 0.0));
        let good = matches!(label, "forward_win_30s" | "forward_win_60s");
        let bad = matches!(
            label,
            "hard_dump_30s" | "hard_dump_60s" | "rug_or_liquidity_collapse"
        );
        out.insert(mint.to_string(), json!({"bad":bad,"good":good,"pnl":pnl,"exit_reason":label,"status":"forward_label","source":"forward"}));
    }
    out
}

fn enrich(signals: &[Value], outcomes: &HashMap<String, Value>) -> Vec<Value> {
    signals
        .iter()
        .map(|s| {
            let mint = strv(s, "mint");
            let o = outcomes.get(mint).unwrap_or(&Value::Null);
            let mut r = s.clone();
            let obj = r.as_object_mut().expect("signal row object");
            obj.insert(
                "outcome_bad".into(),
                json!(boolv(s.get("bad_outcome")) || boolv(o.get("bad"))),
            );
            obj.insert(
                "outcome_good".into(),
                json!(boolv(s.get("good_outcome")) || boolv(o.get("good"))),
            );
            obj.insert(
                "outcome_pnl".into(),
                json!(f64v(s.get("realized_pnl_sol"), f64v(o.get("pnl"), 0.0))),
            );
            obj.insert(
                "outcome_reason".into(),
                json!(if !strv(s, "exit_reason").is_empty() {
                    strv(s, "exit_reason")
                } else {
                    strv(o, "exit_reason")
                }),
            );
            r
        })
        .collect()
}

fn rate_stats<F: Fn(&Value) -> bool>(rows: &[Value], pred: F) -> Value {
    let sub: Vec<&Value> = rows.iter().filter(|r| pred(r)).collect();
    let n = sub.len();
    let bad = sub.iter().filter(|r| boolv(r.get("outcome_bad"))).count();
    let good = sub.iter().filter(|r| boolv(r.get("outcome_good"))).count();
    let pnl: f64 = sub.iter().map(|r| f64v(r.get("outcome_pnl"), 0.0)).sum();
    json!({"count":n,"bad":bad,"good":good,"bad_rate":((bad as f64 / n.max(1) as f64)*10000.0).round()/10000.0,"good_rate":((good as f64 / n.max(1) as f64)*10000.0).round()/10000.0,"pnl_sum":(pnl*1_000_000_000.0).round()/1_000_000_000.0})
}

fn mother_tables(edges: &[Value], outcomes: &HashMap<String, Value>) -> (Vec<Value>, Vec<Value>) {
    let mut by: HashMap<String, HashSet<String>> = HashMap::new();
    for e in edges {
        let m = strv(e, "mother_wallet");
        let mint = strv(e, "mint");
        if !m.is_empty() && !mint.is_empty() {
            by.entry(m.to_string())
                .or_default()
                .insert(mint.to_string());
        }
    }
    let rows: Vec<Value> = by.into_iter().map(|(mother, mints)| {
        let bad = mints.iter().filter(|mint| outcomes.get(*mint).map(|o| boolv(o.get("bad"))).unwrap_or(false)).count();
        let good = mints.iter().filter(|mint| outcomes.get(*mint).map(|o| boolv(o.get("good"))).unwrap_or(false)).count();
        let pnl: f64 = mints.iter().map(|mint| outcomes.get(mint).map(|o| f64v(o.get("pnl"), 0.0)).unwrap_or(0.0)).sum();
        json!({"mother_wallet":mother,"mint_count":mints.len(),"bad_count":bad,"good_count":good,"pnl_sum":(pnl*1_000_000_000.0).round()/1_000_000_000.0})
    }).collect();
    let mut bad = rows.clone();
    bad.sort_by(|a, b| {
        (
            i64v(b.get("bad_count"), 0),
            i64v(b.get("mint_count"), 0),
            -f64v(b.get("pnl_sum"), 0.0) as i64,
        )
            .cmp(&(
                i64v(a.get("bad_count"), 0),
                i64v(a.get("mint_count"), 0),
                -f64v(a.get("pnl_sum"), 0.0) as i64,
            ))
    });
    let mut good = rows;
    good.sort_by(|a, b| {
        (
            i64v(b.get("good_count"), 0),
            (f64v(b.get("pnl_sum"), 0.0) * 1e9) as i64,
            i64v(b.get("mint_count"), 0),
        )
            .cmp(&(
                i64v(a.get("good_count"), 0),
                (f64v(a.get("pnl_sum"), 0.0) * 1e9) as i64,
                i64v(a.get("mint_count"), 0),
            ))
    });
    (
        bad.into_iter().take(15).collect(),
        good.into_iter().take(15).collect(),
    )
}

fn build_summary(signals: &[Value], edges: &[Value], outcomes: &HashMap<String, Value>) -> Value {
    let rows = enrich(signals, outcomes);
    let buckets = ["00-20", "20-40", "40-60", "60-80", "80-100"];
    let mut rb = serde_json::Map::new();
    for b in buckets {
        rb.insert(
            b.into(),
            rate_stats(&rows, |r| risk_bucket(f64v(r.get("risk_score"), 0.0)) == b),
        );
    }
    let mut class_names: Vec<String> = rows
        .iter()
        .map(|r| {
            let c = strv(r, "bundle_classification");
            if c.is_empty() {
                "UNKNOWN".to_string()
            } else {
                c.to_string()
            }
        })
        .collect();
    class_names.sort();
    class_names.dedup();
    let mut classes = serde_json::Map::new();
    for cls in class_names {
        classes.insert(
            cls.clone(),
            rate_stats(&rows, |r| {
                let c = strv(r, "bundle_classification");
                (if c.is_empty() { "UNKNOWN" } else { c }) == cls
            }),
        );
    }
    let decision_proxy = json!({
        "avoid_dev_cluster": rate_stats(&rows, |r| f64v(r.get("risk_score"),0.0) >= 70.0 || strv(r,"bundle_classification") == "DEV_SNIPER_SUSPECT"),
        "follow_strong_candidate": rate_stats(&rows, |r| f64v(r.get("follow_score"),0.0) >= 65.0 && f64v(r.get("risk_score"),0.0) < 45.0),
        "follow_candidate": rate_stats(&rows, |r| f64v(r.get("follow_score"),0.0) >= 45.0 && f64v(r.get("risk_score"),0.0) < 60.0),
    });
    let (bad_mothers, good_mothers) = mother_tables(edges, outcomes);
    let mut source_counts: HashMap<String, usize> = HashMap::new();
    for o in outcomes.values() {
        *source_counts
            .entry(strv(o, "source").to_string())
            .or_default() += 1;
    }
    json!({"signals":rows.len(),"edges":edges.len(),"risk_buckets":rb,"classes":classes,"decision_proxy":decision_proxy,"top_bad_mothers":bad_mothers,"top_good_mothers":good_mothers,"outcome_sources":source_counts,"live_allowed":false})
}

fn write_report(path: &str, summary: &Value) -> anyhow::Result<()> {
    let mut s = String::new();
    s.push_str("# Bundler Score Calibration Report\n\nShadow-only GTFA risk/follow calibration. No live permission.\n\n");
    s.push_str(&format!(
        "- signals: {}\n- edges: {}\n- live_allowed: false\n\n",
        summary["signals"], summary["edges"]
    ));
    s.push_str("## Risk buckets\n\n| Bucket | Count | Bad | Bad rate | Good | Good rate | PnL sum |\n|---|---:|---:|---:|---:|---:|---:|\n");
    if let Some(map) = summary.get("risk_buckets").and_then(|v| v.as_object()) {
        for b in ["00-20", "20-40", "40-60", "60-80", "80-100"] {
            let r = &map[b];
            s.push_str(&format!(
                "| {b} | {} | {} | {:.2}% | {} | {:.2}% | {:.9} |\n",
                r["count"],
                r["bad"],
                f64v(r.get("bad_rate"), 0.0) * 100.0,
                r["good"],
                f64v(r.get("good_rate"), 0.0) * 100.0,
                f64v(r.get("pnl_sum"), 0.0)
            ));
        }
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
    let signals = read_jsonl(&arg_value(args, "--signals", DEFAULT_SIGNALS));
    let edges = read_jsonl(&arg_value(args, "--edges", DEFAULT_EDGES));
    let outcomes = load_outcomes(
        &arg_value(args, "--state", DEFAULT_STATE),
        &arg_value(args, "--forward", DEFAULT_FORWARD),
    );
    let report = arg_value(args, "--report", DEFAULT_REPORT);
    let summary_path = arg_value(args, "--summary", DEFAULT_SUMMARY);
    let summary = build_summary(&signals, &edges, &outcomes);
    write_json(&summary_path, &summary)?;
    write_report(&report, &summary)?;
    Ok(
        json!({"signals":signals.len(),"edges":edges.len(),"report":report,"summary":summary_path,"live_allowed":false}),
    )
}

fn self_test() {
    let signals = vec![
        json!({"mint":"M1","bundle_classification":"DEV_SNIPER_SUSPECT","risk_score":80,"follow_score":10}),
    ];
    let edges = vec![json!({"mint":"M1","mother_wallet":"mother"})];
    let outcomes = HashMap::from([(
        "M1".to_string(),
        json!({"bad":true,"good":false,"pnl":-1,"source":"forward"}),
    )]);
    let summary = build_summary(&signals, &edges, &outcomes);
    assert_eq!(summary["signals"], 1);
    assert_eq!(summary["risk_buckets"]["80-100"]["bad"], 1);
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
