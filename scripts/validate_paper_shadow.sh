#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:-/opt/huragan_core}"
cd "$ROOT"

echo "=== safe env flags ==="
grep -E '^(PAPER_MODE|LIVE_ARMED|AMM_LIVE_CANARY|MAX_TRADES_PER_RUN|AMM_ADVANCED_GATE_MODE|AMM_MIN_POOL_SOL_FOR_ENTRY_LAMPORTS)=' .env || true

echo
echo "=== no-live assertion ==="
if grep -q '^PAPER_MODE=false' .env || grep -q '^LIVE_ARMED=true' .env || grep -q '^AMM_LIVE_CANARY=true' .env; then
  echo "FAIL: live flag found in .env" >&2
  exit 1
fi
echo "OK: .env is paper/shadow"

echo
echo "=== binaries ==="
test -x target/release/huragan_core
test -x target/release/market_supervisor
ls -lh target/release/huragan_core target/release/market_supervisor

echo
echo "=== supervisor dry run ==="
./target/release/market_supervisor \
  --state ./state.jsonl \
  --live-state ./state.jsonl \
  --window-mins 120 \
  --output ./agents_decision.json \
  --report /tmp/market_supervisor_report.md
python3 -m json.tool agents_decision.json >/dev/null
sed -n '1,120p' agents_decision.json

echo
echo "=== services ==="
systemctl status migration-sniper.service --no-pager -l | sed -n '1,24p' || true
systemctl status fresh-momentum.service --no-pager -l | sed -n '1,24p' || true
systemctl status market-supervisor.timer --no-pager -l | sed -n '1,24p' || true

echo
echo "=== running processes ==="
pgrep -af huragan_core || true

echo
echo "VALIDATION_OK"
