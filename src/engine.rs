use serde::{Deserialize, Serialize};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::instruction::{AccountMeta, Instruction};
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use std::env;
use std::str::FromStr;
use tokio::time::{sleep, Duration};

pub const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";
pub const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
pub const PUMP_AMM_PROGRAM: &str = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA";
pub const PUMP_AMM_EVENT_AUTHORITY: &str = "GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR";
pub const SPL_TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const SPL_TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
pub const ASSOCIATED_TOKEN_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
pub const PUMP_AMM_GLOBAL_CONFIG: &str = "ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw";
pub const PUMP_AMM_FEE_RECIPIENT_OWNER: &str = "62qc2CNXwrYqQScmEdiZFFAnJR262PxWEuNQtxfafNgV";
pub const PUMP_AMM_CREATOR_VAULT_OWNER: &str = "8N3GDaZ2iwN65oxVatKTLPNooAVUJTbfiVJ1ahyqwjSk";
pub const PUMP_AMM_FEE_CONFIG: &str = "5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx";
pub const PUMP_AMM_PROTOCOL_FEE_RECIPIENT: &str = "pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ";
pub const PUMP_AMM_COIN_CREATOR_OWNER: &str = "5eHhjP8JaYkz83CWwvGU2uMUXefd3AazWGx4gpcuEEYD";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuoteAsset {
    Wsol,
    Usdc,
    Unsupported,
}

impl QuoteAsset {
    pub fn from_mint(mint: &str) -> Self {
        match mint {
            WSOL_MINT => QuoteAsset::Wsol,
            USDC_MINT => QuoteAsset::Usdc,
            _ => QuoteAsset::Unsupported,
        }
    }

    pub fn symbol(&self) -> &'static str {
        match self {
            QuoteAsset::Wsol => "WSOL",
            QuoteAsset::Usdc => "USDC",
            QuoteAsset::Unsupported => "UNSUPPORTED",
        }
    }

    pub fn decimals(&self) -> u8 {
        match self {
            QuoteAsset::Wsol => 9,
            QuoteAsset::Usdc => 6,
            QuoteAsset::Unsupported => 0,
        }
    }

    pub fn is_supported(&self) -> bool {
        !matches!(self, QuoteAsset::Unsupported)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MigrationTarget {
    pub mint: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub symbol: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub pool_state: String,
    #[serde(default)]
    pub base_mint: String,
    #[serde(default)]
    pub quote_mint: String,
    #[serde(default)]
    pub quote_asset_mint: String,
    #[serde(default)]
    pub lp_mint: String,
    #[serde(default)]
    pub pool_base_token_account: String,
    #[serde(default)]
    pub pool_quote_token_account: String,
    #[serde(default)]
    pub creator: String,
    #[serde(default)]
    pub v_sol_in_bonding_curve: f64,
    #[serde(default)]
    pub market_cap_sol: f64,
    #[serde(default)]
    pub creator_score: f64,
    #[serde(default)]
    pub top10_holder_pct: f64,
    #[serde(default)]
    pub curve_velocity_secs: u64,
    #[serde(default)]
    pub migration_signature: String,
    #[serde(default)]
    pub migration_block_time: i64,
}

impl MigrationTarget {
    pub fn is_amm(&self) -> bool {
        !self.pool_state.is_empty()
            && (!self.base_mint.is_empty() || !self.quote_mint.is_empty())
            && self.source == "helius_migration"
    }

    pub fn is_reversed(&self) -> bool {
        self.base_mint == WSOL_MINT && self.quote_mint == self.mint
    }

    pub fn quote_asset(&self) -> QuoteAsset {
        if !self.quote_asset_mint.is_empty() {
            QuoteAsset::from_mint(&self.quote_asset_mint)
        } else {
            // Legacy targets store the quote asset (WSOL) in `base_mint`.
            QuoteAsset::from_mint(&self.base_mint)
        }
    }

    pub fn token_program_hint(&self) -> &'static str {
        "resolve_on_chain"
    }
}

