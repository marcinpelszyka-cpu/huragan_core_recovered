use crate::engine::{self, MigrationTarget};
use crate::executor::{self, TxTerminalStatus};
use crate::live_env::{env_bool, env_u64};
use crate::live_lifecycle::{
    apply_lifecycle_phase, ensure_wsol_live_target, mark_terminal, sanitize_live_error,
    LifecyclePhase, LiveExitPolicy, Z3ExitPolicy,
};
use crate::notifier;
use crate::state::{LedgerManager, PositionState};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::signature::Keypair;
use std::env;
use std::time::{Duration, Instant};
use tokio::time::sleep;

pub async fn run_z3_live_auto_sell_monitor(
    rpc: &RpcClient,
    executor: &executor::Executor,
    ledger: &LedgerManager,
    target: &MigrationTarget,
    state: &mut PositionState,
    payer: &Keypair,
) -> anyhow::Result<()> {
    if state.variant_id != "Z3" {
        anyhow::bail!("live auto-sell only supports Z3, got {}", state.variant_id);
    }
    ensure_wsol_live_target(target)?;
    let interval_ms = env_u64("LIVE_SELL_CHECK_INTERVAL_MS", 1000);
    let max_hold_secs = 300u64;
    let started = Instant::now();
    let mut highest = 1.0f64;
    let mut lowest = 1.0f64;
    let mut last_value_sol = state.cost_basis_sol;

    println!(
        "👀 LIVE AUTO-SELL MONITOR START: mint={} cost_basis_sol={:.9}",
        state.mint, state.cost_basis_sol
    );

    loop {
        let age = started.elapsed().as_secs();
        let token_balance = match engine::live_sell_user_token_balance(rpc, target, payer).await {
            Ok(balance) => balance,
            Err(e) => {
                if age >= max_hold_secs {
                    return execute_live_sell(
                        rpc,
                        executor,
                        ledger,
                        target,
                        state,
                        payer,
                        "price_unavailable",
                        age,
                        last_value_sol,
                        highest,
                        lowest,
                        Some(format!("token_balance_unavailable:{e}")),
                    )
                    .await;
                }
                sleep(Duration::from_millis(interval_ms)).await;
                continue;
            }
        };
        state.remaining_tokens = token_balance;
        if token_balance == 0 {
            state.status = "completed".into();
            state.exit_reason = "token_balance_zero".into();
            state.live_exit_reason = "token_balance_zero".into();
            state.hold_secs = age;
            state.remaining_tokens = 0;
            mark_terminal(state, "token_balance_zero");
            ledger.save_new_position(state)?;
            println!(
                "✅ LIVE SELL CONFIRMED: {} | already_empty=true",
                state.mint
            );
            notifier::send_telegram_alert(format!(
                "✅ HURAGAN Z3 SELL COMPLETED\nmint={}\nreason=token_balance_zero\nremaining_tokens=0",
                state.mint
            ))
            .await;
            return Ok(());
        }

        if let Ok((quote_reserve, _token_reserve)) = engine::pool_reserves(rpc, target).await {
            state.quote_reserve_raw = quote_reserve;
            state.quote_reserve_ui = quote_reserve as f64 / 1e9;
            state.min_quote_reserve_raw = if state.min_quote_reserve_raw == 0 {
                quote_reserve
            } else {
                state.min_quote_reserve_raw.min(quote_reserve)
            };
        }

        let current_value_sol = match engine::quote_sell_amm(rpc, target, token_balance).await {
            Ok(lamports) if lamports > 0 => lamports as f64 / 1e9,
            Ok(_) | Err(_) => {
                if age >= max_hold_secs {
                    return execute_live_sell(
                        rpc,
                        executor,
                        ledger,
                        target,
                        state,
                        payer,
                        "price_unavailable",
                        age,
                        last_value_sol,
                        highest,
                        lowest,
                        None,
                    )
                    .await;
                }
                sleep(Duration::from_millis(interval_ms)).await;
                continue;
            }
        };
        last_value_sol = current_value_sol;
        let ratio = if state.cost_basis_sol > 0.0 {
            current_value_sol / state.cost_basis_sol
        } else {
            0.0
        };
        highest = highest.max(ratio);
        lowest = lowest.min(ratio);
        let max_favorable_pct = (highest - 1.0) * 100.0;

        if let Some(reason) = Z3ExitPolicy.exit_reason(age, ratio, max_favorable_pct) {
            return execute_live_sell(
                rpc,
                executor,
                ledger,
                target,
                state,
                payer,
                reason,
                age,
                current_value_sol,
                highest,
                lowest,
                None,
            )
            .await;
        }

        sleep(Duration::from_millis(interval_ms)).await;
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn execute_live_sell(
    rpc: &RpcClient,
    executor: &executor::Executor,
    ledger: &LedgerManager,
    target: &MigrationTarget,
    state: &mut PositionState,
    payer: &Keypair,
    reason: &str,
    age: u64,
    current_value_sol: f64,
    highest: f64,
    lowest: f64,
    preknown_error: Option<String>,
) -> anyhow::Result<()> {
    let token_balance = match engine::live_sell_user_token_balance(rpc, target, payer).await {
        Ok(balance) => balance,
        Err(e) => {
            let detail = preknown_error.unwrap_or_else(|| format!("token_balance_unavailable:{e}"));
            return save_live_sell_failed(
                ledger,
                state,
                reason,
                age,
                current_value_sol,
                highest,
                lowest,
                "",
                &detail,
            )
            .await;
        }
    };
    if token_balance == 0 {
        state.status = "completed".into();
        state.remaining_tokens = 0;
        state.exit_reason = reason.into();
        state.live_exit_reason = reason.into();
        state.hold_secs = age;
        mark_terminal(state, reason);
        ledger.save_new_position(state)?;
        println!(
            "✅ LIVE SELL CONFIRMED: {} | already_empty=true",
            state.mint
        );
        notifier::send_telegram_alert(format!(
            "✅ HURAGAN Z3 SELL COMPLETED\nmint={}\nreason=token_balance_zero\nremaining_tokens=0",
            state.mint
        ))
        .await;
        return Ok(());
    }

    let mut sell =
        match engine::build_sell_amm_ixs_real_preflight(rpc, target, token_balance, payer).await {
            Ok(plan) => plan,
            Err(e) => {
                return save_live_sell_failed(
                    ledger,
                    state,
                    reason,
                    age,
                    current_value_sol,
                    highest,
                    lowest,
                    "",
                    &format!("build_sell_failed:{e}"),
                )
                .await;
            }
        };
    state.live_sell_family = sell.instruction_family.clone();
    if let Err(e) = sell.simulate_preflight(rpc, payer).await {
        let standard_error = sanitize_live_error(&format!("sell_preflight_failed:{e}"));
        match build_first_passing_rescue_sell(rpc, target, token_balance, payer, &standard_error)
            .await
        {
            Ok(Some((rescue_sell, rescue_bps))) => {
                sell = rescue_sell;
                state.live_sell_family =
                    format!("{}:rescue_bps_{}", sell.instruction_family, rescue_bps);
                notifier::send_telegram_alert(format!(
                    "🛠 HURAGAN RESCUE SELL PREFLIGHT OK\nmint={}\nreason={}\nrescue_bps={}\nmin_sol_out={}\nstandard_error={}",
                    state.mint, reason, rescue_bps, sell.min_sol_out, standard_error
                ))
                .await;
            }
            Ok(None) => {
                return save_live_sell_failed(
                    ledger,
                    state,
                    "rescue_preflight_failed",
                    age,
                    current_value_sol,
                    highest,
                    lowest,
                    "",
                    &standard_error,
                )
                .await;
            }
            Err(rescue_error) => {
                return save_live_sell_failed(
                    ledger,
                    state,
                    "rescue_preflight_failed",
                    age,
                    current_value_sol,
                    highest,
                    lowest,
                    "",
                    &format!("{standard_error};rescue_error:{rescue_error}"),
                )
                .await;
            }
        }
    }
    if !env_bool("LIVE_SELL_SEND_ENABLED", false) {
        println!(
            "🛡️ LIVE SELL PREFLIGHT: {} | reason={} send=SEND_DISABLED",
            state.mint, reason
        );
        return Ok(());
    }

    match executor
        .send_with_preflight(payer, &sell.instructions)
        .await
    {
        Ok(sig) => {
            println!(
                "🚀 LIVE SELL SUBMITTED: {} | sig={} reason={} tokens={}",
                state.mint, sig, reason, token_balance
            );
            match executor.wait_terminal_status(&sig, 10).await? {
                TxTerminalStatus::Confirmed => {
                    state.status = "completed".into();
                    state.remaining_tokens = 0;
                    state.sell_signature = sig.to_string();
                    state.sell_attempt_no = state.sell_attempt_no.saturating_add(1).max(1);
                    if let Ok((quote_reserve, _token_reserve)) =
                        engine::pool_reserves(rpc, target).await
                    {
                        state.quote_reserve_raw = quote_reserve;
                        state.quote_reserve_ui = quote_reserve as f64 / 1e9;
                        state.exit_quote_reserve_raw = quote_reserve;
                        state.exit_quote_reserve_ui = state.quote_reserve_ui;
                        state.min_quote_reserve_raw = if state.min_quote_reserve_raw == 0 {
                            quote_reserve
                        } else {
                            state.min_quote_reserve_raw.min(quote_reserve)
                        };
                    }
                    state.live_exit_sol = sell.expected_sol_out as f64 / 1e9;
                    state.paper_exit_sol = state.live_exit_sol;
                    state.realized_pnl_sol = state.live_exit_sol - state.cost_basis_sol;
                    state.gross_pnl_sol = state.realized_pnl_sol;
                    state.net_pnl_sol = state.realized_pnl_sol;
                    state.net_pnl_pct = if state.cost_basis_sol > 0.0 {
                        state.realized_pnl_sol / state.cost_basis_sol * 100.0
                    } else {
                        0.0
                    };
                    let final_reason = if state.live_sell_family.contains(":rescue_bps_") {
                        format!("rescue_sell:{reason}")
                    } else {
                        reason.into()
                    };
                    state.exit_reason = final_reason;
                    state.live_exit_reason = reason.into();
                    state.hold_secs = age;
                    mark_terminal(state, reason);
                    state.max_favorable_pct = (highest - 1.0) * 100.0;
                    state.max_drawdown_pct = (lowest - 1.0) * 100.0;
                    if state.live_sell_family.is_empty()
                        || !state.live_sell_family.contains(":rescue_bps_")
                    {
                        state.live_sell_family = sell.instruction_family.clone();
                    }
                    ledger.save_new_position(state)?;
                    println!(
                        "✅ LIVE SELL CONFIRMED: {} | sig={} reason={} exit_sol={:.9}",
                        state.mint, sig, reason, state.live_exit_sol
                    );
                    notifier::send_telegram_alert(format!(
                        "✅ HURAGAN Z3 CANARY COMPLETED
mint={}
buy_sig={}
sell_sig={}
reason={}
exit_sol={:.9}
pnl_sol={:+.9}
pnl_pct={:+.2}%
remaining_tokens=0",
                        state.mint,
                        state.tx_signature,
                        sig,
                        reason,
                        state.live_exit_sol,
                        state.realized_pnl_sol,
                        state.net_pnl_pct
                    ))
                    .await;
                    Ok(())
                }
                TxTerminalStatus::Failed(err) => {
                    save_live_sell_failed(
                        ledger,
                        state,
                        reason,
                        age,
                        current_value_sol,
                        highest,
                        lowest,
                        &sig.to_string(),
                        &format!("sell_confirm_failed:{err}"),
                    )
                    .await
                }
                TxTerminalStatus::TimeoutUnknown => {
                    save_live_sell_failed(
                        ledger,
                        state,
                        reason,
                        age,
                        current_value_sol,
                        highest,
                        lowest,
                        &sig.to_string(),
                        &format!("sell_confirm_timeout_unknown:{sig}"),
                    )
                    .await
                }
            }
        }
        Err(e) => {
            save_live_sell_failed(
                ledger,
                state,
                reason,
                age,
                current_value_sol,
                highest,
                lowest,
                "",
                &format!("sell_submit_failed:{e}"),
            )
            .await
        }
    }
}

pub fn rescue_sell_bps_list() -> Vec<u64> {
    rescue_sell_bps_list_from_env_value(env::var("AMM_LIVE_SELL_RESCUE_BPS_LIST").ok().as_deref())
}

pub fn rescue_sell_bps_list_from_env_value(value: Option<&str>) -> Vec<u64> {
    let parsed: Vec<u64> = value
        .unwrap_or("7000,5000,3000,1000,100")
        .split(',')
        .filter_map(|part| part.trim().parse::<u64>().ok())
        .filter(|bps| *bps > 0)
        .map(|bps| bps.min(10_000))
        .collect();
    if parsed.is_empty() {
        vec![7000, 5000, 3000, 1000, 100]
    } else {
        parsed
    }
}

pub async fn build_first_passing_rescue_sell(
    rpc: &RpcClient,
    target: &MigrationTarget,
    token_balance: u64,
    payer: &Keypair,
    standard_error: &str,
) -> anyhow::Result<Option<(engine::BuiltSellPlan, u64)>> {
    notifier::send_telegram_alert(format!(
        "🚨 HURAGAN RESCUE SELL NEEDED\nmint={}\ntokens={}\nstandard_error={}",
        target.mint, token_balance, standard_error
    ))
    .await;
    let mut last_error = String::new();
    for rescue_bps in rescue_sell_bps_list() {
        println!(
            "🛠️ LIVE SELL RESCUE PREFLIGHT: {} | bps={} tokens={}",
            target.mint, rescue_bps, token_balance
        );
        let mut plan = match engine::build_sell_amm_ixs_real_preflight_with_bps(
            rpc,
            target,
            token_balance,
            payer,
            rescue_bps,
        )
        .await
        {
            Ok(plan) => plan,
            Err(e) => {
                last_error = sanitize_live_error(&format!("build_rescue_sell_failed:{e}"));
                continue;
            }
        };
        match plan.simulate_preflight(rpc, payer).await {
            Ok(()) => return Ok(Some((plan, rescue_bps))),
            Err(e) => {
                last_error = sanitize_live_error(&format!("rescue_sell_preflight_failed:{e}"));
            }
        }
    }
    if !last_error.is_empty() {
        println!(
            "❌ LIVE SELL RESCUE PREFLIGHT EXHAUSTED: {} | detail={}",
            target.mint, last_error
        );
    }
    Ok(None)
}

#[allow(clippy::too_many_arguments)]
pub async fn save_live_sell_failed(
    ledger: &LedgerManager,
    state: &mut PositionState,
    reason: &str,
    age: u64,
    current_value_sol: f64,
    highest: f64,
    lowest: f64,
    sell_signature: &str,
    detail: &str,
) -> anyhow::Result<()> {
    let detail = sanitize_live_error(detail);
    state.status = "live_sell_failed_retryable".into();
    state.sell_attempt_no = state.sell_attempt_no.saturating_add(1).max(1);
    state.exit_reason = format!("{reason}:{detail}");
    state.live_exit_reason = reason.into();
    state.live_exit_sol = current_value_sol;
    state.paper_exit_sol = current_value_sol;
    state.sell_signature = sell_signature.into();
    state.hold_secs = age;
    state.max_favorable_pct = (highest - 1.0) * 100.0;
    state.max_drawdown_pct = (lowest - 1.0) * 100.0;
    apply_lifecycle_phase(state, LifecyclePhase::LiveSellFailedRetryable);
    state.terminal_reason = format!("{reason}:{detail}");
    state.rollback_required = true;
    ledger.save_new_position(state)?;
    println!(
        "❌ LIVE SELL FAILED: {} | reason={} detail={}",
        state.mint, reason, detail
    );
    notifier::send_telegram_alert(format!(
        "🚨 HURAGAN LIVE SELL FAILED\nmint={}\nreason={}\ndetail={}\nstatus=live_sell_failed_retryable",
        state.mint, reason, detail
    ))
    .await;
    Ok(())
}
