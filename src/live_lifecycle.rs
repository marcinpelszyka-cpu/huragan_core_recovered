use crate::engine::{MigrationTarget, QuoteAsset};
use crate::state::{LedgerManager, PositionState};

pub const STATUS_HOLDING: &str = "holding";
pub const STATUS_COMPLETED: &str = "completed";
pub const STATUS_LIVE_FAILED: &str = "live_failed";
pub const STATUS_LIVE_SELL_FAILED_RETRYABLE: &str = "live_sell_failed_retryable";
pub const STATUS_UNRECOVERABLE_DUST_OR_RUG: &str = "unrecoverable_dust_or_rug";

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecyclePhase {
    Idle,
    BuyPlanned,
    BuySubmitted,
    Holding,
    SellMonitoring,
    SellSubmitted,
    Completed,
    LiveFailed,
    LiveSellFailedRetryable,
    UnrecoverableDustOrRug,
}

impl LifecyclePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::BuyPlanned => "buy_planned",
            Self::BuySubmitted => "buy_submitted",
            Self::Holding => "holding",
            Self::SellMonitoring => "sell_monitoring",
            Self::SellSubmitted => "sell_submitted",
            Self::Completed => "completed",
            Self::LiveFailed => "live_failed",
            Self::LiveSellFailedRetryable => "live_sell_failed_retryable",
            Self::UnrecoverableDustOrRug => "unrecoverable_dust_or_rug",
        }
    }
}

pub trait LiveExitPolicy {
    fn exit_reason(
        &self,
        age_secs: u64,
        ratio: f64,
        max_favorable_pct: f64,
    ) -> Option<&'static str>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Z3ExitPolicy;

impl LiveExitPolicy for Z3ExitPolicy {
    fn exit_reason(
        &self,
        age_secs: u64,
        ratio: f64,
        max_favorable_pct: f64,
    ) -> Option<&'static str> {
        z3_live_exit_reason(age_secs, ratio, max_favorable_pct)
    }
}

pub fn z3_live_exit_reason(
    age_secs: u64,
    ratio: f64,
    max_favorable_pct: f64,
) -> Option<&'static str> {
    let current_pnl_pct = (ratio - 1.0) * 100.0;
    if ratio <= 0.80 {
        return Some("hard_stop");
    }
    if age_secs >= 120 && max_favorable_pct < 25.0 {
        return Some("early_no_momentum");
    }
    if max_favorable_pct >= 20.0 && ratio <= 1.0 {
        return Some("breakeven_floor");
    }
    if z3_live_profit_protect_exit(max_favorable_pct, current_pnl_pct) {
        return Some("profit_protect");
    }
    if age_secs >= 300 {
        return Some("max_hold");
    }
    None
}

fn z3_live_profit_protect_exit(max_favorable_pct: f64, current_pnl_pct: f64) -> bool {
    const STAGES: &[(f64, f64)] = &[(150.0, 90.0), (100.0, 60.0), (60.0, 35.0), (30.0, 15.0)];
    STAGES.iter().any(|(mfe_threshold, stop_level)| {
        max_favorable_pct >= *mfe_threshold && current_pnl_pct <= *stop_level
    })
}

pub fn phase_for_status(status: &str) -> LifecyclePhase {
    match status {
        STATUS_HOLDING => LifecyclePhase::Holding,
        STATUS_COMPLETED => LifecyclePhase::Completed,
        STATUS_LIVE_FAILED => LifecyclePhase::LiveFailed,
        STATUS_LIVE_SELL_FAILED_RETRYABLE => LifecyclePhase::LiveSellFailedRetryable,
        STATUS_UNRECOVERABLE_DUST_OR_RUG => LifecyclePhase::UnrecoverableDustOrRug,
        _ => LifecyclePhase::Idle,
    }
}

pub fn lifecycle_id_for_state(state: &PositionState) -> String {
    lifecycle_id(
        &state.variant_id,
        &state.mint,
        &state.pool_state,
        &state.tx_signature,
    )
}

pub fn lifecycle_id(variant_id: &str, mint: &str, pool_state: &str, tx_signature: &str) -> String {
    let variant = if variant_id.is_empty() {
        "unknown"
    } else {
        variant_id
    };
    let anchor = if !tx_signature.is_empty() {
        tx_signature
    } else if !pool_state.is_empty() {
        pool_state
    } else {
        "no_anchor"
    };
    format!("{variant}:{mint}:{anchor}")
}

pub fn apply_lifecycle_phase(state: &mut PositionState, phase: LifecyclePhase) {
    state.lifecycle_phase = phase.as_str().to_string();
    if state.lifecycle_id.is_empty() {
        state.lifecycle_id = lifecycle_id_for_state(state);
    }
}

pub fn mark_terminal(state: &mut PositionState, reason: impl Into<String>) {
    state.terminal_reason = reason.into();
    state.rollback_required = true;
    apply_lifecycle_phase(state, phase_for_status(&state.status));
}