#[derive(Debug, Clone)]
pub struct BuiltBuyPlan {
    pub instructions: Vec<Instruction>,
    pub spend_lamports: u64,
    pub expected_tokens_out: u64,
    pub min_tokens_out: u64,
    pub instruction_family: String,
    pub token_ata: Option<Pubkey>,
    pub wsol_ata: Option<Pubkey>,
    pub simulation_ok: bool,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct BuiltSellPlan {
    pub instructions: Vec<Instruction>,
    pub token_amount: u64,
    pub expected_sol_out: u64,
    pub min_sol_out: u64,
    pub instruction_family: String,
    pub emergency: bool,
    pub token_ata: Option<Pubkey>,
    pub wsol_ata: Option<Pubkey>,
    pub simulation_ok: bool,
}

#[derive(Debug, Clone)]
pub struct AdvancedGateDecision {
    pub passed: bool,
    pub reason: String,
    pub mode: String,
}

pub fn env_u64(key: &str, default: u64) -> u64 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

pub fn env_f64(key: &str, default: f64) -> f64 {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

const DEFAULT_AMM_LIVE_BUY_MIN_OUT_BPS: u64 = 9000;

pub fn live_buy_min_out_bps() -> u64 {
    live_buy_min_out_bps_from_env_value(env::var("AMM_LIVE_BUY_MIN_OUT_BPS").ok().as_deref())
}

fn live_buy_min_out_bps_from_env_value(value: Option<&str>) -> u64 {
    value
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_AMM_LIVE_BUY_MIN_OUT_BPS)
        .min(10_000)
}

pub fn min_out_from_bps(expected: u64, bps: u64) -> u64 {
    expected
        .saturating_mul(bps.min(10_000))
        .saturating_div(10_000)
}

pub async fn check_pool_sol_gate(rpc: &RpcClient, target: &MigrationTarget) -> anyhow::Result<()> {
    let threshold = env_u64("AMM_MIN_POOL_SOL_FOR_ENTRY_LAMPORTS", 2_000_000_000);
    if target.pool_base_token_account.is_empty() {
        anyhow::bail!("pool base token account missing");
    }
    let account = Pubkey::from_str(&target.pool_base_token_account)?;
    let amount = token_account_amount(rpc, &account).await?;
    if amount < threshold {
        anyhow::bail!(
            "pool SOL gate blocked: reserve={} threshold={}",
            amount,
            threshold
        );
    }
    Ok(())
}

pub fn advanced_amm_safety_gate(target: &MigrationTarget) -> AdvancedGateDecision {
    let mode = env::var("AMM_ADVANCED_GATE_MODE").unwrap_or_else(|_| "shadow".to_string());
    let max_top10 = env_f64("AMM_MAX_TOP10_HOLDER_PCT", 0.35);
    let min_creator = env_f64("AMM_MIN_CREATOR_SCORE", 0.20);
    let min_velocity = env_u64("AMM_MIN_CURVE_VELOCITY_SECS", 45);
    let require_creator =
        env::var("AMM_REQUIRE_CREATOR").unwrap_or_else(|_| "true".into()) == "true";

    let mut missing = Vec::new();
    if require_creator && target.creator.is_empty() {
        missing.push("creator");
    }
    if target.top10_holder_pct <= 0.0 {
        missing.push("holders");
    }
    if target.curve_velocity_secs == 0 {
        missing.push("velocity");
    }
    if !missing.is_empty() {
        return AdvancedGateDecision {
            passed: mode != "enforce",
            reason: format!("advanced_gate_data_missing:{}", missing.join(",")),
            mode,
        };
    }
    let mut reasons = Vec::new();
    if target.top10_holder_pct > max_top10 {
        reasons.push("top10_holder_pct");
    }
    if target.creator_score < min_creator {
        reasons.push("creator_score");
    }
    if target.curve_velocity_secs < min_velocity {
        reasons.push("curve_velocity");
    }
    AdvancedGateDecision {
        passed: reasons.is_empty() || mode != "enforce",
        reason: if reasons.is_empty() {
            "advanced_gate_passed".into()
        } else {
            format!("advanced_gate_failed:{}", reasons.join(","))
        },
        mode,
    }
}

pub async fn process_migration_and_build_amm_ixs(
    rpc: &RpcClient,
    target: &MigrationTarget,
    spend_lamports: u64,
    payer: Option<&Keypair>,
    simulate: bool,
) -> anyhow::Result<BuiltBuyPlan> {
    if !target.is_amm() {
        anyhow::bail!("non-AMM target blocked from AMM builder");
    }
    check_pool_sol_gate(rpc, target).await?;
    let quote = quote_buy_amm(rpc, target, spend_lamports).await?;
    if quote == 0 {
        anyhow::bail!("amm buy quote unavailable");
    }

    // If live mode with a payer key, build real instructions
    if let Some(payer) = payer {
        let live_send = env::var("LIVE_SEND_ENABLED").unwrap_or_default() == "true";
        let mut plan = build_buy_amm_ixs_real(rpc, target, spend_lamports, quote, payer).await?;

        if simulate {
            let simulation_ok = plan.simulate_preflight(rpc, payer).await;
            plan.simulation_ok = simulation_ok.is_ok();
            if let Err(e) = simulation_ok {
                eprintln!("⚠️ LIVE PREFLIGHT FAIL for {}: {e}", target.mint);
            } else {
                println!(
                    "✅ LIVE PREFLIGHT OK for {} | tokens_out={} ixs={}",
                    target.mint,
                    plan.expected_tokens_out,
                    plan.instructions.len()
                );
            }
        }

        if !live_send {
            println!(
                "🛡️ LIVE DRY-RUN for {} | send=disabled simulation={}",
                target.mint, plan.simulation_ok
            );
            // Keep instructions for later, but mark as dry
            plan.instruction_family = format!("live_dry_run_{}", plan.instruction_family);
        }

        return Ok(plan);
    }

    // Paper mode: shadow quote only
    Ok(BuiltBuyPlan {
        instructions: vec![],
        spend_lamports,
        expected_tokens_out: quote,
        min_tokens_out: quote.saturating_mul(95).saturating_div(100), // 5% slippage
        instruction_family: "buy_amm_quote_shadow".to_string(),
        token_ata: None,
        wsol_ata: None,
        simulation_ok: false,
    })
}

pub async fn build_sell_amm_ixs(
    rpc: &RpcClient,
    target: &MigrationTarget,
    token_amount: u64,
    emergency: bool,
) -> anyhow::Result<BuiltSellPlan> {
    if !target.is_amm() {
        anyhow::bail!("non-AMM target blocked from AMM sell");
    }
    let expected = quote_sell_amm(rpc, target, token_amount).await?;
    if expected == 0 {
        anyhow::bail!("amm sell quote unavailable");
    }
    Ok(BuiltSellPlan {
        instructions: vec![],
        token_amount,
        expected_sol_out: expected,
        min_sol_out: expected.saturating_mul(8).saturating_div(10),
        instruction_family: if target.is_reversed() {
            "buy_amm_reversed_token_to_sol_conservative"
        } else {
            "sell_amm_quote_shadow"
        }
        .to_string(),
        emergency,
        token_ata: None,
        wsol_ata: None,
        simulation_ok: false,
    })
}

pub async fn quote_buy_amm(
    rpc: &RpcClient,
    target: &MigrationTarget,
    spend_lamports: u64,
) -> anyhow::Result<u64> {
    let (sol_reserve, token_reserve) = pool_reserves(rpc, target).await?;
    let haircut_bps = env_u64("PAPER_AMM_QUOTE_HAIRCUT_BPS", 9700);
    let out = cpmm_out(spend_lamports, sol_reserve, token_reserve);
    Ok(out.saturating_mul(haircut_bps).saturating_div(10_000))
}

pub async fn quote_sell_amm(
    rpc: &RpcClient,
    target: &MigrationTarget,
    token_amount: u64,
) -> anyhow::Result<u64> {
    let (sol_reserve, token_reserve) = pool_reserves(rpc, target).await?;
    // Paper quote symmetry: buy and sell must use the same conservative quote haircut.
    // `is_reversed()` describes instruction-family orientation, not an extra 15% AMM fee.
    // Using AMM_REVERSED_SELL_TARGET_BPS here made every WSOL/token migration look
    // structurally unprofitable in paper mode after the quote-aware resolver normalized
    // base_mint=WSOL and quote_mint=token.
    let haircut_bps = env_u64("PAPER_AMM_QUOTE_HAIRCUT_BPS", 9700);
    let out = cpmm_out(token_amount, token_reserve, sol_reserve);
    Ok(out.saturating_mul(haircut_bps).saturating_div(10_000))
}

pub async fn pool_reserves(
    rpc: &RpcClient,
    target: &MigrationTarget,
) -> anyhow::Result<(u64, u64)> {
    if target.pool_base_token_account.is_empty() || target.pool_quote_token_account.is_empty() {
        anyhow::bail!("pool reserve accounts missing");
    }
    let base_account = Pubkey::from_str(&target.pool_base_token_account)?;
    let quote_account = Pubkey::from_str(&target.pool_quote_token_account)?;
    let base_amount = token_account_amount(rpc, &base_account).await?;
    let quote_amount = token_account_amount(rpc, &quote_account).await?;
    if target.base_mint == WSOL_MINT {
        Ok((base_amount, quote_amount))
    } else if target.quote_mint == WSOL_MINT {
        Ok((quote_amount, base_amount))
    } else {
        anyhow::bail!("pool has no WSOL side")
    }
}

async fn token_account_amount(rpc: &RpcClient, account: &Pubkey) -> anyhow::Result<u64> {
    let attempts = env_u64("AMM_RESERVE_BALANCE_RETRIES", 12).max(1);
    let delay_ms = env_u64("AMM_RESERVE_BALANCE_RETRY_MS", 600);
    let mut last_err: Option<anyhow::Error> = None;

    for attempt in 0..attempts {
        match rpc.get_token_account_balance(account).await {
            Ok(bal) => return Ok(bal.amount.parse::<u64>().unwrap_or(0)),
            Err(e) => {
                last_err = Some(e.into());
                if attempt + 1 >= attempts {
                    break;
                }
                sleep(Duration::from_millis(delay_ms.saturating_mul(attempt + 1))).await;
            }
        }
    }

    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("token account balance unavailable: {account}")))
}

