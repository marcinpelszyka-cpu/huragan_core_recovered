#!/usr/bin/env python3
import json, statistics, math, collections
from pathlib import Path

STATE=Path('/opt/huragan_core/state.jsonl')
REPORT=Path('/opt/huragan_core/reports/z3h_followup_2026-06-04.md')
ACTIVATION_LINE=42527  # first observed Z3H_SHADOW paper_entry after 2026-06-04 11:07:29 UTC
VARIANTS=['Z3','Z3.1','Z3H_SHADOW']

def pct(x):
    return float(x or 0.0)

def sol(x):
    return float(x or 0.0)

def median(xs):
    return statistics.median(xs) if xs else 0.0

def quantile(xs,q):
    if not xs: return 0.0
    xs=sorted(xs)
    k=(len(xs)-1)*q
    f=math.floor(k); c=math.ceil(k)
    if f==c: return xs[int(k)]
    return xs[f]*(c-k)+xs[c]*(k-f)

def equity_mdd(pnls):
    eq=0.0; peak=0.0; mdd=0.0
    for p in pnls:
        eq+=p
        peak=max(peak,eq)
        mdd=max(mdd,peak-eq)
    return mdd

def metrics(rows):
    pnls=[pct(r.get('net_pnl_pct')) for _,r in rows]
    sols=[sol(r.get('net_pnl_sol')) for _,r in rows]
    mfes=[pct(r.get('max_favorable_pct')) for _,r in rows]
    draws=[pct(r.get('max_drawdown_pct')) for _,r in rows]
    return {
        'n':len(rows),
        'wr':sum(1 for x in pnls if x>0)/len(pnls)*100 if pnls else 0,
        'avg':sum(pnls)/len(pnls) if pnls else 0,
        'median':median(pnls),
        'p25':quantile(pnls,0.25),
        'p75':quantile(pnls,0.75),
        'total_sol':sum(sols),
        'mdd_sol':equity_mdd(sols),
        'avg_mfe':sum(mfes)/len(mfes) if mfes else 0,
        'med_mfe':median(mfes),
        'avg_draw':sum(draws)/len(draws) if draws else 0,
    }

def fmt(m):
    return f"n={m['n']} WR={m['wr']:.2f}% avg={m['avg']:.3f}% med={m['median']:.3f}% p25={m['p25']:.3f}% p75={m['p75']:.3f}% total={m['total_sol']:.6f} SOL MDD={m['mdd_sol']:.6f} SOL medMFE={m['med_mfe']:.3f}%"

rows=[]
with STATE.open() as f:
    for i,line in enumerate(f,1):
        if not line.strip(): continue
        try: r=json.loads(line)
        except Exception: continue
        rows.append((i,r))
post=[(i,r) for i,r in rows if i>=ACTIVATION_LINE]
completed=[(i,r) for i,r in post if r.get('status')=='paper_completed' and r.get('variant_id') in VARIANTS]
by={v:[] for v in VARIANTS}
for item in completed:
    by[item[1].get('variant_id')].append(item)

# paired by mint for post window: compare only mints where all variants completed
by_mint=collections.defaultdict(dict)
for i,r in completed:
    by_mint[r.get('mint')][r.get('variant_id')]=(i,r)
paired=[]
for mint,d in by_mint.items():
    if all(v in d for v in VARIANTS):
        paired.append((mint,d))
paired_by={v:[] for v in VARIANTS}
for mint,d in paired:
    for v in VARIANTS: paired_by[v].append(d[v])

z3h=[r for _,r in by['Z3H_SHADOW']]
mode_counts=collections.Counter(r.get('z3h_selected_mode') or '<empty>' for r in z3h)
exit_counts={v:collections.Counter(r.get('exit_reason') or '<empty>' for _,r in by[v]) for v in VARIANTS}
mode_exit=collections.defaultdict(collections.Counter)
for r in z3h:
    mode_exit[r.get('z3h_selected_mode') or '<empty>'][r.get('exit_reason') or '<empty>']+=1

