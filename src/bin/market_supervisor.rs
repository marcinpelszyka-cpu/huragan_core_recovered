use chrono::{FixedOffset, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::env;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize)]
struct PositionLike {
    #[serde(default)]
    variant_id: String,
    #[serde(default)]
    mint: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    paper_entry_sol: f64,
    #[serde(default)]
    net_pnl_sol: f64,
    #[serde(default)]
    net_pnl_pct: f64,
    #[serde(default)]
    exit_reason: String,
    #[serde(default)]
    excluded_from_stats: bool,
    #[serde(default)]
    advanced_gate_passed: bool,
}

#[derive(Debug, Serialize)]
struct RecommendedEnv {
    #[serde(rename = "LIVE_VARIANT")]
    live_variant: String,
    #[serde(rename = "BUY_AMOUNT_SOL")]
    buy_amount_sol: String,
    #[serde(rename = "MAX_TRADES_PER_RUN")]
    max_trades_per_run: String,
    #[serde(rename = "AMM_MIN_POOL_SOL_FOR_ENTRY_LAMPORTS")]
    amm_min_pool_sol_for_entry_lamports: String,
    #[serde(rename = "AMM_ADVANCED_GATE_MODE")]
    amm_advanced_gate_mode: String,
}

#[derive(Debug, Default, Serialize)]
struct Metrics {
    z_completed: usize,
    z_wr_pct: f64,
    z_median_pnl_pct: f64,
    z_avg_pnl_pct: f64,
    z_total_sol: f64,
    price_unavailable_pct: f64,
    coverage_per_hour: f64,
    fresh_precision_pct: f64,
    capacity_skip: usize,
    excluded_rate_pct: f64,
    advanced_gate_passed_count: usize,
    live_blocker_count: usize,
}

#[derive(Debug, Default, Serialize)]
struct VariantMetrics {
    completed: usize,
    wr_pct: f64,
    median_pnl_pct: f64,
    avg_pnl_pct: f64,
    total_sol: f64,
    price_unavailable_pct: f64,
}

#[derive(Debug, Default, Serialize)]
struct MigrationDatasetMetrics {
    clean_winners: usize,
    quote_spike_suspects: usize,
    gtfa_enriched_winners: usize,
    usdc_pool_observations: usize,
}

#[derive(Debug, Default, Serialize)]
struct FreshDatasetMetrics {
    tracked_mints: usize,
    wsol_tracked_mints: usize,
    usdc_tracked_mints: usize,
    moonshot_winners: usize,
    rug_cases: usize,
    no_trade_data_count: usize,
    no_trade_data_pct: f64,
}

#[derive(Debug, Default, Serialize)]
struct DataQualityMetrics {
    migration_gtfa_rows: usize,
    fresh_gtfa_rows: usize,
    fresh_trade_stream_missing_pct: f64,
}

#[derive(Debug, Default, Serialize)]
struct RecommendedActions {
    migration_live_canary_allowed: bool,
    fresh_shadow_only: bool,
    fresh_data_insufficient: bool,
    fresh_candidate_quality_improving: bool,
    notes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct DecisionDoc {
    timestamp_warsaw: String,
    window_minutes: u64,
    decision: String,
    live_allowed: bool,
    strategy: String,
    market_regime: String,
    recommended_env: RecommendedEnv,
    metrics: Metrics,
    variant_metrics: HashMap<String, VariantMetrics>,
    migration_metrics: MigrationDatasetMetrics,
    fresh_metrics: FreshDatasetMetrics,
    data_quality: DataQualityMetrics,
    recommended_actions: RecommendedActions,
    reasons: Vec<String>,
}

fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    let args: Vec<String> = env::args().collect();
    let state_path = arg(&args, "--state", "state.jsonl");
    let live_state_path = arg(&args, "--live-state", "state.jsonl");
    let output_path = arg(&args, "--output", "agents_decision.json");
    let report_path = arg(&args, "--report", "/tmp/market_supervisor_report.md");
    let dataset_dir = arg(&args, "--dataset-dir", "datasets");
    let window_minutes = arg(&args, "--window-mins", "120")
        .parse::<u64>()
        .unwrap_or(120);

    let rows = parse_positions(&read_recent_jsonl(&state_path, 5000));
    let latest = dedupe_by_mint_variant(&rows);
    let latest_rows = latest.values().cloned().collect::<Vec<_>>();

