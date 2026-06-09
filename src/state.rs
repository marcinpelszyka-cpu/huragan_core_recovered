use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct PositionState {
    #[serde(default)]
    pub variant_id: String,
    pub mint: String,
    #[serde(default)]
    pub tx_signature: String,
    #[serde(default)]
    pub total_tokens_bought: u64,
    #[serde(default)]
    pub remaining_tokens: u64,
    #[serde(default)]
    pub cost_basis_sol: f64,
    #[serde(default)]
    pub realized_pnl_sol: f64,
    #[serde(default)]
    pub is_moon_bag: bool,
    #[serde(default)]
    pub status: String,

    #[serde(default)]
    pub token_name: String,
    #[serde(default)]
    pub token_symbol: String,
    #[serde(default)]
    pub virtual_sol_reserves: u64,
    #[serde(default)]
    pub virtual_token_reserves: u64,
    #[serde(default)]
    pub helius_filter_passed: bool,
    #[serde(default)]
    pub helius_filter_reason: String,

    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub pool_state: String,
    #[serde(default)]
    pub base_mint: String,
    #[serde(default)]
    pub quote_mint: String,
    #[serde(default)]
    pub lp_mint: String,
    #[serde(default)]
    pub pool_base_token_account: String,
    #[serde(default)]
    pub pool_quote_token_account: String,

    #[serde(default)]
    pub quote_asset_mint: String,
    #[serde(default)]
    pub quote_symbol: String,
    #[serde(default)]
    pub quote_decimals: u8,
    #[serde(default)]
    pub quote_reserve_raw: u64,
    #[serde(default)]
    pub quote_reserve_ui: f64,
    #[serde(default)]
    pub entry_quote_reserve_raw: u64,
    #[serde(default)]
    pub exit_quote_reserve_raw: u64,
    #[serde(default)]
    pub exit_quote_reserve_ui: f64,
    #[serde(default)]
    pub min_quote_reserve_raw: u64,

    #[serde(default)]
    pub creator_address: String,
    #[serde(default)]
    pub holder_concentration_pct: f64,
    #[serde(default)]
    pub creator_token_count: u64,
    #[serde(default)]
    pub curve_velocity_secs: u64,
    #[serde(default)]
    pub liquidity_safety_score: f64,
    #[serde(default)]
    pub creator_score: f64,
    #[serde(default)]
    pub top10_holder_pct: f64,

    #[serde(default)]
    pub advanced_gate_passed: bool,
    #[serde(default)]
    pub advanced_gate_reason: String,
    #[serde(default)]
    pub advanced_gate_mode: String,

    #[serde(default)]
    pub paper_entry_sol: f64,
    #[serde(default)]
    pub paper_exit_sol: f64,
    #[serde(default)]
    pub gross_pnl_sol: f64,
    #[serde(default)]
    pub estimated_costs_sol: f64,
    #[serde(default)]
    pub net_pnl_sol: f64,
    #[serde(default)]
    pub net_pnl_pct: f64,
    #[serde(default)]
    pub paper_entry_quote: f64,
    #[serde(default)]
    pub paper_exit_quote: f64,
    #[serde(default)]
    pub net_pnl_quote: f64,
    #[serde(default)]
    pub exit_reason: String,
    #[serde(default)]
    pub hold_secs: u64,
    #[serde(default)]
    pub max_drawdown_pct: f64,
    #[serde(default)]
    pub max_favorable_pct: f64,
    #[serde(default)]
    pub paper_buy_family: String,
    #[serde(default)]
    pub paper_sell_family: String,
    #[serde(default)]
    pub last_valid_quote_sol: f64,
    #[serde(default)]
    pub sell_signature: String,
    #[serde(default)]
    pub live_exit_sol: f64,
    #[serde(default)]
    pub live_exit_reason: String,
    #[serde(default)]
    pub live_sell_family: String,
    #[serde(default)]
    pub lifecycle_id: String,
    #[serde(default)]
    pub lifecycle_phase: String,
    #[serde(default)]
    pub buy_attempt_no: u64,
    #[serde(default)]
    pub sell_attempt_no: u64,
    #[serde(default)]
    pub terminal_reason: String,
    #[serde(default)]
    pub rollback_required: bool,

    #[serde(default)]
    pub live_send_backend: String,
    #[serde(default)]
    pub live_send_day: String,
    #[serde(default)]
    pub sender_endpoint_mode: String,
    #[serde(default)]
    pub sender_tip_lamports: u64,
    #[serde(default)]
    pub sender_cu_limit: u32,
    #[serde(default)]
    pub sender_cu_price_micro_lamports: u64,
    #[serde(default)]
    pub diagnostic_label: String,
    #[serde(default)]
    pub diagnostic_day: String,
    #[serde(default)]
    pub excluded_from_stats: bool,
    #[serde(default)]
    pub exited_early_no_momentum: bool,
    #[serde(default)]
    pub exited_rug_guard: bool,
    #[serde(default)]
    pub exited_breakeven_floor: bool,
}

pub struct LedgerManager {
    file_path: PathBuf,
    lock: Mutex<()>,
}

impl LedgerManager {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            file_path: path.into(),
            lock: Mutex::new(()),
        }
    }

    pub fn default() -> Self {
        Self::new("state.jsonl")
    }

    pub fn save_new_position(&self, state: &PositionState) -> anyhow::Result<()> {
        let _guard = self.lock.lock().expect("ledger lock poisoned");
        if let Some(parent) = self.file_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }
        let mut opts = OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut file = opts.open(&self.file_path)?;
        serde_json::to_writer(&mut file, state)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(())
    }

    pub fn get_latest_state(&self) -> anyhow::Result<HashMap<String, PositionState>> {
        let mut latest = HashMap::new();
        if !self.file_path.exists() {
            return Ok(latest);
        }
        for line in BufReader::new(File::open(&self.file_path)?)
            .lines()
            .map_while(Result::ok)
        {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(state) = serde_json::from_str::<PositionState>(&line) {
                latest.insert(state.mint.clone(), state);
            }
        }
        Ok(latest)
    }

    pub fn read_all(&self) -> anyhow::Result<Vec<PositionState>> {
        let mut rows = Vec::new();
        if !self.file_path.exists() {
            return Ok(rows);
        }
        for line in BufReader::new(File::open(&self.file_path)?)
            .lines()
            .map_while(Result::ok)
        {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(state) = serde_json::from_str::<PositionState>(&line) {
                rows.push(state);
            }
        }
        Ok(rows)
    }

    pub fn get_latest_by_mint_variant(
        &self,
    ) -> anyhow::Result<HashMap<(String, String), PositionState>> {
        let mut latest = HashMap::new();
        if !self.file_path.exists() {
            return Ok(latest);
        }
        for line in BufReader::new(File::open(&self.file_path)?)
            .lines()
            .map_while(Result::ok)
        {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(state) = serde_json::from_str::<PositionState>(&line) {
                latest.insert((state.mint.clone(), state.variant_id.clone()), state);
            }
        }
        Ok(latest)
    }
}

pub fn append_jsonl(path: impl AsRef<Path>, value: &Value) -> anyhow::Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let mut file = opts.open(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
