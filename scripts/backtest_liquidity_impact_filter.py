#!/usr/bin/env python3
"""
Offline liquidity/impact filter backtest — Candidate D from parallel strategy plan.

Reads state.jsonl (read-only). Groups by quote_reserve_ui buckets,
splits by variant (Z3, Z3.1, Z3H_SHADOW), and by Z3H mode (baseline_z31 vs tail_z3).
Outputs a markdown report.

No live, no config changes, no restarts.
"""
import json, statistics, math, collections
from pathlib import Path
from datetime import datetime, timezone

STATE = Path('/opt/huragan_core/state.jsonl')
NOW = datetime.now(timezone.utc).strftime('%Y-%m-%d')
REPORT = Path(f'/opt/huragan_core/reports/liquidity_impact_filter_backtest_{NOW}.md')
REPORT.parent.mkdir(parents=True, exist_ok=True)

VARIANTS = ['Z3', 'Z3.1', 'Z3H_SHADOW']
Z3H_MODES = ['baseline_z31', 'tail_z3']

# ── helper functions (same patterns as other backtest scripts) ──
def f(x, default=0.0):
    """Safe float conversion."""
    if x is None or x == '':
        return default
    try:
        v = float(x)
        if math.isnan(v) or math.isinf(v):
            return default
        return v
    except (ValueError, TypeError):
        return default

def med(xs):
    return statistics.median(xs) if xs else 0.0

def avg(xs):
    return sum(xs) / len(xs) if xs else 0.0

def q(xs, p):
    """Quantile, linear interpolation."""
    if not xs:
        return 0.0
    xs = sorted(xs)
    idx = (len(xs) - 1) * p
    lo = math.floor(idx)
    hi = math.ceil(idx)
    if lo == hi:
        return xs[lo]
    return xs[lo] * (hi - idx) + xs[hi] * (idx - lo)

def mdd(sol_vals):
    """Maximum drawdown from equity curve."""
    eq = 0.0
    peak = 0.0
    worst = 0.0
    for v in sol_vals:
        eq += v
        if eq > peak:
            peak = eq
        dd = peak - eq
        if dd > worst:
            worst = dd
    return worst

# ── buckets ──
BUCKETS = [
    ('0-25',   0,   25),
    ('25-50',  25,  50),
    ('50-75',  50,  75),
    ('75-100', 75, 100),
    ('100-150',100,150),
    ('150-300',150,300),
    ('300+',   300, float('inf')),
]

def bucket_for(qr):
    for label, lo, hi in BUCKETS:
        if lo <= qr < hi:
            return label
    return '300+'

# ── load rows ──
rows = []
with STATE.open() as fh:
    for i, line in enumerate(fh, 1):
        if not line.strip():
            continue
        try:
            r = json.loads(line)
        except Exception:
            continue
        # variant filtering
        v = r.get('variant_id') or r.get('variant') or ''
        if v not in VARIANTS:
            continue
        if r.get('status') != 'paper_completed':
            continue
        if r.get('excluded_from_stats') is True:
            continue

        qr = f(r.get('quote_reserve_ui'))
        er = f(r.get('entry_quote_reserve_raw'))
        mr = f(r.get('min_quote_reserve_raw'))

        rows.append({
            '_line': i,
            'variant': v,
            'mint': r.get('mint') or '',
            'pool': r.get('pool_state') or '',
            'quote_reserve_ui': qr,
            'entry_quote_reserve_raw': er,
            'min_quote_reserve_raw': mr,
            'pnl_pct': f(r.get('net_pnl_pct')),
            'pnl_sol': f(r.get('net_pnl_sol')),
            'mfe': f(r.get('max_favorable_pct')),
            'mdd_pct': f(r.get('max_drawdown_pct')),
            'hold': f(r.get('hold_secs')),
            'exit_reason': r.get('exit_reason') or 'unknown',
            'z3h_mode': r.get('z3h_selected_mode') or '',
        })

print(f"Loaded {len(rows)} completed rows (variants: {VARIANTS})")

