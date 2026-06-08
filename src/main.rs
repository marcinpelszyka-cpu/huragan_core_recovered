mod engine;
mod executor;
mod filter;
mod fresh_momentum;
mod helius_filter;
mod helius_log_scout;
mod liquidity_predictor;
mod notifier;
mod paper_amm;
mod position_manager;
mod scout;
mod sniper_shadow;
mod state;
mod strategy;

use crate::engine::{MigrationTarget, QuoteAsset};
use crate::executor::TxTerminalStatus;
use crate::state::{LedgerManager, PositionState};
use chrono::Utc;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::signature::{Keypair, Signature};
use std::env;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio::time::sleep;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();
    if env::var("FRESH_MOMENTUM_CAPTURE").unwrap_or_default() == "only" {
        return fresh_momentum::run_fresh_momentum_daemon().await;
    }
    if env::var("SNIPER_SHADOW_CAPTURE").unwrap_or_default() == "only" {
        return sniper_shadow::run_sniper_shadow_daemon().await;
    }

    let paper_mode = env_bool("PAPER_MODE", true);
    let live_armed = env_bool("LIVE_ARMED", false);
    validate_live_start(paper_mode, live_armed)?;

    let rpc_url = env::var("RPC_URL").unwrap_or_else(|_| "http://127.0.0.1:8899".into());
    let ledger = Arc::new(LedgerManager::default());
    startup_recovery(&ledger)?;

    let (tx, mut rx) = mpsc::channel::<MigrationTarget>(2048);
    if env_bool("HELIUS_MIGRATION_ENABLED", true) {
        tokio::spawn(helius_log_scout::run_helius_log_scout(tx.clone()));
    }
    if env_bool("PUMPPORTAL_ENABLED", false) {
        tokio::spawn(scout::run_pumpportal_scout(tx.clone()));
    }

    let evaluator = strategy::StrategyEvaluator::new();
    let live_variant = env::var("LIVE_VARIANT").unwrap_or_else(|_| "Z".into());
    let buy_lamports = (env_f64("BUY_AMOUNT_SOL", 0.003) * 1e9) as u64;
    let max_trades = env_u64("MAX_TRADES_PER_RUN", 1);
    let rpc = RpcClient::new(rpc_url.clone());
    let mut trades_seen = 0u64;

    // Load wallet key only when explicitly allowed. Plaintext private keys in .env are
    // unsafe after a server-side key leak; paper/shadow modes must never require one.
    let live_send = env_bool("LIVE_SEND_ENABLED", false);
    let allow_plaintext_key = env_bool("ALLOW_PLAINTEXT_PRIVATE_KEY", false);
    if (live_send || live_armed) && !allow_plaintext_key {
        anyhow::bail!(
            "live mode blocked: set ALLOW_PLAINTEXT_PRIVATE_KEY=true only after rotating wallet and accepting server key risk"
        );
    }
    let payer: Option<Keypair> = if !paper_mode && allow_plaintext_key {
        let key_bs58 = env::var("SOLANA_PRIVATE_KEY_BASE58")
            .map_err(|_| anyhow::anyhow!("SOLANA_PRIVATE_KEY_BASE58 required for live mode"))?;
        let bytes = bs58::decode(&key_bs58).into_vec()?;
        Some(Keypair::try_from(bytes.as_slice()).map_err(|e| anyhow::anyhow!("invalid key: {e}"))?)
    } else {
        None
    };

    println!("🧬 huragan_core recovered boot | paper_mode={paper_mode} live_armed={live_armed} live_send={live_send} variants=F/I/Z/Z2/Z3/Z3.1");

    if !paper_mode {
        if let Some(mut open) = latest_open_live_holding(&ledger)? {
            if !env_bool("LIVE_AUTO_SELL_ENABLED", false) {
                anyhow::bail!(
                    "AMM CANARY BLOCKED: open live holding {} requires LIVE_AUTO_SELL_ENABLED=true",
                    open.mint
                );
            }
            let payer_ref = payer
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("open live holding requires payer key"))?;
            let target = target_from_live_state(&open)?;
            let executor = executor::Executor::new(rpc_url.clone());
            println!(
                "🔄 LIVE AUTO-SELL RESUME: mint={} remaining_tokens={}",
                open.mint, open.remaining_tokens
            );
            notifier::send_telegram_alert(format!(
                "⚠️ HURAGAN LIVE RECOVERY\nopen holding detected\nmint={}\nremaining_tokens={}\naction=auto_sell_resume",
                open.mint, open.remaining_tokens
            ))
            .await;
            run_z3_live_auto_sell_monitor(&rpc, &executor, &ledger, &target, &mut open, payer_ref)
                .await?;
            return Ok(());
        }
    }

    while let Some(mut target) = rx.recv().await {
        if filter::static_filter(&target).is_err() {
            continue;
        }
        let pred = liquidity_predictor::score(&target).await;
        target.creator_score = target.creator_score.max(pred.creator_score);
        target.top10_holder_pct = target.top10_holder_pct.max(pred.top10_holder_pct);
        target.curve_velocity_secs = target.curve_velocity_secs.max(pred.curve_velocity_secs);

        let gate = engine::advanced_amm_safety_gate(&target);
        if !paper_mode && !gate.passed {
            println!("⛔ advanced gate blocked {}: {}", target.mint, gate.reason);
            continue;
        }

        // Quote-aware guard: only WSOL-quoted pools are tradeable today.
        // USDC (and any other quote mint) is detected, recorded as shadow and
        // excluded from stats — no paper/live trade until quote-aware AMM math.
        let quote_asset = target.quote_asset();
        if target.is_amm() && quote_asset != QuoteAsset::Wsol {
            record_quote_unsupported_shadow(&ledger, &target, quote_asset);
            println!(
                "🟡 quote-unsupported shadow ({}) recorded, skipping trade for {}",
                quote_asset.symbol(),
                target.mint
            );
            continue;
        }

        if paper_mode {
            if !target.is_amm() {
                continue;
            }
            // Fresh pool vaults may not be visible to RPC immediately.
            // Small delay avoids "could not find account" on first quote.
            let entry_delay = env_u64("AMM_PAPER_ENTRY_DELAY_SECS", 8);
            if entry_delay > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(entry_delay)).await;
            }
            let plan = match engine::process_migration_and_build_amm_ixs(
                &rpc,
                &target,
                buy_lamports,
                None,
                false,
            )
            .await
            {
                Ok(p) => p,
                Err(e) => {
                    println!("paper plan skip {}: {e}", target.mint);
                    continue;
                }
            };
            for variant in evaluator.variants() {
                paper_amm::spawn_lifecycle(
                    rpc_url.clone(),
                    ledger.clone(),
                    target.clone(),
                    variant.clone(),
                    plan.spend_lamports as f64 / 1e9,
                    plan.expected_tokens_out,
                );
            }
            continue;
        }

        // Live path: build real instructions, preflight-only until LIVE_SEND_ENABLED
        if !paper_mode
            && target.is_amm()
            && target.source == "helius_migration"
            && evaluator.variant(&live_variant).is_some()
        {
            let payer_ref = match payer.as_ref() {
                Some(k) => k,
                None => {
                    println!("⛔ LIVE SKIP {}: no payer key loaded", target.mint);
                    continue;
                }
            };

            let plan = match engine::process_migration_and_build_amm_ixs(
                &rpc,
                &target,
                buy_lamports,
                payer.as_ref(),
                true,
            )
            .await
            {
                Ok(p) => p,
                Err(e) => {
                    println!("⛔ LIVE SKIP {}: {e}", target.mint);
                    continue;
                }
            };

            if plan.instructions.is_empty() {
                anyhow::bail!("LIVE BLOCKED: empty instructions for {}", target.mint);
            }

            if plan.simulation_ok && live_send {
                // Final reserve gate directly before submit. A pool can pass the
                // initial quote/preflight checks and still be drained before the
                // live transaction is sent. Re-checking the WSOL reserve here
                // prevents paying fees into already-dusted/rugged pools.
                if let Err(e) = engine::check_pool_sol_gate(&rpc, &target).await {
                    let reason = sanitize_live_error(&format!("pool_sol_final_gate_blocked:{e}"));
                    let state = live_position_state(
                        &live_variant,
                        &target,
                        &plan,
                        &gate,
                        "live_failed",
                        String::new(),
                        &reason,
                    );
                    if let Err(save_err) = ledger.save_new_position(&state) {
                        eprintln!(
                            "⚠️ LIVE FINAL GATE STATE SAVE FAILED for {}: {save_err}",
                            target.mint
                        );
                    }
                    println!(
                        "⛔ LIVE FINAL GATE BLOCKED: {} | reason={}",
                        target.mint, reason
                    );
                    notifier::send_telegram_alert(format!(
                        "⛔ HURAGAN LIVE FINAL GATE BLOCKED\nmint={}\nreason={}",
                        target.mint, reason
                    ))
                    .await;
                    trades_seen += 1;
                    if trades_seen >= max_trades {
                        break;
                    }
                    continue;
                }

                let stability = match engine::live_entry_stability_gate(&rpc, &target, buy_lamports)
                    .await
                {
                    Ok(decision) => decision,
                    Err(e) => {
                        let reason = sanitize_live_error(&format!("live_entry_unstable_pool:{e}"));
                        let state = live_position_state(
                            &live_variant,
                            &target,
                            &plan,
                            &gate,
                            "live_failed",
                            String::new(),
                            &reason,
                        );
                        if let Err(save_err) = ledger.save_new_position(&state) {
                            eprintln!(
                                "⚠️ LIVE ENTRY GATE STATE SAVE FAILED for {}: {save_err}",
                                target.mint
                            );
                        }
                        println!(
                            "⛔ LIVE ENTRY GATE BLOCKED: {} | reason={}",
                            target.mint, reason
                        );
                        notifier::send_telegram_alert(format!(
                            "⛔ HURAGAN LIVE ENTRY GATE BLOCKED\nmint={}\nreason={}",
                            target.mint, reason
                        ))
                        .await;
                        trades_seen += 1;
                        if trades_seen >= max_trades {
                            break;
                        }
                        continue;
                    }
                };
                if !stability.passed {
                    let reason = sanitize_live_error(&format!(
                        "{}:min_reserve={}:reserve_drop_bps={}:quote_drop_bps={}",
                        stability.reason,
                        stability.min_quote_reserve_raw,
                        stability.max_quote_reserve_drop_bps,
                        stability.max_buy_quote_drop_bps
                    ));
                    let mut state = live_position_state(
                        &live_variant,
                        &target,
                        &plan,
                        &gate,
                        "live_failed",
                        String::new(),
                        &reason,
                    );
                    apply_live_entry_stability_state(&mut state, &stability);
                    if let Err(save_err) = ledger.save_new_position(&state) {
                        eprintln!(
                            "⚠️ LIVE ENTRY GATE STATE SAVE FAILED for {}: {save_err}",
                            target.mint
                        );
                    }
                    println!(
                        "⛔ LIVE ENTRY GATE BLOCKED: {} | reason={} samples={:?}",
                        target.mint, reason, stability.samples
                    );
                    notifier::send_telegram_alert(format!(
                        "⛔ HURAGAN LIVE ENTRY GATE BLOCKED\nmint={}\nreason={}",
                        target.mint, reason
                    ))
                    .await;
                    trades_seen += 1;
                    if trades_seen >= max_trades {
                        break;
                    }
                    continue;
                }
                println!(
                    "✅ LIVE ENTRY GATE OK: {} | min_reserve={} reserve_drop_bps={} quote_drop_bps={}",
                    target.mint,
                    stability.min_quote_reserve_raw,
                    stability.max_quote_reserve_drop_bps,
                    stability.max_buy_quote_drop_bps
                );

                // The anti-rug gate intentionally waits ~2s and samples reserves.
                // That makes the original buy plan stale in fast pools: min_out can be
                // based on a quote from before the gate and fail with ExceededSlippage
                // at RPC preflight. Rebuild and re-simulate immediately after the gate,
                // then submit only the fresh transaction.
                let fresh_plan = match engine::process_migration_and_build_amm_ixs(
                    &rpc,
                    &target,
                    buy_lamports,
                    payer.as_ref(),
                    true,
                )
                .await
                {
                    Ok(p) => p,
                    Err(e) => {
                        let reason = sanitize_live_error(&format!("live_entry_rebuild_failed:{e}"));
                        let mut state = live_position_state(
                            &live_variant,
                            &target,
                            &plan,
                            &gate,
                            "live_failed",
                            String::new(),
                            &reason,
                        );
                        apply_live_entry_stability_state(&mut state, &stability);
                        if let Err(save_err) = ledger.save_new_position(&state) {
                            eprintln!(
                                "⚠️ LIVE ENTRY REBUILD STATE SAVE FAILED for {}: {save_err}",
                                target.mint
                            );
                        }
                        println!(
                            "⛔ LIVE ENTRY REBUILD FAILED: {} | reason={}",
                            target.mint, reason
                        );
                        notifier::send_telegram_alert(format!(
                            "⛔ HURAGAN LIVE ENTRY REBUILD FAILED\nmint={}\nreason={}",
                            target.mint, reason
                        ))
                        .await;
                        trades_seen += 1;
                        if trades_seen >= max_trades {
                            break;
                        }
                        continue;
                    }
                };
                if !fresh_plan.simulation_ok {
                    let reason = "live_entry_rebuild_preflight_failed".to_string();
                    let mut state = live_position_state(
                        &live_variant,
                        &target,
                        &fresh_plan,
                        &gate,
                        "live_failed",
                        String::new(),
                        &reason,
                    );
                    apply_live_entry_stability_state(&mut state, &stability);
                    if let Err(save_err) = ledger.save_new_position(&state) {
                        eprintln!(
                            "⚠️ LIVE ENTRY REBUILD PREFLIGHT STATE SAVE FAILED for {}: {save_err}",
                            target.mint
                        );
                    }
                    println!(
                        "⛔ LIVE ENTRY REBUILD PREFLIGHT FAILED: {} | expected={} min={}",
                        target.mint, fresh_plan.expected_tokens_out, fresh_plan.min_tokens_out
                    );
                    notifier::send_telegram_alert(format!(
                        "⛔ HURAGAN LIVE ENTRY REBUILD PREFLIGHT FAILED\nmint={}\nexpected={}\nmin={}",
                        target.mint, fresh_plan.expected_tokens_out, fresh_plan.min_tokens_out
                    ))
                    .await;
                    trades_seen += 1;
                    if trades_seen >= max_trades {
                        break;
                    }
                    continue;
                }
                println!(
                    "🔁 LIVE ENTRY REBUILT: {} | old_expected={} fresh_expected={} fresh_min={}",
                    target.mint,
                    plan.expected_tokens_out,
                    fresh_plan.expected_tokens_out,
                    fresh_plan.min_tokens_out
                );

                let executor = executor::Executor::new(rpc_url.clone());

                submit_live_buy_with_optional_diagnostic(
                    &rpc,
                    &executor,
                    &ledger,
                    &target,
                    &fresh_plan,
                    &gate,
                    &stability,
                    payer_ref,
                    &live_variant,
                    buy_lamports,
                )
                .await?;

                // A real-send attempt consumes the canary slot regardless of success/failure.
                // This prevents systemd/on-failure loops from submitting another canary.
                trades_seen += 1;
            } else {
                // Preflight-only: log but don't save fake state, don't count as trade
                let send_status = if live_send {
                    "SEND_READY"
                } else {
                    "SEND_DISABLED"
                };
                println!(
                    "🛡️ LIVE PREFLIGHT: {} | sim={} send={}",
                    target.mint, plan.simulation_ok, send_status
                );
                // NO fake holding state saved — deliberate
                // NO trades_seen increment — preflight doesn't consume trade slot
            }
        }
        if trades_seen >= max_trades {
            break;
        }
    }

    Ok(())
}

