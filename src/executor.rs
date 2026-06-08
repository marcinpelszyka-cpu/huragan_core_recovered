use base64::{engine::general_purpose, Engine as _};
use serde_json::Value;
use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_sdk::commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_sdk::compute_budget::ComputeBudgetInstruction;
use solana_sdk::instruction::Instruction;
use solana_sdk::pubkey::Pubkey;
use solana_sdk::signature::{Keypair, Signature};
use solana_sdk::signer::Signer;
use solana_sdk::transaction::Transaction;
use std::str::FromStr;
use std::time::Duration;
use tokio::time::sleep;

pub struct Executor {
    rpc: RpcClient,
    backend: LiveSendBackend,
    preflight_commitment: CommitmentConfig,
    preflight_commitment_level: CommitmentLevel,
    sender_client: reqwest::Client,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxTerminalStatus {
    Confirmed,
    Failed(String),
    TimeoutUnknown,
}

impl Executor {
    pub fn new(rpc_url: String) -> Self {
        let send_url = live_send_rpc_url_from_env(&rpc_url);
        let preflight_commitment = live_send_preflight_commitment();
        let preflight_commitment_level = preflight_commitment.commitment;
        Self {
            rpc: RpcClient::new_with_commitment(send_url, preflight_commitment),
            backend: live_send_backend(),
            preflight_commitment,
            preflight_commitment_level,
            sender_client: reqwest::Client::new(),
        }
    }

    pub async fn simulate_preflight(
        &self,
        payer: &Keypair,
        ixs: &[Instruction],
    ) -> anyhow::Result<()> {
        let bh = self.rpc.get_latest_blockhash().await?;
        let mut tx = Transaction::new_with_payer(ixs, Some(&payer.pubkey()));
        tx.sign(&[payer], bh);
        let result = self.rpc.simulate_transaction(&tx).await?;
        if let Some(err) = result.value.err {
            anyhow::bail!("simulate_preflight failed: {:?}", err);
        }
        Ok(())
    }