# ── summary function ──
def summarize(records):
    """Return dict summary for a set of records."""
    n = len(records)
    if n == 0:
        return {
            'n': 0, 'wr': 0.0, 'avg': 0.0, 'med': 0.0,
            'p25': 0.0, 'p75': 0.0, 'total_sol': 0.0, 'eq_mdd_sol': 0.0,
            'avg_loss': 0.0, 'avg_win': 0.0, 'loss_n': 0, 'win_n': 0,
            'price_unavailable': 0, 'invalid_quote': 0,
            'mfe_avg': 0.0, 'mfe_med': 0.0,
        }
    pn = [r['pnl_pct'] for r in records]
    sol = [r['pnl_sol'] for r in records]
    mfes = [r['mfe'] for r in records]
    wins = [x for x in pn if x > 0]
    losses = [x for x in pn if x < 0]
    pu = sum(1 for r in records if r['exit_reason'] == 'price_unavailable')
    iq = sum(1 for r in records if r['exit_reason'] == 'invalid_quote')
    return {
        'n': n,
        'wr': sum(1 for x in pn if x > 0) / n * 100.0,
        'avg': avg(pn),
        'med': med(pn),
        'p25': q(pn, 0.25),
        'p75': q(pn, 0.75),
        'total_sol': sum(sol),
        'eq_mdd_sol': mdd(sol),
        'avg_loss': avg(losses),
        'avg_win': avg(wins),
        'loss_n': len(losses),
        'win_n': len(wins),
        'price_unavailable': pu,
        'invalid_quote': iq,
        'mfe_avg': avg(mfes),
        'mfe_med': med(mfes),
    }

# ── fmt for markdown table row ──
def fmt_row(label, s, extra=''):
    return (
        f"| {label:<14} | {s['n']:>5d} | {s['wr']:>6.2f}% | {s['avg']:>8.3f}% | {s['med']:>8.3f}% "
        f"| {s['p25']:>8.3f}% | {s['p75']:>8.3f}% | {s['total_sol']:>12.6f} | {s['eq_mdd_sol']:>12.6f} "
        f"| {s['avg_loss']:>8.3f}% | {s['avg_win']:>8.3f}% | {s['loss_n']:>5d} | {s['win_n']:>5d} "
        f"| {s['price_unavailable']:>3d} | {s['invalid_quote']:>3d} "
        f"| {s['mfe_avg']:>8.3f}% | {s['mfe_med']:>8.3f}% |{extra}"
    )

def table_header():
    return (
        "| Bucket         |     N |     WR |     Avg% |    Med% |     p25% |     p75% |    Total SOL |   EQ MDD SOL "
        "| AvgLoss% | AvgWin% | LossN |  WinN |  PU |  IQ | MFE_Avg% | MFE_Med% |"
    )

def table_sep():
    return (
        "|----------------|-------|--------|----------|---------|----------|----------|--------------|--------------"
        "|----------|---------|-------|-------|-----|-----|----------|----------|"
    )

# ══════════════════════════════════════════════════════════════
# Build the report
# ══════════════════════════════════════════════════════════════

lines = []
lines.append(f'# Liquidity / Impact Filter Backtest — Candidate D')
lines.append(f'')
lines.append(f'Generated: {NOW}')
lines.append(f'Source: `{STATE}` (read-only)')
lines.append(f'Variants analyzed: {", ".join(VARIANTS)}')
lines.append(f'Z3H modes: {", ".join(Z3H_MODES)}')
lines.append(f'')
lines.append(f'## Overview')
lines.append(f'')
lines.append(f'This backtest groups all `paper_completed` trades by `quote_reserve_ui` '
            f'buckets and evaluates performance within each bucket, split by variant and '
            f'(for Z3H_SHADOW) by selected mode (`baseline_z31` vs `tail_z3`).')
lines.append(f'')
lines.append(f'The goal is to identify a minimum `quote_reserve_ui` threshold for live '
            f'trading that filters out toxic low-liquidity pairs while retaining strong '
            f'high-liquidity trades.')
lines.append(f'')

# ── 1. Global variant summary (all buckets) ──
lines.append(f'## 1. Global variant summary (all quote reserves)')
lines.append('')
lines.append(table_header())
lines.append(table_sep())
for v in VARIANTS:
    vrows = [r for r in rows if r['variant'] == v]
    s = summarize(vrows)
    lines.append(fmt_row(v, s))
