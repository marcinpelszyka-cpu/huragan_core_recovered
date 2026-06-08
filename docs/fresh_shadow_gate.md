# Fresh Shadow Gate v1

Combines sniper-follow and bundler/mother-wallet graph outputs into one shadow-only decision file.

Inputs:

```text
datasets/sniper_follow_signals.jsonl
datasets/fresh_bundle_risk_signals.jsonl
```

Outputs:

```text
datasets/fresh_shadow_gate_signals.jsonl
datasets/fresh_shadow_gate_report.md
```

Decisions:

```text
FOLLOW_SHADOW_STRONG      sniper signal + non-toxic bundler/funding structure
FOLLOW_SHADOW_CANDIDATE   sniper signal present, but bundle confidence is weaker
AVOID_DEV_CLUSTER         toxic shared mother / dev sniper suspect / high risk
UNKNOWN_WAIT              insufficient combined signal
```

Safety:

```text
live_allowed=false for every row
no runtime changes
no wallet/private key access
no canary permission
```

Run:

```bash
python3 scripts/fresh_shadow_gate_report.py
```

Acceptance before any live use:

```text
risk_score not zero-heavy
AVOID_DEV_CLUSTER catches known hard_stop/rug/dust rows
FOLLOW_SHADOW_STRONG has better 30s/60s forward outcomes than baseline
```
