#!/usr/bin/env bash
# Huragan .env validator + secret rotation check
# Reports key lengths and presence without revealing values.
# Run as: bash /opt/huragan_core/scripts/validate_env.sh
set -u
ENV_FILE="${1:-/opt/huragan_core/.env}"
TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)
echo "=== Huragan .env validator @ $TS ==="
echo "file: $ENV_FILE"
if [ ! -f "$ENV_FILE" ]; then
  echo "MISSING"
  exit 1
fi
ls -la "$ENV_FILE"
echo
echo "--- safety ---"
grep -E '^(PAPER_MODE|LIVE_ARMED|LIVE_SEND_ENABLED|SOLANA_PRIVATE_KEY_BASE58)=' "$ENV_FILE" \
  | sed 's/=.*/=REDACTED/'
SOL_LINE=$(grep -E '^SOLANA_PRIVATE_KEY_BASE58=' "$ENV_FILE" | head -1)
if [ -n "$SOL_LINE" ]; then
  echo "WARN: SOLANA_PRIVATE_KEY_BASE58 is set in $ENV_FILE"
fi
echo
echo "--- secrets (length only) ---"
for K in RPC_URL RPC_WS_URL PUMPPORTAL_API_KEY TELEGRAM_BOT_TOKEN TELEGRAM_CHAT_ID; do
  V=$(grep -E "^$K=" "$ENV_FILE" | head -1 | cut -d= -f2-)
  if [ -z "$V" ]; then
    printf "  %-22s NOT_SET\n" "$K"
  else
    printf "  %-22s length=%d\n" "$K" "${#V}"
  fi
done
echo
echo "--- services ---"
for S in migration-sniper.service fresh-momentum.service market-supervisor.timer huragan-monitor-agent.timer huragan-env-audit-alert.timer; do
  ACT=$(systemctl is-active "$S" 2>/dev/null)
  ENB=$(systemctl is-enabled "$S" 2>/dev/null)
  printf "  %-38s active=%s enabled=%s\n" "$S" "$ACT" "$ENB"
done
echo
echo "--- env mtime vs latest secret rotation hint ---"
stat -c '  .env mtime: %y' "$ENV_FILE"
echo "  (Rotated file would have mtime >= today UTC)"
echo
echo "=== done ==="