lines.append('')
lines.append(f'Total rows in analysis: {len(rows)}')
lines.append('')

# ── 2. Bucket analysis: by variant ──
for v in VARIANTS:
    vrows = [r for r in rows if r['variant'] == v]
    lines.append(f'## 2. Quote reserve buckets — {v}')
    lines.append('')
    lines.append(table_header())
    lines.append(table_sep())
    for blabel, lo, hi in BUCKETS:
        bros = [r for r in vrows if lo <= r['quote_reserve_ui'] < hi]
        s = summarize(bros)
        lines.append(fmt_row(blabel, s))
    # Also add a row for quote_reserve_ui == 0 (missing data)
    bros_zero = [r for r in vrows if r['quote_reserve_ui'] == 0]
    if bros_zero:
        s = summarize(bros_zero)
        lines.append(fmt_row('0 (no data)', s))
    lines.append('')

# ── 3. Bucket analysis: by Z3H mode ──
z3h_rows = [r for r in rows if r['variant'] == 'Z3H_SHADOW' and r['z3h_mode']]
for mode in Z3H_MODES:
    mrows = [r for r in z3h_rows if r['z3h_mode'] == mode]
    if not mrows:
        continue
    lines.append(f'## 3. Quote reserve buckets — Z3H_SHADOW mode={mode}')
    lines.append('')
    lines.append(table_header())
    lines.append(table_sep())
    for blabel, lo, hi in BUCKETS:
        bros = [r for r in mrows if lo <= r['quote_reserve_ui'] < hi]
        s = summarize(bros)
        lines.append(fmt_row(blabel, s))
    bros_zero = [r for r in mrows if r['quote_reserve_ui'] == 0]
    if bros_zero:
        s = summarize(bros_zero)
        lines.append(fmt_row('0 (no data)', s))
    lines.append('')

# ── 4. Bucket analysis: by entry_quote_reserve_raw ──
lines.append(f'## 4. Quote reserve buckets — by entry_quote_reserve_raw (all variants)')
lines.append('')
lines.append(table_header())
lines.append(table_sep())
for blabel, lo, hi in BUCKETS:
    bros = [r for r in rows if lo <= r['entry_quote_reserve_raw'] < hi]
    s = summarize(bros)
    lines.append(fmt_row(blabel, s))
bros_zero = [r for r in rows if r['entry_quote_reserve_raw'] == 0]
if bros_zero:
    s = summarize(bros_zero)
    lines.append(fmt_row('0 (no data)', s))
lines.append('')

# ── 5. Bucket analysis: by min_quote_reserve_raw ──
lines.append(f'## 5. Quote reserve buckets — by min_quote_reserve_raw (all variants)')
lines.append('')
lines.append(table_header())
lines.append(table_sep())
for blabel, lo, hi in BUCKETS:
    bros = [r for r in rows if lo <= r['min_quote_reserve_raw'] < hi]
    s = summarize(bros)
    lines.append(fmt_row(blabel, s))
bros_zero = [r for r in rows if r['min_quote_reserve_raw'] == 0]
if bros_zero:
    s = summarize(bros_zero)
    lines.append(fmt_row('0 (no data)', s))
lines.append('')

# ── 6. Price_unavailable and invalid_quote exits per bucket ──
lines.append(f'## 6. Toxic exit counts by quote_reserve_ui bucket')
lines.append('')
lines.append('| Bucket         |     N | price_unavailable | % of N | invalid_quote | % of N |')
lines.append('|----------------|-------|-------------------|--------|---------------|--------|')
for blabel, lo, hi in BUCKETS:
    bros = [r for r in rows if lo <= r['quote_reserve_ui'] < hi]
    pu = sum(1 for r in bros if r['exit_reason'] == 'price_unavailable')
    iq = sum(1 for r in bros if r['exit_reason'] == 'invalid_quote')
    n = len(bros)
    lines.append(f'| {blabel:<14} | {n:>5d} | {pu:>17d} | {pu/n*100 if n else 0:>6.2f}% | {iq:>13d} | {iq/n*100 if n else 0:>6.2f}% |')
lines.append('')