    pub async fn send_with_preflight(
        &self,
        payer: &Keypair,
        ixs: &[Instruction],
    ) -> anyhow::Result<Signature> {
        if self.backend == LiveSendBackend::HeliusSender {
            return self.send_with_sender(payer, ixs).await;
        }
        if self.backend != LiveSendBackend::Rpc {
            anyhow::bail!(
                "LIVE SEND BACKEND unsupported in this build: {}",
                self.backend.as_str()
            );
        }
        let attempts = live_send_preflight_attempts();
        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 1..=attempts {
            let (bh, _last_valid_block_height) = self
                .rpc
                .get_latest_blockhash_with_commitment(self.preflight_commitment)
                .await?;
            let mut tx = Transaction::new_with_payer(ixs, Some(&payer.pubkey()));
            tx.sign(&[payer], bh);
            println!(
                "🛰️ LIVE SEND backend={} commitment={} attempt={}/{}",
                self.backend.as_str(),
                commitment_label(self.preflight_commitment_level),
                attempt,
                attempts
            );
            match self
                .rpc
                .send_transaction_with_config(
                    &tx,
                    RpcSendTransactionConfig {
                        skip_preflight: false,
                        preflight_commitment: Some(self.preflight_commitment_level),
                        max_retries: Some(live_send_rpc_max_retries()),
                        ..RpcSendTransactionConfig::default()
                    },
                )
                .await
            {
                Ok(sig) => return Ok(sig),
                Err(e) => {
                    let err = anyhow::Error::new(e);
                    let retryable = is_retryable_blockhash_error(&err.to_string());
                    if retryable && attempt < attempts {
                        eprintln!(
                            "⚠️ LIVE SEND PREFLIGHT RETRY attempt={}/{} reason=blockhash",
                            attempt + 1,
                            attempts
                        );
                        last_err = Some(err);
                        sleep(Duration::from_millis(150)).await;
                        continue;
                    }
                    return Err(err);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("send_with_preflight_failed")))
    }

    async fn send_with_sender(
        &self,
        payer: &Keypair,
        ixs: &[Instruction],
    ) -> anyhow::Result<Signature> {
        let cfg = HeliusSenderConfig::from_env()?;
        let (bh, _last_valid_block_height) = self
            .rpc
            .get_latest_blockhash_with_commitment(self.preflight_commitment)
            .await?;
        let wrapped_ixs = wrap_sender_instructions(ixs, payer, &cfg)?;
        let mut tx = Transaction::new_with_payer(&wrapped_ixs, Some(&payer.pubkey()));
        tx.sign(&[payer], bh);
        let raw = bincode::serialize(&tx)?;
        let encoded = general_purpose::STANDARD.encode(raw);
        println!(
            "🛰️ LIVE SEND backend=helius_sender endpoint_mode={} skip_preflight=true tip_lamports={} cu_price_micro_lamports={} ixs={}",
            cfg.endpoint_mode.as_str(),
            cfg.tip_lamports,
            cfg.cu_price_micro_lamports,
            wrapped_ixs.len()
        );
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": uuid::Uuid::new_v4().to_string(),
            "method": "sendTransaction",
            "params": [encoded, {"encoding": "base64", "skipPreflight": true, "maxRetries": 0}]
        });
        let resp: Value = self
            .sender_client
            .post(&cfg.endpoint)
            .json(&body)
            .send()
            .await?
            .json()
            .await?;
        if let Some(err) = resp.get("error") {
            anyhow::bail!(
                "helius_sender_error:{}",
                sanitize_sender_error(&err.to_string())
            );
        }
        let sig = resp
            .get("result")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("helius_sender_missing_signature"))?;
        Ok(Signature::from_str(sig)?)
    }

    pub async fn send_onchain_diagnostic_skip_preflight(
        &self,
        payer: &Keypair,
        ixs: &[Instruction],
        reason: &str,
    ) -> anyhow::Result<Signature> {
        if self.backend != LiveSendBackend::Rpc {
            anyhow::bail!(
                "LIVE SEND BACKEND unsupported for diagnostic in this build: {}",
                self.backend.as_str()
            );
        }
        let (bh, _last_valid_block_height) = self
            .rpc
            .get_latest_blockhash_with_commitment(self.preflight_commitment)
            .await?;
        let mut tx = Transaction::new_with_payer(ixs, Some(&payer.pubkey()));
        tx.sign(&[payer], bh);
        println!(
            "🧪 ONCHAIN_DIAGNOSTIC_TEST backend={} commitment={} skip_preflight=true reason={}",
            self.backend.as_str(),
            commitment_label(self.preflight_commitment_level),
            reason
        );
        let sig = self
            .rpc
            .send_transaction_with_config(
                &tx,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    preflight_commitment: Some(self.preflight_commitment_level),
                    max_retries: Some(live_send_rpc_max_retries()),
                    ..RpcSendTransactionConfig::default()
                },
            )
            .await?;
        Ok(sig)
    }

    pub async fn wait_terminal_status(
        &self,
        sig: &Signature,
        attempts: usize,
    ) -> anyhow::Result<TxTerminalStatus> {
        for _ in 0..attempts {
            let statuses = self.rpc.get_signature_statuses(&[*sig]).await?;
            if let Some(Some(status)) = statuses.value.first() {
                return Ok(tx_terminal_status_from_error(
                    status.err.as_ref().map(|e| format!("{:?}", e)),
                ));
            }
            sleep(Duration::from_millis(500)).await;
        }

        let statuses = self
            .rpc
            .get_signature_statuses_with_history(&[*sig])
            .await?;
        if let Some(Some(status)) = statuses.value.first() {
            return Ok(tx_terminal_status_from_error(
                status.err.as_ref().map(|e| format!("{:?}", e)),
            ));
        }
        Ok(TxTerminalStatus::TimeoutUnknown)
    }

    #[allow(dead_code)]
    pub async fn wait_confirmed(&self, sig: &Signature, attempts: usize) -> anyhow::Result<()> {
        match self.wait_terminal_status(sig, attempts).await? {
            TxTerminalStatus::Confirmed => Ok(()),
            TxTerminalStatus::Failed(err) => anyhow::bail!("transaction failed: {err}"),
            TxTerminalStatus::TimeoutUnknown => {
                anyhow::bail!("confirmation timeout unknown: {sig}")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeliusSenderEndpointMode {
    SwqosOnly,
    Dual,
}

impl HeliusSenderEndpointMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SwqosOnly => "swqos_only",
            Self::Dual => "dual",
        }
    }
}

