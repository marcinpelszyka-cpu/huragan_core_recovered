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
FOLLOW_SHADOW_STRONG      sniper signal + follow_score >= 65 + risk_score < 45
FOLLOW_SHADOW_CANDIDATE   sniper signal + follow_score >= 45 + risk_score < 60
AVOID_DEV_CLUSTER         risk_score >= 70, DEV_SNIPER_SUSPECT, or repeated bad mother
UNKNOWN_WAIT              insufficient combined signal or needs more outcome validation
```

Safety:

```text
live_allowed=false for every row
no runtime changes
no wallet/private key access
no canary permission
Wallet API optional/disabled until Helius enables beta access
GTFA graph is the default source for funding/mother-wallet scoring
```

Run:

```bash
python3 scripts/fresh_shadow_gate_report.py
python3 scripts/bundler_score_calibration_report.py
```

Acceptance before any live use:

```text
risk_score not zero-heavy for early clusters
AVOID_DEV_CLUSTER catches known hard_stop/rug/dust rows
FOLLOW_SHADOW_STRONG remains small/selective
FOLLOW_SHADOW_CANDIDATE excludes high-risk shared mother clusters
```
