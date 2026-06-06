#![allow(dead_code)]

#[path = "../engine.rs"]
mod engine;
#[path = "../state.rs"]
mod state;

use engine::{MigrationTarget, QuoteAsset, SPL_TOKEN_2022_PROGRAM, SPL_TOKEN_PROGRAM};
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::Keypair;
use solana_sdk::signer::Signer;
use state::PositionState;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::str::FromStr;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let mint = arg_value("--mint")?;
    let state_path = arg_value("--state").unwrap_or_else(|_| "state.jsonl".to_string());
    let rpc_url = env::var("RPC_URL").unwrap_or_else(|_| "http://127.0.0.1:8899".into());
    if env::var("ALLOW_PLAINTEXT_PRIVATE_KEY").unwrap_or_default() != "true" {
        anyhow::bail!(
            "sell preflight blocked: set ALLOW_PLAINTEXT_PRIVATE_KEY=true only for explicit local/server preflight"
        );
    }
    let key_bs58 = env::var("SOLANA_PRIVATE_KEY_BASE58").map_err(|_| {
        anyhow::anyhow!("SOLANA_PRIVATE_KEY_BASE58 required for sell preflight signing")
    })?;
    let key_bytes = bs58::decode(&key_bs58).into_vec()?;
    let payer =
        Keypair::try_from(key_bytes.as_slice()).map_err(|e| anyhow::anyhow!("invalid key: {e}"))?;
    let rpc = RpcClient::new(rpc_url);

    let state = find_best_state_for_mint(&state_path, &mint)?;
    let target = target_from_state(&state)?;
    if target.quote_asset() != QuoteAsset::Wsol {
        anyhow::bail!(
            "sell preflight only supports WSOL quote; got {}",
            target.quote_asset().symbol()
        );
    }

    let token_amount = actual_user_token_balance(&rpc, &target, &payer).await?;
    let mut plan =
        engine::build_sell_amm_ixs_real_preflight(&rpc, &target, token_amount, &payer).await?;
    let preflight = plan.simulate_preflight(&rpc, &payer).await;

    println!("--- AMM SELL PREFLIGHT REPORT ---");
    println!("mint={}", target.mint);
    println!("pool_state={}", target.pool_state);
    println!("token_amount={}", token_amount);
    println!("expected_sol_out_lamports={}", plan.expected_sol_out);
    println!(
        "expected_sol_out_sol={:.9}",
        plan.expected_sol_out as f64 / 1e9
    );
    println!("min_sol_out_lamports={}", plan.min_sol_out);
    println!("min_sol_out_sol={:.9}", plan.min_sol_out as f64 / 1e9);
    println!("instruction_family={}", plan.instruction_family);
    println!("ixs={}", plan.instructions.len());
    println!(
        "token_ata={}",
        plan.token_ata.map(|p| p.to_string()).unwrap_or_default()
    );
    println!(
        "wsol_ata={}",
        plan.wsol_ata.map(|p| p.to_string()).unwrap_or_default()
    );

    match preflight {
        Ok(()) => {
            println!("preflight=PASS");
            Ok(())
        }
        Err(e) => {
            println!("preflight=FAIL");
            anyhow::bail!(e)
        }
    }
}

fn arg_value(flag: &str) -> anyhow::Result<String> {
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == flag {
            return args
                .next()
                .ok_or_else(|| anyhow::anyhow!("missing value for {flag}"));
        }
        if let Some(value) = arg.strip_prefix(&format!("{flag}=")) {
            return Ok(value.to_string());
        }
    }
    anyhow::bail!("required argument missing: {flag}")
}

fn find_best_state_for_mint(path: &str, mint: &str) -> anyhow::Result<PositionState> {
    let file =
        File::open(path).map_err(|e| anyhow::anyhow!("cannot open state file {path}: {e}"))?;
    let mut latest_any: Option<PositionState> = None;
    let mut latest_complete: Option<PositionState> = None;
    let mut latest_holding: Option<PositionState> = None;

    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<PositionState>(&line) else {
            continue;
        };
        if row.mint != mint {
            continue;
        }
        latest_any = Some(row.clone());
        if row.status == "holding" {
            latest_holding = Some(row.clone());
        }
        if has_live_sell_target_fields(&row) {
            latest_complete = Some(row);
        }
    }

    if let Some(row) = latest_holding.filter(has_live_sell_target_fields) {
        return Ok(row);
    }
    if let Some(row) = latest_complete {
        return Ok(row);
    }
    if let Some(row) = latest_any {
        target_from_state(&row)
            .map(|_| row)
            .map_err(|_| anyhow::anyhow!("live_sell_target_incomplete for mint {mint}"))
    } else {
        anyhow::bail!("mint not found in state: {mint}")
    }
}

fn has_live_sell_target_fields(row: &PositionState) -> bool {
    !row.pool_state.is_empty()
        && !row.base_mint.is_empty()
        && !row.quote_mint.is_empty()
        && !row.quote_asset_mint.is_empty()
        && !row.pool_base_token_account.is_empty()
        && !row.pool_quote_token_account.is_empty()
}

fn target_from_state(row: &PositionState) -> anyhow::Result<MigrationTarget> {
    if !has_live_sell_target_fields(row) {
        anyhow::bail!("live_sell_target_incomplete");
    }
    Ok(MigrationTarget {
        mint: row.mint.clone(),
        name: row.token_name.clone(),
        symbol: row.token_symbol.clone(),
        source: if row.source.is_empty() {
            "helius_migration".into()
        } else {
            row.source.clone()
        },
        pool_state: row.pool_state.clone(),
        base_mint: row.base_mint.clone(),
        quote_mint: row.quote_mint.clone(),
        quote_asset_mint: row.quote_asset_mint.clone(),
        pool_base_token_account: row.pool_base_token_account.clone(),
        pool_quote_token_account: row.pool_quote_token_account.clone(),
        creator: row.creator_address.clone(),
        creator_score: row.creator_score,
        top10_holder_pct: row.top10_holder_pct,
        curve_velocity_secs: row.curve_velocity_secs,
        ..Default::default()
    })
}

async fn actual_user_token_balance(
    rpc: &RpcClient,
    target: &MigrationTarget,
    payer: &Keypair,
) -> anyhow::Result<u64> {
    let token_mint = Pubkey::from_str(&target.quote_mint)?;
    let spl_token = Pubkey::from_str(SPL_TOKEN_PROGRAM)?;
    let token_2022 = Pubkey::from_str(SPL_TOKEN_2022_PROGRAM)?;
    let mint_account = rpc.get_account(&token_mint).await?;
    let token_program = mint_account.owner;
    if token_program != spl_token && token_program != token_2022 {
        anyhow::bail!(
            "unsupported token program for {}: {}",
            target.quote_mint,
            token_program
        );
    }
    let ata = spl_associated_token_account::get_associated_token_address_with_program_id(
        &payer.pubkey(),
        &token_mint,
        &token_program,
    );
    let bal = rpc
        .get_token_account_balance(&ata)
        .await
        .map_err(|e| anyhow::anyhow!("cannot read user token ATA {}: {e}", ata))?;
    let amount = bal.amount.parse::<u64>().unwrap_or(0);
    if amount == 0 {
        anyhow::bail!("user token ATA balance is zero: {}", ata);
    }
    Ok(amount)
}
