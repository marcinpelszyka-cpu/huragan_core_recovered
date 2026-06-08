# Sniper Follow Layer v1

## Purpose

The Sniper Follow Layer identifies which wallets consistently achieve profitable entries on new Pump AMM pools. Rather than blindly copying every early buyer, we:

1. Collect historical trade events via Helius `getTransactionsForAddress`
2. Rank wallets by forward PnL, hold quality, and dump behavior
3. Generate per-token sniper-follow signals when ≥2 high-quality snipers enter early

This is an **observation layer**, not a live execution path. It runs in shadow mode and does NOT place trades.

## Architecture

```
Helius gTFA → sniper_trade_events.jsonl → wallet ranker → scores.csv/jsonl
                                                      → signals.jsonl
                                                      
Live Helius WS → migration target → sniper_shadow.rs → sniper_follow_shadow.jsonl
```

## How It Works

### 1. Data Collection (`sniper_follow_backtest.py`)

Queries `getTransactionsForAddress` for each pool_state from completed Z3 pools.

For each transaction:
- Parses `preTokenBalances` / `postTokenBalances` for per-account token deltas
- Parses `preBalances` / `postBalances` for SOL deltas
- Excludes pool vaults, fee vaults, system/program accounts
- Labels side: `buy` (positive token delta) or `sell` (negative)

Output: `datasets/sniper_trade_events.jsonl`

### 2. Wallet Ranking (`sniper_wallet_ranker.py`)

For each wallet across all observed mints:
- Computes forward PnL at 10s/30s/60s windows (via proportional sell approximation)
- Computes hold quality (weighted avg of hold% at 10s/30s/60s)
- Flags fast dumpers (≥70% sold within 10s)
- Computes composite score

Categories:

| Category | Threshold |
|----------|-----------|
| GOOD_SNIPER | score ≥ 50, fast_dump_rate < 30%, ≥2 mints seen |
| FAST_DUMPER | fast_dump_rate ≥ 70% |
| DEV_SNIPER_SUSPECT | rug_rate ≥ 50% |
| UNKNOWN | all others |

### 3. Signal Generation

For each token/mint, generates a sniper-follow signal if:
- ≥2 GOOD_SNIPER wallets entered within 10s of launch
- Combined buy ≥ 0.03 SOL
- Cohort hold_pct at 10s ≥ 50%

### 4. Live Shadow (Rust)

`src/sniper_shadow.rs` runs alongside Z3, observing migration targets:
- Fetches early transactions via gTFA on detected pools
- Checks buyers against known GOOD_SNIPER wallets
- Writes `sniper_follow_shadow.jsonl` with signal decisions
- NEVER buys, NEVER sells, NEVER modifies Z3 behavior

## What Is NOT Activated

- ❌ No sniper-gated Z3 entries yet
- ❌ No copying of snipers
- ❌ No live execution
- ✅ Shadow observation only

## When to Activate Z3 Integration

Only after backtest shows:

- Sample ≥ 50 tokens
- GOOD_SNIPER signal count ≥ 20
- Forward PnL of signaled tokens > baseline Z3
- Rug rate after signal < baseline Z3

Then add env:

```env
Z3_REQUIRE_SNIPER_FOLLOW_SIGNAL=true
```

Which gates Z3 entries behind:

```
anti-rug gate PASS
entry stability gate PASS
sniper_follow_signal = true
```

## API: Helius getTransactionsForAddress

Endpoint: `POST https://beta.helius-rpc.com/?api-key=KEY`

Key features:
- Returns up to 1000 full transactions per call
- `sortOrder=desc` with `paginationToken` for cursor-based pagination
- `transactionDetails=full` includes pre/post token balances
- `filters.status=succeeded` for confirmed transactions only
- `tokenAccounts=balanceChanged` includes ATA transactions

### Token Delta Extraction

From `meta.preTokenBalances` and `meta.postTokenBalances`:
- `accountIndex` + `mint` = unique key per token position
- `owner` = wallet controlling the token account (NOT signer)
- `uiTokenAmount.amount` = raw token amount as string

**Important**: `owner` is the token account authority, not necessarily the transaction signer. This is correct for detecting who received/sent tokens.

## Data Fields

### sniper_trade_events.jsonl

```json
{
  "mint": "TOKEN_MINT",
  "pool_state": "POOL_STATE",
  "signature": "TX_SIG",
  "slot": 123456789,
  "block_time": 1712345678,
  "owner": "WALLET_OWNER",
  "token_delta_raw": 5000000000,
  "quote_delta_sol": 0.003,
  "side": "buy"
}
```

### sniper_wallet_scores.jsonl

```json
{
  "owner": "WALLET",
  "mints_seen": 5,
  "total_buy_sol": 0.250,
  "avg_hold_pct_10s": 0.85,
  "hold_quality": 78.5,
  "fast_dump_rate": 0.10,
  "score": 62.3,
  "category": "GOOD_SNIPER"
}
```

### sniper_follow_signals.jsonl

```json
{
  "mint": "TOKEN_MINT",
  "signal": true,
  "good_sniper_count": 3,
  "total_good_sniper_buy_sol": 0.080,
  "cohort_hold_pct_10s": 0.91,
  "reason": "signal"
}
```

## Usage

```bash
# Backfill trade events from recent Z3 pools
python3 scripts/sniper_follow_backtest.py --limit 20

# Rank wallets and generate signals
python3 scripts/sniper_wallet_ranker.py

# Self-test both
python3 scripts/sniper_follow_backtest.py --self-test
python3 scripts/sniper_wallet_ranker.py --self-test
```

## Safety

- Read-only: no signing, no sending, no .env modification
- Runs on existing Helius RPC key
- All output goes to `datasets/` directory
- Never trades, never arms live
