use crate::engine::{self, MigrationTarget};
use crate::executor::{self, TxTerminalStatus};
use crate::live_env::{env_bool, env_u64};
use crate::live_guards::{diagnostic_day_utc, validate_onchain_diagnostic_allowed};
use crate::live_lifecycle::{
    apply_lifecycle_phase, mark_terminal, sanitize_live_error, LifecyclePhase,
};
use crate::live_sell::run_z3_live_auto_sell_monitor;
use crate::notifier;
use crate::state::{LedgerManager, PositionState};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::signature::{Keypair, Signature};
use std::env;

pub fn live_position_state(
    variant_id: &str,
    target: &MigrationTarget,
    plan: &engine::BuiltBuyPlan,
    gate: &engine::AdvancedGateDecision,
    status: &str,
    tx_signature: String,
    exit_reason: &str,
) -> PositionState {
    let failed = status == "live_failed";
    let live_send_day = if should_count_live_sender_attempt(status, &tx_signature) {
        diagnostic_day_utc()
    } else {
        String::new()
    };
    let mut state = PositionState {
        variant_id: variant_id.to_string(),
        mint: target.mint.clone(),
        tx_signature,
        total_tokens_bought: if failed { 0 } else { plan.expected_tokens_out },
        remaining_tokens: if failed { 0 } else { plan.expected_tokens_out },
        cost_basis_sol: if failed {
            0.0
        } else {
            plan.spend_lamports as f64 / 1e9
        },
        status: status.into(),
        source: target.source.clone(),
        pool_state: target.pool_state.clone(),
        base_mint: target.base_mint.clone(),
        quote_mint: target.quote_mint.clone(),
        quote_asset_mint: target.quote_asset_mint.clone(),
        quote_symbol: target.quote_asset().symbol().into(),
        quote_decimals: target.quote_asset().decimals(),
        pool_base_token_account: target.pool_base_token_account.clone(),
        pool_quote_token_account: target.pool_quote_token_account.clone(),
        paper_entry_sol: if failed {
            0.0
        } else {
            plan.spend_lamports as f64 / 1e9
        },
        paper_entry_quote: if failed {
            0.0
        } else {
            plan.spend_lamports as f64 / 1e9
        },
        quote_reserve_raw: plan.entry_quote_reserve_raw,
        quote_reserve_ui: plan.entry_quote_reserve_raw as f64 / 1e9,
        entry_quote_reserve_raw: plan.entry_quote_reserve_raw,
        min_quote_reserve_raw: plan.entry_quote_reserve_raw,
        paper_buy_family: plan.instruction_family.clone(),
        live_send_backend: live_send_backend_label(),
        live_send_day,
        sender_endpoint_mode: live_sender_endpoint_mode_label(),
        sender_tip_lamports: env_u64("HELIUS_SENDER_TIP_LAMPORTS", 5_000),
        sender_cu_limit: env_u64("HELIUS_SENDER_CU_LIMIT", 250_000).clamp(50_000, 1_400_000) as u32,
        sender_cu_price_micro_lamports: env_u64("HELIUS_SENDER_CU_PRICE_MICRO_LAMPORTS", 200_000),
        advanced_gate_passed: gate.passed,
        advanced_gate_reason: gate.reason.clone(),
        advanced_gate_mode: gate.mode.clone(),
        exit_reason: exit_reason.into(),
        excluded_from_stats: failed,
        ..Default::default()
    };
    state.lifecycle_id = crate::live_lifecycle::lifecycle_id(
        variant_id,
        &target.mint,
        &target.pool_state,
        &state.tx_signature,
    );
    apply_lifecycle_phase(&mut state, crate::live_lifecycle::phase_for_status(status));
    state.buy_attempt_no = if state.tx_signature.is_empty() && failed {
        0
    } else {
        1
    };
    if failed {
        mark_terminal(&mut state, exit_reason);
    }
    state
}

pub fn apply_live_entry_stability_state(
    state: &mut PositionState,
    stability: &engine::LiveEntryStabilityDecision,
) {
    state.min_quote_reserve_raw = if state.min_quote_reserve_raw == 0 {
        stability.min_quote_reserve_raw
    } else if stability.min_quote_reserve_raw == 0 {
        state.min_quote_reserve_raw
    } else {
        state
            .min_quote_reserve_raw
            .min(stability.min_quote_reserve_raw)
    };
    state.quote_reserve_raw = stability
        .samples
        .last()
        .map(|s| s.quote_reserve_raw)
        .unwrap_or(state.quote_reserve_raw);
    state.quote_reserve_ui = state.quote_reserve_raw as f64 / 1e9;
}

pub fn should_count_live_sender_attempt(status: &str, tx_signature: &str) -> bool {
    live_send_backend_label() == "helius_sender"
        && !tx_signature.is_empty()
        && matches!(
            status,
            "holding" | "live_failed" | "completed" | "live_sell_failed_retryable"
        )
}

