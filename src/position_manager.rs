use crate::engine::{self, MigrationTarget};
use crate::state::{LedgerManager, PositionState};
use solana_client::nonblocking::rpc_client::RpcClient;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;

#[derive(Clone)]
pub struct PositionManager {
    rpc_url: String,
    ledger: Arc<LedgerManager>,
}

impl PositionManager {
    pub fn new(rpc_url: String, ledger: Arc<LedgerManager>) -> Self {
        Self { rpc_url, ledger }
    }

    pub async fn monitor_position(&self, mut state: PositionState, target: MigrationTarget) {
        let rpc = RpcClient::new(self.rpc_url.clone());
        let max_hold = std::env::var("MANAGER_MAX_HOLD_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300);
        let started = Instant::now();
        let mut highest_ratio = 1.0f64;

        loop {
            let age = started.elapsed().as_secs();
            let quote =
                engine::build_sell_amm_ixs(&rpc, &target, state.remaining_tokens.max(1), false)
                    .await;
            let ratio = match quote {
                Ok(plan) if state.cost_basis_sol > 0.0 => {
                    (plan.expected_sol_out as f64 / 1e9) / state.cost_basis_sol
                }
                _ => {
                    if age >= max_hold {
                        state.status = "rug_liquidity_drained".into();
                        state.exit_reason = "price_unavailable".into();
                        let _ = self.ledger.save_new_position(&state);
                        return;
                    }
                    sleep(Duration::from_millis(500)).await;
                    continue;
                }
            };
            highest_ratio = highest_ratio.max(ratio);

            let trailing_armed = highest_ratio > 1.10;
            let trailing_hit = trailing_armed && ratio <= highest_ratio * 0.85;
            if trailing_hit || age >= max_hold {
                state.status = if state.remaining_tokens == 0 {
                    "completed"
                } else {
                    "completed_with_dust_small"
                }
                .into();
                state.exit_reason = if trailing_hit {
                    "trailing_stop"
                } else {
                    "max_hold"
                }
                .into();
                state.hold_secs = age;
                let _ = self.ledger.save_new_position(&state);
                return;
            }

            sleep(Duration::from_millis(500)).await;
        }
    }
}