fn cpmm_out(amount_in: u64, reserve_in: u64, reserve_out: u64) -> u64 {
    if amount_in == 0 || reserve_in == 0 || reserve_out == 0 {
        return 0;
    }
    let fee_bps = env_u64("PAPER_AMM_FEE_BPS", 100);
    let amount_less_fee = (amount_in as u128)
        .saturating_mul((10_000u64.saturating_sub(fee_bps)) as u128)
        / 10_000u128;
    let numerator = amount_less_fee.saturating_mul(reserve_out as u128);
    let denominator = (reserve_in as u128).saturating_add(amount_less_fee);
    if denominator == 0 {
        0
    } else {
        (numerator / denominator).min(u64::MAX as u128) as u64
    }
}

/// Pump AMM token→SOL instruction for current pool orientation (`base=WSOL`, `quote=coin`).
/// IDL name: `buy_exact_quote_in`; args = spendable_quote_in, min_base_amount_out, track_volume.
#[allow(dead_code)]
pub const PUMP_AMM_BUY_EXACT_QUOTE_IN_DISCRIMINATOR: [u8; 8] =
    [198, 46, 21, 82, 180, 217, 232, 112];

#[allow(dead_code)]
pub fn buy_exact_quote_in_account_count() -> usize {
    23
}

#[allow(dead_code)]
pub fn buy_exact_quote_in_data(spendable_quote_in: u64, min_base_amount_out: u64) -> Vec<u8> {
    let mut data = Vec::with_capacity(25);
    data.extend_from_slice(&PUMP_AMM_BUY_EXACT_QUOTE_IN_DISCRIMINATOR);
    data.extend_from_slice(&spendable_quote_in.to_le_bytes());
    data.extend_from_slice(&min_base_amount_out.to_le_bytes());
    data.push(0);
    data
}

