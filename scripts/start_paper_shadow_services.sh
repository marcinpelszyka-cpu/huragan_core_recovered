#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:-/opt/huragan_core}"
cd "$ROOT"

if grep -q '^PAPER_MODE=false' .env || grep -q '^LIVE_ARMED=true' .env || grep -q '^AMM_LIVE_CANARY=true' .env; then
  echo "Refusing to start: .env is not paper/shadow safe" >&2
  exit 1
fi

systemctl start migration-sniper.service
systemctl start fresh-momentum.service
systemctl enable --now market-supervisor.timer

"$ROOT/scripts/validate_paper_shadow.sh" "$ROOT"