    let z_all = latest_rows
        .iter()
        .filter(|r| r.variant_id == "Z" && r.status == "paper_completed" && r.paper_entry_sol > 0.0)
        .count();
    let z_price_unavailable = latest_rows
        .iter()
        .filter(|r| {
            r.variant_id == "Z"
                && r.status == "paper_completed"
                && r.exit_reason == "price_unavailable"
        })
        .count();
    let z_clean = latest_rows
        .iter()
        .filter(|r| clean_pnl_row(r) && r.variant_id == "Z")
        .collect::<Vec<_>>();
    let z_completed = z_clean.len();
    let z_wins = z_clean.iter().filter(|r| r.net_pnl_sol > 0.0).count();
    let z_wr = pct(z_wins, z_completed);
    let mut pcts = z_clean.iter().map(|r| r.net_pnl_pct).collect::<Vec<_>>();
    let z_median = median(&mut pcts);
    let z_avg = if z_completed == 0 {
        0.0
    } else {
        z_clean.iter().map(|r| r.net_pnl_pct).sum::<f64>() / z_completed as f64
    };
    let z_total = z_clean.iter().map(|r| r.net_pnl_sol).sum::<f64>();
    let price_unavailable_pct = pct(z_price_unavailable, z_all);
    let capacity_skip = latest_rows
        .iter()
        .filter(|r| r.status.contains("capacity"))
        .count();
    let completed_total = latest_rows
        .iter()
        .filter(|r| r.status == "paper_completed" && r.paper_entry_sol > 0.0)
        .count();
    let excluded = latest_rows
        .iter()
        .filter(|r| {
            r.status == "paper_completed"
                && r.paper_entry_sol > 0.0
                && (r.excluded_from_stats
                    || ["price_unavailable", "data_quality_fail", "invalid_quote"]
                        .contains(&r.exit_reason.as_str()))
        })
        .count();
    let excluded_rate = pct(excluded, completed_total);
    let adv_pass = latest_rows
        .iter()
        .filter(|r| r.variant_id == "Z" && r.status == "paper_completed" && r.advanced_gate_passed)
        .count();
    let variant_metrics = build_variant_metrics(&latest_rows, &["F", "I", "Z", "Z2", "Z3", "Z3.1"]);
    let migration_dataset_metrics = read_migration_dataset_metrics(&dataset_dir);
    let fresh_dataset_metrics = read_fresh_dataset_metrics(&dataset_dir);
    let data_quality = DataQualityMetrics {
        migration_gtfa_rows: count_jsonl(&format!("{dataset_dir}/migration_gtfa_lifecycle.jsonl")),
        fresh_gtfa_rows: count_jsonl(&format!("{dataset_dir}/fresh_gtfa_lifecycle.jsonl")),
        fresh_trade_stream_missing_pct: fresh_dataset_metrics.no_trade_data_pct,
    };

    let live_rows = parse_positions(&read_recent_jsonl(&live_state_path, 5000));
    let live_latest = dedupe_by_mint(&live_rows);
    let blockers = live_blocker_count(live_latest.values());

    let mut reasons = Vec::new();
    if z_completed < 50 {
        reasons.push(format!("Z completed < 50: {z_completed}"));
    }
    if z_median <= 0.0 {
        reasons.push(format!("Z median <= 0: {z_median:.2}%"));
    }
    if z_avg < 0.0 {
        reasons.push(format!("Z avg < 0: {z_avg:.2}%"));
    }
    if price_unavailable_pct >= 40.0 {
        reasons.push(format!(
            "price_unavailable >= 40%: {price_unavailable_pct:.1}%"
        ));
    }
    if capacity_skip > 0 {
        reasons.push(format!("capacity_skip > 0: {capacity_skip}"));
    }
    if blockers > 0 {
        reasons.push(format!("live blockers present: {blockers}"));
    }

    let decision = if !reasons.is_empty() {
        if price_unavailable_pct >= 40.0 || z_median <= 0.0 || z_avg < 0.0 || blockers > 0 {
            "NO_GO"
        } else {
            "BORDERLINE"
        }
    } else if price_unavailable_pct >= 30.0 {
        reasons.push("borderline drain window".into());
        "BORDERLINE"
    } else {
        reasons.push("Z meets single-canary thresholds; manual approval still required".into());
        "GO_SINGLE_CANARY"
    };