pub fn is_open_live_blocker(state: &PositionState) -> bool {
    state.variant_id == "Z3"
        && matches!(
            state.status.as_str(),
            STATUS_HOLDING | STATUS_LIVE_SELL_FAILED_RETRYABLE
        )
        && state.remaining_tokens > 0
}

#[allow(dead_code)]
pub fn is_terminal_operational_state(state: &PositionState) -> bool {
    matches!(
        state.status.as_str(),
        STATUS_COMPLETED | STATUS_LIVE_FAILED | STATUS_UNRECOVERABLE_DUST_OR_RUG
    )
}

pub fn latest_open_live_holding(ledger: &LedgerManager) -> anyhow::Result<Option<PositionState>> {
    let latest = ledger.get_latest_by_mint_variant()?;
    Ok(latest.into_values().find(is_open_live_blocker))
}

pub fn target_from_live_state(state: &PositionState) -> anyhow::Result<MigrationTarget> {
    if state.pool_state.is_empty()
        || state.base_mint.is_empty()
        || state.quote_mint.is_empty()
        || state.quote_asset_mint.is_empty()
        || state.pool_base_token_account.is_empty()
        || state.pool_quote_token_account.is_empty()
    {
        anyhow::bail!("live_sell_target_incomplete for {}", state.mint);
    }
    Ok(MigrationTarget {
        mint: state.mint.clone(),
        name: state.token_name.clone(),
        symbol: state.token_symbol.clone(),
        source: "helius_migration".into(),
        pool_state: state.pool_state.clone(),
        base_mint: state.base_mint.clone(),
        quote_mint: state.quote_mint.clone(),
        quote_asset_mint: state.quote_asset_mint.clone(),
        pool_base_token_account: state.pool_base_token_account.clone(),
        pool_quote_token_account: state.pool_quote_token_account.clone(),
        creator: state.creator_address.clone(),
        creator_score: state.creator_score,
        top10_holder_pct: state.top10_holder_pct,
        curve_velocity_secs: state.curve_velocity_secs,
        ..Default::default()
    })
}

pub fn ensure_wsol_live_target(target: &MigrationTarget) -> anyhow::Result<()> {
    if target.quote_asset() != QuoteAsset::Wsol {
        anyhow::bail!(
            "live lifecycle only supports WSOL quote, got {}",
            target.quote_asset().symbol()
        );
    }
    Ok(())
}

pub fn sanitize_live_error(error: &str) -> String {
    let compact = error
        .chars()
        .map(|c| if c.is_ascii_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_");
    compact.chars().take(180).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn z3_exit_policy_order_is_stable() {
        let policy = Z3ExitPolicy;
        assert_eq!(policy.exit_reason(10, 0.80, 0.0), Some("hard_stop"));
        assert_eq!(
            policy.exit_reason(120, 1.10, 24.9),
            Some("early_no_momentum")
        );
        assert_eq!(policy.exit_reason(60, 1.00, 20.0), Some("breakeven_floor"));
        assert_eq!(policy.exit_reason(60, 1.15, 30.0), Some("profit_protect"));
        assert_eq!(policy.exit_reason(300, 1.30, 29.0), Some("max_hold"));
        assert_eq!(policy.exit_reason(30, 1.25, 25.0), None);
    }

    #[test]
    fn blocker_and_terminal_classification_are_conservative() {
        let holding = PositionState {
            variant_id: "Z3".into(),
            status: STATUS_HOLDING.into(),
            remaining_tokens: 1,
            ..Default::default()
        };
        assert!(is_open_live_blocker(&holding));
        assert!(!is_terminal_operational_state(&holding));

        let failed = PositionState {
            variant_id: "Z3".into(),
            status: STATUS_LIVE_FAILED.into(),
            remaining_tokens: 0,
            ..Default::default()
        };
        assert!(!is_open_live_blocker(&failed));
        assert!(is_terminal_operational_state(&failed));

        let retryable = PositionState {
            variant_id: "Z3".into(),
            status: STATUS_LIVE_SELL_FAILED_RETRYABLE.into(),
            remaining_tokens: 1,
            ..Default::default()
        };
        assert!(is_open_live_blocker(&retryable));
        assert!(!is_terminal_operational_state(&retryable));
    }

    #[test]
    fn lifecycle_metadata_is_additive_and_deterministic() {
        let mut state = PositionState {
            variant_id: "Z3".into(),
            mint: "Mint".into(),
            pool_state: "Pool".into(),
            status: STATUS_HOLDING.into(),
            ..Default::default()
        };
        apply_lifecycle_phase(&mut state, LifecyclePhase::Holding);
        assert_eq!(state.lifecycle_phase, "holding");
        assert_eq!(state.lifecycle_id, "Z3:Mint:Pool");
        state.status = STATUS_COMPLETED.into();
        mark_terminal(&mut state, "profit_protect");
        assert_eq!(state.lifecycle_phase, "completed");
        assert_eq!(state.terminal_reason, "profit_protect");
        assert!(state.rollback_required);
    }
}
