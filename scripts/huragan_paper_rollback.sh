#!/usr/bin/env bash
set -euo pipefail
cd /opt/huragan_core
TS=$(date -u +%Y%m%d_%H%M%S)
mkdir -p backups
cp -a .env "backups/env_before_paper_rollback_${TS}.env"
python3 - <<'PY'
from pathlib import Path
p=Path('.env')
lines=p.read_text().splitlines() if p.exists() else []
setvals={
 'PAPER_MODE':'true',
 'LIVE_ARMED':'false',
 'LIVE_SEND_ENABLED':'false',
 'LIVE_AUTO_SELL_ENABLED':'false',
 'LIVE_SELL_SEND_ENABLED':'false',
 'ALLOW_PLAINTEXT_PRIVATE_KEY':'false',
 'AMM_LIVE_CANARY':'false',
 'AMM_LIVE_BUY_MIN_OUT_BPS':'9000',
 'LIVE_SEND_BACKEND':'rpc',
}
remove={'SOLANA_PRIVATE_KEY_BASE58'}
out=[]; seen=set()
for line in lines:
    if not line or line.lstrip().startswith('#') or '=' not in line:
        out.append(line); continue
    k=line.split('=',1)[0]
    if k in remove:
        continue
    if k in setvals:
        out.append(f'{k}={setvals[k]}'); seen.add(k)
    else:
        out.append(line)
for k,v in setvals.items():
    if k not in seen:
        out.append(f'{k}={v}')
p.write_text('\n'.join(out)+'\n')
PY
install -d -m 755 /etc/systemd/system/migration-sniper.service.d
rm -f /etc/systemd/system/migration-sniper.service.d/90-live-canary.conf
cat > /etc/systemd/system/migration-sniper.service.d/10-paper-mode.conf <<'EOF'
[Service]
Environment=PAPER_MODE=true
Environment=LIVE_ARMED=false
Environment=LIVE_SEND_ENABLED=false
Environment=LIVE_AUTO_SELL_ENABLED=false
Environment=LIVE_SELL_SEND_ENABLED=false
Environment=ALLOW_PLAINTEXT_PRIVATE_KEY=false
Environment=AMM_LIVE_CANARY=false
Environment=LIVE_SEND_BACKEND=rpc
EOF
systemctl daemon-reload
systemctl reset-failed migration-sniper.service || true
systemctl restart migration-sniper.service
sleep 3
/opt/huragan_core/scripts/huragan_runtime_verify.sh
