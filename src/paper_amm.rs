use crate::engine::{self, MigrationTarget};
use crate::live_env::env_bool;
use crate::state::{LedgerManager, PositionState};
use crate::strategy::StrategyVariant;
use solana_client::nonblocking::rpc_client::RpcClient;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;

pub fn estimate_costs_sol(simulated_sell_count: u64) -> f64 {
    let base = env_lamports("PAPER_BASE_FEE_LAMPORTS", 5000);
    let prio = env_lamports("PAPER_PRIORITY_FEE_LAMPORTS", 0);
    let tip = env_lamports("PAPER_JITO_TIP_LAMPORTS", 0);
    ((base + prio + tip) * (1 + simulated_sell_count)) as f64 / 1e9
}

fn env_lamports(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

pub fn spawn_lifecycle(
    rpc_url: String,
    ledger: Arc<LedgerManager>,
    target: MigrationTarget,
    variant: StrategyVariant,
    entry_sol: f64,
    expected_tokens: u64,
) {
    tokio::spawn(async move {
        let quote_asset = target.quote_asset();
        let quote_asset_mint = if target.quote_asset_mint.is_empty() {
            target.base_mint.clone()
        } else {
            target.quote_asset_mint.clone()
        };
        let mut state = PositionState {
            variant_id: variant.id.to_string(),
            mint: target.mint.clone(),
            tx_signature: format!("PAPER_AMM_{}", uuid::Uuid::new_v4()),
            total_tokens_bought: expected_tokens,
            remaining_tokens: expected_tokens,
            cost_basis_sol: entry_sol,
            status: "paper_entry".into(),
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
            paper_entry_sol: entry_sol,
            paper_entry_quote: entry_sol,
            paper_buy_family: "buy_amm_shadow_recovered".into(),
            ..Default::default()
        };
        let gate = engine::advanced_amm_safety_gate(&target);
        state.advanced_gate_passed = gate.passed;
        state.advanced_gate_reason = gate.reason;
        state.advanced_gate_mode = gate.mode;
        let rpc = RpcClient::new(rpc_url);
        let entry_quote_reserve = quote_reserve_raw(&rpc, &target).await.unwrap_or(0);
        state.quote_reserve_raw = entry_quote_reserve;
        state.quote_reserve_ui =
            entry_quote_reserve as f64 / 10f64.powi(quote_asset.decimals() as i32);
        state.entry_quote_reserve_raw = entry_quote_reserve;
        state.min_quote_reserve_raw = entry_quote_reserve;
        let _ = ledger.save_new_position(&state);

        let started = Instant::now();
        let interval = std::env::var("PAPER_AMM_CHECK_INTERVAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1000);
        let mut highest = 1.0f64;
        let mut lowest = 1.0f64;
        let mut simulated_sells = 0u64;
        let mut last_value = 0.0f64;
        let mut partial_done = false;

        loop {
            let age = started.elapsed().as_secs();
            let sell =
                engine::build_sell_amm_ixs(&rpc, &target, state.remaining_tokens.max(1), false)
                    .await;
            let current = match sell {
                Ok(plan) => {
                    state.paper_sell_family = plan.instruction_family;
                    plan.expected_sol_out as f64 / 1e9
                }
                Err(_) => {
                    if age >= variant.max_hold_secs {
                        finalize(
                            &rpc,
                            &target,
                            &ledger,
                            &mut state,
                            "price_unavailable",
                            age,
                            last_value,
                            simulated_sells,
                            highest,
                            lowest,
                        )
                        .await;
                        return;
                    }
                    sleep(Duration::from_millis(interval)).await;
                    continue;
                }
            };
            if let Ok(reserve) = quote_reserve_raw(&rpc, &target).await {
                state.quote_reserve_raw = reserve;
                state.quote_reserve_ui = reserve as f64 / 10f64.powi(quote_asset.decimals() as i32);
                state.min_quote_reserve_raw = if state.min_quote_reserve_raw == 0 {
                    reserve
                } else {
                    state.min_quote_reserve_raw.min(reserve)
                };
            }
            last_value = current;
            let ratio = if entry_sol > 0.0 {
                current / entry_sol
            } else {
                0.0
            };
            highest = highest.max(ratio);
            lowest = lowest.min(ratio);
            let max_favorable_pct = (highest - 1.0) * 100.0;
            let current_pnl_pct = (ratio - 1.0) * 100.0;
            let drawdown_pct = (lowest - 1.0) * 100.0;

            if !partial_done
                && variant.take_profit_ratio < 100.0
                && ratio >= variant.take_profit_ratio
                && variant.partial_sell_bps > 0
            {
                simulated_sells += 1;
                partial_done = true;
                state.status = "paper_partial_sold".into();
                state.remaining_tokens = state
                    .remaining_tokens
                    .saturating_mul(10_000 - variant.partial_sell_bps)
                    .saturating_div(10_000);
                let _ = ledger.save_new_position(&state);
            }

            if z3_mfe_gate_exit(&variant, age, max_favorable_pct)
                || (variant.early_no_momentum_secs > 0
                    && age >= variant.early_no_momentum_secs
                    && ratio < variant.early_no_momentum_min_ratio)
            {
                finalize(
                    &rpc,
                    &target,
                    &ledger,
                    &mut state,
                    "early_no_momentum",
                    age,
                    current,
                    simulated_sells + 1,
                    highest,
                    lowest,
                )
                .await;
                return;
            }
            if variant.rug_guard_drawdown_pct > 0.0
                && drawdown_pct <= -variant.rug_guard_drawdown_pct
                && max_favorable_pct < variant.rug_guard_requires_mfe_below_pct
            {
                finalize(
                    &rpc,
                    &target,
                    &ledger,
                    &mut state,
                    "rug_guard",
                    age,
                    current,
                    simulated_sells + 1,
                    highest,
                    lowest,
                )
                .await;
                return;
            }
            if variant.breakeven_floor_after_mfe_pct > 0.0
                && max_favorable_pct >= variant.breakeven_floor_after_mfe_pct
                && ratio <= 1.0
            {
                finalize(
                    &rpc,
                    &target,
                    &ledger,
                    &mut state,
                    "breakeven_floor",
                    age,
                    current.max(entry_sol),
                    simulated_sells + 1,
                    highest,
                    lowest,
                )
                .await;
                return;
            }
            // TASK_07: V2 distribution exit — pump reversing after 50%+ MFE
            if env_bool("Z3_EXIT_POLICY_V2", false)
                && max_favorable_pct >= 50.0
                && current_pnl_pct < max_favorable_pct * 0.3
            {
                finalize(
                    &rpc,
                    &target,
                    &ledger,
                    &mut state,
                    "distribution",
                    age,
                    current,
                    simulated_sells + 1,
                    highest,
                    lowest,
                )
                .await;
                return;
            }
            if variant.stop_loss_ratio > 0.0 && ratio <= variant.stop_loss_ratio {
                finalize(
                    &rpc,
                    &target,
                    &ledger,
                    &mut state,
                    "hard_stop",
                    age,
                    current,
                    simulated_sells + 1,
                    highest,
                    lowest,
                )
                .await;
                return;
            }
            // TASK_07: V2 controlled pump exit.
            // Units: max_favorable_pct and current_pnl_pct are both percentages.
            // Example: MFE=30%, exit if current PnL < 21% (30 * 0.70).
            if env_bool("Z3_EXIT_POLICY_V2", false)
                && variant.id == "Z3"
                && max_favorable_pct >= 25.0
                && max_favorable_pct < 50.0
                && current_pnl_pct < max_favorable_pct * 0.70
            {
                finalize(
                    &rpc,
                    &target,
                    &ledger,
                    &mut state,
                    "controlled_pump_exit",
                    age,
                    current,
                    simulated_sells + 1,
                    highest,
                    lowest,
                )
                .await;
                return;
            }
            if z3_staged_profit_protect_exit(&variant, max_favorable_pct, current_pnl_pct) {
                finalize(
                    &rpc,
                    &target,
                    &ledger,
                    &mut state,
                    "profit_protect",
                    age,
                    current,
                    simulated_sells + 1,
                    highest,
                    lowest,
                )
                .await;
                return;
            }
            if variant.trailing_stop_pct > 0.0
                && highest >= variant.trailing_activation_ratio
                && ratio <= highest * (1.0 - variant.trailing_stop_pct / 100.0)
            {
                finalize(
                    &rpc,
                    &target,
                    &ledger,
                    &mut state,
                    "trailing_stop",
                    age,
                    current,
                    simulated_sells + 1,
                    highest,
                    lowest,
                )
                .await;
                return;
            }
            if age >= variant.max_hold_secs {
                finalize(
                    &rpc,
                    &target,
                    &ledger,
                    &mut state,
                    "max_hold",
                    age,
                    current,
                    simulated_sells + 1,
                    highest,
                    lowest,
                )
                .await;
                return;
            }
            sleep(Duration::from_millis(interval)).await;
        }
    });
}

fn z3_mfe_gate_exit(variant: &StrategyVariant, age_secs: u64, max_favorable_pct: f64) -> bool {
    // TASK_07: V2 policy uses stricter 10% threshold (was 25%)
    let threshold = if env_bool("Z3_EXIT_POLICY_V2", false) {
        10.0
    } else {
        25.0
    };
    variant.id == "Z3" && age_secs >= 120 && max_favorable_pct < threshold
}

fn z3_staged_profit_protect_exit(
    variant: &StrategyVariant,
    max_favorable_pct: f64,
    current_pnl_pct: f64,
) -> bool {
    if variant.id != "Z3" {
        return false;
    }
    const STAGES: &[(f64, f64)] = &[(150.0, 90.0), (100.0, 60.0), (60.0, 35.0), (30.0, 15.0)];
    STAGES.iter().any(|(mfe_threshold, stop_level)| {
        max_favorable_pct >= *mfe_threshold && current_pnl_pct <= *stop_level
    })
}

async fn quote_reserve_raw(rpc: &RpcClient, target: &MigrationTarget) -> anyhow::Result<u64> {
    let (quote_reserve, _) = engine::pool_reserves(rpc, target).await?;
    Ok(quote_reserve)
}

async fn finalize(
    rpc: &RpcClient,
    target: &MigrationTarget,
    ledger: &LedgerManager,
    state: &mut PositionState,
    reason: &str,
    age: u64,
    exit_sol: f64,
    simulated_sells: u64,
    highest: f64,
    lowest: f64,
) {
    if let Ok(reserve) = quote_reserve_raw(rpc, target).await {
        state.quote_reserve_raw = reserve;
        state.quote_reserve_ui = reserve as f64 / 10f64.powi(state.quote_decimals as i32);
        state.exit_quote_reserve_raw = reserve;
        state.exit_quote_reserve_ui = state.quote_reserve_ui;
        state.min_quote_reserve_raw = if state.min_quote_reserve_raw == 0 {
            reserve
        } else {
            state.min_quote_reserve_raw.min(reserve)
        };
    }
    let gross = exit_sol - state.paper_entry_sol;
    let costs = estimate_costs_sol(simulated_sells);
    let net = gross - costs;
    if !net.is_finite()
        || state.paper_entry_sol <= 0.0
        || (exit_sol > state.paper_entry_sol * 100.0)
    {
        state.excluded_from_stats = true;
        state.exit_reason = "invalid_quote".into();
    } else {
        state.exit_reason = reason.into();
    }
    state.exited_early_no_momentum = reason == "early_no_momentum";
    state.exited_rug_guard = reason == "rug_guard";
    state.exited_breakeven_floor = reason == "breakeven_floor";
    state.status = "paper_completed".into();
    state.paper_exit_sol = exit_sol;
    state.gross_pnl_sol = gross;
    state.estimated_costs_sol = costs;
    state.net_pnl_sol = net;
    state.net_pnl_pct = if state.paper_entry_sol > 0.0 {
        net / state.paper_entry_sol * 100.0
    } else {
        0.0
    };
    state.paper_exit_quote = exit_sol;
    state.net_pnl_quote = net;
    state.hold_secs = age;
    state.max_drawdown_pct = (lowest - 1.0) * 100.0;
    state.max_favorable_pct = (highest - 1.0) * 100.0;
    state.remaining_tokens = 0;
    let _ = ledger.save_new_position(state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::StrategyVariant;

    #[test]
    fn z3_mfe_gate_requires_120s_and_less_than_25pct_mfe() {
        let z3 = StrategyVariant::z3();
        assert!(!z3_mfe_gate_exit(&z3, 119, 24.9));
        assert!(!z3_mfe_gate_exit(&z3, 120, 25.0));
        assert!(z3_mfe_gate_exit(&z3, 120, 24.9));
        assert!(!z3_mfe_gate_exit(&StrategyVariant::z(), 120, 24.9));
    }

    #[test]
    fn z3_staged_profit_protection_thresholds() {
        let z3 = StrategyVariant::z3();
        assert!(!z3_staged_profit_protect_exit(&z3, 29.0, 10.0));
        assert!(z3_staged_profit_protect_exit(&z3, 30.0, 15.0));
        assert!(z3_staged_profit_protect_exit(&z3, 60.0, 35.0));
        assert!(z3_staged_profit_protect_exit(&z3, 100.0, 60.0));
        assert!(z3_staged_profit_protect_exit(&z3, 150.0, 90.0));
        assert!(!z3_staged_profit_protect_exit(&z3, 60.0, 35.1));
        assert!(!z3_staged_profit_protect_exit(
            &StrategyVariant::z(),
            150.0,
            90.0
        ));
    }
}
