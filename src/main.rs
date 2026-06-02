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
mod state;
mod strategy;

use crate::engine::{MigrationTarget, QuoteAsset};
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

    // Load wallet key for live mode
    let live_send = env_bool("LIVE_SEND_ENABLED", false);
    let payer: Option<Keypair> = if !paper_mode {
        let key_bs58 = env::var("SOLANA_PRIVATE_KEY_BASE58")
            .map_err(|_| anyhow::anyhow!("SOLANA_PRIVATE_KEY_BASE58 required for live mode"))?;
        let bytes = bs58::decode(&key_bs58).into_vec()?;
        Some(Keypair::try_from(bytes.as_slice()).map_err(|e| anyhow::anyhow!("invalid key: {e}"))?)
    } else {
        None
    };

    println!("🧬 huragan_core recovered boot | paper_mode={paper_mode} live_armed={live_armed} live_send={live_send} variants=F/I/Z/Z2/Z3/Z3.1");

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
                // Send the transaction
                let executor = executor::Executor::new(rpc_url.clone());
                let sig = executor
                    .send_skip_preflight(payer_ref, &plan.instructions)
                    .await?;
                println!(
                    "🚀 LIVE SENT: {} | sig={} tokens={}",
                    target.mint, sig, plan.expected_tokens_out
                );

                // Wait for confirmation
                executor.wait_confirmed(&sig, 10).await?;

                let state = PositionState {
                    variant_id: live_variant.clone(),
                    mint: target.mint.clone(),
                    tx_signature: sig.to_string(),
                    total_tokens_bought: plan.expected_tokens_out,
                    remaining_tokens: plan.expected_tokens_out,
                    cost_basis_sol: plan.spend_lamports as f64 / 1e9,
                    status: "holding".into(),
                    source: target.source.clone(),
                    pool_state: target.pool_state.clone(),
                    base_mint: target.base_mint.clone(),
                    quote_mint: target.quote_mint.clone(),
                    quote_asset_mint: target.quote_asset_mint.clone(),
                    quote_symbol: target.quote_asset().symbol().into(),
                    quote_decimals: target.quote_asset().decimals(),
                    pool_base_token_account: target.pool_base_token_account.clone(),
                    pool_quote_token_account: target.pool_quote_token_account.clone(),
                    paper_entry_sol: plan.spend_lamports as f64 / 1e9,
                    paper_entry_quote: plan.spend_lamports as f64 / 1e9,
                    paper_buy_family: plan.instruction_family.clone(),
                    advanced_gate_passed: gate.passed,
                    advanced_gate_reason: gate.reason,
                    advanced_gate_mode: gate.mode,
                    ..Default::default()
                };
                ledger.save_new_position(&state)?;
                println!("📝 LIVE POSITION SAVED: {} holding", target.mint);
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