pub fn live_send_backend_label() -> String {
    env::var("LIVE_SEND_BACKEND").unwrap_or_else(|_| "rpc".into())
}

pub fn live_sender_endpoint_mode_label() -> String {
    if live_send_backend_label() != "helius_sender" {
        return String::new();
    }
    let endpoint = env::var("HELIUS_SENDER_ENDPOINT")
        .unwrap_or_else(|_| "https://sender.helius-rpc.com/fast?swqos_only=true".into());
    executor::helius_sender_endpoint_mode(&endpoint)
        .as_str()
        .into()
}

pub async fn submit_live_buy_with_optional_diagnostic(
    rpc: &RpcClient,
    executor: &executor::Executor,
    ledger: &LedgerManager,
    target: &MigrationTarget,
    fresh_plan: &engine::BuiltBuyPlan,
    gate: &engine::AdvancedGateDecision,
    stability: &engine::LiveEntryStabilityDecision,
    payer_ref: &Keypair,
    live_variant: &str,
    buy_lamports: u64,
) -> anyhow::Result<()> {
    match executor
        .send_with_preflight(payer_ref, &fresh_plan.instructions)
        .await
    {
        Ok(sig) => {
            handle_live_buy_signature(
                rpc,
                executor,
                ledger,
                target,
                fresh_plan,
                gate,
                stability,
                payer_ref,
                live_variant,
                sig,
                false,
            )
            .await?;
        }
        Err(first_err) => {
            let first_err_text = first_err.to_string();
            if !executor::is_preflight_6004_error(&first_err_text) {
                save_live_failed(
                    ledger,
                    live_variant,
                    target,
                    fresh_plan,
                    gate,
                    stability,
                    String::new(),
                    &first_err_text,
                    "",
                );
                return Ok(());
            }

            println!(
                "⚠️ LIVE PREFLIGHT 6004: {} | rebuilding once before diagnostic",
                target.mint
            );
            let second_plan = match engine::process_migration_and_build_amm_ixs(
                rpc,
                target,
                buy_lamports,
                Some(payer_ref),
                false,
            )
            .await
            {
                Ok(plan) => plan,
                Err(e) => {
                    save_live_failed(
                        ledger,
                        live_variant,
                        target,
                        fresh_plan,
                        gate,
                        stability,
                        String::new(),
                        &format!("live_entry_second_rebuild_failed:{e}"),
                        "",
                    );
                    return Ok(());
                }
            };
            println!(
                "🔁 LIVE ENTRY REBUILT SECOND: {} | previous_expected={} second_expected={} second_min={}",
                target.mint,
                fresh_plan.expected_tokens_out,
                second_plan.expected_tokens_out,
                second_plan.min_tokens_out
            );

            match executor
                .send_with_preflight(payer_ref, &second_plan.instructions)
                .await
            {
                Ok(sig) => {
                    handle_live_buy_signature(
                        rpc,
                        executor,
                        ledger,
                        target,
                        &second_plan,
                        gate,
                        stability,
                        payer_ref,
                        live_variant,
                        sig,
                        false,
                    )
                    .await?;
                }
                Err(second_err) => {
                    let second_err_text = second_err.to_string();
                    if !executor::is_preflight_6004_error(&second_err_text) {
                        save_live_failed(
                            ledger,
                            live_variant,
                            target,
                            &second_plan,
                            gate,
                            stability,
                            String::new(),
                            &second_err_text,
                            "",
                        );
                        return Ok(());
                    }

                    if let Err(reason) = validate_onchain_diagnostic_allowed(ledger, target) {
                        let label = if reason == "diagnostic_daily_limit_reached" {
                            "diagnostic_daily_limit_reached"
                        } else {
                            ""
                        };
                        save_live_failed(
                            ledger,
                            live_variant,
                            target,
                            &second_plan,
                            gate,
                            stability,
                            String::new(),
                            &reason,
                            label,
                        );
                        return Ok(());
                    }

                    println!(
                        "🧪 ONCHAIN_DIAGNOSTIC_TEST QUALIFIED: {} | reason=double_preflight_6004",
                        target.mint
                    );
                    match executor
                        .send_onchain_diagnostic_skip_preflight(
                            payer_ref,
                            &second_plan.instructions,
                            "double_preflight_6004",
                        )
                        .await
                    {
                        Ok(sig) => {
                            handle_live_buy_signature(
                                rpc,
                                executor,
                                ledger,
                                target,
                                &second_plan,
                                gate,
                                stability,
                                payer_ref,
                                live_variant,
                                sig,
                                true,
                            )
                            .await?;
                        }
                        Err(e) => {
                            save_live_failed(
                                ledger,
                                live_variant,
                                target,
                                &second_plan,
                                gate,
                                stability,
                                String::new(),
                                &format!("pool_level_rejected:{e}"),
                                "POOL_LEVEL_REJECTED",
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

pub async fn handle_live_buy_signature(
    rpc: &RpcClient,
    executor: &executor::Executor,
    ledger: &LedgerManager,
    target: &MigrationTarget,
    plan: &engine::BuiltBuyPlan,
    gate: &engine::AdvancedGateDecision,
    stability: &engine::LiveEntryStabilityDecision,
    payer_ref: &Keypair,
    live_variant: &str,
    sig: Signature,
    diagnostic: bool,
) -> anyhow::Result<()> {
    println!(
        "🚀 LIVE SUBMITTED: {} | sig={} tokens={} diagnostic={}",
        target.mint, sig, plan.expected_tokens_out, diagnostic
    );
    match executor.wait_terminal_status(&sig, 10).await? {
        TxTerminalStatus::Confirmed => {
            let exit_reason = if diagnostic {
                "rpc_preflight_false_rejection"
            } else {
                ""
            };
            let mut state = live_position_state(
                live_variant,
                target,
                plan,
                gate,
                "holding",
                sig.to_string(),
                exit_reason,
            );
            apply_live_entry_stability_state(&mut state, stability);
            if diagnostic {
                mark_diagnostic(&mut state, "RPC_PREFLIGHT_FALSE_REJECTION");
            }
            apply_lifecycle_phase(&mut state, LifecyclePhase::Holding);
            if let Err(e) = ledger.save_new_position(&state) {
                eprintln!(
                    "⚠️ LIVE STATE SAVE FAILED for {} sig={}: {e}",
                    target.mint, sig
                );
            }
            println!(
                "✅ LIVE CONFIRMED: {} | sig={} tokens={} diagnostic={}",
                target.mint, sig, plan.expected_tokens_out, diagnostic
            );
            println!("📝 LIVE POSITION SAVED: {} holding", target.mint);
            notifier::send_telegram_alert(format!(
                "✅ HURAGAN Z3 BUY CONFIRMED\nmint={}\nbuy_sig={}\ntokens={}\ncost_sol={:.9}\nauto_sell={}\ndiagnostic={}",
                target.mint,
                sig,
                plan.expected_tokens_out,
                plan.spend_lamports as f64 / 1e9,
                env_bool("LIVE_AUTO_SELL_ENABLED", false),
                diagnostic
            ))
            .await;
            if env_bool("LIVE_AUTO_SELL_ENABLED", false) {
                let mut live_state = state;
                run_z3_live_auto_sell_monitor(
                    rpc,
                    executor,
                    ledger,
                    target,
                    &mut live_state,
                    payer_ref,
                )
                .await?;
            }
        }
        TxTerminalStatus::Failed(err) => {
            let reason = if diagnostic {
                format!("pool_level_rejected:{err}")
            } else {
                format!("transaction_failed:{err}")
            };
            save_live_failed(
                ledger,
                live_variant,
                target,
                plan,
                gate,
                stability,
                sig.to_string(),
                &reason,
                if diagnostic {
                    "POOL_LEVEL_REJECTED"
                } else {
                    ""
                },
            );
        }
        TxTerminalStatus::TimeoutUnknown => {
            save_live_failed(
                ledger,
                live_variant,
                target,
                plan,
                gate,
                stability,
                sig.to_string(),
                &format!("confirmation_timeout_unknown:{sig}"),
                if diagnostic {
                    "ONCHAIN_DIAGNOSTIC_TEST"
                } else {
                    ""
                },
            );
        }
    }
    Ok(())
}

pub fn save_live_failed(
    ledger: &LedgerManager,
    live_variant: &str,
    target: &MigrationTarget,
    plan: &engine::BuiltBuyPlan,
    gate: &engine::AdvancedGateDecision,
    stability: &engine::LiveEntryStabilityDecision,
    tx_signature: String,
    reason: &str,
    diagnostic_label: &str,
) {
    let reason = sanitize_live_error(reason);
    let mut state = live_position_state(
        live_variant,
        target,
        plan,
        gate,
        "live_failed",
        tx_signature.clone(),
        &reason,
    );
    apply_live_entry_stability_state(&mut state, stability);
    if !diagnostic_label.is_empty() {
        mark_diagnostic(&mut state, diagnostic_label);
    }
    if let Err(save_err) = ledger.save_new_position(&state) {
        eprintln!(
            "⚠️ LIVE FAILED STATE SAVE FAILED for {} sig={}: {save_err}",
            target.mint, tx_signature
        );
    }
    println!(
        "❌ LIVE FAILED: {} | sig={} reason={} diagnostic_label={}",
        target.mint,
        if tx_signature.is_empty() {
            "<none>"
        } else {
            &tx_signature
        },
        reason,
        diagnostic_label
    );
}

pub fn mark_diagnostic(state: &mut PositionState, label: &str) {
    state.diagnostic_label = label.to_string();
    state.diagnostic_day = diagnostic_day_utc();
}
