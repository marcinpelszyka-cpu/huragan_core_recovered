use solana_client::nonblocking::rpc_client::RpcClient;
use solana_client::rpc_config::RpcSendTransactionConfig;
use solana_sdk::commitment_config::CommitmentConfig;
use solana_sdk::instruction::Instruction;
use solana_sdk::signature::{Keypair, Signature};
use solana_sdk::signer::Signer;
use solana_sdk::transaction::Transaction;
use std::time::Duration;
use tokio::time::sleep;

pub struct Executor {
    rpc: RpcClient,
}

impl Executor {
    pub fn new(rpc_url: String) -> Self {
        Self {
            rpc: RpcClient::new_with_commitment(rpc_url, CommitmentConfig::confirmed()),
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
        let attempts = live_send_preflight_attempts();
        let mut last_err: Option<anyhow::Error> = None;

        for attempt in 1..=attempts {
            let bh = self.rpc.get_latest_blockhash().await?;
            let mut tx = Transaction::new_with_payer(ixs, Some(&payer.pubkey()));
            tx.sign(&[payer], bh);
            match self
                .rpc
                .send_transaction_with_config(
                    &tx,
                    RpcSendTransactionConfig {
                        skip_preflight: false,
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
    use super::{is_retryable_blockhash_error, live_send_preflight_attempts};
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
}