# Z3H mode metrics
z3h_mode_rows=collections.defaultdict(list)
for i,r in by['Z3H_SHADOW']:
    z3h_mode_rows[r.get('z3h_selected_mode') or '<empty>'].append((i,r))

# online trigger quality
trigger=[]
for _,r in by['Z3H_SHADOW']:
    trigger.append({
        'mode':r.get('z3h_selected_mode') or '<empty>',
        'pnl':pct(r.get('net_pnl_pct')),
        'mfe30':pct(r.get('z3h_mfe_30s')),
        'mfe60':pct(r.get('z3h_mfe_60s')),
        'mfe120':pct(r.get('z3h_mfe_120s')),
        'stable':bool(r.get('z3h_quote_stable')),
        'exit':r.get('exit_reason') or '<empty>',
    })

lines=[]
lines.append('# Z3H_SHADOW follow-up — 2026-06-04')
lines.append('')
lines.append('Window: post activation, first Z3H line >= 42527, after service restart 2026-06-04 11:07:29 UTC.')
lines.append('')
lines.append('## Counts')
lines.append('')
lines.append(f'post_rows={len(post)}')
lines.append(f'completed_target_variants={len(completed)}')
for v in VARIANTS:
    lines.append(f'{v}: completed={len(by[v])}')
lines.append(f'paired_mints_all_3={len(paired)}')
lines.append('')
lines.append('## Unpaired post-activation metrics')
lines.append('')
for v in VARIANTS:
    lines.append(f'{v}: {fmt(metrics(by[v]))}')
lines.append('')
lines.append('## Paired metrics: same mints where Z3/Z3.1/Z3H all completed')
lines.append('')
for v in VARIANTS:
    lines.append(f'{v}: {fmt(metrics(paired_by[v]))}')
lines.append('')
lines.append('## Z3H modes')
lines.append('')
for k,v in sorted(mode_counts.items()): lines.append(f'{k}: {v}')
lines.append('')
for mode,items in sorted(z3h_mode_rows.items()):
    lines.append(f'Z3H mode {mode}: {fmt(metrics(items))}')
lines.append('')
lines.append('## Exit reasons')
lines.append('')
for v in VARIANTS:
    lines.append(f'{v}:')
    for k,c in exit_counts[v].most_common(): lines.append(f'  {k}: {c}')
lines.append('')
lines.append('Z3H exit by mode:')
for mode,cnt in sorted(mode_exit.items()):
    lines.append(f'  {mode}:')
    for k,c in cnt.most_common(): lines.append(f'    {k}: {c}')
lines.append('')
lines.append('## Z3H trigger checkpoints')
lines.append('')
for mode in sorted(set(t['mode'] for t in trigger)):
    group=[t for t in trigger if t['mode']==mode]
    if not group: continue
    for field in ['mfe30','mfe60','mfe120','pnl']:
        xs=[g[field] for g in group]
        lines.append(f'{mode} {field}: avg={sum(xs)/len(xs):.3f}% med={median(xs):.3f}% p75={quantile(xs,0.75):.3f}%')
    lines.append('')
lines.append('## Last Z3H completed samples')
lines.append('')
for i,r in by['Z3H_SHADOW'][-10:]:
    lines.append(f"line={i} mint={r.get('mint')} exit={r.get('exit_reason')} pnl={pct(r.get('net_pnl_pct')):.3f}% mode={r.get('z3h_selected_mode')} mfe30={pct(r.get('z3h_mfe_30s')):.3f}% mfe60={pct(r.get('z3h_mfe_60s')):.3f}% mfe120={pct(r.get('z3h_mfe_120s')):.3f}% stable={r.get('z3h_quote_stable')}")

REPORT.write_text('\n'.join(lines)+'\n')
print('\n'.join(lines[:80]))
print(f'\nREPORT={REPORT}')
