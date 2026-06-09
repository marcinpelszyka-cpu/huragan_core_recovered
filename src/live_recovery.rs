use crate::executor;
use crate::live_env::env_bool;
use crate::live_lifecycle::{latest_open_live_holding, target_from_live_state};
use crate::live_sell::run_z3_live_auto_sell_monitor;
use crate::notifier;
use crate::state::LedgerManager;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::signature::Keypair;

pub fn startup_recovery(ledger: &LedgerManager) -> anyhow::Result<()> {
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

pub async fn resume_open_live_holding_if_needed(
    rpc: &RpcClient,
    rpc_url: &str,
    ledger: &LedgerManager,
    payer: Option<&Keypair>,
) -> anyhow::Result<bool> {
    let Some(mut open) = latest_open_live_holding(ledger)? else {
        return Ok(false);
    };
    if !env_bool("LIVE_AUTO_SELL_ENABLED", false) {
        anyhow::bail!(
            "AMM CANARY BLOCKED: open live holding {} requires LIVE_AUTO_SELL_ENABLED=true",
            open.mint
        );
    }
    let payer_ref = payer.ok_or_else(|| anyhow::anyhow!("open live holding requires payer key"))?;
    let target = target_from_live_state(&open)?;
    let executor = executor::Executor::new(rpc_url.to_string());
    println!(
        "🔄 LIVE AUTO-SELL RESUME: mint={} remaining_tokens={}",
        open.mint, open.remaining_tokens
    );
    notifier::send_telegram_alert(format!(
        "⚠️ HURAGAN LIVE RECOVERY\nopen holding detected\nmint={}\nremaining_tokens={}\naction=auto_sell_resume",
        open.mint, open.remaining_tokens
    ))
    .await;
    run_z3_live_auto_sell_monitor(rpc, &executor, ledger, &target, &mut open, payer_ref).await?;
    Ok(true)
}