    let migration_live_canary_allowed = decision == "GO_SINGLE_CANARY" || decision == "BORDERLINE";
    let fresh_data_insufficient = fresh_dataset_metrics.tracked_mints < 100
        || fresh_dataset_metrics.no_trade_data_pct >= 50.0;
    let recommended_actions = RecommendedActions {
        migration_live_canary_allowed,
        fresh_shadow_only: true,
        fresh_data_insufficient,
        fresh_candidate_quality_improving: fresh_dataset_metrics.moonshot_winners > 0
            && !fresh_data_insufficient,
        notes: vec![
            "Z3/Z migration and Fresh F are evaluated separately".into(),
            "Fresh remains SHADOW_ONLY until labelled lifecycle data is sufficient".into(),
            "USDC pools are observed but no live USDC builder is enabled".into(),
        ],
    };

    let doc = DecisionDoc {
        timestamp_warsaw: Utc::now()
            .with_timezone(&FixedOffset::east_opt(2 * 3600).unwrap())
            .to_rfc3339(),
        window_minutes,
        decision: decision.into(),
        live_allowed: decision == "GO_SINGLE_CANARY",
        strategy: if decision == "NO_GO" {
            "paper_only"
        } else {
            "migration_z"
        }
        .into(),
        market_regime: if price_unavailable_pct >= 40.0 {
            "toxic"
        } else if z_wr >= 55.0 && z_median > 2.0 {
            "hot"
        } else {
            "normal"
        }
        .into(),
        recommended_env: RecommendedEnv {
            live_variant: "Z".into(),
            buy_amount_sol: "0.003".into(),
            max_trades_per_run: "1".into(),
            amm_min_pool_sol_for_entry_lamports: "2000000000".into(),
            amm_advanced_gate_mode: "shadow".into(),
        },
        metrics: Metrics {
            z_completed,
            z_wr_pct: z_wr,
            z_median_pnl_pct: z_median,
            z_avg_pnl_pct: z_avg,
            z_total_sol: z_total,
            price_unavailable_pct,
            coverage_per_hour: z_all as f64 / (window_minutes as f64 / 60.0),
            fresh_precision_pct: 0.0,
            capacity_skip,
            excluded_rate_pct: excluded_rate,
            advanced_gate_passed_count: adv_pass,
            live_blocker_count: blockers,
        },
        variant_metrics,
        migration_metrics: migration_dataset_metrics,
        fresh_metrics: fresh_dataset_metrics,
        data_quality,
        recommended_actions,
        reasons,
    };

    fs::write(&output_path, serde_json::to_string_pretty(&doc)? + "\n")?;
    write_report(&report_path, &doc)?;
    println!(
        "MARKET_SUPERVISOR decision={} live_allowed={} strategy={} regime={}",
        doc.decision, doc.live_allowed, doc.strategy, doc.market_regime
    );
    Ok(())
}

fn arg(args: &[String], name: &str, default: &str) -> String {
    args.windows(2)
        .find(|w| w[0] == name)
        .map(|w| w[1].clone())
        .unwrap_or_else(|| default.into())
}
fn read_recent_jsonl(path: &str, max_records: usize) -> Vec<Value> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return vec![],
    };
    let mut q = VecDeque::with_capacity(max_records + 1);
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if let Ok(v) = serde_json::from_str::<Value>(&line) {
            q.push_back(v);
            if q.len() > max_records {
                q.pop_front();
            }
        }
    }
    q.into_iter().collect()
}
fn parse_positions(values: &[Value]) -> Vec<PositionLike> {
    values
        .iter()
        .filter_map(|v| serde_json::from_value(v.clone()).ok())
        .collect()
}
fn dedupe_by_mint_variant(rows: &[PositionLike]) -> HashMap<(String, String), PositionLike> {
    let mut latest = HashMap::new();
    for r in rows {
        if !r.mint.is_empty() {
            latest.insert((r.mint.clone(), r.variant_id.clone()), r.clone());
        }
    }
    latest
}
fn dedupe_by_mint(rows: &[PositionLike]) -> HashMap<String, PositionLike> {
    let mut latest = HashMap::new();
    for r in rows {
        if !r.mint.is_empty() {
            latest.insert(r.mint.clone(), r.clone());
        }
    }
    latest
}
fn live_blocker_count<'a>(rows: impl IntoIterator<Item = &'a PositionLike>) -> usize {
    rows.into_iter()
        .filter(|r| {
            matches!(
                r.status.as_str(),
                "holding"
                    | "dust_unwind_required"
                    | "amm_sell_failed_retryable"
                    | "live_sell_failed_retryable"
            )
        })
        .count()
}