#[derive(Debug, Clone)]
pub struct HeliusSenderConfig {
    pub endpoint: String,
    pub endpoint_mode: HeliusSenderEndpointMode,
    pub tip_lamports: u64,
    pub cu_limit: u32,
    pub cu_price_micro_lamports: u64,
}

impl HeliusSenderConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        let endpoint = std::env::var("HELIUS_SENDER_ENDPOINT")
            .unwrap_or_else(|_| "https://sender.helius-rpc.com/fast?swqos_only=true".into());
        let endpoint_mode = helius_sender_endpoint_mode(&endpoint);
        let tip_lamports = env_u64("HELIUS_SENDER_TIP_LAMPORTS", 5_000);
        validate_sender_tip(endpoint_mode, tip_lamports)?;
        Ok(Self {
            endpoint,
            endpoint_mode,
            tip_lamports,
            cu_limit: env_u64("HELIUS_SENDER_CU_LIMIT", 250_000).clamp(50_000, 1_400_000) as u32,
            cu_price_micro_lamports: env_u64("HELIUS_SENDER_CU_PRICE_MICRO_LAMPORTS", 200_000),
        })
    }
}

pub fn helius_sender_endpoint_mode(endpoint: &str) -> HeliusSenderEndpointMode {
    if endpoint.to_ascii_lowercase().contains("swqos_only=true") {
        HeliusSenderEndpointMode::SwqosOnly
    } else {
        HeliusSenderEndpointMode::Dual
    }
}

pub fn validate_sender_tip(
    mode: HeliusSenderEndpointMode,
    tip_lamports: u64,
) -> anyhow::Result<()> {
    let min = match mode {
        HeliusSenderEndpointMode::SwqosOnly => 5_000,
        HeliusSenderEndpointMode::Dual => 200_000,
    };
    if tip_lamports < min {
        anyhow::bail!(
            "HELIUS_SENDER_TIP_LAMPORTS too low for {}: {} < {}",
            mode.as_str(),
            tip_lamports,
            min
        );
    }
    Ok(())
}

pub fn helius_sender_max_per_day() -> usize {
    env_u64("HELIUS_SENDER_MAX_PER_DAY", 2).clamp(0, 10) as usize
}

pub fn wrap_sender_instructions(
    ixs: &[Instruction],
    payer: &Keypair,
    cfg: &HeliusSenderConfig,
) -> anyhow::Result<Vec<Instruction>> {
    if ixs.is_empty() {
        anyhow::bail!("helius_sender_empty_instruction_set");
    }
    let mut out = Vec::with_capacity(ixs.len() + 3);
    out.push(ComputeBudgetInstruction::set_compute_unit_limit(
        cfg.cu_limit,
    ));
    out.push(ComputeBudgetInstruction::set_compute_unit_price(
        cfg.cu_price_micro_lamports,
    ));
    out.extend(ixs.iter().cloned());
    out.push(solana_sdk::system_instruction::transfer(
        &payer.pubkey(),
        &sender_tip_account_for_seed(&ixs[0].program_id.to_string())?,
        cfg.tip_lamports,
    ));
    Ok(out)
}

pub fn sender_tip_account_for_seed(seed: &str) -> anyhow::Result<Pubkey> {
    let accounts = sender_tip_accounts();
    let sum = seed
        .bytes()
        .fold(0usize, |acc, b| acc.wrapping_add(b as usize));
    Pubkey::from_str(accounts[sum % accounts.len()]).map_err(|e| anyhow::anyhow!(e))
}