#[allow(dead_code)]
fn global_config_buyback_recipients(data: &[u8]) -> anyhow::Result<Vec<Pubkey>> {
    // Anchor discriminator + GlobalConfig fields up to buyback config.
    // Layout from pump-public-docs `GlobalConfig`.
    let mut offset = 8usize;
    offset += 32; // admin
    offset += 8; // lp_fee_basis_points
    offset += 8; // protocol_fee_basis_points
    offset += 1; // disable_flags
    offset += 32 * 8; // protocol_fee_recipients
    offset += 8; // coin_creator_fee_basis_points
    offset += 32; // admin_set_coin_creator_authority
    offset += 32; // whitelist_pda
    offset += 32; // reserved_fee_recipient
    offset += 1; // mayhem_mode_enabled
    offset += 32 * 7; // reserved_fee_recipients
    if data.len() < offset + 1 + 32 * 8 + 8 {
        return Ok(Vec::new());
    }
    let is_cashback_enabled = data[offset] != 0;
    offset += 1;
    let mut recipients = Vec::with_capacity(8);
    for i in 0..8 {
        let start = offset + i * 32;
        let bytes: [u8; 32] = data[start..start + 32].try_into()?;
        recipients.push(Pubkey::new_from_array(bytes));
    }
    offset += 32 * 8;
    let buyback_basis_points = u64::from_le_bytes(data[offset..offset + 8].try_into()?);
    if is_cashback_enabled || buyback_basis_points > 0 {
        Ok(recipients)
    } else {
        Ok(Vec::new())
    }
}

fn pda(seed: &[u8], program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[seed], program_id).0
}

#[allow(dead_code)]
fn pda_with_user(seed: &[u8], user: &Pubkey, program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[seed, user.as_ref()], program_id).0
}

#[allow(dead_code)]
impl BuiltSellPlan {
    /// Simulate the sell transaction without sending it.
    pub async fn simulate_preflight(
        &mut self,
        rpc: &RpcClient,
        payer: &Keypair,
    ) -> anyhow::Result<()> {
        if self.instructions.is_empty() {
            anyhow::bail!("cannot simulate empty sell instructions");
        }
        use solana_sdk::transaction::Transaction;

        let bh = rpc.get_latest_blockhash().await?;
        let mut tx = Transaction::new_with_payer(&self.instructions, Some(&payer.pubkey()));
        tx.sign(&[payer], bh);
        let result = rpc.simulate_transaction(&tx).await?;
        if let Some(err) = result.value.err {
            if let Some(logs) = result.value.logs.as_ref() {
                for line in logs.iter().rev().take(16).rev() {
                    eprintln!("  sell simlog: {line}");
                }
            }
            anyhow::bail!("sell simulation failed: {:?}", err);
        }
        let units = result.value.units_consumed.unwrap_or(0);
        println!(
            "  📊 sell simulation: OK | CU={} accounts={} ixs={}",
            units,
            result.value.accounts.as_ref().map_or(0, |v| v.len()),
            self.instructions.len()
        );
        self.simulation_ok = true;
        Ok(())
    }
}

/// Build real Pump AMM token→SOL sell instructions for preflight only.
/// This does not send and does not close/unwrap WSOL; future live-send code must handle that explicitly.
#[allow(dead_code)]
pub async fn build_sell_amm_ixs_real_preflight(
    rpc: &RpcClient,
    target: &MigrationTarget,
    token_amount: u64,
    payer: &Keypair,
) -> anyhow::Result<BuiltSellPlan> {
    let slippage_bps = env_u64("AMM_LIVE_SELL_SLIPPAGE_BPS", 8000).min(10_000);
    build_sell_amm_ixs_real_preflight_with_bps(rpc, target, token_amount, payer, slippage_bps).await
}

