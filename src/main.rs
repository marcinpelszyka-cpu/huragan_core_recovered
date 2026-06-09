mod engine;
mod executor;
mod filter;
mod fresh_momentum;
mod helius_filter;
mod helius_log_scout;
mod liquidity_predictor;
mod live_buy;
mod live_env;
mod live_guards;
mod live_lifecycle;
mod live_recovery;
mod live_sell;
mod notifier;
mod paper_amm;
mod position_manager;
mod mint_audit;
mod recovery;
mod scout;
mod sniper_shadow;
mod state;
mod strategy;

use crate::engine::{MigrationTarget, QuoteAsset};
use crate::live_buy::{apply_live_entry_stability_state, live_position_state};
use crate::live_env::{env_bool, env_f64, env_u64};
use crate::live_guards::validate_live_start;
use crate::live_lifecycle::sanitize_live_error;
use crate::state::{LedgerManager, PositionState};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::signature::Keypair;
use std::env;
use std::sync::Arc;
use tokio::sync::mpsc;

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
    live_recovery::startup_recovery(&ledger)?;

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

    // Recovery: detect open live holdings. Recovery attempts are best-effort
    // and NEVER block the paper detection loop.
    match recovery::latest_open_live_holding(&ledger) {
        Ok(Some(holding)) => {
            println!(
                "LIVE HOLDING DETECTED: mint={} status={} tokens={}",
                holding.mint, holding.status, holding.remaining_tokens
            );
            if paper_mode {
                println!("📝 paper mode — live holding exists but recovery skipped");
            } else {
                match recovery::target_from_live_state(&holding) {
                    Ok(target) => {
                        let target_rec = target.clone();
                        let rpc_url_rec = rpc_url.clone();
                        let ledger_rec = ledger.clone();
                        let holding_rec = holding.clone();
                        tokio::spawn(async move {
                            let key_bs58 = match std::env::var("SOLANA_PRIVATE_KEY_BASE58") {
                                Ok(k) => k,
                                Err(_) => {
                                    eprintln!("⚠️ recovery sell skipped — no SOLANA_PRIVATE_KEY_BASE58");
                                    return;
                                }
                            };
                            let bytes = match bs58::decode(&key_bs58).into_vec() {
                                Ok(b) => b,
                                Err(e) => {
                                    eprintln!("⚠️ recovery sell: bs58 decode failed: {e}");
                                    return;
                                }
                            };
                            let payer_key = match Keypair::try_from(bytes.as_slice()) {
                                Ok(k) => k,
                                Err(e) => {
                                    eprintln!("⚠️ recovery sell: invalid key: {e}");
                                    return;
                                }
                            };
                            let rpc_inner = RpcClient::new(rpc_url_rec.clone());
                            let executor = executor::Executor::new(rpc_url_rec);
                            let mut state = holding_rec;
                            match crate::live_sell::run_z3_live_auto_sell_monitor(
                                &rpc_inner,
                                &executor,
                                &ledger_rec,
                                &target_rec,
                                &mut state,
                                &payer_key,
                            )
                            .await
                            {
                                Ok(()) => {
                                    println!("✅ recovery sell completed for {}", target_rec.mint);
                                }
                                Err(e) => {
                                    eprintln!(
                                        "⚠️ recovery sell failed for {}: {e}",
                                        target_rec.mint
                                    );
                                }
                            }
                        });
                        println!(
                            "🔄 recovery sell dispatched for {}, continuing to paper detection",
                            target.mint
                        );
                    }
                    Err(e) => {
                        eprintln!(
                            "⚠️ recovery target build failed for {}: {e}",
                            holding.mint
                        );
                    }
                }
            }
        }
        Ok(None) => { /* no open holding — normal start */ }
        Err(e) => {
            eprintln!("⚠️ recovery check error: {e}");
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

        // Mint authority audit — blocks tokens with active mint_authority or freeze_authority
        if target.is_amm() {
            match mint_audit::audit_target_mint(&rpc, &target).await {
                Ok(audit) if audit.passed => {
                    // Audit metadata will be filled into paper_entry/live rows
                    // by fill_state_fields when creating PositionState
                }
                Ok(audit) => {
                    let reason = audit.reason.clone();
                    println!(
                        "🔒 mint audit blocked {}: {}",
                        target.mint, reason
                    );
                    if paper_mode {
                        let quote_asset_mint = if target.quote_asset_mint.is_empty() {
                            target.base_mint.clone()
                        } else {
                            target.quote_asset_mint.clone()
                        };
                        let state = PositionState {
                            mint: target.mint.clone(),
                            status: "prelive_mint_audit_shadow".into(),
                            source: target.source.clone(),
                            pool_state: target.pool_state.clone(),
                            base_mint: target.base_mint.clone(),
                            quote_mint: target.quote_mint.clone(),
                            pool_base_token_account: target.pool_base_token_account.clone(),
                            pool_quote_token_account: target.pool_quote_token_account.clone(),
                            quote_asset_mint,
                            creator_address: target.creator.clone(),
                            creator_score: target.creator_score,
                            top10_holder_pct: target.top10_holder_pct,
                            curve_velocity_secs: target.curve_velocity_secs,
                            exit_reason: reason,
                            excluded_from_stats: true,
                            ..Default::default()
                        };
                        let _ = ledger.save_new_position(&state);
                    }
                    continue;
                }
                Err(e) => {
                    eprintln!(
                        "⚠️ mint audit RPC error for {}: {e}",
                        target.mint
                    );
                    if paper_mode {
                        let quote_asset_mint = if target.quote_asset_mint.is_empty() {
                            target.base_mint.clone()
                        } else {
                            target.quote_asset_mint.clone()
                        };
                        let state = PositionState {
                            mint: target.mint.clone(),
                            status: "prelive_mint_audit_shadow".into(),
                            source: target.source.clone(),
                            pool_state: target.pool_state.clone(),
                            base_mint: target.base_mint.clone(),
                            quote_mint: target.quote_mint.clone(),
                            pool_base_token_account: target.pool_base_token_account.clone(),
                            pool_quote_token_account: target.pool_quote_token_account.clone(),
                            quote_asset_mint,
                            creator_address: target.creator.clone(),
                            creator_score: target.creator_score,
                            top10_holder_pct: target.top10_holder_pct,
                            curve_velocity_secs: target.curve_velocity_secs,
                            exit_reason: format!("mint_audit_rpc_error:{e}"),
                            excluded_from_stats: true,
                            ..Default::default()
                        };
                        let _ = ledger.save_new_position(&state);
                    }
                    continue;
                }
            }
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

                live_buy::submit_live_buy_with_optional_diagnostic(
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

#[cfg(test)]
mod tests {
    use crate::live_guards::{
        diagnostic_already_used_for_pool, diagnostic_count_for_day, diagnostic_day_utc,
        helius_sender_submit_count_for_day, is_diagnostic_label,
        validate_helius_sender_daily_limit, validate_live_start,
        validate_onchain_diagnostic_allowed,
    };
    use crate::live_lifecycle::{
        latest_open_live_holding, sanitize_live_error, target_from_live_state, z3_live_exit_reason,
    };
    use crate::live_sell::rescue_sell_bps_list_from_env_value;
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
    fn live_start_allows_helius_sender_backend_with_valid_tip() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_live_env();
        set_required_canary_env();
        std::env::set_var("LIVE_SEND_ENABLED", "true");
        std::env::set_var("LIVE_AUTO_SELL_ENABLED", "true");
        std::env::set_var("LIVE_SELL_SEND_ENABLED", "true");
        std::env::set_var("LIVE_SEND_BACKEND", "helius_sender");
        std::env::set_var(
            "HELIUS_SENDER_ENDPOINT",
            "https://sender.helius-rpc.com/fast?swqos_only=true",
        );
        std::env::set_var("HELIUS_SENDER_TIP_LAMPORTS", "5000");
        std::env::set_var("HELIUS_SENDER_MAX_PER_DAY", "2");

        validate_live_start(false, true).unwrap();
        clear_live_env();
    }

    #[test]
    fn live_start_blocks_unknown_send_backend() {
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
    fn helius_sender_daily_limit_counts_buy_and_sell_submits() {
        let today = diagnostic_day_utc();
        let rows = vec![
            PositionState {
                live_send_backend: "helius_sender".into(),
                live_send_day: today.clone(),
                tx_signature: "buy_sig".into(),
                sell_signature: "sell_sig".into(),
                ..Default::default()
            },
            PositionState {
                live_send_backend: "rpc".into(),
                live_send_day: today.clone(),
                tx_signature: "ignored".into(),
                ..Default::default()
            },
        ];
        assert_eq!(helius_sender_submit_count_for_day(&rows, &today), 2);
    }

    #[test]
    fn helius_sender_daily_limit_blocks_at_configured_max() {
        let _guard = ENV_LOCK.lock().unwrap();
        clear_live_env();
        std::env::set_var("HELIUS_SENDER_MAX_PER_DAY", "2");
        let path = std::env::temp_dir().join(format!(
            "huragan_sender_limit_test_{}.jsonl",
            uuid::Uuid::new_v4()
        ));
        let ledger = LedgerManager::new(&path);
        ledger
            .save_new_position(&PositionState {
                live_send_backend: "helius_sender".into(),
                live_send_day: diagnostic_day_utc(),
                tx_signature: "buy_sig".into(),
                sell_signature: "sell_sig".into(),
                ..Default::default()
            })
            .unwrap();
        let err = validate_helius_sender_daily_limit(&ledger)
            .unwrap_err()
            .to_string();
        assert!(err.contains("HELIUS_SENDER_MAX_PER_DAY exceeded"));
        let _ = std::fs::remove_file(path);
        clear_live_env();
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
                diagnostic_day: diagnostic_day_utc(),
                ..Default::default()
            })
            .unwrap();
        ledger
            .save_new_position(&PositionState {
                mint: "B".into(),
                pool_state: "B".into(),
                diagnostic_label: "RPC_PREFLIGHT_FALSE_REJECTION".into(),
                diagnostic_day: diagnostic_day_utc(),
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
            "HELIUS_SENDER_ENDPOINT",
            "HELIUS_SENDER_TIP_LAMPORTS",
            "HELIUS_SENDER_MAX_PER_DAY",
            "HELIUS_SENDER_CU_LIMIT",
            "HELIUS_SENDER_CU_PRICE_MICRO_LAMPORTS",
        ] {
            std::env::remove_var(key);
        }
    }
}