# ── 7. Recommendations ──
lines.append(f'## 7. Interpretation & Recommendations')
lines.append('')
lines.append('### Key findings')
lines.append('')

# Compute per-bucket results for recommendations
bucket_results = []
for blabel, lo, hi in BUCKETS:
    bros = [r for r in rows if lo <= r['quote_reserve_ui'] < hi]
    s = summarize(bros)
    s['label'] = blabel
    bucket_results.append(s)

# Find the "sweet spot" — first bucket where WR > 50% and avg > 0
lines.append('```text')
lines.append('Bucket | N | WR% | Avg% | Total SOL | Recommendation')
lines.append('-------|----|-----|------|-----------|--------------')
for s in bucket_results:
    rec = ''
    if s['n'] == 0:
        rec = 'no data'
    elif s['wr'] >= 80 and s['avg'] > 0:
        rec = 'STRONG — trade'
    elif s['wr'] >= 60 and s['avg'] > 0:
        rec = 'MODERATE — trade with caution'
    elif s['wr'] >= 40:
        rec = 'MARGINAL — shadow only'
    elif s['n'] > 0:
        rec = 'TOXIC — avoid'
    lines.append(f"{s['label']:>6} | {s['n']:>4d} | {s['wr']:>5.2f}% | {s['avg']:>8.3f}% | {s['total_sol']:>12.6f} | {rec}")
lines.append('```')
lines.append('')

# Find recommended minimum
min_rec = None
for blabel, lo, hi in BUCKETS:
    bros = [r for r in rows if lo <= r['quote_reserve_ui'] < hi]
    s = summarize(bros)
    if s['n'] > 5 and s['wr'] >= 70 and s['avg'] > 0:
        min_rec = lo
        break

if min_rec is not None:
    lines.append(f'**Recommended minimum `quote_reserve_ui` for live: `{min_rec}`**')
    lines.append('')
    lines.append(f'Trades with `quote_reserve_ui >= {min_rec}` show strong win rates and positive average returns.')
else:
    lines.append('**No single clear threshold found.** Review bucket tables above for trade-off between volume and quality.')
lines.append('')

# Check Z3H mode preferences
for mode in Z3H_MODES:
    mrows = [r for r in z3h_rows if r['z3h_mode'] == mode]
    if not mrows:
        continue
    s = summarize(mrows)
    lines.append(f'### Z3H_SHADOW mode `{mode}`: n={s["n"]}, WR={s["wr"]:.2f}%, avg={s["avg"]:.3f}%, total={s["total_sol"]:.6f} SOL')
lines.append('')

lines.append('### Notes')
lines.append('')
lines.append('- `quote_reserve_ui` = the quote-side reserve at time of trade entry (UI units)')
lines.append('- `entry_quote_reserve_raw` = raw quote reserve at entry')
lines.append('- `min_quote_reserve_raw` = minimum quote reserve observed during trade')
lines.append('- PU = price_unavailable exits, IQ = invalid_quote exits')
lines.append('- EQ MDD SOL = equity-curve maximum drawdown in SOL')
lines.append('- This is a diagnostic backtest. No live trading logic was changed.')
lines.append('')

# ── Write report ──
REPORT.write_text('\n'.join(lines) + '\n')
print(f'\nReport written: {REPORT}')

# ── Quick terminal summary ──
print('\n=== Quick Summary ===')
for blabel, lo, hi in BUCKETS:
    bros = [r for r in rows if lo <= r['quote_reserve_ui'] < hi]
    s = summarize(bros)
    if s['n'] > 0:
        print(f'  quote_reserve_ui {blabel:>7}: n={s["n"]:>5d} WR={s["wr"]:>6.2f}% avg={s["avg"]:>8.3f}% med={s["med"]:>8.3f}% total={s["total_sol"]:>12.6f} SOL MDD={s["eq_mdd_sol"]:>10.6f} SOL')
print()
for v in VARIANTS:
    vrows = [r for r in rows if r['variant'] == v]
    s = summarize(vrows)
    print(f'  {v:>14}: n={s["n"]:>5d} WR={s["wr"]:>6.2f}% avg={s["avg"]:>8.3f}% total={s["total_sol"]:>12.6f} SOL')
print()
print('Done.')
