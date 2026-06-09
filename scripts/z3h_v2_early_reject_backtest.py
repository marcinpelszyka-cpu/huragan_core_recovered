#!/usr/bin/env python3
import json, statistics, math, collections, itertools
from pathlib import Path

STATE=Path('/opt/huragan_core/state.jsonl')
REPORT=Path('/opt/huragan_core/reports/z3h_v2_early_reject_backtest_2026-06-04.md')
ACT=42527

# Conservative approximation: checkpoint MFE is the best observable upside by that time.
# If we early-exit a weak baseline, use min(final pnl, checkpoint_mfe - haircut) capped near flat.
# This avoids pretending we can exit at a good future price.
HAIRCUT=3.0

def f(x):
    try: return float(x or 0.0)
    except Exception: return 0.0

def med(xs): return statistics.median(xs) if xs else 0.0

def q(xs,qq):
    if not xs: return 0.0
    xs=sorted(xs); k=(len(xs)-1)*qq; lo=math.floor(k); hi=math.ceil(k)
    if lo==hi: return xs[lo]
    return xs[lo]*(hi-k)+xs[hi]*(k-lo)

def clean(r):
    return f(r.get('max_favorable_pct'))<=200 and abs(f(r.get('net_pnl_pct')))<=300 and r.get('exit_reason') not in ('price_unavailable','invalid_quote')

def mdd(sols):
    eq=peak=dd=0.0
    for s in sols:
        eq+=s; peak=max(peak,eq); dd=max(dd, peak-eq)
    return dd

def metrics(items):
    ps=[x['pnl_pct'] for x in items]
    ss=[x['pnl_sol'] for x in items]
    return dict(n=len(items), wr=sum(p>0 for p in ps)/len(ps)*100 if ps else 0, avg=sum(ps)/len(ps) if ps else 0, med=med(ps), p25=q(ps,0.25), p75=q(ps,0.75), total=sum(ss), mdd=mdd(ss))

def fmt(mm):
    return f"n={mm['n']} WR={mm['wr']:.2f}% avg={mm['avg']:.3f}% med={mm['med']:.3f}% p25={mm['p25']:.3f}% p75={mm['p75']:.3f}% total={mm['total']:.6f} MDD={mm['mdd']:.6f}"

rows=[]
for i,line in enumerate(STATE.open(),1):
    if i<ACT or not line.strip(): continue
    try: r=json.loads(line)
    except Exception: continue
    if r.get('variant_id')=='Z3H_SHADOW' and r.get('status')=='paper_completed' and clean(r):
        rows.append((i,r))

base=[]
for i,r in rows:
    base.append(dict(
        line=i,
        mint=r.get('mint'),
        mode=r.get('z3h_selected_mode') or '',
        exit=r.get('exit_reason') or '',
        pnl_pct=f(r.get('net_pnl_pct')),
        pnl_sol=f(r.get('net_pnl_sol')),
        mfe30=f(r.get('z3h_mfe_30s')),
        mfe60=f(r.get('z3h_mfe_60s')),
        mfe120=f(r.get('z3h_mfe_120s')),
    ))

entry_sol=statistics.median([abs(x['pnl_sol']/x['pnl_pct']*100) for x in base if abs(x['pnl_pct'])>1e-9 and abs(x['pnl_sol'])>1e-12] or [0.003])

def early_pnl_from_checkpoint(x, t):
    mfe=x[f'mfe{t}']
    # if weak by checkpoint, assume exit around checkpoint quality minus quote/haircut, capped to not exceed original final pnl.
    approx_pct=min(x['pnl_pct'], max(-20.0, mfe-HAIRCUT))
    # if original was a small win but weak early, be conservative: allow near-flat/slightly negative.
    if x['pnl_pct'] > approx_pct:
        pnl_pct=approx_pct
    else:
        pnl_pct=x['pnl_pct']
    return pnl_pct, entry_sol*pnl_pct/100.0

