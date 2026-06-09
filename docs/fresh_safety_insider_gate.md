# Fresh Safety + Insider Gate v1

Shadow-only selection gate for fresh token research. It combines fresh shadow decisions, bundler/mother-wallet graph, forward outcomes, mint authority checks, and top-holder concentration.

## Run

```bash
cd /opt/huragan_core
target/release/fresh_forward_labeler
target/release/fresh_shadow_gate
target/release/bundler_score_report
target/release/fresh_safety_gate
```

Smoke without RPC writes:

```bash
target/release/fresh_safety_gate --limit-mints 20 --dry-run
```

Disable RPC enrichment explicitly:

```bash
target/release/fresh_safety_gate --no-rpc
```

## Outputs

```text
datasets/fresh_safety_signals.jsonl
datasets/fresh_insider_risk_signals.jsonl
datasets/fresh_selection_gate_v1.jsonl
datasets/fresh_selection_gate_v1_summary.json
datasets/fresh_selection_gate_v1_report.md
```

All rows are shadow-only and must have `live_allowed=false`.

## Decision rules

Hard rejects:

```text
active freeze authority
active mint authority
top_5_holders_ex_pool_pct > 30
top_10_holders_ex_pool_pct > 45
shared_mother_count >= 3 and risk_score >= 60
repeated bad mother wallet
forward hard dump/rug or sell_flow_60s >= 0.80
```

`FOLLOW_CANDIDATE` requires a V2 fresh shadow follow signal plus low risk and no safety reject.

LP burn/locker status is recorded as `not_applicable_or_unknown` in V1 and is not a hard reject for Pump AMM.

## Safety

This gate never signs, never sends, never mutates `.env`, and does not authorize canary/live. Canary remains blocked until this gate shows measurable separation between candidates and rejected groups.
