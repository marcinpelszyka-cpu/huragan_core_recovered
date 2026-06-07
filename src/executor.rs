use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_sdk::commitment_config::{CommitmentConfig, CommitmentLevel};
use solana_sdk::instruction::Instruction;
use solana_sdk::signature::{Keypair, Signature};
use solana_sdk::signer::Signer;
use solana_sdk::transaction::Transaction;
use std::time::Duration;
use tokio::time::sleep;

pub struct Executor {
    rpc: RpcClient,
    backend: LiveSendBackend,
    preflight_commitment: CommitmentConfig,
    preflight_commitment_level: CommitmentLevel,
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

    #[allow(dead_code)]
    pub async fn send_skip_preflight(
        &self,
        payer: &Keypair,
        ixs: &[Instruction],
    ) -> anyhow::Result<Signature> {
        let bh = self.rpc.get_latest_blockhash().await?;
        let mut tx = Transaction::new_with_payer(ixs, Some(&payer.pubkey()));
        tx.sign(&[payer], bh);
        let sig = self
            .rpc
            .send_transaction_with_config(
                &tx,
                RpcSendTransactionConfig {
                    skip_preflight: true,
                    ..RpcSendTransactionConfig::default()
                },
            )
            .await?;
        Ok(sig)
    }

    pub async fn wait_confirmed(&self, sig: &Signature, attempts: usize) -> anyhow::Result<()> {
        for _ in 0..attempts {
            let statuses = self.rpc.get_signature_statuses(&[*sig]).await?;
            if let Some(Some(status)) = statuses.value.first() {
                if status.err.is_none() {
                    return Ok(());
                }
                anyhow::bail!("transaction failed: {:?}", status.err);
            }
            sleep(Duration::from_millis(500)).await;
        }
        anyhow::bail!("confirmation timeout: {}", sig);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveSendBackend {
    Rpc,
    PumpPortalLightningLater,
}

impl LiveSendBackend {
    fn as_str(self) -> &'static str {
        match self {
            Self::Rpc => "rpc",
            Self::PumpPortalLightningLater => "pumpportal_lightning_later",
        }
    }
}

fn live_send_backend() -> LiveSendBackend {
    live_send_backend_from_env_value(std::env::var("LIVE_SEND_BACKEND").ok().as_deref())
}

fn live_send_backend_from_env_value(value: Option<&str>) -> LiveSendBackend {
    match value.unwrap_or("rpc").to_ascii_lowercase().as_str() {
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
        commitment_label, is_retryable_blockhash_error, live_send_backend_from_env_value,
        live_send_preflight_attempts, live_send_preflight_commitment_level_from_env_value,
        live_send_rpc_max_retries, live_send_rpc_url_from_env_value, LiveSendBackend,
    };
    use solana_sdk::commitment_config::CommitmentLevel;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

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
}