pub fn sender_tip_accounts() -> &'static [&'static str] {
    &[
        "4ACfpUFoaSD9bfPdeu6DBt89gB6ENTeHBXCAi87NhDEE",
        "D2L6yPZ2FmmmTKPgzaMKdhu6EWZcTpLy1Vhx8uvZe7NZ",
        "9bnz4RShgq1hAnLnZbP8kbgBg1kEmcJBYQq3gQbmnSta",
        "5VY91ws6B2hMmBFRsXkoAAdsPHBJwRfBht4DXox3xkwn",
        "2nyhqdwKcJZR2vcqCyrYsaPVdAnFoJjiksCXJ7hfEYgD",
        "2q5pghRs6arqVjRvT5gfgWfWcHWmw1ZuCzphgd5KfWGJ",
        "wyvPkWjVZz1M8fHQnMMCDTQDbkManefNNhweYk5WkcF",
        "3KCKozbAaF75qEU33jtzozcJ29yJuaLJTy2jFdzUY8bT",
        "4vieeGHPYPG2MmyPRcYjdiDmmhN3ww7hsFNap8pVN3Ey",
        "4TQLFNWK8AovT1gFvda5jfw2oJeRMKEmw7hsFNap8pVN3Ey",
    ]
}

fn sanitize_sender_error(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || " .,:;_=-/()".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .chars()
        .take(240)
        .collect()
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn tx_terminal_status_from_error(error: Option<String>) -> TxTerminalStatus {
    match error {
        Some(err) => TxTerminalStatus::Failed(err),
        None => TxTerminalStatus::Confirmed,
    }
}

#[cfg(test)]
fn tx_terminal_status_from_optional_error(
    error: Option<Option<String>>,
) -> Option<TxTerminalStatus> {
    error.map(tx_terminal_status_from_error)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveSendBackend {
    Rpc,
    HeliusSender,
    PumpPortalLightningLater,
}

impl LiveSendBackend {
    fn as_str(self) -> &'static str {
        match self {
            Self::Rpc => "rpc",
            Self::HeliusSender => "helius_sender",
            Self::PumpPortalLightningLater => "pumpportal_lightning_later",
        }
    }
}

fn live_send_backend() -> LiveSendBackend {
    live_send_backend_from_env_value(std::env::var("LIVE_SEND_BACKEND").ok().as_deref())
}

fn live_send_backend_from_env_value(value: Option<&str>) -> LiveSendBackend {
    match value.unwrap_or("rpc").to_ascii_lowercase().as_str() {
        "helius_sender" => LiveSendBackend::HeliusSender,
        "pumpportal_lightning_later" => LiveSendBackend::PumpPortalLightningLater,
        _ => LiveSendBackend::Rpc,
    }
}

fn live_send_rpc_url_from_env(default_rpc_url: &str) -> String {
    live_send_rpc_url_from_env_value(
        default_rpc_url,
        std::env::var("RPC_SEND_URL").ok().as_deref(),
    )
}

fn live_send_rpc_url_from_env_value(default_rpc_url: &str, send_url: Option<&str>) -> String {
    send_url
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(default_rpc_url)
        .to_string()
}

fn live_send_preflight_commitment() -> CommitmentConfig {
    CommitmentConfig {
        commitment: live_send_preflight_commitment_level_from_env_value(
            std::env::var("LIVE_SEND_PREFLIGHT_COMMITMENT")
                .ok()
                .as_deref(),
        ),
    }
}

fn live_send_preflight_commitment_level_from_env_value(value: Option<&str>) -> CommitmentLevel {
    match value.unwrap_or("processed").to_ascii_lowercase().as_str() {
        "finalized" => CommitmentLevel::Finalized,
        "confirmed" => CommitmentLevel::Confirmed,
        "processed" => CommitmentLevel::Processed,
        _ => CommitmentLevel::Processed,
    }
}

fn commitment_label(commitment: CommitmentLevel) -> &'static str {
    match commitment {
        CommitmentLevel::Processed => "processed",
        CommitmentLevel::Confirmed => "confirmed",
        CommitmentLevel::Finalized => "finalized",
    }
}

fn live_send_rpc_max_retries() -> usize {
    std::env::var("LIVE_SEND_RPC_MAX_RETRIES")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0)
        .min(1)
}

fn live_send_preflight_attempts() -> u64 {
    std::env::var("LIVE_SEND_PREFLIGHT_ATTEMPTS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(3)
        .clamp(1, 3)
}

pub fn is_preflight_6004_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("custom(6004)")
        || lower.contains("custom\":6004")
        || lower.contains("custom:6004")
        || lower.contains("exceededslippage")
        || lower.contains("6004") && lower.contains("instructionerror")
}

