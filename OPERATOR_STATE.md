# Huragan Operator State

**Last updated:** 2026-06-08
**Updated by:** Codex

## Current Runtime

```
HEAD=051d531 feat: fresh sniper follow 3500 MC shadow v1
build=OK
tests=OK (huragan_core: 49, market_supervisor: 2)
runtime=paper
services=active, active
open_blockers=0
```

## Sniper Follow Status

```
mints_processed=200
trade_events=11821
wallet_scores=563
FOLLOW_SHADOW=18
GOOD_FLIP_SNIPER=35
errors_total=270
threshold_met=NO (need >=20, have 18)
```

## Next Allowed Actions

```
✅ shadow data collection (sniper_follow_backtest.py, fresh_sniper_collector.py)
✅ shadow analysis (sniper_wallet_ranker.py, fresh_sniper_shadow.py)
✅ cargo test / cargo build
✅ git commit / push
✅ canary #18 single 8500 (only after explicit GO by user)
❌ live arm without GO
❌ live without rollback
❌ multi-position
❌ Sender backend without separate plan
❌ Fresh live trading
```

## Blocked Actions

```
live arm (requires GO SINGLE SEND)
restart migration outside rollback/canary arm scripts
```

## Data Status

```
sniper_trade_events.jsonl: present (200 mints)
fresh_sniper_events.jsonl: not yet populated
rate_limit_issues: 429 errors need rpc-sleep >= 0.35s
```
