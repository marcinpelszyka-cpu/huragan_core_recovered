# GMGN Shadow Scanner — Cron Job Proposal

## Scanner Status
- Script: `scripts/gmgn_shadow_scanner.py` — fully functional
- Test run: `python3 scripts/gmgn_shadow_scanner.py --once --limit 10` — **PASSED**
- 10 signals written to `gmgn_shadow_signals.jsonl` with 0 errors
- All 10 were SKIP (expected: shadow pool is mostly low-quality memecoins)

## Recommended Cron Entry

```
# GMGN Shadow Scanner — paper-only, every 20 minutes
# Logs: /opt/huragan_core/gmgn_shadow_cron.log
*/20 * * * * cd /opt/huragan_core && /usr/bin/python3 scripts/gmgn_shadow_scanner.py --once --limit 50 >> gmgn_shadow_cron.log 2>&1
```

### Rationale
- **Cadence: every 20 minutes** — hits the sweet spot between 15/30 min. Fresh pairs arrive every block (~400ms), but GMGN API calls take ~8-12s per token. At limit=50 with sequential calls, a batch completes in ~8 minutes worst case. 20 min gives comfortable headroom.
- **Limit: 50** — reasonable. With 55K candidates and 54K already scanned, each run processes ~50 new tokens. Most are SKIP but occasionally a WATCH or ALERT appears.
- **Log file**: `gmgn_shadow_cron.log` — captures stdout (signal table) for audit. The structured data still goes to `gmgn_shadow_signals.jsonl` (appended by the script).

### Alternative: every 30 minutes with limit=100
```
*/30 * * * * cd /opt/huragan_core && /usr/bin/python3 scripts/gmgn_shadow_scanner.py --once --limit 100 >> gmgn_shadow_cron.log 2>&1
```
Fewer API calls per hour, larger batches. Better if rate-limiting is a concern.

## Signal Breakdown (test run, limit=10)
| Signal | Count | Typical Reasons |
|--------|-------|-----------------|
| SKIP   | 10    | thin_liquidity (<$5K), not_renounced, few_holders (<25) |
| WATCH  | 0     | Would need: liq ≥ $5K, renounced, top10 ≤ 40%, rug ≤ 30%, holders ≥ 25 |
| ALERT  | 0     | Would need: WATCH criteria + smart_money_count > 0 + kol_count > 0 |

## What the Scanner Does
1. Reads `fresh_momentum_candidates.jsonl` for new token mints
2. Deduplicates against already-scanned tokens in `gmgn_shadow_signals.jsonl` and `state.jsonl`
3. Calls `gmgn-cli token info` and `gmgn-cli token security` for each mint
4. Applies conservative safety gate:
   - `liquidity_ok`: liquidity ≥ $5,000
   - `security_ok`: renounced + top10 ≤ 40% + rug ≤ 30% + holders ≥ 25
5. Classifies each token as WATCH, SKIP, or ALERT
6. Appends structured JSONL row to `gmgn_shadow_signals.jsonl`

## Hard Constraints (NEVER violated)
- **Paper-only** — `"paper_only": true` in every signal row
- **Read-only** — never trades, never touches live bot, never modifies `.env`
- **GMGN API only** — no on-chain transactions, no wallet access

## Install Command
```bash
crontab -e
# Paste the cron line above
```

## Verification
```bash
# Check recent runs
tail -20 /opt/huragan_core/gmgn_shadow_cron.log

# Count today's signals
grep "$(date +%Y-%m-%d)" /opt/huragan_core/gmgn_shadow_signals.jsonl | wc -l
```
