# Fresh Forward Outcome Labeling v1

Shadow-only labeler for `FOLLOW_SHADOW_STRONG` and `FOLLOW_SHADOW_CANDIDATE` decisions.

Inputs:

```text
datasets/fresh_shadow_gate_signals.jsonl
datasets/sniper_trade_events.jsonl
datasets/fresh_bundle_risk_signals.jsonl
datasets/sniper_follow_signals.jsonl
```

Outputs:

```text
datasets/fresh_forward_outcomes.jsonl
datasets/fresh_forward_outcome_report.md
datasets/fresh_forward_outcome_summary.json
```

Run:

```bash
python3 scripts/fresh_forward_outcome_labeler.py
python3 scripts/bundler_score_calibration_report.py
```

Labels:

```text
forward_win_30s
forward_win_60s
flat_or_noise
hard_dump_30s
hard_dump_60s
rug_or_liquidity_collapse
no_trade_data
insufficient_price_data
not_evaluated
```

Rules:

```text
win >= +25%
hard dump <= -40%
rug/liquidity collapse <= -80% or severe sell-flow collapse
flat/noise between roughly -20% and +25%
```

Safety:

```text
shadow-only
live_allowed=false
no private key
no runtime changes
no Wallet API
no canary authorization
```