pub async fn build_sell_amm_ixs_real_preflight_with_bps(
    rpc: &RpcClient,
    target: &MigrationTarget,
    token_amount: u64,
    payer: &Keypair,
    slippage_bps: u64,
) -> anyhow::Result<BuiltSellPlan> {
    if !target.is_amm() {
        anyhow::bail!("non-AMM target blocked from live sell builder");
    }
    if target.quote_asset() != QuoteAsset::Wsol {
        anyhow::bail!(
            "non-WSOL quote not supported for live sell: {}",
            target.quote_asset_mint
        );
    }
    if target.pool_state.is_empty()
        || target.base_mint.is_empty()
        || target.quote_mint.is_empty()
        || target.quote_asset_mint.is_empty()
        || target.pool_base_token_account.is_empty()
        || target.pool_quote_token_account.is_empty()
    {
        anyhow::bail!("live_sell_target_incomplete");
    }
    if token_amount == 0 {
        anyhow::bail!("live_sell_token_amount_zero");
    }

    let expected_sol_out = quote_sell_amm(rpc, target, token_amount).await?;
    if expected_sol_out == 0 {
        anyhow::bail!("live_sell_quote_unavailable");
    }

    let payer_pubkey = payer.pubkey();
    let program_id = Pubkey::from_str(PUMP_AMM_PROGRAM)?;
    let pool_state = Pubkey::from_str(&target.pool_state)?;
    let base_mint = Pubkey::from_str(&target.base_mint)?;
    let quote_mint = Pubkey::from_str(&target.quote_mint)?;
    let pool_base_token_account = Pubkey::from_str(&target.pool_base_token_account)?;
    let pool_quote_token_account = Pubkey::from_str(&target.pool_quote_token_account)?;
    let system_program = Pubkey::from_str("11111111111111111111111111111111")?;
    let base_token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM)?;
    let token_2022_program = Pubkey::from_str(SPL_TOKEN_2022_PROGRAM)?;
    let associated_token_program = Pubkey::from_str(ASSOCIATED_TOKEN_PROGRAM)?;
    let event_authority = Pubkey::from_str(PUMP_AMM_EVENT_AUTHORITY)?;
    let wsol_mint = Pubkey::from_str(WSOL_MINT)?;

    if base_mint != wsol_mint {
        anyhow::bail!("live sell expects base_mint=WSOL, got {}", target.base_mint);
    }

    let quote_mint_account = rpc.get_account(&quote_mint).await?;
    let quote_token_program = quote_mint_account.owner;
    if quote_token_program != base_token_program && quote_token_program != token_2022_program {
        anyhow::bail!(
            "unsupported quote token program for {}: {}",
            target.quote_mint,
            quote_token_program
        );
    }

    let user_base_ata = derive_ata_with_program(&payer_pubkey, &base_mint, &base_token_program);
    let user_quote_ata = derive_ata_with_program(&payer_pubkey, &quote_mint, &quote_token_program);

    let user_quote_balance = token_account_amount(rpc, &user_quote_ata)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "live_sell_user_token_ata_unavailable:{}:{e}",
                user_quote_ata
            )
        })?;
    if user_quote_balance == 0 {
        anyhow::bail!("live_sell_user_token_balance_zero:{}", user_quote_ata);
    }
    if token_amount > user_quote_balance {
        anyhow::bail!(
            "live_sell_token_amount_exceeds_balance: amount={} balance={}",
            token_amount,
            user_quote_balance
        );
    }

    let protocol_fee_recipient = Pubkey::from_str(PUMP_AMM_FEE_RECIPIENT_OWNER)?;
    let protocol_fee_recipient_token_account =
        derive_ata_with_program(&protocol_fee_recipient, &quote_mint, &quote_token_program);
    let coin_creator_vault_authority = Pubkey::from_str(PUMP_AMM_CREATOR_VAULT_OWNER)?;
    let coin_creator_vault_ata = derive_ata_with_program(
        &coin_creator_vault_authority,
        &quote_mint,
        &quote_token_program,
    );
    let global_config = Pubkey::from_str(PUMP_AMM_GLOBAL_CONFIG)?;
    let fee_config = Pubkey::from_str(PUMP_AMM_FEE_CONFIG)?;
    let fee_program = Pubkey::from_str(PUMP_AMM_PROTOCOL_FEE_RECIPIENT)?;
    let global_volume_accumulator = pda(b"global_volume_accumulator", &program_id);
    let user_volume_accumulator =
        pda_with_user(b"user_volume_accumulator", &payer_pubkey, &program_id);

    let mut instructions = Vec::new();
    if rpc.get_token_account_balance(&user_base_ata).await.is_err() {
        instructions.push(
            spl_associated_token_account::instruction::create_associated_token_account(
                &payer_pubkey,
                &payer_pubkey,
                &base_mint,
                &base_token_program,
            ),
        );
    }

    let slippage_bps = slippage_bps.min(10_000);
    let min_sol_out = expected_sol_out
        .saturating_mul(slippage_bps)
        .saturating_div(10_000);

    let accounts = vec![
        AccountMeta::new(pool_state, false),
        AccountMeta::new(payer_pubkey, true),
        AccountMeta::new_readonly(global_config, false),
        AccountMeta::new_readonly(base_mint, false),
        AccountMeta::new_readonly(quote_mint, false),
        AccountMeta::new(user_base_ata, false),
        AccountMeta::new(user_quote_ata, false),
        AccountMeta::new(pool_base_token_account, false),
        AccountMeta::new(pool_quote_token_account, false),
        AccountMeta::new_readonly(protocol_fee_recipient, false),
        AccountMeta::new(protocol_fee_recipient_token_account, false),
        AccountMeta::new_readonly(base_token_program, false),
        AccountMeta::new_readonly(quote_token_program, false),
        AccountMeta::new_readonly(system_program, false),
        AccountMeta::new_readonly(associated_token_program, false),
        AccountMeta::new_readonly(event_authority, false),
        AccountMeta::new_readonly(program_id, false),
        AccountMeta::new(coin_creator_vault_ata, false),
        AccountMeta::new_readonly(coin_creator_vault_authority, false),
        AccountMeta::new_readonly(global_volume_accumulator, false),
        AccountMeta::new(user_volume_accumulator, false),
        AccountMeta::new_readonly(fee_config, false),
        AccountMeta::new_readonly(fee_program, false),
    ];
    let mut accounts = accounts;
    let buyback_recipient = Pubkey::from_str(PUMP_AMM_COIN_CREATOR_OWNER)?;
    let buyback_recipient_ata =
        derive_ata_with_program(&buyback_recipient, &quote_mint, &quote_token_program);
    accounts.push(AccountMeta::new_readonly(buyback_recipient, false));
    accounts.push(AccountMeta::new(buyback_recipient_ata, false));

    // OptionBool::None — do not track volume in preflight v1.
    let data = buy_exact_quote_in_data(token_amount, min_sol_out);
    let account_count = accounts.len();

    instructions.push(Instruction {
        program_id,
        accounts,
        data,
    });

    println!("  🔨 live sell preflight(buy_exact_quote_in): mint={} tokens={} expect_sol={} min_sol={} min_out_bps={} accounts={} quote_program={}",
        target.mint, token_amount, expected_sol_out, min_sol_out, slippage_bps, account_count, quote_token_program);

    Ok(BuiltSellPlan {
        instructions,
        token_amount,
        expected_sol_out,
        min_sol_out,
        instruction_family: "buy_exact_quote_in_live_token_to_sol_preflight".to_string(),
        emergency: false,
        token_ata: Some(user_quote_ata),
        wsol_ata: Some(user_base_ata),
        simulation_ok: false,
    })
}

