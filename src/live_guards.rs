use crate::engine::MigrationTarget;
use crate::executor;
use crate::live_env::{env_bool, env_f64, env_u64};
use crate::state::{LedgerManager, PositionState};
use chrono::Utc;
use std::env;

pub fn diagnostic_day_utc() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

pub fn live_onchain_diagnostic_max_per_day() -> usize {
    env_u64("LIVE_ONCHAIN_DIAGNOSTIC_MAX_PER_DAY", 2).clamp(0, 10) as usize
}

pub fn validate_onchain_diagnostic_allowed(
    ledger: &LedgerManager,
    target: &MigrationTarget,
) -> Result<(), String> {
    if !env_bool("LIVE_ONCHAIN_DIAGNOSTIC_ENABLED", false) {
        return Err("diagnostic_disabled".into());
    }
    if env_bool("PAPER_MODE", true) || !env_bool("LIVE_ARMED", false) {
        return Err("diagnostic_requires_live_armed".into());
    }
    if !env_bool("AMM_LIVE_CANARY", false) {
        return Err("diagnostic_requires_canary".into());
    }
    if env_f64("BUY_AMOUNT_SOL", 0.003) > 0.003 {
        return Err("diagnostic_buy_amount_too_large".into());
    }
    if env_u64("MAX_TRADES_PER_RUN", 1) != 1 {
        return Err("diagnostic_requires_single_trade".into());
    }
    if !env_bool("LIVE_AUTO_SELL_ENABLED", false) || !env_bool("LIVE_SELL_SEND_ENABLED", false) {
        return Err("diagnostic_requires_auto_sell".into());
    }

    let rows = ledger
        .read_all()
        .map_err(|e| format!("diagnostic_ledger_read_failed:{e}"))?;
    if diagnostic_already_used_for_pool(&rows, target) {
        return Err("diagnostic_pool_already_tested".into());
    }
    let today = diagnostic_day_utc();
    let count = diagnostic_count_for_day(&rows, &today);
    let max = live_onchain_diagnostic_max_per_day();
    if count >= max {
        return Err("diagnostic_daily_limit_reached".into());
    }
    Ok(())
}

pub fn diagnostic_count_for_day(rows: &[PositionState], day: &str) -> usize {
    rows.iter()
        .filter(|r| r.diagnostic_day == day && is_diagnostic_label(&r.diagnostic_label))
        .count()
}

pub fn diagnostic_already_used_for_pool(rows: &[PositionState], target: &MigrationTarget) -> bool {
    rows.iter().any(|r| {
        is_diagnostic_label(&r.diagnostic_label)
            && (r.mint == target.mint
                || (!target.pool_state.is_empty() && r.pool_state == target.pool_state))
    })
}

pub fn is_diagnostic_label(label: &str) -> bool {
    matches!(
        label,
        "ONCHAIN_DIAGNOSTIC_TEST" | "RPC_PREFLIGHT_FALSE_REJECTION" | "POOL_LEVEL_REJECTED"
    )
}

pub fn helius_sender_submit_count_for_day(rows: &[PositionState], day: &str) -> usize {
    rows.iter()
        .filter(|r| r.live_send_backend == "helius_sender" && r.live_send_day == day)
        .map(|r| {
            usize::from(!r.tx_signature.is_empty()) + usize::from(!r.sell_signature.is_empty())
        })
        .sum()
}

pub fn validate_helius_sender_daily_limit(ledger: &LedgerManager) -> anyhow::Result<()> {
    let max = executor::helius_sender_max_per_day();
    if max == 0 {
        anyhow::bail!("HELIUS_SENDER_MAX_PER_DAY must be > 0");
    }
    let rows = ledger.read_all()?;
    let today = diagnostic_day_utc();
    let count = helius_sender_submit_count_for_day(&rows, &today);
    if count >= max {
        anyhow::bail!(
            "HELIUS_SENDER_MAX_PER_DAY exceeded: {} >= {} for {}",
            count,
            max,
            today
        );
    }
    Ok(())
}

pub fn validate_live_start(paper_mode: bool, live_armed: bool) -> anyhow::Result<()> {
    if paper_mode || !live_armed {
        return Ok(());
    }
    let required = [
        ("AMM_LIVE_CANARY", "true"),
        ("HELIUS_MIGRATION_ENABLED", "true"),
        ("PUMPPORTAL_ENABLED", "false"),
        ("MIGRATION_CAPTURE_MODE", "false"),
        ("MAX_TRADES_PER_RUN", "1"),
        ("JITO_TIP_LAMPORTS", "0"),
        ("EMERGENCY_JITO_TIP_LAMPORTS", "0"),
    ];
    for (k, v) in required {
        if env::var(k).unwrap_or_default() != v {
            anyhow::bail!("AMM CANARY BLOCKED: {k} must be {v}");
        }
    }
    if env_f64("BUY_AMOUNT_SOL", 0.003) > 0.003 {
        anyhow::bail!("AMM CANARY BLOCKED: BUY_AMOUNT_SOL must be <= 0.003");
    }
    if env::var("LIVE_VARIANT").unwrap_or_else(|_| "Z".into()) != "Z3" {
        anyhow::bail!("AMM CANARY BLOCKED: LIVE_VARIANT must be Z3");
    }
    if env_bool("LIVE_SEND_ENABLED", false) {
        let backend = env::var("LIVE_SEND_BACKEND").unwrap_or_else(|_| "rpc".into());
        if backend != "rpc" && backend != "helius_sender" {
            anyhow::bail!(
                "AMM CANARY BLOCKED: LIVE_SEND_BACKEND must be rpc or helius_sender in this build"
            );
        }
        if backend == "helius_sender" {
            let endpoint = env::var("HELIUS_SENDER_ENDPOINT")
                .unwrap_or_else(|_| "https://sender.helius-rpc.com/fast?swqos_only=true".into());
            let mode = executor::helius_sender_endpoint_mode(&endpoint);
            let tip = env_u64("HELIUS_SENDER_TIP_LAMPORTS", 5_000);
            executor::validate_sender_tip(mode, tip)
                .map_err(|e| anyhow::anyhow!("AMM CANARY BLOCKED: {e}"))?;
            validate_helius_sender_daily_limit(&LedgerManager::default())
                .map_err(|e| anyhow::anyhow!("AMM CANARY BLOCKED: {e}"))?;
        }
        if !env_bool("LIVE_AUTO_SELL_ENABLED", false) {
            anyhow::bail!("AMM CANARY BLOCKED: LIVE_AUTO_SELL_ENABLED must be true for live send");
        }
        if !env_bool("LIVE_SELL_SEND_ENABLED", false) {
            anyhow::bail!("AMM CANARY BLOCKED: LIVE_SELL_SEND_ENABLED must be true for live send");
        }
    }
    Ok(())
}
