# Canary Series 2026-06-11 — Final Report

## Executive Summary
- **Date**: 2026-06-11 (UTC)
- **Gate**: 100 SOL
- **Canary executed today**: 10/10 (daily_trade_limit reached)
- **Status**: STOPPED by Risk Manager (expected behavior)

## Performance
- **Win rate**: 6/10 = 60%
- **Net PnL**: +0.000016 SOL
- **Avg win**: +0.001 SOL
- **Avg loss**: -0.002 SOL

## Exit Reasons Distribution
- max_hold: 4 (40%)
- early_no_momentum: 4 (40%)
- hard_stop: 2 (20%)
- profit_protect: 0 (0%)

## By Pool Size Bucket
| Bucket | Trades | WR | Net PnL | Hard Stop | Max Hold |
|--------|--------|-----|---------|-----------|----------|
| 100-200 SOL | 1 | 100% | +0.0006 | 0 | 1 |
| 200-500 SOL | 5 | 40% | -0.004 | 1 | 2 |
| 500+ SOL | 4 | 75% | +0.003 | 1 | 2 |

## Key Observations
1. **500+ SOL bucket performs best**: 75% WR, positive PnL
2. **200-500 SOL underperforms**: 40% WR, negative PnL due to hard_stop
3. **tail_z3 (max_hold) = 4 wins** vs early_no_momentum = 4 neutral/small wins
4. **Risk manager worked correctly** — stopped at 10/10 trades

## Decisions for 2026-06-12
1. **Z3H_500_ONLY mini-series**: 10 canary, gate 500+ SOL only
2. **Parallel paper replay**: tail_z3 on historical reserve >=500 data
3. **Fresh/sniper**: stay shadow, no live until real FOLLOW_CANDIDATE signals

## Risk Manager State (End of Day)
- daily_trades: 10/10 (limit reached)
- consecutive_losses: 1 (safe)
- daily_pnl: -0.0006 SOL (safe)
- open_blockers: 0
- **Action**: No override — wait for UTC midnight reset
