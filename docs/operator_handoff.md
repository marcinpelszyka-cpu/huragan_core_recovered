# Huragan Operator Handoff

## Purpose

`huragan_ops_lock.py` coordinates Codex, Hermes, and the human operator so only one actor owns operational changes at a time.

## Runtime files

These files are local runtime files and are not committed:

```text
.ops_lock.json
OPERATOR_STATE.md
```

Use `OPERATOR_STATE.template.md` as the initial shape.

## Required precheck

Every agent must run before acting:

```bash
python3 scripts/huragan_ops_lock.py status || true
cat OPERATOR_STATE.md 2>/dev/null || true
```

## Ownership model

Codex owns development:

```text
code, tests, build, commit, push, docs, skill update
```

Hermes owns operations:

```text
monitor, backtest, reports, Telegram outbound, canary runbook
```

Forbidden without explicit live runbook:

```text
live_arm
private_key_insert
LIVE_SEND_ENABLED=true
multi_position
```

## Common commands

Acquire development lock:

```bash
python3 scripts/huragan_ops_lock.py acquire \
  --owner codex \
  --task "deploy patch" \
  --ttl-min 30 \
  --allowed-action build \
  --allowed-action test \
  --forbidden-action live_arm \
  --forbidden-action private_key_insert
```

Acquire Hermes backtest lock:

```bash
python3 scripts/huragan_ops_lock.py acquire \
  --owner hermes \
  --task "sniper follow backtest 500" \
  --ttl-min 180 \
  --allowed-action backtest \
  --allowed-action report \
  --forbidden-action live_arm \
  --forbidden-action private_key_insert
```

Write state:

```bash
python3 scripts/huragan_ops_lock.py write-state \
  --owner codex \
  --next-action sniper_backtest_500 \
  --head "$(git rev-parse --short HEAD)" \
  --build OK \
  --tests OK \
  --services active \
  --key-status KEY_ABSENT \
  --open-blockers 0 \
  --data-status "sniper_follow ready for 500 mint shadow backtest"
```

Release:

```bash
python3 scripts/huragan_ops_lock.py release --owner codex
```

## Sniper follow 500 run

Only after `next_allowed_action=sniper_backtest_500`:

```bash
python3 scripts/huragan_ops_lock.py acquire --owner hermes --task "sniper follow backtest 500" --ttl-min 180
python3 scripts/sniper_follow_backtest.py --limit-mints 500 --rpc-sleep 0.35
```

Report:

```text
processed_mints
trade_events
wallet_scores
GOOD_FLIP_SNIPER count
FOLLOW_SHADOW count
429 error count / pct
top GOOD_FLIP_SNIPER wallets
GO/NO_GO
```

Then update `OPERATOR_STATE.md` and release the lock.