fn is_retryable_blockhash_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("blockhashnotfound")
        || lower.contains("blockhash not found")
        || lower.contains("block height exceeded")
        || lower.contains("blockhash expired")
        || lower.contains("expired blockhash")
}

#[cfg(test)]
mod tests {
    use super::{
        commitment_label, helius_sender_endpoint_mode, is_preflight_6004_error,
        is_retryable_blockhash_error, live_send_backend_from_env_value,
        live_send_preflight_attempts, live_send_preflight_commitment_level_from_env_value,
        live_send_rpc_max_retries, live_send_rpc_url_from_env_value, sender_tip_account_for_seed,
        tx_terminal_status_from_optional_error, validate_sender_tip, wrap_sender_instructions,
        HeliusSenderConfig, HeliusSenderEndpointMode, LiveSendBackend, TxTerminalStatus,
    };
    use solana_sdk::commitment_config::CommitmentLevel;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn terminal_status_mapping_handles_confirmed_failed_and_unknown() {
        assert_eq!(
            tx_terminal_status_from_optional_error(Some(None)),
            Some(TxTerminalStatus::Confirmed)
        );
        assert_eq!(
            tx_terminal_status_from_optional_error(Some(Some("InstructionError".into()))),
            Some(TxTerminalStatus::Failed("InstructionError".into()))
        );
        assert_eq!(tx_terminal_status_from_optional_error(None), None);
    }

    #[test]
    fn preflight_6004_errors_are_detected() {
        assert!(is_preflight_6004_error(
            r#"RPC response error -32002: Transaction simulation failed: {"InstructionError":[3,{"Custom":6004}]}"#
        ));
        assert!(is_preflight_6004_error("InstructionError(3, Custom(6004))"));
        assert!(is_preflight_6004_error("ExceededSlippage(6004)"));
        assert!(!is_preflight_6004_error(
            "InstructionError(3, Custom(6001))"
        ));
    }

    #[test]
    fn blockhash_errors_are_retryable() {
        assert!(is_retryable_blockhash_error(
            "RPC response error -32002: Transaction simulation failed: \"BlockhashNotFound\";"
        ));
        assert!(is_retryable_blockhash_error("blockhash not found"));
        assert!(is_retryable_blockhash_error(
            "TransactionExpiredBlockheightExceededError: block height exceeded"
        ));
        assert!(!is_retryable_blockhash_error("ExceededSlippage(6004)"));
    }

