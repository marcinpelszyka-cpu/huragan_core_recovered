#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 root@SERVER_IP [/opt/huragan_core]" >&2
  exit 2
fi

HOST="$1"
DEST="${2:-/opt/huragan_core}"
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o PreferredAuthentications=publickey,password -o IdentitiesOnly=yes)
SSH=(ssh)
RSYNC_RSH=(ssh)
if [[ -n "${SSHPASS:-}" ]] && command -v sshpass >/dev/null 2>&1; then
  SSH=(sshpass -e ssh)
  RSYNC_RSH=(sshpass -e ssh)
fi

echo "==> [1/6] Bootstrap server packages + Rust"
"${SSH[@]}" "${SSH_OPTS[@]}" "$HOST" "bash -s" <<'REMOTE'
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y build-essential pkg-config libssl-dev curl rsync ca-certificates
if ! command -v cargo >/dev/null 2>&1; then
  curl https://sh.rustup.rs -sSf | sh -s -- -y
fi
REMOTE

echo "==> [2/6] Create destination: $DEST"
"${SSH[@]}" "${SSH_OPTS[@]}" "$HOST" "mkdir -p '$DEST'"

echo "==> [3/6] Rsync code without secrets/state/build artifacts"
rsync -az --delete -e "$(printf '%q ' "${RSYNC_RSH[@]}" "${SSH_OPTS[@]}")" \
  --exclude target \
  --exclude .env \
  --exclude '*.jsonl' \
  --exclude agents_decision.json \
  ./ "$HOST:$DEST/"

echo "==> [4/6] Create safe .env if missing"
"${SSH[@]}" "${SSH_OPTS[@]}" "$HOST" "cd '$DEST' && if [ ! -f .env ]; then cp .env.example .env; fi && \
  python3 - <<'PY'
from pathlib import Path
p=Path('.env')
s=p.read_text()
repls={
 'PAPER_MODE=':'PAPER_MODE=true',
 'LIVE_ARMED=':'LIVE_ARMED=false',
 'AMM_LIVE_CANARY=':'AMM_LIVE_CANARY=false',
 'MAX_TRADES_PER_RUN=':'MAX_TRADES_PER_RUN=1',
}
lines=[]
for line in s.splitlines():
    done=False
    for prefix,value in repls.items():
        if line.startswith(prefix):
            lines.append(value); done=True; break
    if not done:
        lines.append(line)
p.write_text('\\n'.join(lines)+'\\n')
PY"

echo "==> [5/6] Build release binaries"
"${SSH[@]}" "${SSH_OPTS[@]}" "$HOST" "bash -lc 'cd \"$DEST\" && cargo build --release --bin huragan_core && cargo build --release --bin market_supervisor'"

echo "==> [6/6] Install systemd units but do not enable live"
"${SSH[@]}" "${SSH_OPTS[@]}" "$HOST" "cd '$DEST' && \
  cp systemd/migration-sniper.service /etc/systemd/system/ && \
  cp systemd/fresh-momentum.service /etc/systemd/system/ && \
  cp systemd/market-supervisor.service /etc/systemd/system/ && \
  cp systemd/market-supervisor.timer /etc/systemd/system/ && \
  systemctl daemon-reload && \
  systemctl reset-failed migration-sniper.service fresh-momentum.service market-supervisor.service market-supervisor.timer || true"

echo
echo "Deploy complete on $HOST:$DEST"
echo
echo "Next manual step: edit $DEST/.env on the server and fill RPC_URL/RPC_WS_URL/PUMPPORTAL_API_KEY."
echo "Then start safe paper services:"
echo "  systemctl start migration-sniper.service"
echo "  systemctl start fresh-momentum.service"
echo "  systemctl enable --now market-supervisor.timer"
echo
echo "Validation:"
echo "  $DEST/scripts/validate_paper_shadow.sh"