#[allow(dead_code)]
pub async fn live_sell_user_token_balance(
    rpc: &RpcClient,
    target: &MigrationTarget,
    payer: &Keypair,
) -> anyhow::Result<u64> {
    if target.quote_asset() != QuoteAsset::Wsol {
        anyhow::bail!(
            "non-WSOL quote not supported for live sell balance: {}",
            target.quote_asset_mint
        );
    }
    let quote_mint = Pubkey::from_str(&target.quote_mint)?;
    let base_token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM)?;
    let token_2022_program = Pubkey::from_str(SPL_TOKEN_2022_PROGRAM)?;
    let quote_mint_account = rpc.get_account(&quote_mint).await?;
    let quote_token_program = quote_mint_account.owner;
    if quote_token_program != base_token_program && quote_token_program != token_2022_program {
        anyhow::bail!(
            "unsupported quote token program for {}: {}",
            target.quote_mint,
            quote_token_program
        );
    }
    let user_quote_ata =
        derive_ata_with_program(&payer.pubkey(), &quote_mint, &quote_token_program);
    token_account_amount(rpc, &user_quote_ata).await
}

// ── Pump AMM Buy Instruction Builder v1 ──────────────────────────────────

/// Pump AMM SOL→coin instruction is logged as `Sell`.
/// Real discriminator observed on-chain for `Instruction: Sell`.
const PUMP_AMM_SELL_DISCRIMINATOR: [u8; 8] = [0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad];

fn derive_ata_with_program(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    spl_associated_token_account::get_associated_token_address_with_program_id(
        owner,
        mint,
        token_program,
    )
}

impl BuiltBuyPlan {
    /// Simulate the buy transaction without sending it
    pub async fn simulate_preflight(
        &mut self,
        rpc: &RpcClient,
        payer: &Keypair,
    ) -> anyhow::Result<()> {
        if self.instructions.is_empty() {
            anyhow::bail!("cannot simulate empty instructions");
        }
        use solana_sdk::transaction::Transaction;

        let bh = rpc.get_latest_blockhash().await?;
        let mut tx = Transaction::new_with_payer(&self.instructions, Some(&payer.pubkey()));
        tx.sign(&[payer], bh);
        let result = rpc.simulate_transaction(&tx).await?;
        if let Some(err) = result.value.err {
            if let Some(logs) = result.value.logs.as_ref() {
                for line in logs.iter().rev().take(12).rev() {
                    eprintln!("  simlog: {line}");
                }
            }
            anyhow::bail!("simulation failed: {:?}", err);
        }
        // Log simulation details
        let units = result.value.units_consumed.unwrap_or(0);
        println!(
            "  📊 simulation: OK | CU={} accounts={} ixs={}",
            units,
            result.value.accounts.as_ref().map_or(0, |v| v.len()),
            self.instructions.len()
        );
        Ok(())
    }
}

