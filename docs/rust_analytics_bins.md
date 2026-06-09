# Rust Analytics Bins v1

Shadow/offline analytics were ported from Python to Rust for deterministic local dataset processing.

## Scope

Rust bins are offline only. They read local JSONL/CSV-like datasets and write compatible JSONL/JSON/Markdown outputs. They do not touch `.env`, runtime services, wallets, private keys, live flags, canary, or RPC backtests.

Bins:

```bash
target/release/fresh_forward_labeler
target/release/fresh_shadow_gate
target/release/bundler_score_report
```

Python scripts remain the reference fallback until operators explicitly switch runbooks.

## Fault-tolerant JSONL

`src/analytics.rs` uses fault-tolerant JSONL readers:

- missing files return empty rows;
- empty lines are skipped;
- malformed rows are logged to stderr and skipped;
- the process does not abort on one corrupt row.

For typed/streaming use there is `process_jsonl_stream<T, F>()`. For very large raw JSONL blobs there is optional `process_jsonl_parallel<T>()` using Rayon.

## Parity rules

Discrete values must match exactly:

- row counts;
- outcome label counts;
- gate decision counts;
- mint lists for `FOLLOW_SHADOW_STRONG_V2` and `AVOID_FORWARD_DUMP`;
- `live_allowed=false`.

Continuous values use epsilon tolerance:

```text
EPSILON <= 1e-6
```

Covered fields include:

```text
pnl_30s_pct
pnl_60s_pct
sell_flow_ratio_60s
```

## Automated parity check

Run:

```bash
scripts/rust_analytics_parity.sh
```

The script builds Rust bins, runs Python reference and Rust outputs into `/tmp/huragan_rust_analytics_parity`, and compares summaries and key JSONL fields.

Custom tolerance/path:

```bash
EPSILON=0.000001 TMP_DIR=/tmp/huragan_parity scripts/rust_analytics_parity.sh
```

Expected result:

```text
RUST_ANALYTICS_PARITY_OK
```

## Operational rule

Analytics deploy does not require restarting bot services. Runtime must remain paper-only after deploy.
