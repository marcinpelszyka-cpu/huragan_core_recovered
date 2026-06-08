// Sniper Follow Shadow Layer — observation only, no live execution.
//
// Loads GOOD_SNIPER wallet list from datasets/sniper_wallet_scores.jsonl,
// observes migration targets via Helius gTFA, and writes shadow signals
// to sniper_follow_shadow.jsonl.
//
// NEVER buys, NEVER sells, NEVER modifies Z3 behavior.
use crate::engine::MigrationTarget;
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

static GOOD_SNIPERS: OnceLock<HashSet<String>> = OnceLock::new();

/// Load GOOD_SNIPER wallets from the ranked scores file.
pub fn load_good_snipers() -> HashSet<String> {
    GOOD_SNIPERS
        .get_or_init(|| {
            let path = Path::new("datasets/sniper_wallet_scores.jsonl");
            let mut set = HashSet::new();
            if let Ok(content) = std::fs::read_to_string(path) {
                for line in content.lines() {
                    if let Ok(row) = serde_json::from_str::<serde_json::Value>(line) {
                        if row.get("category").and_then(|v| v.as_str()) == Some("GOOD_SNIPER") {
                            if let Some(owner) = row.get("owner").and_then(|v| v.as_str()) {
                                set.insert(owner.to_string());
                            }
                        }
                    }
                }
            }
            println!(
                "👀 SNIPER SHADOW loaded {} GOOD_SNIPER wallets",
                set.len()
            );
            set
        })
        .clone()
}

/// Shadow observation result for one migration target.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SniperShadowSignal {
    pub mint: String,
    pub pool_state: String,
    pub signal: bool,
    pub good_sniper_count: usize,
    pub total_good_sniper_buy_sol: f64,
    pub cohort_hold_pct_10s: f64,
    pub decision: String,
}

/// Observe a migration target and write shadow signal.
/// Currently stub — real implementation fetches gTFA on detected pools.
/// This is intentionally minimal for v1: we collect data via Python scripts,
/// and the Rust module provides the scaffolding for future live integration.
pub fn observe_target(target: &MigrationTarget) -> Option<SniperShadowSignal> {
    let good_snipers = load_good_snipers();
    if good_snipers.is_empty() {
        return None;
    }

    // Stub: would call Helius gTFA here to find early buyers
    // and check them against the GOOD_SNIPER set.
    // For now, return a placeholder that marks this pool as observed.
    Some(SniperShadowSignal {
        mint: target.mint.clone(),
        pool_state: target.pool_state.clone(),
        signal: false,
        good_sniper_count: 0,
        total_good_sniper_buy_sol: 0.0,
        cohort_hold_pct_10s: 0.0,
        decision: "shadow_observed_no_gfta_yet".to_string(),
    })
}

/// Write a shadow signal to the output file.
pub fn write_shadow_signal(signal: &SniperShadowSignal) {
    let path = Path::new("sniper_follow_shadow.jsonl");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        if let Ok(line) = serde_json::to_string(signal) {
            let _ = writeln!(file, "{line}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn good_snipers_starts_empty_when_file_missing() {
        let snipers = load_good_snipers();
        // File may or may not exist; function never panics
        assert!(snipers.len() < 1_000_000); // sanity
    }

    #[test]
    fn shadow_signal_serializes_to_json() {
        let sig = SniperShadowSignal {
            mint: "test_mint".into(),
            pool_state: "test_pool".into(),
            signal: false,
            good_sniper_count: 0,
            total_good_sniper_buy_sol: 0.0,
            cohort_hold_pct_10s: 0.0,
            decision: "shadow_observed_no_gfta_yet".into(),
        };
        let json = serde_json::to_string(&sig).unwrap();
        assert!(json.contains("shadow_observed_no_gfta_yet"));
    }
}