/// Build real Pump AMM buy instructions with complete account layout
pub async fn build_buy_amm_ixs_real(
    rpc: &RpcClient,
    target: &MigrationTarget,
    spend_lamports: u64,
    expected_tokens: u64,
    payer: &Keypair,
) -> anyhow::Result<BuiltBuyPlan> {
    let payer_pubkey = payer.pubkey();
    let program_id = Pubkey::from_str(PUMP_AMM_PROGRAM)?;
    let pool_state = Pubkey::from_str(&target.pool_state)?;
    let coin_mint = Pubkey::from_str(&target.quote_mint)?;
    let quote_mint = Pubkey::from_str(&target.quote_asset_mint)?;
    let quote_vault = Pubkey::from_str(&target.pool_base_token_account)?;
    let coin_vault = Pubkey::from_str(&target.pool_quote_token_account)?;
    let system_program = Pubkey::from_str("11111111111111111111111111111111")?;
    let quote_token_program = Pubkey::from_str(SPL_TOKEN_PROGRAM)?;
    let token_2022_program = Pubkey::from_str(SPL_TOKEN_2022_PROGRAM)?;
    let associated_token_program = Pubkey::from_str(ASSOCIATED_TOKEN_PROGRAM)?;
    let event_authority = Pubkey::from_str(PUMP_AMM_EVENT_AUTHORITY)?;
    let wsol_mint = Pubkey::from_str(WSOL_MINT)?;

    if quote_mint != wsol_mint {
        anyhow::bail!(
            "non-WSOL quote not supported yet: {}",
            target.quote_asset_mint
        );
    }

    let coin_mint_account = rpc.get_account(&coin_mint).await?;
    let coin_token_program = coin_mint_account.owner;
    if coin_token_program != quote_token_program && coin_token_program != token_2022_program {
        anyhow::bail!(
            "unsupported coin token program for {}: {}",
            target.quote_mint,
            coin_token_program
        );
    }
    if coin_token_program == token_2022_program && spend_lamports <= 3_000_000 {
        anyhow::bail!(
            "Token-2022 mint blocked for small canary due rent risk: {}",
            target.quote_mint
        );
    }

    let user_quote_ata = derive_ata_with_program(&payer_pubkey, &quote_mint, &quote_token_program);
    let user_coin_ata = derive_ata_with_program(&payer_pubkey, &coin_mint, &coin_token_program);

    let fee_recipient_owner = Pubkey::from_str(PUMP_AMM_FEE_RECIPIENT_OWNER)?;
    let fee_recipient_ata =
        derive_ata_with_program(&fee_recipient_owner, &coin_mint, &coin_token_program);
    let creator_vault_owner = Pubkey::from_str(PUMP_AMM_CREATOR_VAULT_OWNER)?;
    let creator_vault_ata =
        derive_ata_with_program(&creator_vault_owner, &coin_mint, &coin_token_program);
    let fee_config = Pubkey::from_str(PUMP_AMM_FEE_CONFIG)?;
    let protocol_fee_recipient = Pubkey::from_str(PUMP_AMM_PROTOCOL_FEE_RECIPIENT)?;
    let coin_creator_owner = Pubkey::from_str(PUMP_AMM_COIN_CREATOR_OWNER)?;
    let coin_creator_ata =
        derive_ata_with_program(&coin_creator_owner, &coin_mint, &coin_token_program);
    let global_config = Pubkey::from_str(PUMP_AMM_GLOBAL_CONFIG)?;

    let needs_wsol_ata = rpc
        .get_token_account_balance(&user_quote_ata)
        .await
        .is_err();
    let needs_coin_ata = rpc.get_token_account_balance(&user_coin_ata).await.is_err();

    let mut instructions = Vec::new();

    if needs_wsol_ata {
        instructions.push(
            spl_associated_token_account::instruction::create_associated_token_account(
                &payer_pubkey,
                &payer_pubkey,
                &wsol_mint,
                &quote_token_program,
            ),
        );
    }
    if needs_coin_ata {
        println!(
            "  🪙 coin ATA {} needs creation for mint {} program={}",
            user_coin_ata, target.quote_mint, coin_token_program
        );
        instructions.push(
            spl_associated_token_account::instruction::create_associated_token_account(
                &payer_pubkey,
                &payer_pubkey,
                &coin_mint,
                &coin_token_program,
            ),
        );
    }

    instructions.push(solana_sdk::system_instruction::transfer(
        &payer_pubkey,
        &user_quote_ata,
        spend_lamports,
    ));
    instructions.push(spl_token::instruction::sync_native(
        &spl_token::ID,
        &user_quote_ata,
    )?);

    let min_out_bps = live_buy_min_out_bps();
    let min_tokens = min_out_from_bps(expected_tokens, min_out_bps);

    // Real Pump AMM SOL→coin account layout observed from successful `Instruction: Sell` tx:
    // data = discriminator + quote_lamports_in + min_coin_out.
    let accounts = vec![
        AccountMeta::new(pool_state, false),
        AccountMeta::new(payer_pubkey, true),
        AccountMeta::new_readonly(global_config, false),
        AccountMeta::new_readonly(quote_mint, false),
        AccountMeta::new_readonly(coin_mint, false),
        AccountMeta::new(user_quote_ata, false),
        AccountMeta::new(user_coin_ata, false),
        AccountMeta::new(quote_vault, false),
        AccountMeta::new(coin_vault, false),
        AccountMeta::new_readonly(fee_recipient_owner, false),
        AccountMeta::new(fee_recipient_ata, false),
        AccountMeta::new_readonly(quote_token_program, false),
        AccountMeta::new_readonly(coin_token_program, false),
        AccountMeta::new_readonly(system_program, false),
        AccountMeta::new_readonly(associated_token_program, false),
        AccountMeta::new_readonly(event_authority, false),
        AccountMeta::new_readonly(program_id, false),
        AccountMeta::new(creator_vault_ata, false),
        AccountMeta::new_readonly(creator_vault_owner, false),
        AccountMeta::new_readonly(fee_config, false),
        AccountMeta::new_readonly(protocol_fee_recipient, false),
        AccountMeta::new_readonly(coin_creator_owner, false),
        AccountMeta::new(coin_creator_ata, false),
    ];

    let mut data = Vec::with_capacity(24);
    data.extend_from_slice(&PUMP_AMM_SELL_DISCRIMINATOR);
    data.extend_from_slice(&spend_lamports.to_le_bytes());
    data.extend_from_slice(&min_tokens.to_le_bytes());

    instructions.push(Instruction {
        program_id,
        accounts,
        data,
    });

    println!("  🔨 live buy(Sell ix): mint={} spend={} lamports expect={} tokens min={} tokens min_out_bps={} accounts=23 coin_program={}",
        target.mint, spend_lamports, expected_tokens, min_tokens, min_out_bps, coin_token_program);
    let payer_str = payer_pubkey.to_string();
    println!(
        "     accounts: pool={} user={} q_vault={} c_vault={}",
        &target.pool_state[..12],
        &payer_str[..12],
        &target.pool_base_token_account[..12],
        &target.pool_quote_token_account[..12]
    );

    Ok(BuiltBuyPlan {
        instructions,
        spend_lamports,
        expected_tokens_out: expected_tokens,
        min_tokens_out: min_tokens,
        instruction_family: "sell_amm_live_sol_to_token_preflight".to_string(),
        token_ata: Some(user_coin_ata),
        wsol_ata: Some(user_quote_ata),
        simulation_ok: false,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        buy_exact_quote_in_account_count, buy_exact_quote_in_data, cpmm_out,
        live_buy_min_out_bps_from_env_value, min_out_from_bps,
        PUMP_AMM_BUY_EXACT_QUOTE_IN_DISCRIMINATOR,
    };

    #[test]
    fn buy_exact_quote_in_layout_is_stable() {
        let data = buy_exact_quote_in_data(123, 45);
        assert_eq!(&data[..8], &PUMP_AMM_BUY_EXACT_QUOTE_IN_DISCRIMINATOR);
        assert_eq!(u64::from_le_bytes(data[8..16].try_into().unwrap()), 123);
        assert_eq!(u64::from_le_bytes(data[16..24].try_into().unwrap()), 45);
        assert_eq!(data[24], 0);
        assert_eq!(buy_exact_quote_in_account_count(), 23);
    }

    #[test]
    fn cpmm_quote_is_positive_and_bounded() {
        let out = cpmm_out(3_000_000, 89_000_000_000, 1_000_000_000_000_000);
        assert!(out > 0);
        assert!(out < 1_000_000_000_000_000);
    }

    #[test]
    fn live_buy_min_out_bps_controls_min_tokens() {
        assert_eq!(min_out_from_bps(1_000_000, 9000), 900_000);
        assert_eq!(min_out_from_bps(1_000_000, 9500), 950_000);
        assert_eq!(min_out_from_bps(1_000_000, 12_000), 1_000_000);
    }

    #[test]
    fn live_buy_min_out_bps_defaults_to_9000_and_clamps() {
        assert_eq!(live_buy_min_out_bps_from_env_value(None), 9000);
        assert_eq!(live_buy_min_out_bps_from_env_value(Some("8500")), 8500);
        assert_eq!(live_buy_min_out_bps_from_env_value(Some("12000")), 10_000);
        assert_eq!(live_buy_min_out_bps_from_env_value(Some("bad")), 9000);
    }
}
