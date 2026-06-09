#!/usr/bin/env python3
import json, statistics, math, collections
from pathlib import Path

STATE=Path('/opt/huragan_core/state.jsonl')
REPORT=Path('/opt/huragan_core/reports/profit_regularities_2026-06-04.md')
ACT=42527
VARIANTS=['Z3','Z3.1','Z3H_SHADOW']
TP_THRESHOLDS=[3,5,8,10,12,15,20,25,30,35,40,50,60,75,100]
BUCKETS=[(-999,-20),(-20,-10),(-10,0),(0,5),(5,10),(10,15),(15,20),(20,30),(30,50),(50,75),(75,100),(100,200),(200,999999)]
MFE_BUCKETS=[(0,5),(5,10),(10,15),(15,20),(20,25),(25,30),(30,40),(40,50),(50,75),(75,100),(100,200),(200,999999)]

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
    ps=[f(r.get('net_pnl_pct')) for _,r in items]
    ss=[f(r.get('net_pnl_sol')) for _,r in items]
    mf=[f(r.get('max_favorable_pct')) for _,r in items]
    return dict(n=len(items),wr=sum(p>0 for p in ps)/len(ps)*100 if ps else 0,avg=sum(ps)/len(ps) if ps else 0,med=med(ps),p10=q(ps,.1),p25=q(ps,.25),p75=q(ps,.75),p90=q(ps,.9),total=sum(ss),mdd=mdd(ss),mfe_med=med(mf),mfe_p75=q(mf,.75),mfe_p90=q(mf,.9))

def fmt(m):
    return f"n={m['n']} WR={m['wr']:.2f}% avg={m['avg']:.3f}% med={m['med']:.3f}% p25={m['p25']:.3f}% p75={m['p75']:.3f}% total={m['total']:.6f} MDD={m['mdd']:.6f} MFE_med={m['mfe_med']:.3f}% MFE_p75={m['mfe_p75']:.3f}%"

rows=[]
for i,line in enumerate(STATE.open(),1):
    if not line.strip(): continue
    try: r=json.loads(line)
    except Exception: continue
    rows.append((i,r))
post=[(i,r) for i,r in rows if i>=ACT]
completed=[(i,r) for i,r in post if r.get('status')=='paper_completed' and r.get('variant_id') in VARIANTS]
cleaned=[(i,r) for i,r in completed if clean(r)]
by={v:[(i,r) for i,r in cleaned if r.get('variant_id')==v] for v in VARIANTS}

# Paired clean all 3
bymint=collections.defaultdict(dict)
for i,r in cleaned:
    bymint[r.get('mint')][r.get('variant_id')]=(i,r)
paired=[]
for mint,d in bymint.items():
    if all(v in d for v in VARIANTS): paired.append((mint,d))
paired_by={v:[d[v] for _,d in paired] for v in VARIANTS}

# For "one common profit point": evaluate MFE hit probabilities and conservative fixed TP proxy.
# If MFE reaches threshold T, assume a fixed TP at T would win about (T - 3% friction). If not reached, final current strategy pnl remains? For threshold quality we report hit-rate and conditional final outcomes.
def threshold_table(items):
    out=[]
    n=len(items)
    for t in TP_THRESHOLDS:
        hit=[r for _,r in items if f(r.get('max_favorable_pct'))>=t]
        miss=[r for _,r in items if f(r.get('max_favorable_pct'))<t]
        hitrate=len(hit)/n*100 if n else 0
        final_after_hit=[f(r.get('net_pnl_pct')) for r in hit]
        final_miss=[f(r.get('net_pnl_pct')) for r in miss]
        # expected if always take profit at t when touched and otherwise keep current final result for misses
        fixed_exp=(sum((t-3.0) for _ in hit)+sum(final_miss))/n if n else 0
        out.append(dict(t=t,hits=len(hit),hitrate=hitrate,hit_final_avg=sum(final_after_hit)/len(final_after_hit) if hit else 0,hit_final_med=med(final_after_hit),miss_avg=sum(final_miss)/len(final_miss) if miss else 0,fixed_exp=fixed_exp))
    return out

# Profit bucket distribution
bucket_counts={}
for v,items in by.items():
    cnt=[]
    for lo,hi in BUCKETS:
        g=[r for _,r in items if lo<=f(r.get('net_pnl_pct'))<hi]
        cnt.append((lo,hi,len(g),len(g)/len(items)*100 if items else 0))
    bucket_counts[v]=cnt