fn clean_pnl_row(r: &PositionLike) -> bool {
    r.status == "paper_completed"
        && r.paper_entry_sol > 0.0
        && !r.excluded_from_stats
        && !["price_unavailable", "data_quality_fail", "invalid_quote"]
            .contains(&r.exit_reason.as_str())
}
fn build_variant_metrics(
    rows: &[PositionLike],
    variants: &[&str],
) -> HashMap<String, VariantMetrics> {
    let mut out = HashMap::new();
    for id in variants {
        let all = rows
            .iter()
            .filter(|r| {
                r.variant_id == *id && r.status == "paper_completed" && r.paper_entry_sol > 0.0
            })
            .count();
        let price_unavailable = rows
            .iter()
            .filter(|r| {
                r.variant_id == *id
                    && r.status == "paper_completed"
                    && r.exit_reason == "price_unavailable"
            })
            .count();
        let clean = rows
            .iter()
            .filter(|r| r.variant_id == *id && clean_pnl_row(r))
            .collect::<Vec<_>>();
        let completed = clean.len();
        let wins = clean.iter().filter(|r| r.net_pnl_sol > 0.0).count();
        let mut pcts = clean.iter().map(|r| r.net_pnl_pct).collect::<Vec<_>>();
        let avg = if completed == 0 {
            0.0
        } else {
            clean.iter().map(|r| r.net_pnl_pct).sum::<f64>() / completed as f64
        };
        out.insert(
            (*id).to_string(),
            VariantMetrics {
                completed,
                wr_pct: pct(wins, completed),
                median_pnl_pct: median(&mut pcts),
                avg_pnl_pct: avg,
                total_sol: clean.iter().map(|r| r.net_pnl_sol).sum::<f64>(),
                price_unavailable_pct: pct(price_unavailable, all),
            },
        );
    }
    out
}
fn pct(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        n as f64 / d as f64 * 100.0
    }
}
fn median(vals: &mut [f64]) -> f64 {
    if vals.is_empty() {
        return 0.0;
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = vals.len() / 2;
    if vals.len().is_multiple_of(2) {
        (vals[mid - 1] + vals[mid]) / 2.0
    } else {
        vals[mid]
    }
}

fn count_jsonl(path: &str) -> usize {
    File::open(path)
        .ok()
        .map(|f| {
            BufReader::new(f)
                .lines()
                .map_while(Result::ok)
                .filter(|l| !l.trim().is_empty())
                .count()
        })
        .unwrap_or(0)
}

fn parse_boolish(s: &str) -> bool {
    matches!(s, "true" | "True" | "1" | "yes" | "YES")
}

fn csv_rows(path: &str) -> Vec<HashMap<String, String>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return vec![],
    };
    let mut lines = BufReader::new(file).lines().map_while(Result::ok);
    let Some(header) = lines.next() else {
        return vec![];
    };
    let headers = split_csv_simple(&header);
    lines
        .map(|line| {
            let vals = split_csv_simple(&line);
            headers
                .iter()
                .enumerate()
                .map(|(i, h)| (h.clone(), vals.get(i).cloned().unwrap_or_default()))
                .collect()
        })
        .collect()
}

