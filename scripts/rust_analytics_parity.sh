#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TMP_DIR="${TMP_DIR:-/tmp/huragan_rust_analytics_parity}"
EPSILON="${EPSILON:-0.000001}"
PY_DIR="$TMP_DIR/py"
RS_DIR="$TMP_DIR/rs"
rm -rf "$TMP_DIR"
mkdir -p "$PY_DIR" "$RS_DIR"

cargo build --release --bin fresh_forward_labeler --bin fresh_shadow_gate --bin bundler_score_report >/dev/null

python3 scripts/fresh_forward_outcome_labeler.py \
  --out "$PY_DIR/fresh_forward_outcomes.jsonl" \
  --summary "$PY_DIR/fresh_forward_outcome_summary.json" \
  --report "$PY_DIR/fresh_forward_outcome_report.md" >/"$PY_DIR/forward.stdout"

target/release/fresh_forward_labeler \
  --out "$RS_DIR/fresh_forward_outcomes.jsonl" \
  --summary "$RS_DIR/fresh_forward_outcome_summary.json" \
  --report "$RS_DIR/fresh_forward_outcome_report.md" >/"$RS_DIR/forward.stdout"

python3 scripts/fresh_shadow_gate_report.py \
  --forward "$PY_DIR/fresh_forward_outcomes.jsonl" \
  --out "$PY_DIR/fresh_shadow_gate_signals.jsonl" \
  --report "$PY_DIR/fresh_shadow_gate_report.md" >/"$PY_DIR/gate.stdout"

target/release/fresh_shadow_gate \
  --forward "$RS_DIR/fresh_forward_outcomes.jsonl" \
  --out "$RS_DIR/fresh_shadow_gate_signals.jsonl" \
  --report "$RS_DIR/fresh_shadow_gate_report.md" >/"$RS_DIR/gate.stdout"

python3 scripts/bundler_score_calibration_report.py \
  --forward "$PY_DIR/fresh_forward_outcomes.jsonl" \
  --summary "$PY_DIR/bundler_score_calibration_summary.json" \
  --report "$PY_DIR/bundler_score_calibration_report.md" >/"$PY_DIR/bundler.stdout"

target/release/bundler_score_report \
  --forward "$RS_DIR/fresh_forward_outcomes.jsonl" \
  --summary "$RS_DIR/bundler_score_calibration_summary.json" \
  --report "$RS_DIR/bundler_score_calibration_report.md" >/"$RS_DIR/bundler.stdout"

python3 - "$TMP_DIR" "$EPSILON" <<'PY'
import json, collections, math, pathlib, sys
base=pathlib.Path(sys.argv[1]); eps=float(sys.argv[2])
def jsonl(p): return [json.loads(x) for x in pathlib.Path(p).read_text().splitlines() if x.strip()]
def load(p): return json.load(open(p))
def counts(rows,k): return dict(collections.Counter(r.get(k,'') for r in rows))
def assert_same(name,a,b):
    if a!=b:
        print(f'{name}: DIFF\nPY={a}\nRS={b}')
        raise SystemExit(2)
    print(f'{name}: OK')
def assert_close(name,a,b):
    if a is None or b is None:
        assert_same(name,a,b); return
    if not math.isclose(float(a), float(b), abs_tol=eps):
        print(f'{name}: DIFF PY={a} RS={b} eps={eps}')
        raise SystemExit(2)

py=jsonl(base/'py/fresh_forward_outcomes.jsonl'); rs=jsonl(base/'rs/fresh_forward_outcomes.jsonl')
assert_same('forward_rows', len(py), len(rs))
assert_same('forward_labels', counts(py,'outcome_label'), counts(rs,'outcome_label'))
for idx,(a,b) in enumerate(zip(py,rs)):
    assert_same(f'forward_mint[{idx}]', a.get('mint'), b.get('mint'))
    assert_same(f'forward_label[{idx}]', a.get('outcome_label'), b.get('outcome_label'))
    for key in ('pnl_30s_pct','pnl_60s_pct','sell_flow_ratio_60s'):
        assert_close(f'{key}[{idx}]', a.get(key), b.get(key))

pyg=jsonl(base/'py/fresh_shadow_gate_signals.jsonl'); rsg=jsonl(base/'rs/fresh_shadow_gate_signals.jsonl')
assert_same('gate_rows', len(pyg), len(rsg))
assert_same('gate_decisions', counts(pyg,'decision'), counts(rsg,'decision'))
assert_same('strong_v2_mints', sorted(r['mint'] for r in pyg if r.get('decision')=='FOLLOW_SHADOW_STRONG_V2'), sorted(r['mint'] for r in rsg if r.get('decision')=='FOLLOW_SHADOW_STRONG_V2'))
assert_same('avoid_forward_dump_mints', sorted(r['mint'] for r in pyg if r.get('decision')=='AVOID_FORWARD_DUMP'), sorted(r['mint'] for r in rsg if r.get('decision')=='AVOID_FORWARD_DUMP'))

if rsg:
    assert_same('live_allowed_gate', {r.get('live_allowed') for r in rsg}, {False})
else:
    print('live_allowed_gate: OK empty')

pyb=load(base/'py/bundler_score_calibration_summary.json'); rsb=load(base/'rs/bundler_score_calibration_summary.json')
for key in ('signals','edges','risk_buckets','decision_proxy','outcome_sources','live_allowed'):
    assert_same(f'bundler_{key}', pyb.get(key), rsb.get(key))
print('RUST_ANALYTICS_PARITY_OK')
PY