# MFE bucket conditional final WR/avg
mfe_condition={}
for v,items in by.items():
    arr=[]
    for lo,hi in MFE_BUCKETS:
        g=[r for _,r in items if lo<=f(r.get('max_favorable_pct'))<hi]
        ps=[f(r.get('net_pnl_pct')) for r in g]
        arr.append((lo,hi,len(g),sum(p>0 for p in ps)/len(ps)*100 if ps else 0,sum(ps)/len(ps) if ps else 0,med(ps)))
    mfe_condition[v]=arr

# Regularity over time: chunks of completed rows by sequence
chunk_stats={}
for v,items in by.items():
    chunks=[]
    ordered=items
    size=25
    for k in range(0,len(ordered),size):
        ch=ordered[k:k+size]
        if len(ch)<10: continue
        mm=metrics(ch)
        chunks.append((k//size+1,mm))
    chunk_stats[v]=chunks

# Z3H modes
z3h_modes=collections.defaultdict(list)
for i,r in by['Z3H_SHADOW']:
    z3h_modes[r.get('z3h_selected_mode') or '<empty>'].append((i,r))

lines=[]
lines.append('# Profit regularities and common profit point analysis — 2026-06-04')
lines.append('')
lines.append('Scope: post Z3H activation clean rows only. Filters: MFE<=200, abs(PnL)<=300, exclude price_unavailable/invalid_quote.')
lines.append('No runtime changes, no live, no restart.')
lines.append('')
lines.append('## Counts / base metrics')
lines.append('')
lines.append(f'post_completed_target={len(completed)} clean={len(cleaned)} paired_clean_all3={len(paired)}')
for v in VARIANTS:
    lines.append(f'{v}: {fmt(metrics(by[v]))}')
lines.append('')
lines.append('## Paired clean metrics')
lines.append('')
for v in VARIANTS:
    lines.append(f'{v}: {fmt(metrics(paired_by[v]))}')
lines.append('')
lines.append('## Z3H modes')
lines.append('')
for mode,items in sorted(z3h_modes.items()):
    lines.append(f'{mode}: {fmt(metrics(items))}')
lines.append('')
lines.append('## Probability of reaching profit thresholds (MFE hit-rate)')
lines.append('')
for v in VARIANTS:
    lines.append(f'### {v}')
    lines.append('threshold | hit_rate | hit_n | final_after_hit_med | miss_final_avg | fixed_TP_proxy_avg')
    for row in threshold_table(by[v]):
        lines.append(f"{row['t']:>3}% | {row['hitrate']:6.2f}% | {row['hits']:3d} | {row['hit_final_med']:8.3f}% | {row['miss_avg']:8.3f}% | {row['fixed_exp']:8.3f}%")
    lines.append('')
lines.append('## Final PnL bucket distribution')
lines.append('')
for v in VARIANTS:
    lines.append(f'### {v}')
    for lo,hi,c,p in bucket_counts[v]:
        lines.append(f'{lo:>6}..{hi:<6}: n={c:3d} {p:6.2f}%')
    lines.append('')
lines.append('## Conditional final result by MFE bucket')
lines.append('')
for v in VARIANTS:
    lines.append(f'### {v}')
    lines.append('MFE bucket | n | final_WR | final_avg | final_median')
    for lo,hi,c,wr,avg,md in mfe_condition[v]:
        if c:
            lines.append(f'{lo:>3}..{hi:<6} | {c:3d} | {wr:6.2f}% | {avg:8.3f}% | {md:8.3f}%')
    lines.append('')
lines.append('## Time/chunk regularity, chunks of 25 completed clean rows')
lines.append('')
for v in VARIANTS:
    lines.append(f'### {v}')
    for idx,mm in chunk_stats[v]:
        lines.append(f'chunk={idx:02d} {fmt(mm)}')
    lines.append('')
lines.append('## Preliminary conclusion')
lines.append('')
lines.append('Regular edge is not a single universal token property. The recurring phenomenon is a two-regime split:')
lines.append('')
lines.append('```text')
lines.append('1) MFE reaches roughly 25-40%: probability of profitable final result becomes high; Z3H tail_z3 captures this well.')
lines.append('2) MFE stays low through checkpoints: baseline bucket is weak/negative and needs better rejection, but MFE alone is not enough for an executable rule yet.')
lines.append('```')
lines.append('')
lines.append('Candidate common profit zone appears around 12-20% for frequent modest wins, and 30-40% for tail confirmation. A single hard TP is likely worse than regime logic, but threshold table identifies which TP proxy is most plausible.')
REPORT.write_text('\n'.join(lines)+'\n')
print('\n'.join(lines[:120]))
print('REPORT='+str(REPORT))
