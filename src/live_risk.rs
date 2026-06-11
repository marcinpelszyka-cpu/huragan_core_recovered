use crate::live_env::env_f64;
use crate::state::{LedgerManager, PositionState};

pub fn max_daily_loss_sol() -> f64 {
    env_f64("MAX_DAILY_LOSS_SOL", 0.01)
}

pub fn max_daily_trades() -> usize {
    env_f64("MAX_DAILY_TRADES", 10.0) as usize
}

pub fn max_consecutive_losses() -> usize {
    env_f64("MAX_CONSECUTIVE_LOSSES", 3.0) as usize
}

pub fn min_wallet_balance_sol() -> f64 {
    env_f64("MIN_WALLET_BALANCE_SOL", 0.03)
}

pub fn live_risk_manager_enabled() -> bool {
    std::env::var("LIVE_RISK_MANAGER_ENABLED")
        .map(|v| v == "true")
        .unwrap_or(true)
}

pub fn validate_live_risk(ledger: &LedgerManager) -> anyhow::Result<()> {
    if !live_risk_manager_enabled() {
        return Ok(());
    }

    let rows = ledger.read_all()?;
    let today = crate::live_guards::diagnostic_day_utc();

    let daily_pnl = daily_realized_pnl(&rows, &today);
    if daily_pnl <= -max_daily_loss_sol() {
        anyhow::bail!("daily_loss_limit");
    }

    let trade_count = daily_trade_count(&rows, &today);
    if trade_count >= max_daily_trades() {
        anyhow::bail!("daily_trade_limit");
    }

    let consecutive = consecutive_losses(&rows);
    if consecutive >= max_consecutive_losses() {
        anyhow::bail!("consecutive_loss_limit");
    }

    if has_live_sell_failed_today(&rows, &today) {
        anyhow::bail!("live_sell_failed_retryable_today");
    }

    Ok(())
}

pub fn daily_realized_pnl(rows: &[PositionState], day: &str) -> f64 {
    rows.iter()
        .filter(|r| r.live_send_day == day)
        .map(|r| r.realized_pnl_sol)
        .sum()
}

pub fn daily_trade_count(rows: &[PositionState], day: &str) -> usize {
    rows.iter()
        .filter(|r| r.live_send_day == day)
        .filter(|r| {
            matches!(
                r.status.as_str(),
                "completed_profit" | "completed_loss" | "live_failed"
            )
        })
        .count()
}

pub fn consecutive_losses(rows: &[PositionState]) -> usize {
    rows.iter()
        .rev()
        .take_while(|r| {
            matches!(
                r.status.as_str(),
                "completed_loss" | "live_failed" | "unrecoverable_dust_or_rug"
            )
        })
        .count()
}

pub fn has_live_sell_failed_today(rows: &[PositionState], day: &str) -> bool {
    rows.iter()
        .filter(|r| r.live_send_day == day)
        .any(|r| r.status == "live_sell_failed_retryable")
}

pub fn risk_manager_status(ledger: &LedgerManager) -> String {
    match validate_live_risk(ledger) {
        Ok(()) => "GO".into(),
        Err(e) => format!("NO_GO:{e}"),
    }
}
