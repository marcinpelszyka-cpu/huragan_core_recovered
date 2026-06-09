#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

echo "== secret hygiene =="
if grep -Eq '^SOLANA_PRIVATE_KEY_BASE58=.' .env.example; then
  echo ".env.example must not contain a private key placeholder/value" >&2
  exit 2
fi
if rg -n --hidden \
  'ghp_|github_pat_|BEGIN (RSA|OPENSSH|PRIVATE)|SOLANA_PRIVATE_KEY_BASE58=[1-9A-HJ-NP-Za-km-z]{40,}|PUMPPORTAL_API_KEY=[^P]|TELEGRAM_BOT_TOKEN=[0-9]+:' \
  --glob '!target/**' \
  --glob '!.git/**' \
  --glob '!datasets/**' \
  --glob '!*.jsonl' \
  --glob '!scripts/__pycache__/**' \
  --glob '!scripts/huragan_ci_check.sh' \
  .; then
  echo "potential real secret found" >&2
  exit 2
fi

echo "== python static/self-tests =="
python3 -m py_compile \
  scripts/backfill_gtfa_dataset.py \
  scripts/bundler_funding_backtest.py \
  scripts/bundler_score_calibration_report.py \
  scripts/fresh_forward_outcome_labeler.py \
  scripts/fresh_shadow_gate_report.py \
  scripts/fresh_sniper_collector.py \
  scripts/fresh_sniper_shadow.py \
  scripts/huragan_monitor_agent.py \
  scripts/huragan_ops_lock.py \
  scripts/sniper_follow_backtest.py \
  scripts/sniper_wallet_ranker.py \
  scripts/z3_outcome_audit.py

python3 scripts/backfill_gtfa_dataset.py --self-test
python3 scripts/bundler_funding_backtest.py --self-test
python3 scripts/fresh_forward_outcome_labeler.py --self-test
python3 scripts/fresh_shadow_gate_report.py --self-test
python3 scripts/fresh_sniper_collector.py --self-test
python3 scripts/fresh_sniper_shadow.py --self-test
python3 scripts/sniper_follow_backtest.py --self-test
python3 scripts/sniper_wallet_ranker.py --self-test

echo "== rust tests =="
cargo test --release --bin huragan_core
cargo test --release --bin market_supervisor
cargo test --release --bin fresh_forward_labeler
cargo test --release --bin fresh_shadow_gate
cargo test --release --bin bundler_score_report
cargo test --release --bin fresh_safety_gate

echo "== rust build =="
cargo build --release \
  --bin huragan_core \
  --bin market_supervisor \
  --bin amm_sell_preflight \
  --bin fresh_forward_labeler \
  --bin fresh_shadow_gate \
  --bin bundler_score_report \
  --bin fresh_safety_gate

echo "HURAGAN_CI_CHECK_OK"