    #[test]
    fn live_send_attempts_default_and_clamp() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("LIVE_SEND_PREFLIGHT_ATTEMPTS");
        assert_eq!(live_send_preflight_attempts(), 3);
        std::env::set_var("LIVE_SEND_PREFLIGHT_ATTEMPTS", "1");
        assert_eq!(live_send_preflight_attempts(), 1);
        std::env::set_var("LIVE_SEND_PREFLIGHT_ATTEMPTS", "9");
        assert_eq!(live_send_preflight_attempts(), 3);
        std::env::set_var("LIVE_SEND_PREFLIGHT_ATTEMPTS", "bad");
        assert_eq!(live_send_preflight_attempts(), 3);
        std::env::remove_var("LIVE_SEND_PREFLIGHT_ATTEMPTS");
    }

    #[test]
    fn send_rpc_url_override_falls_back_to_rpc_url() {
        assert_eq!(
            live_send_rpc_url_from_env_value("https://default", None),
            "https://default"
        );
        assert_eq!(
            live_send_rpc_url_from_env_value("https://default", Some("")),
            "https://default"
        );
        assert_eq!(
            live_send_rpc_url_from_env_value("https://default", Some(" https://send ")),
            "https://send"
        );
    }

    #[test]
    fn send_preflight_commitment_defaults_to_processed() {
        assert_eq!(
            live_send_preflight_commitment_level_from_env_value(None),
            CommitmentLevel::Processed
        );
        assert_eq!(
            live_send_preflight_commitment_level_from_env_value(Some("processed")),
            CommitmentLevel::Processed
        );
        assert_eq!(
            live_send_preflight_commitment_level_from_env_value(Some("confirmed")),
            CommitmentLevel::Confirmed
        );
        assert_eq!(
            live_send_preflight_commitment_level_from_env_value(Some("finalized")),
            CommitmentLevel::Finalized
        );
        assert_eq!(
            live_send_preflight_commitment_level_from_env_value(Some("bad")),
            CommitmentLevel::Processed
        );
        assert_eq!(commitment_label(CommitmentLevel::Processed), "processed");
    }

    #[test]
    fn send_backend_is_rpc_unless_explicit_future_backend() {
        assert_eq!(live_send_backend_from_env_value(None), LiveSendBackend::Rpc);
        assert_eq!(
            live_send_backend_from_env_value(Some("rpc")),
            LiveSendBackend::Rpc
        );
        assert_eq!(
            live_send_backend_from_env_value(Some("pumpportal_lightning_later")),
            LiveSendBackend::PumpPortalLightningLater
        );
        assert_eq!(
            live_send_backend_from_env_value(Some("pumpportal")),
            LiveSendBackend::Rpc
        );
    }

    #[test]
    fn rpc_max_retries_defaults_zero_and_clamps() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("LIVE_SEND_RPC_MAX_RETRIES");
        assert_eq!(live_send_rpc_max_retries(), 0);
        std::env::set_var("LIVE_SEND_RPC_MAX_RETRIES", "1");
        assert_eq!(live_send_rpc_max_retries(), 1);
        std::env::set_var("LIVE_SEND_RPC_MAX_RETRIES", "9");
        assert_eq!(live_send_rpc_max_retries(), 1);
        std::env::set_var("LIVE_SEND_RPC_MAX_RETRIES", "bad");
        assert_eq!(live_send_rpc_max_retries(), 0);
        std::env::remove_var("LIVE_SEND_RPC_MAX_RETRIES");
    }
    #[test]
    fn sender_backend_and_endpoint_mode_are_parsed() {
        assert_eq!(
            live_send_backend_from_env_value(Some("helius_sender")),
            LiveSendBackend::HeliusSender
        );
        assert_eq!(
            helius_sender_endpoint_mode("https://sender.helius-rpc.com/fast?swqos_only=true"),
            HeliusSenderEndpointMode::SwqosOnly
        );
        assert_eq!(
            helius_sender_endpoint_mode("https://sender.helius-rpc.com/fast"),
            HeliusSenderEndpointMode::Dual
        );
    }

    #[test]
    fn sender_tip_validation_enforces_mode_minimums() {
        assert!(validate_sender_tip(HeliusSenderEndpointMode::SwqosOnly, 5_000).is_ok());
        assert!(validate_sender_tip(HeliusSenderEndpointMode::SwqosOnly, 4_999).is_err());
        assert!(validate_sender_tip(HeliusSenderEndpointMode::Dual, 200_000).is_ok());
        assert!(validate_sender_tip(HeliusSenderEndpointMode::Dual, 199_999).is_err());
    }

    #[test]
    fn sender_wrapper_adds_compute_budget_and_tip() {
        let payer = solana_sdk::signature::Keypair::new();
        let original = solana_sdk::instruction::Instruction {
            program_id: solana_sdk::pubkey::Pubkey::new_unique(),
            accounts: vec![],
            data: vec![1, 2, 3],
        };
        let cfg = HeliusSenderConfig {
            endpoint: "https://sender.helius-rpc.com/fast?swqos_only=true".into(),
            endpoint_mode: HeliusSenderEndpointMode::SwqosOnly,
            tip_lamports: 5_000,
            cu_limit: 250_000,
            cu_price_micro_lamports: 200_000,
        };
        let wrapped = wrap_sender_instructions(&[original.clone()], &payer, &cfg).unwrap();
        assert_eq!(wrapped.len(), 4);
        assert_eq!(wrapped[2].program_id, original.program_id);
        assert_eq!(wrapped[3].program_id, solana_sdk::system_program::id());
        assert!(sender_tip_account_for_seed("abc").is_ok());
    }
}