def simulate(rule):
    # rule tuple: (t, threshold) or two-stage ((t1,thr1),(t2,thr2))
    out=[]
    for x in base:
        y=x.copy(); y['sim_exit']=x['exit']; y['sim_mode']=x['mode']; y['early']=False
        if x['mode']=='baseline_z31':
            stages=rule if isinstance(rule[0], tuple) else [rule]
            for t,thr in stages:
                if x[f'mfe{t}'] < thr:
                    pnl_pct,pnl_sol=early_pnl_from_checkpoint(x,t)
                    y['pnl_pct']=pnl_pct; y['pnl_sol']=pnl_sol; y['sim_exit']=f'early_no_momentum_{t}s_lt_{thr:g}'; y['early']=True
                    break
        out.append(y)
    return out

rules=[]
for t,thresholds in [(60,[3,5,8,10,12,15]),(120,[5,8,10,12,15,20,25])]:
    for thr in thresholds: rules.append((t,thr))
for thr60 in [3,5,8,10,12]:
    for thr120 in [8,10,12,15,20]:
        rules.append(((60,thr60),(120,thr120)))

baseline=metrics(base)
results=[]
for rule in rules:
    sim=simulate(rule)
    mm=metrics(sim)
    early=sum(1 for x in sim if x['early'])
    tail_unchanged=sum(1 for x in sim if x['mode']=='tail_z3' and not x['early'])
    baseline_early=sum(1 for x in sim if x['mode']=='baseline_z31' and x['early'])
    results.append((rule,mm,early,baseline_early,tail_unchanged,sim))

# Objective: improve total/avg and MDD, avoid overfitting too hard: at least 100 rows, preserve tail, not too many early (>70% baseline suspicious).
def score(item):
    rule,mm,early,be,tu,sim=item
    return (mm['total']-baseline['total']) - 0.5*max(0, mm['mdd']-baseline['mdd']) + 0.0001*(mm['wr']-baseline['wr'])
results.sort(key=score, reverse=True)

lines=[]
lines.append('# Z3H_V2 early rejection offline backtest — 2026-06-04')
lines.append('')
lines.append('Scope: clean Z3H_SHADOW completed rows after activation. Tail_z3 is left untouched; only baseline_z31 can be early-rejected.')
lines.append('')
lines.append(f'clean_z3h_rows={len(base)}')
lines.append(f'estimated_entry_sol={entry_sol:.9f}')
lines.append(f'baseline: {fmt(baseline)}')
lines.append('')
lines.append('## Top rules')
lines.append('')
for rule,mm,early,be,tu,sim in results[:15]:
    lines.append(f'rule={rule} {fmt(mm)} early_total={early} baseline_early={be} score={score((rule,mm,early,be,tu,sim)):.6f}')
lines.append('')
lines.append('## Selected rule')
sel=results[0]
rule,mm,early,be,tu,sim=sel
lines.append(f'selected_rule={rule}')
lines.append(f'selected: {fmt(mm)}')
lines.append(f'delta_total={mm["total"]-baseline["total"]:.6f} SOL')
lines.append(f'delta_avg={mm["avg"]-baseline["avg"]:.3f}%')
lines.append(f'delta_wr={mm["wr"]-baseline["wr"]:.3f}%')
lines.append(f'delta_mdd={mm["mdd"]-baseline["mdd"]:.6f} SOL')
lines.append(f'baseline_early={be}')
lines.append('')
lines.append('## Selected exit counts')
for k,c in collections.Counter(x['sim_exit'] for x in sim).most_common():
    lines.append(f'{k}: {c}')
lines.append('')
lines.append('## Selected by original mode')
for mode in ['baseline_z31','tail_z3']:
    items=[x for x in sim if x['mode']==mode]
    lines.append(f'{mode}: {fmt(metrics(items))}')
lines.append('')
lines.append('## Caveats')
lines.append('')
lines.append('- This is offline and approximate; checkpoint MFE is not exact executable exit price.')
lines.append('- Uses conservative haircut=3 pct points and caps early-exit against original final pnl.')
lines.append('- If selected rule helps, implement as new paper-only shadow variant, not live.')
REPORT.write_text('\n'.join(lines)+'\n')
print('\n'.join(lines[:60]))
print('REPORT='+str(REPORT))
