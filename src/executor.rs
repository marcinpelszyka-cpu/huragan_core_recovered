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
        let bh = self.rpc.get_latest_blockhash().await?;
        let mut tx = Transaction::new_with_payer(ixs, Some(&payer.pubkey()));
        tx.sign(&[payer], bh);
        let sig = self
            .rpc
            .send_transaction_with_config(
                &tx,
                RpcSendTransactionConfig {
                    skip_preflight: false,
                    ..RpcSendTransactionConfig::default()
                },
            )
            .await?;
        Ok(sig)
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