async fn submit_live_buy_with_optional_diagnostic(
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

async fn handle_live_buy_signature(
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

fn save_live_failed(
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

fn mark_diagnostic(state: &mut PositionState, label: &str) {
    state.diagnostic_label = label.to_string();
    state.diagnostic_day = diagnostic_day_utc();
}

fn diagnostic_day_utc() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

fn live_onchain_diagnostic_max_per_day() -> usize {
    env_u64("LIVE_ONCHAIN_DIAGNOSTIC_MAX_PER_DAY", 2).clamp(0, 10) as usize
}

fn validate_onchain_diagnostic_allowed(
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

fn diagnostic_count_for_day(rows: &[PositionState], day: &str) -> usize {
    rows.iter()
        .filter(|r| r.diagnostic_day == day && is_diagnostic_label(&r.diagnostic_label))
        .count()
}

fn diagnostic_already_used_for_pool(rows: &[PositionState], target: &MigrationTarget) -> bool {
    rows.iter().any(|r| {
        is_diagnostic_label(&r.diagnostic_label)
            && (r.mint == target.mint
                || (!target.pool_state.is_empty() && r.pool_state == target.pool_state))
    })
}

fn is_diagnostic_label(label: &str) -> bool {
    matches!(
        label,
        "ONCHAIN_DIAGNOSTIC_TEST" | "RPC_PREFLIGHT_FALSE_REJECTION" | "POOL_LEVEL_REJECTED"
    )
}

fn validate_live_start(paper_mode: bool, live_armed: bool) -> anyhow::Result<()> {
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
        if env::var("LIVE_SEND_BACKEND").unwrap_or_else(|_| "rpc".into()) != "rpc" {
            anyhow::bail!("AMM CANARY BLOCKED: LIVE_SEND_BACKEND must be rpc in this build");
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

fn live_position_state(
    variant_id: &str,
    target: &MigrationTarget,
    plan: &engine::BuiltBuyPlan,
    gate: &engine::AdvancedGateDecision,
    status: &str,
    tx_signature: String,
    exit_reason: &str,
) -> PositionState {
    let failed = status == "live_failed";
    PositionState {
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
        advanced_gate_passed: gate.passed,
        advanced_gate_reason: gate.reason.clone(),
        advanced_gate_mode: gate.mode.clone(),
        exit_reason: exit_reason.into(),
        excluded_from_stats: failed,
        ..Default::default()
    }
}

fn apply_live_entry_stability_state(
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

fn sanitize_live_error(error: &str) -> String {
    let compact = error
        .chars()
        .map(|c| if c.is_ascii_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("_");
    compact.chars().take(180).collect()
}

async fn run_z3_live_auto_sell_monitor(
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
    if target.quote_asset() != QuoteAsset::Wsol {
        anyhow::bail!(
            "live auto-sell only supports WSOL quote, got {}",
            target.quote_asset().symbol()
        );
    }
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

        if let Some(reason) = z3_live_exit_reason(age, ratio, max_favorable_pct) {
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
async fn execute_live_sell(
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

fn rescue_sell_bps_list() -> Vec<u64> {
    rescue_sell_bps_list_from_env_value(env::var("AMM_LIVE_SELL_RESCUE_BPS_LIST").ok().as_deref())
}

fn rescue_sell_bps_list_from_env_value(value: Option<&str>) -> Vec<u64> {
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

async fn build_first_passing_rescue_sell(
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
async fn save_live_sell_failed(
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
    state.exit_reason = format!("{reason}:{detail}");
    state.live_exit_reason = reason.into();
    state.live_exit_sol = current_value_sol;
    state.paper_exit_sol = current_value_sol;
    state.sell_signature = sell_signature.into();
    state.hold_secs = age;
    state.max_favorable_pct = (highest - 1.0) * 100.0;
    state.max_drawdown_pct = (lowest - 1.0) * 100.0;
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

fn z3_live_exit_reason(age_secs: u64, ratio: f64, max_favorable_pct: f64) -> Option<&'static str> {
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

fn latest_open_live_holding(ledger: &LedgerManager) -> anyhow::Result<Option<PositionState>> {
    let latest = ledger.get_latest_by_mint_variant()?;
    Ok(latest.into_values().find(|p| {
        p.variant_id == "Z3"
            && matches!(p.status.as_str(), "holding" | "live_sell_failed_retryable")
            && p.remaining_tokens > 0
    }))
}

fn target_from_live_state(state: &PositionState) -> anyhow::Result<MigrationTarget> {
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

fn record_quote_unsupported_shadow(
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

fn startup_recovery(ledger: &LedgerManager) -> anyhow::Result<()> {
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

fn env_bool(key: &str, default: bool) -> bool {
    env::var(key).map(|v| v == "true").unwrap_or(default)
}
fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
fn env_f64(key: &str, default: f64) -> f64 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::{
        diagnostic_already_used_for_pool, diagnostic_count_for_day, is_diagnostic_label,
        latest_open_live_holding, rescue_sell_bps_list_from_env_value, sanitize_live_error,
        target_from_live_state, validate_live_start, validate_onchain_diagnostic_allowed,
        z3_live_exit_reason,
    };
    use crate::state::{LedgerManager, PositionState};
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn live_error_sanitizer_is_single_line_and_bounded() {
        let raw = "transaction failed:\nSome(InstructionError(4, Custom(6004)))";
        let sanitized = sanitize_live_error(raw);
        assert!(!sanitized.contains('\n'));
        assert!(sanitized.len() <= 180);
        assert!(sanitized.contains("InstructionError"));
    }

    #[test]
    fn z3_live_exit_reasons_match_canary_policy() {
        assert_eq!(z3_live_exit_reason(10, 0.80, 0.0), Some("hard_stop"));
        assert_eq!(
            z3_live_exit_reason(120, 1.10, 24.9),
            Some("early_no_momentum")
        );
        assert_eq!(z3_live_exit_reason(60, 1.00, 20.0), Some("breakeven_floor"));
        assert_eq!(z3_live_exit_reason(60, 1.15, 30.0), Some("profit_protect"));
        assert_eq!(z3_live_exit_reason(60, 1.349, 60.0), Some("profit_protect"));
        assert_eq!(
            z3_live_exit_reason(60, 1.599, 100.0),
            Some("profit_protect")
        );
        assert_eq!(
            z3_live_exit_reason(60, 1.899, 150.0),
            Some("profit_protect")
        );
        assert_eq!(z3_live_exit_reason(300, 1.30, 29.0), Some("max_hold"));
        assert_eq!(z3_live_exit_reason(30, 1.25, 25.0), None);
    }

    #[test]
    fn live_start_blocks_buy_only_live_without_auto_sell_flags() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_live_env();
        set_required_canary_env();
        std::env::set_var("LIVE_SEND_ENABLED", "true");
        std::env::set_var("LIVE_AUTO_SELL_ENABLED", "false");
        std::env::set_var("LIVE_SELL_SEND_ENABLED", "false");

        let err = validate_live_start(false, true).unwrap_err().to_string();
        assert!(err.contains("LIVE_AUTO_SELL_ENABLED"));
        clear_live_env();
    }

    #[test]
    fn live_start_accepts_full_lifecycle_canary_flags() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_live_env();
        set_required_canary_env();
        std::env::set_var("LIVE_SEND_ENABLED", "true");
        std::env::set_var("LIVE_AUTO_SELL_ENABLED", "true");
        std::env::set_var("LIVE_SELL_SEND_ENABLED", "true");

        validate_live_start(false, true).unwrap();
        clear_live_env();
    }

    #[test]
    fn live_start_blocks_future_non_rpc_backend() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_live_env();
        set_required_canary_env();
        std::env::set_var("LIVE_SEND_ENABLED", "true");
        std::env::set_var("LIVE_AUTO_SELL_ENABLED", "true");
        std::env::set_var("LIVE_SELL_SEND_ENABLED", "true");
        std::env::set_var("LIVE_SEND_BACKEND", "pumpportal_lightning_later");

        let err = validate_live_start(false, true).unwrap_err().to_string();
        assert!(err.contains("LIVE_SEND_BACKEND"));
        clear_live_env();
    }

    #[test]
    fn live_send_error_classifier_detects_pump_amm_exceeded_slippage() {
        let err = "RPC response error -32002: Transaction simulation failed: {\"InstructionError\":[3,{\"Custom\":6004}]}";
        assert!(crate::executor::is_preflight_6004_error(err));
        assert!(crate::executor::is_preflight_6004_error("ExceededSlippage"));
        assert!(!crate::executor::is_preflight_6004_error("Custom\\\":6005"));
    }

    #[test]
    fn rescue_sell_bps_parser_uses_safe_defaults_and_clamps() {
        assert_eq!(
            rescue_sell_bps_list_from_env_value(Some("7000,5000,100")),
            vec![7000, 5000, 100]
        );
        assert_eq!(
            rescue_sell_bps_list_from_env_value(Some("bad,0,")),
            vec![7000, 5000, 3000, 1000, 100]
        );
        assert_eq!(
            rescue_sell_bps_list_from_env_value(Some("12000,1")),
            vec![10_000, 1]
        );
        assert_eq!(
            rescue_sell_bps_list_from_env_value(None),
            vec![7000, 5000, 3000, 1000, 100]
        );
    }

    #[test]
    fn target_from_live_state_normalizes_source_for_recovery_sell() {
        let target = target_from_live_state(&PositionState {
            variant_id: "Z3".into(),
            mint: "Mint".into(),
            status: "holding".into(),
            source: "live".into(),
            pool_state: "Pool".into(),
            base_mint: "So11111111111111111111111111111111111111112".into(),
            quote_mint: "Mint".into(),
            quote_asset_mint: "So11111111111111111111111111111111111111112".into(),
            pool_base_token_account: "BaseVault".into(),
            pool_quote_token_account: "TokenVault".into(),
            remaining_tokens: 10,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(target.source, "helius_migration");
        assert!(target.is_amm());
    }

    #[test]
    fn latest_open_live_holding_ignores_live_failed() {
        let path = std::env::temp_dir().join(format!(
            "huragan_live_holding_test_{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let ledger = LedgerManager::new(&path);
        ledger
            .save_new_position(&PositionState {
                variant_id: "Z3".into(),
                mint: "FailedMint".into(),
                status: "live_failed".into(),
                remaining_tokens: 0,
                ..Default::default()
            })
            .unwrap();
        assert!(latest_open_live_holding(&ledger).unwrap().is_none());

        ledger
            .save_new_position(&PositionState {
                variant_id: "Z3".into(),
                mint: "OpenMint".into(),
                status: "live_sell_failed_retryable".into(),
                remaining_tokens: 10,
                ..Default::default()
            })
            .unwrap();
        let open = latest_open_live_holding(&ledger).unwrap().unwrap();
        assert_eq!(open.mint, "OpenMint");
        assert_eq!(open.status, "live_sell_failed_retryable");
        let _ = std::fs::remove_file(path);
    }

    fn diagnostic_target() -> crate::engine::MigrationTarget {
        crate::engine::MigrationTarget {
            mint: "DiagMint".into(),
            source: "helius_migration".into(),
            pool_state: "DiagPool".into(),
            base_mint: "So11111111111111111111111111111111111111112".into(),
            quote_mint: "DiagMint".into(),
            quote_asset_mint: "So11111111111111111111111111111111111111112".into(),
            pool_base_token_account: "BaseVault".into(),
            pool_quote_token_account: "TokenVault".into(),
            ..Default::default()
        }
    }

    #[test]
    fn diagnostic_label_helpers_count_daily_and_pool_usage() {
        let rows = vec![
            PositionState {
                mint: "DiagMint".into(),
                pool_state: "DiagPool".into(),
                diagnostic_label: "RPC_PREFLIGHT_FALSE_REJECTION".into(),
                diagnostic_day: "2026-06-08".into(),
                ..Default::default()
            },
            PositionState {
                mint: "OtherMint".into(),
                pool_state: "OtherPool".into(),
                diagnostic_label: "POOL_LEVEL_REJECTED".into(),
                diagnostic_day: "2026-06-08".into(),
                ..Default::default()
            },
            PositionState {
                mint: "Ignored".into(),
                diagnostic_label: "".into(),
                diagnostic_day: "2026-06-08".into(),
                ..Default::default()
            },
        ];
        assert!(is_diagnostic_label("ONCHAIN_DIAGNOSTIC_TEST"));
        assert_eq!(diagnostic_count_for_day(&rows, "2026-06-08"), 2);
        assert!(diagnostic_already_used_for_pool(
            &rows,
            &diagnostic_target()
        ));
    }

    #[test]
    fn diagnostic_guard_requires_flag_and_daily_limit() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_live_env();
        set_required_canary_env();
        std::env::set_var("PAPER_MODE", "false");
        std::env::set_var("LIVE_ARMED", "true");
        std::env::set_var("LIVE_SEND_ENABLED", "true");
        std::env::set_var("LIVE_AUTO_SELL_ENABLED", "true");
        std::env::set_var("LIVE_SELL_SEND_ENABLED", "true");
        std::env::set_var("LIVE_ONCHAIN_DIAGNOSTIC_MAX_PER_DAY", "2");

        let path = std::env::temp_dir().join(format!(
            "huragan_diag_guard_test_{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let ledger = LedgerManager::new(&path);
        let target = diagnostic_target();
        assert_eq!(
            validate_onchain_diagnostic_allowed(&ledger, &target).unwrap_err(),
            "diagnostic_disabled"
        );

        std::env::set_var("LIVE_ONCHAIN_DIAGNOSTIC_ENABLED", "true");
        validate_onchain_diagnostic_allowed(&ledger, &target).unwrap();
        ledger
            .save_new_position(&PositionState {
                mint: "A".into(),
                pool_state: "A".into(),
                diagnostic_label: "POOL_LEVEL_REJECTED".into(),
                diagnostic_day: super::diagnostic_day_utc(),
                ..Default::default()
            })
            .unwrap();
        ledger
            .save_new_position(&PositionState {
                mint: "B".into(),
                pool_state: "B".into(),
                diagnostic_label: "RPC_PREFLIGHT_FALSE_REJECTION".into(),
                diagnostic_day: super::diagnostic_day_utc(),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            validate_onchain_diagnostic_allowed(&ledger, &target).unwrap_err(),
            "diagnostic_daily_limit_reached"
        );
        let _ = std::fs::remove_file(path);
        clear_live_env();
        std::env::remove_var("PAPER_MODE");
        std::env::remove_var("LIVE_ARMED");
    }

    fn set_required_canary_env() {
        std::env::set_var("AMM_LIVE_CANARY", "true");
        std::env::set_var("HELIUS_MIGRATION_ENABLED", "true");
        std::env::set_var("PUMPPORTAL_ENABLED", "false");
        std::env::set_var("MIGRATION_CAPTURE_MODE", "false");
        std::env::set_var("MAX_TRADES_PER_RUN", "1");
        std::env::set_var("JITO_TIP_LAMPORTS", "0");
        std::env::set_var("EMERGENCY_JITO_TIP_LAMPORTS", "0");
        std::env::set_var("BUY_AMOUNT_SOL", "0.003");
        std::env::set_var("LIVE_VARIANT", "Z3");
    }

    fn clear_live_env() {
        for key in [
            "AMM_LIVE_CANARY",
            "HELIUS_MIGRATION_ENABLED",
            "PUMPPORTAL_ENABLED",
            "MIGRATION_CAPTURE_MODE",
            "MAX_TRADES_PER_RUN",
            "JITO_TIP_LAMPORTS",
            "EMERGENCY_JITO_TIP_LAMPORTS",
            "BUY_AMOUNT_SOL",
            "LIVE_VARIANT",
            "LIVE_SEND_ENABLED",
            "LIVE_AUTO_SELL_ENABLED",
            "LIVE_SELL_SEND_ENABLED",
            "LIVE_SEND_BACKEND",
            "LIVE_ONCHAIN_DIAGNOSTIC_ENABLED",
            "LIVE_ONCHAIN_DIAGNOSTIC_MAX_PER_DAY",
        ] {
            std::env::remove_var(key);
        }
    }
}
