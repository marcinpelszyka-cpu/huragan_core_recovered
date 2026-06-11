#[path = "../analytics.rs"]
mod analytics;

use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Deserialize, Default)]
struct TradeRow {
    mint: String,
    variant_id: String,
    status: String,
    exit_reason: String,
    #[serde(default)]
    net_pnl_pct: f64,
    #[serde(default)]
    net_pnl_sol: f64,
    #[serde(default)]
    max_favorable_pct: f64,
    #[serde(default)]
    max_drawdown_pct: f64,
    #[serde(default)]
    hold_secs: f64,
    #[serde(default)]
    entry_quote_reserve_ui: f64,
    #[serde(default)]
    excluded_from_stats: bool,
}

#[derive(Debug)]
struct BucketStats {
    count: usize,
    wins: usize,
    total_pnl_sol: f64,
    avg_pnl_pct: f64,
    avg_hold: f64,
    avg_mfe: f64,
}

fn clean(row: &TradeRow) -> bool {
    row.status == "paper_completed"
        && !row.excluded_from_stats
        && row.max_favorable_pct <= 200.0
        && row.net_pnl_pct.abs() <= 300.0
        && row.exit_reason != *"price_unavailable"
        && row.exit_reason != *"invalid_quote"
}

fn main() {
    let text = fs::read_to_string("state.jsonl").unwrap_or_default();
    let rows: Vec<TradeRow> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<TradeRow>(l).ok())
        .collect();

    let cleaned: Vec<&TradeRow> = rows.iter().filter(|r| clean(r)).collect();

    // Gate thresholds to test (SOL)
    let gates = vec![10.0, 25.0, 50.0, 75.0, 100.0, 150.0, 200.0, 300.0];

    // Variants
    let variants = vec!["Z3", "Z3.1", "Z3H_SHADOW"];

    // Exit strategies
    let exit_types = vec![
        "all",
        "max_hold",
        "trailing_stop",
        "early_no_momentum",
        "profit_protect",
    ];

    println!("# Z3 Optimizer — Brute-force parameter search\n");
    println!("rows_total={} cleaned={}\n", rows.len(), cleaned.len());

    // For each (gate, variant, exit) combination
    let mut best_per_gate: HashMap<String, (f64, f64, f64)> = HashMap::new();

    for &gate in &gates {
        let filtered: Vec<&&TradeRow> = cleaned
            .iter()
            .filter(|r| r.entry_quote_reserve_ui >= gate)
            .collect();

        for &variant in &variants {
            let var_rows: Vec<&&TradeRow> = filtered
                .iter()
                .filter(|r| r.variant_id == variant)
                .copied()
                .collect();

            for &exit in &exit_types {
                let ex_rows: Vec<&&TradeRow> = if exit == "all" {
                    var_rows.clone()
                } else {
                    var_rows.iter().filter(|r| r.exit_reason == exit).copied().collect()
                };

                if ex_rows.len() < 5 {
                    continue;
                }

                let n = ex_rows.len();
                let wins = ex_rows.iter().filter(|r| r.net_pnl_pct > 0.0).count();
                let total_sol: f64 = ex_rows.iter().map(|r| r.net_pnl_sol).sum();
                let avg_pnl: f64 = ex_rows.iter().map(|r| r.net_pnl_pct).sum::<f64>() / n as f64;
                let wr = wins as f64 / n as f64 * 100.0;
                let avg_hold: f64 =
                    ex_rows.iter().map(|r| r.hold_secs).sum::<f64>() / n as f64;

                let key = format!("gate={gate} variant={variant} exit={exit}");
                let entry = best_per_gate
                    .entry(key.clone())
                    .or_insert((0.0, 0.0, 0.0));
                if total_sol > entry.0 {
                    *entry = (total_sol, wr, avg_pnl);
                }

                // Print only reasonable combinations
                if n >= 10 && total_sol.abs() > 0.001 {
                    println!(
                        "gate={gate:>4}SOL var={variant:<12} exit={exit:<20} n={n:>5} wr={wr:>5.1}% avg_pnl={avg_pnl:>6.1}% total={total_sol:>8.4}SOL hold={avg_hold:>5.0}s"
                    );
                }
            }
        }
        println!();
    }

    // Top 10 by total SOL
    println!("\n## Top 10 by total SOL\n");
    let mut sorted: Vec<_> = best_per_gate.iter().collect();
    sorted.sort_by(|a, b| b.1 .0.partial_cmp(&a.1 .0).unwrap());
    for (key, (sol, wr, avg)) in sorted.iter().take(10) {
        println!("{key} total={sol:.4}SOL wr={wr:.1}% avg={avg:.1}%");
    }

    // Best WR at each gate
    println!("\n## Best WR per gate (min 20 rows)\n");
    for &gate in &gates {
        let mut best_wr = 0.0f64;
        let mut best_key = String::new();
        for (key, (_, wr, _)) in &best_per_gate {
            if key.starts_with(&format!("gate={gate} ")) && *wr > best_wr {
                let n = key.split("n=").nth(1);
                best_wr = *wr;
                best_key = key.clone();
            }
        }
        if best_wr > 0.0 {
            println!("{best_key}");
        }
    }
}
