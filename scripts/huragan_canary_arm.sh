#!/usr/bin/env bash
set -euo pipefail
cd /opt/huragan_core
BPS=${1:-8500}
WALLET_FILE=${HURAGAN_CANARY_WALLET_FILE:-/root/.huragan_wallets/huragan_new_wallet_20260604_003229.env}
case "$BPS" in
  ''|*[!0-9]*) echo "invalid bps: $BPS" >&2; exit 2;;
esac
if [ "$BPS" -lt 1000 ] || [ "$BPS" -gt 10000 ]; then
  echo "invalid bps range: $BPS" >&2; exit 2
fi
if [ ! -r "$WALLET_FILE" ]; then
  echo "wallet file not readable: $WALLET_FILE" >&2; exit 3
fi
if ! grep -q '^SOLANA_PRIVATE_KEY_BASE58=' "$WALLET_FILE"; then
  echo "wallet file lacks SOLANA_PRIVATE_KEY_BASE58" >&2; exit 3
fi
if ! grep -q '^RPC_SEND_URL=' .env; then
  echo "RPC_SEND_URL missing in .env" >&2; exit 4
fi
OPEN=$(python3 - <<'PY'
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
)
if [ "$OPEN" != "0" ]; then
  echo "open live blockers=$OPEN; refusing new canary" >&2; exit 5
fi
TS=$(date -u +%Y%m%d_%H%M%S)
mkdir -p backups
cp -a .env "backups/env_before_canary_arm_${TS}.env"
cp -a state.jsonl "backups/state_before_canary_arm_${TS}.jsonl" 2>/dev/null || true
# Runtime secrets must not live in .env. Keep .env paper-safe; live uses EnvironmentFile below.
python3 - <<'PY'
from pathlib import Path
p=Path('.env')
lines=p.read_text().splitlines()
out=[]
for line in lines:
    if line.split('=',1)[0] == 'SOLANA_PRIVATE_KEY_BASE58':
        continue
    out.append(line)
p.write_text('\n'.join(out)+'\n')
PY
install -d -m 755 /etc/systemd/system/migration-sniper.service.d
cat > /etc/systemd/system/migration-sniper.service.d/90-live-canary.conf <<EOF
[Service]
EnvironmentFile=$WALLET_FILE
Environment=PAPER_MODE=false
Environment=LIVE_ARMED=true
Environment=LIVE_SEND_ENABLED=true
Environment=LIVE_AUTO_SELL_ENABLED=true
Environment=LIVE_SELL_SEND_ENABLED=true
Environment=ALLOW_PLAINTEXT_PRIVATE_KEY=true
Environment=AMM_LIVE_CANARY=true
Environment=LIVE_VARIANT=Z3
Environment=MAX_TRADES_PER_RUN=1
Environment=BUY_AMOUNT_SOL=0.003
Environment=AMM_LIVE_BUY_MIN_OUT_BPS=$BPS
Environment=AMM_LIVE_SELL_SLIPPAGE_BPS=8000
Environment=PUMPPORTAL_ENABLED=false
Environment=HELIUS_MIGRATION_ENABLED=true
Environment=MIGRATION_CAPTURE_MODE=false
Environment=JITO_TIP_LAMPORTS=0
Environment=EMERGENCY_JITO_TIP_LAMPORTS=0
Environment=AMM_MIN_POOL_SOL_FOR_ENTRY_LAMPORTS=2000000000
Environment=LIVE_SEND_BACKEND=rpc
Environment=LIVE_SEND_PREFLIGHT_COMMITMENT=processed
Environment=LIVE_ONCHAIN_DIAGNOSTIC_ENABLED=true
Environment=LIVE_ONCHAIN_DIAGNOSTIC_MAX_PER_DAY=2
EOF
systemctl daemon-reload
systemctl reset-failed migration-sniper.service || true
systemctl restart migration-sniper.service
sleep 3
/opt/huragan_core/scripts/huragan_runtime_verify.sh
