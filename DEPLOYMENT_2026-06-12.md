# Deployment Instructions — Z3H_500_ONLY Mini-Series

**Date:** 2026-06-12 (after UTC midnight)  
**Goal:** Execute 10 canary trades with gate 500+ SOL only

## Pre-requisites
1. Wait for UTC midnight reset (risk manager daily_trades counter resets)
2. Verify risk manager state: `python3 scripts/strategy_advisor.py`
   - Expected: `daily_trades: 0/10` or similar
   - Should NOT be blocked

## Deployment Steps

### 1. Verify System State
```bash
cd /opt/huragan_core
bash scripts/huragan_runtime_verify.sh
```

Expected output:
- `effective=PAPER_MODE=true LIVE_ARMED=false ...`
- `env_key=KEY_ABSENT`
- `open_live_blockers=0`

### 2. Arm First Canary
```bash
bash scripts/huragan_canary_arm.sh 8500
```

This will:
- Set gate to 500 SOL (already configured in code + script)
- Enable live mode with auto-sell
- Start migration-sniper service

### 3. Monitor Execution
System will auto-rearm after each successful trade. Risk manager will:
- Stop at 10 trades (daily limit)
- Stop if consecutive_losses >= 3
- Stop if daily_pnl <= -0.01 SOL

### 4. Final Report
After 10 canaries or risk manager stop:
```bash
bash scripts/canary_checkpoint.sh
```

## Configuration Summary
- **Gate:** 500 SOL (changed from 100 SOL)
- **Buy amount:** 0.003 SOL (0.01 SOL if explicitly requested)
- **Backend:** Helius Sender (skip_preflight=true)
- **Auto-sell:** enabled
- **Multi-position:** DISABLED
- **Fresh strategy:** shadow only (no live)

## Rollback Procedure
If anything goes wrong:
```bash
bash scripts/huragan_paper_rollback.sh
bash scripts/huragan_runtime_verify.sh
```

## Success Criteria
- 10 canary executed at 500+ SOL gate
- Analyze by bucket: 500+ SOL performance
- Compare with 100 SOL gate results from 2026-06-11
