use crate::engine::{MigrationTarget, QuoteAsset};
use crate::state::{LedgerManager, PositionState};

pub(crate) fn latest_open_live_holding(ledger: &LedgerManager) -> anyhow::Result<Option<PositionState>> {
    let latest = ledger.get_latest_by_mint_variant()?;
    Ok(latest.into_values().find(|p| {
        p.variant_id == "Z3"
            && matches!(p.status.as_str(), "holding" | "live_sell_failed_retryable")
            && p.remaining_tokens > 0
    }))
}

pub(crate) fn target_from_live_state(state: &PositionState) -> anyhow::Result<MigrationTarget> {
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

pub(crate) fn record_quote_unsupported_shadow(
    ledger: &LedgerManager,
    target: &MigrationTarget,
    quote_asset: QuoteAsset,
) {
    let quote_asset_mint = if target.quote_asset_mint.is_empty() {
        target.base_mint.clone()
    } else {
        target.quote_asset_mint.clone()
    };
    let state = PositionState {
        mint: target.mint.clone(),
        status: "quote_unsupported_shadow".into(),
        source: target.source.clone(),
        pool_state: target.pool_state.clone(),
        base_mint: target.base_mint.clone(),
        quote_mint: target.quote_mint.clone(),
        pool_base_token_account: target.pool_base_token_account.clone(),
        pool_quote_token_account: target.pool_quote_token_account.clone(),
        quote_asset_mint,
        quote_symbol: quote_asset.symbol().into(),
        quote_decimals: quote_asset.decimals(),
        creator_address: target.creator.clone(),
        creator_score: target.creator_score,
        top10_holder_pct: target.top10_holder_pct,
        curve_velocity_secs: target.curve_velocity_secs,
        exit_reason: "quote_mint_unsupported".into(),
        excluded_from_stats: true,
        ..Default::default()
    };
    let _ = ledger.save_new_position(&state);
}

pub(crate) fn startup_recovery(ledger: &LedgerManager) -> anyhow::Result<()> {
    let latest = ledger.get_latest_by_mint_variant()?;
    let mut stale = 0usize;
    for (_, mut p) in latest {
        if matches!(p.status.as_str(), "paper_entry" | "paper_partial_sold") {
            p.status = "paper_lifecycle_orphaned_restart".into();
            p.excluded_from_stats = true;
            ledger.save_new_position(&p)?;
            stale += 1;
        }
    }
    if stale > 0 {
        println!("🔄 recovery marked {stale} paper lifecycle entries as orphaned");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_open_live_holding_empty_ledger_returns_none() {
        let path = std::env::temp_dir().join(format!(
            "huragan_recovery_empty_test_{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let ledger = LedgerManager::new(&path);
        let result = latest_open_live_holding(&ledger).unwrap();
        assert!(result.is_none());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn latest_open_live_holding_finds_holding_position() {
        let path = std::env::temp_dir().join(format!(
            "huragan_recovery_holding_test_{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let ledger = LedgerManager::new(&path);
        ledger
            .save_new_position(&PositionState {
                variant_id: "Z3".into(),
                mint: "HoldingMint".into(),
                status: "holding".into(),
                remaining_tokens: 100,
                ..Default::default()
            })
            .unwrap();
        let result = latest_open_live_holding(&ledger).unwrap();
        assert!(result.is_some());
        let holding = result.unwrap();
        assert_eq!(holding.mint, "HoldingMint");
        assert_eq!(holding.status, "holding");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn target_from_live_state_complete_succeeds() {
        let state = PositionState {
            mint: "TestMint".into(),
            token_name: "TestToken".into(),
            token_symbol: "TEST".into(),
            pool_state: "TestPool".into(),
            base_mint: "BaseMint".into(),
            quote_mint: "QuoteMint".into(),
            quote_asset_mint: "QuoteAssetMint".into(),
            pool_base_token_account: "BaseVault".into(),
            pool_quote_token_account: "QuoteVault".into(),
            creator_address: "Creator".into(),
            creator_score: 50.0,
            top10_holder_pct: 25.0,
            curve_velocity_secs: 10,
            ..Default::default()
        };
        let target = target_from_live_state(&state).unwrap();
        assert_eq!(target.mint, "TestMint");
        assert_eq!(target.source, "helius_migration");
        assert_eq!(target.pool_state, "TestPool");
    }

    #[test]
    fn target_from_live_state_incomplete_errors() {
        let state = PositionState {
            mint: "TestMint".into(),
            pool_state: "".into(),
            base_mint: "BaseMint".into(),
            quote_mint: "QuoteMint".into(),
            quote_asset_mint: "QuoteAssetMint".into(),
            pool_base_token_account: "BaseVault".into(),
            pool_quote_token_account: "QuoteVault".into(),
            ..Default::default()
        };
        let result = target_from_live_state(&state);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("live_sell_target_incomplete"));
    }
}
