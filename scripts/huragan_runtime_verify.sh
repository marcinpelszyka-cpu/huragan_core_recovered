#!/usr/bin/env bash
set -euo pipefail
cd /opt/huragan_core
printf 'head='; git log --oneline -1
printf 'services='; systemctl is-active migration-sniper.service fresh-momentum.service | paste -sd ',' - || true
printf 'mainpid='; systemctl show -p MainPID --value migration-sniper.service || true
printf 'effective='; systemctl show migration-sniper.service -p Environment --value \
  | tr ' ' '\n' \
  | grep -E '^(PAPER_MODE|LIVE_ARMED|LIVE_SEND_ENABLED|LIVE_AUTO_SELL_ENABLED|LIVE_SELL_SEND_ENABLED|ALLOW_PLAINTEXT_PRIVATE_KEY|AMM_LIVE_CANARY|PUMPPORTAL_ENABLED|MAX_TRADES_PER_RUN|BUY_AMOUNT_SOL|AMM_LIVE_BUY_MIN_OUT_BPS|AMM_MIN_POOL_SOL_FOR_ENTRY_LAMPORTS|LIVE_SEND_BACKEND|LIVE_SEND_PREFLIGHT_COMMITMENT|LIVE_ONCHAIN_DIAGNOSTIC_ENABLED|LIVE_ONCHAIN_DIAGNOSTIC_MAX_PER_DAY|HELIUS_SENDER_TIP_LAMPORTS|HELIUS_SENDER_CU_LIMIT|HELIUS_SENDER_CU_PRICE_MICRO_LAMPORTS|HELIUS_SENDER_MAX_PER_DAY|MAX_DAILY_LOSS_SOL|MAX_DAILY_TRADES|MAX_CONSECUTIVE_LOSSES|LIVE_RISK_MANAGER_ENABLED)=' \
  | paste -sd ' ' - || true
printf 'env_file='; grep -E '^(PAPER_MODE|LIVE_ARMED|LIVE_SEND_ENABLED|LIVE_AUTO_SELL_ENABLED|LIVE_SELL_SEND_ENABLED|ALLOW_PLAINTEXT_PRIVATE_KEY|AMM_LIVE_CANARY|PUMPPORTAL_ENABLED|MAX_TRADES_PER_RUN|BUY_AMOUNT_SOL|AMM_LIVE_BUY_MIN_OUT_BPS|RPC_SEND_URL|HELIUS_SENDER_ENDPOINT|LIVE_SEND_BACKEND|LIVE_SEND_PREFLIGHT_COMMITMENT|LIVE_ONCHAIN_DIAGNOSTIC_ENABLED|LIVE_ONCHAIN_DIAGNOSTIC_MAX_PER_DAY|HELIUS_SENDER_TIP_LAMPORTS|HELIUS_SENDER_CU_LIMIT|HELIUS_SENDER_CU_PRICE_MICRO_LAMPORTS|HELIUS_SENDER_MAX_PER_DAY)=' .env \
  | sed -E 's#(RPC_SEND_URL=).*#\1PRESENT#; s#(HELIUS_SENDER_ENDPOINT=).*#\1PRESENT#' \
  | paste -sd ' ' - || true
if grep -q '^SOLANA_PRIVATE_KEY_BASE58=' .env; then echo 'env_key=KEY_PRESENT_BAD'; else echo 'env_key=KEY_ABSENT'; fi
printf 'dropins='; find /etc/systemd/system/migration-sniper.service.d -maxdepth 1 -type f -printf '%f ' 2>/dev/null | sort; echo
printf 'procs='; ps -eo pid,ppid,cmd | awk '/\/opt\/huragan_core\/target\/release\/huragan_core/ && !/awk/ {n++; out=out $0 "|"} END {print n+0 ":" out}'
printf 'open_blockers='; python3 - <<'PY'
import json
latest={}
try:
    f=open('state.jsonl')
except FileNotFoundError:
    print(0); raise SystemExit
for line in f:
    try: r=json.loads(line)
    except Exception: continue
    latest[(r.get('mint',''),r.get('variant_id',''))]=r
print(sum(1 for r in latest.values() if r.get('variant_id')=='Z3' and r.get('status') in ('holding','live_sell_failed_retryable') and int(r.get('remaining_tokens') or 0)>0))
PY
