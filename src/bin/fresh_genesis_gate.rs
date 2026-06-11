use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{read_to_string, File};
use std::io::{BufRead, BufReader, Write};

#[derive(Debug, Deserialize)]
struct SniperEvent {
    mint: String,
    age_secs: u64,
    side: String,
    quote_delta_sol: f64,
    signer: String,
    entry_market_cap_sol: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct BundleRisk {
    mint: String,
    shared_mother_count: u32,
    risk_score: f64,
    bad_mother_repeated: bool,
}

#[derive(Debug, Deserialize)]
struct ForwardOutcome {
    mint: String,
    forward_label_30s_60s: String,
}

#[derive(Debug, Serialize)]
struct GenesisSignal {
    mint: String,
    age_sec: u64,
    market_cap_bucket: String,
    good_sniper_count: u32,
    early_buyer_count: u32,
    total_early_buy_sol: f64,
    shared_mother_count: u32,
    risk_score: f64,
    buy_flow_10s: f64,
    sell_flow_10s: f64,
    forward_label_30s_60s: String,
    decision: String,
    live_allowed: bool,
}

fn load_jsonl<T>(path: &str) -> Vec<T>
where
    T: for<'de> Deserialize<'de>,
{
    let mut result = Vec::new();
    if let Ok(file) = File::open(path) {
        let reader = BufReader::new(file);
        for line in reader.lines() {
            if let Ok(l) = line {
                if let Ok(parsed) = serde_json::from_str::<T>(&l) {
                    result.push(parsed);
                }
            }
        }
    }
    result
}

fn main() {
    let events: Vec<SniperEvent> = load_jsonl("datasets/sniper_trade_events.jsonl");
    let risks: Vec<BundleRisk> = load_jsonl("datasets/fresh_bundle_risk_signals.jsonl");
    let outcomes: Vec<ForwardOutcome> = load_jsonl("datasets/fresh_forward_outcomes.jsonl");

    let mut risk_map: HashMap<String, BundleRisk> = HashMap::new();
    for r in risks {
        risk_map.insert(r.mint.clone(), r);
    }

    let mut outcome_map: HashMap<String, String> = HashMap::new();
    for o in outcomes {
        outcome_map.insert(o.mint.clone(), o.forward_label_30s_60s);
    }

    let mut by_mint: HashMap<String, Vec<&SniperEvent>> = HashMap::new();
    for ev in &events {
        by_mint.entry(ev.mint.clone()).or_default().push(ev);
    }

    let mut signals = Vec::new();

    for (mint, evts) in by_mint {
        let mut age_sec = evts.iter().map(|e| e.age_secs).min().unwrap_or(999);
        if age_sec > 60 {
            continue;
        }

        let early_evts: Vec<_> = evts.iter().filter(|e| e.age_secs <= 10).collect();
        let early_buyers: Vec<_> = early_evts.iter().filter(|e| e.side == "buy").collect();
        
        let early_buyer_count = early_buyers.len() as u32;
        let total_early_buy_sol: f64 = early_buyers.iter().map(|e| e.quote_delta_sol.abs()).sum();
        let good_sniper_count = early_buyer_count; // Simplified: all early buyers count as snipers

        let buy_flow_10s: f64 = early_evts.iter().filter(|e| e.side == "buy").map(|e| e.quote_delta_sol.abs()).sum();
        let sell_flow_10s: f64 = early_evts.iter().filter(|e| e.side == "sell").map(|e| e.quote_delta_sol.abs()).sum();

        let risk = risk_map.get(&mint);
        let shared_mother_count = risk.map(|r| r.shared_mother_count).unwrap_or(0);
        let risk_score = risk.map(|r| r.risk_score).unwrap_or(0.0);
        let bad_mother_repeated = risk.map(|r| r.bad_mother_repeated).unwrap_or(false);

        let forward_label = outcome_map
            .get(&mint)
            .cloned()
            .unwrap_or_else(|| "UNKNOWN".to_string());

        // MC Bucket estimation: ~$3.5k-$10k at ~$140/SOL is roughly 25-71 SOL
        let estimated_mc = evts
            .first()
            .and_then(|e| e.entry_market_cap_sol)
            .unwrap_or(25.0 + total_early_buy_sol);
        let market_cap_bucket = if estimated_mc >= 25.0 && estimated_mc <= 71.0 {
            "3.5k-10k".to_string()
        } else {
            "other".to_string()
        };

        // Decision logic
        let decision = if bad_mother_repeated || shared_mother_count >= 3 || risk_score >= 45.0 {
            "GENESIS_AVOID_BUNDLE".to_string()
        } else if forward_label == "HARD_DUMP" || forward_label == "RUG" {
            "GENESIS_AVOID_DUMP".to_string()
        } else if forward_label == "CONCENTRATION_REJECT" {
            "GENESIS_AVOID_CONCENTRATION".to_string()
        } else if market_cap_bucket != "3.5k-10k" {
            "GENESIS_UNKNOWN".to_string()
        } else if good_sniper_count >= 2 
                  && total_early_buy_sol >= 0.03 
                  && shared_mother_count <= 1 
                  && risk_score < 45.0 
                  && buy_flow_10s > sell_flow_10s 
                  && forward_label != "HARD_DUMP" 
                  && forward_label != "RUG" 
        {
            "GENESIS_FOLLOW_STRONG".to_string()
        } else {
            "GENESIS_WATCH".to_string()
        };

        signals.push(GenesisSignal {
            mint,
            age_sec,
            market_cap_bucket,
            good_sniper_count,
            early_buyer_count,
            total_early_buy_sol,
            shared_mother_count,
            risk_score,
            buy_flow_10s,
            sell_flow_10s,
            forward_label_30s_60s: forward_label,
            decision,
            live_allowed: false,
        });
    }

    signals.sort_by(|a, b| a.age_sec.partial_cmp(&b.age_sec).unwrap());

    // Summary stats
    let mut counts = HashMap::new();
    for s in &signals {
        *counts.entry(s.decision.clone()).or_insert(0) += 1;
    }

    let report = format!(
        "# Fresh Genesis Shadow Strategy Report\n\
         \n## Config\n\
         - token_age <= 60s\n\
         - target MC: 3.5k-10k\n\
         - live_allowed: false\n\
         \n## Summary\n\
         - total evaluated: {}\n\
         - GENESIS_FOLLOW_STRONG: {}\n\
         - GENESIS_WATCH: {}\n\
         - GENESIS_AVOID_BUNDLE: {}\n\
         - GENESIS_AVOID_DUMP: {}\n\
         - GENESIS_AVOID_CONCENTRATION: {}\n\
         - GENESIS_UNKNOWN: {}\n",
        signals.len(),
        counts.get("GENESIS_FOLLOW_STRONG").unwrap_or(&0),
        counts.get("GENESIS_WATCH").unwrap_or(&0),
        counts.get("GENESIS_AVOID_BUNDLE").unwrap_or(&0),
        counts.get("GENESIS_AVOID_DUMP").unwrap_or(&0),
        counts.get("GENESIS_AVOID_CONCENTRATION").unwrap_or(&0),
        counts.get("GENESIS_UNKNOWN").unwrap_or(&0),
    );

    std::fs::create_dir_all("datasets").ok();

    let mut sig_file = File::create("datasets/fresh_genesis_signals.jsonl").unwrap();
    for s in &signals {
        writeln!(sig_file, "{}", serde_json::to_string(s).unwrap()).ok();
    }

    std::fs::write("datasets/fresh_genesis_summary.json", serde_json::to_string_pretty(&counts).unwrap()).ok();
    std::fs::write("datasets/fresh_genesis_report.md", report).ok();

    // Candidates (just strong ones for now)
    let candidates: Vec<_> = signals.iter().filter(|s| s.decision == "GENESIS_FOLLOW_STRONG").collect();
    let mut cand_file = File::create("datasets/fresh_genesis_candidates.jsonl").unwrap();
    for c in &candidates {
        writeln!(cand_file, "{}", serde_json::to_string(c).unwrap()).ok();
    }

    println!("Analyzed {} tokens.", signals.len());
    println!("Signals written to datasets/fresh_genesis_signals.jsonl");
    println!("Report: datasets/fresh_genesis_report.md");
}