fn split_csv_simple(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                out.push(cur.clone());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

fn read_migration_dataset_metrics(dir: &str) -> MigrationDatasetMetrics {
    let winners = csv_rows(&format!("{dir}/migration_profit_winners.csv"));
    let suspects = csv_rows(&format!("{dir}/migration_quote_spike_suspects.csv"));
    let all = csv_rows(&format!("{dir}/migration_all_mint_summary.csv"));
    MigrationDatasetMetrics {
        clean_winners: winners.len(),
        quote_spike_suspects: suspects.len(),
        gtfa_enriched_winners: winners
            .iter()
            .filter(|r| parse_boolish(r.get("gtfa_enriched").map(String::as_str).unwrap_or("")))
            .count(),
        usdc_pool_observations: all
            .iter()
            .filter(|r| r.get("quote_symbol").map(String::as_str) == Some("USDC"))
            .count(),
    }
}

fn read_fresh_dataset_metrics(dir: &str) -> FreshDatasetMetrics {
    let rows = csv_rows(&format!("{dir}/fresh_all_mint_summary.csv"));
    let winners = csv_rows(&format!("{dir}/fresh_moonshot_winners.csv"));
    let rugs = csv_rows(&format!("{dir}/fresh_rug_cases.csv"));
    let no_trade = rows
        .iter()
        .filter(|r| {
            parse_boolish(
                r.get("trade_stream_missing")
                    .map(String::as_str)
                    .unwrap_or(""),
            ) || r.get("final_exit_label").map(String::as_str) == Some("no_trade_data")
        })
        .count();
    FreshDatasetMetrics {
        tracked_mints: rows.len(),
        wsol_tracked_mints: rows
            .iter()
            .filter(|r| r.get("quote_symbol").map(String::as_str) == Some("WSOL"))
            .count(),
        usdc_tracked_mints: rows
            .iter()
            .filter(|r| r.get("quote_symbol").map(String::as_str) == Some("USDC"))
            .count(),
        moonshot_winners: winners.len(),
        rug_cases: rugs.len(),
        no_trade_data_count: no_trade,
        no_trade_data_pct: pct(no_trade, rows.len()),
    }
}

fn write_report(path: &str, doc: &DecisionDoc) -> std::io::Result<()> {
    let mut f = File::create(Path::new(path))?;
    writeln!(f, "# Market Supervisor Report")?;
    writeln!(f, "- Decision: **{}**", doc.decision)?;
    writeln!(f, "- Strategy: `{}`", doc.strategy)?;
    writeln!(f, "- Regime: `{}`", doc.market_regime)?;
    writeln!(f, "- Z completed: {}", doc.metrics.z_completed)?;
    writeln!(f, "- Z WR: {:.1}%", doc.metrics.z_wr_pct)?;
    writeln!(f, "- Z median PnL: {:.2}%", doc.metrics.z_median_pnl_pct)?;
    writeln!(
        f,
        "- Price unavailable: {:.1}%",
        doc.metrics.price_unavailable_pct
    )?;
    writeln!(f, "## Variant Metrics")?;
    for id in ["F", "I", "Z", "Z2", "Z3", "Z3.1"] {
        if let Some(m) = doc.variant_metrics.get(id) {
            writeln!(
                f,
                "- {id}: n={} WR={:.1}% median={:.2}% avg={:.2}% total={:.5} SOL PU={:.1}%",
                m.completed,
                m.wr_pct,
                m.median_pnl_pct,
                m.avg_pnl_pct,
                m.total_sol,
                m.price_unavailable_pct
            )?;
        }
    }
    writeln!(f, "## Migration Dataset")?;
    writeln!(f, "- Winners: {}", doc.migration_metrics.clean_winners)?;
    writeln!(
        f,
        "- Quote spike suspects: {}",
        doc.migration_metrics.quote_spike_suspects
    )?;
    writeln!(
        f,
        "- gTFA enriched winners: {}",
        doc.migration_metrics.gtfa_enriched_winners
    )?;
    writeln!(
        f,
        "- USDC pool observations: {}",
        doc.migration_metrics.usdc_pool_observations
    )?;
    writeln!(f, "## Fresh Dataset")?;
    writeln!(f, "- Tracked: {}", doc.fresh_metrics.tracked_mints)?;
    writeln!(f, "- Moonshots: {}", doc.fresh_metrics.moonshot_winners)?;
    writeln!(f, "- Rugs: {}", doc.fresh_metrics.rug_cases)?;
    writeln!(
        f,
        "- No trade data: {:.1}%",
        doc.fresh_metrics.no_trade_data_pct
    )?;
    writeln!(f, "## Recommended Actions")?;
    writeln!(
        f,
        "- Z3 live/canary: {}",
        if doc.recommended_actions.migration_live_canary_allowed {
            "GO/BORDERLINE"
        } else {
            "NO_GO"
        }
    )?;
    writeln!(f, "- Fresh F: SHADOW_ONLY")?;
    writeln!(f, "- USDC pools: observed but no live builder")?;
    writeln!(f, "## Reasons")?;
    for r in &doc.reasons {
        writeln!(f, "- {r}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_failed_is_not_a_live_blocker() {
        let rows = [PositionLike {
            mint: "MintA".into(),
            status: "live_failed".into(),
            ..Default::default()
        }];
        assert_eq!(live_blocker_count(rows.iter()), 0);
    }

    #[test]
    fn latest_terminal_state_clears_historical_holding_blocker() {
        let rows = vec![
            PositionLike {
                mint: "MintA".into(),
                status: "holding".into(),
                ..Default::default()
            },
            PositionLike {
                mint: "MintA".into(),
                status: "paper_completed".into(),
                ..Default::default()
            },
        ];
        let latest = dedupe_by_mint(&rows);
        assert_eq!(live_blocker_count(latest.values()), 0);
    }
}
