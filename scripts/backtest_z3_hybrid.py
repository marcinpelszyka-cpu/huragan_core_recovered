#!/usr/bin/env python3
"""
Offline diagnostic backtest for Z3/Z3.1 hybrid hypotheses.

Important: using final MFE/exit_reason is lookahead. Treat HYBRID_* here as
upper-bound / diagnostic, not deployable live logic. A deployable Z3H requires
paper_amm time-series snapshots or explicit online triggers.
"""
import json, statistics, math, collections
from pathlib import Path

STATE=Path('/opt/huragan_core/state.jsonl')
REPORT=Path('/opt/huragan_core/reports/z3_hybrid_backtest_2026-06-04.md')
REPORT.parent.mkdir(parents=True, exist_ok=True)

VARIANTS=['Z','Z3','Z3.1']

def q(xs,p):
    if not xs: return 0.0
    xs=sorted(xs); idx=(len(xs)-1)*p; lo=math.floor(idx); hi=math.ceil(idx)
    if lo==hi: return xs[lo]
    return xs[lo]*(hi-idx)+xs[hi]*(idx-lo)
def med(xs): return statistics.median(xs) if xs else 0.0
def avg(xs): return sum(xs)/len(xs) if xs else 0.0
def equity_mdd(vals):
    eq=0; peak=0; mdd=0
    for v in vals:
        eq+=v
        if eq>peak: peak=eq
        dd=peak-eq
        if dd>mdd: mdd=dd
    return mdd

def load_rows():
    rows=[]
    for i,line in enumerate(STATE.open(),1):
        if not line.strip(): continue
        try: r=json.loads(line)
        except Exception: continue
        v=r.get('variant_id') or r.get('variant')
        if v not in VARIANTS: continue
        if r.get('status')!='paper_completed': continue
        if r.get('excluded_from_stats') is True: continue
        rows.append({
            'i': i,
            'variant': v,
            'mint': r.get('mint') or '',
            'pool': r.get('pool_state') or '',
            'pnl_pct': float(r.get('net_pnl_pct') or 0.0),
            'pnl_sol': float(r.get('net_pnl_sol') or 0.0),
            'mfe': float(r.get('max_favorable_pct') or 0.0),
            'mdd_pct': float(r.get('max_drawdown_pct') or 0.0),
            'hold': float(r.get('hold_secs') or 0.0),
            'reason': r.get('exit_reason') or 'unknown',
        })
    return rows

rows=load_rows()
max_i=max(r['i'] for r in rows) if rows else 0

def latest_pairs(filter_fn):
    grouped=collections.defaultdict(dict)
    for r in rows:
        if not filter_fn(r): continue
        key=(r['mint'], r['pool'])
        v=r['variant']
        if v not in grouped[key] or r['i']>grouped[key][v]['i']:
            grouped[key][v]=r
    pairs=[]
    triples=[]
    for key,d in grouped.items():
        if 'Z3' in d and 'Z3.1' in d:
            pairs.append((key,d['Z3'],d['Z3.1']))
        if all(v in d for v in VARIANTS):
            triples.append((key,d['Z'],d['Z3'],d['Z3.1']))
    pairs.sort(key=lambda x: max(x[1]['i'],x[2]['i']))
    triples.sort(key=lambda x: max(x[1]['i'],x[2]['i'],x[3]['i']))
    return pairs, triples

def summarize(name, records):
    pn=[r['pnl_pct'] for r in records]; so=[r['pnl_sol'] for r in records]
    if not records:
        return dict(name=name,n=0,wr=0,avg=0,med=0,total=0,q10=0,q25=0,q75=0,q90=0,mdd=0,avg_loss=0,avg_win=0,loss_n=0,win_n=0,price_unavail=0)
    losses=[x for x in pn if x<0]; wins=[x for x in pn if x>0]
    return dict(
        name=name,n=len(records),wr=sum(x>0 for x in pn)/len(pn)*100,avg=avg(pn),med=med(pn),total=sum(so),
        q10=q(pn,.10),q25=q(pn,.25),q75=q(pn,.75),q90=q(pn,.90),mdd=equity_mdd(so),
        avg_loss=avg(losses),avg_win=avg(wins),loss_n=len(losses),win_n=len(wins),
        price_unavail=sum(1 for r in records if r['reason']=='price_unavailable')/len(records)*100,
    )

def format_summary(s):
    return (f"{s['name']:<22} n={s['n']:4d} WR={s['wr']:6.2f}% avg={s['avg']:7.3f}% med={s['med']:7.3f}% "
            f"total={s['total']: .6f} SOL q10={s['q10']:7.2f}% q90={s['q90']:7.2f}% eqMDD={s['mdd']:.6f} SOL pu={s['price_unavail']:5.2f}%")

def evaluate_set(label, filter_fn):
    pairs, triples=latest_pairs(filter_fn)
    out=[]
    out.append(f'## {label}')
    out.append('')
    out.append(f'paired Z3/Z3.1 rows: `{len(pairs)}`; triples Z/Z3/Z3.1: `{len(triples)}`')
    out.append('')
    # Baselines on paired only for fair comparison
    z3=[z3 for _,z3,z31 in pairs]
    z31=[z31 for _,z3,z31 in pairs]
    out.append('```text')
    base=[summarize('Z3 paired',z3), summarize('Z3.1 paired',z31)]
    for s in base: out.append(format_summary(s))
    out.append('```')
    out.append('')
    # Oracle hybrids: select Z3 if Z3 final MFE threshold crossed else Z3.1
    thresholds=[25,30,40,50,60,75,100]
    hybrids=[]
    for th in thresholds:
        rec=[]; use_z3=0
        for key,z3,z31 in pairs:
            if z3['mfe']>=th:
                rec.append({**z3,'chosen':'Z3'}); use_z3+=1
            else:
                rec.append({**z31,'chosen':'Z3.1'})
        s=summarize(f'H_ORACLE_MFE>={th}',rec)
        s['use_z3']=use_z3; s['use_z31']=len(rec)-use_z3
        hybrids.append((th,s,rec))
    out.append('### Oracle hybrid: choose Z3 if final Z3 MFE >= threshold else Z3.1')
    out.append('')
    out.append('LOOKAHEAD / diagnostic only. This is not directly live deployable.')
    out.append('')
    out.append('```text')
    for th,s,rec in hybrids:
        out.append(format_summary(s)+f" choose_Z3={s['use_z3']:4d} choose_Z31={s['use_z31']:4d}")
    out.append('```')
    out.append('')
    # Conservative hybrid: use Z3 only for final MFE>=threshold AND Z3 reason not hard_stop, else Z3.1
    out.append('### Conservative diagnostic: choose Z3 only for MFE>=threshold and not hard_stop')
    out.append('')
    out.append('```text')
    conservative=[]
    for th in thresholds:
        rec=[]; use_z3=0
        for key,z3,z31 in pairs:
            if z3['mfe']>=th and z3['reason']!='hard_stop':
                rec.append({**z3,'chosen':'Z3'}); use_z3+=1
            else:
                rec.append({**z31,'chosen':'Z3.1'})
        s=summarize(f'H_CONS_MFE>={th}',rec)
        s['use_z3']=use_z3; s['use_z31']=len(rec)-use_z3
        conservative.append((th,s,rec))
        out.append(format_summary(s)+f" choose_Z3={s['use_z3']:4d} choose_Z31={s['use_z31']:4d}")
    out.append('```')
    out.append('')
    # Best candidates by total and mdd-adjusted rough score
    all_candidates=[('oracle',th,s,rec) for th,s,rec in hybrids]+[('conservative',th,s,rec) for th,s,rec in conservative]
    best_total=max(all_candidates, key=lambda x:x[2]['total']) if all_candidates else None
    best_score=max(all_candidates, key=lambda x:x[2]['total']-x[2]['mdd']) if all_candidates else None
    if best_total:
        typ,th,s,rec=best_total
        out.append(f'Best by total: `{typ} MFE>={th}` -> total={s["total"]:.6f} SOL, avg={s["avg"]:.3f}%, WR={s["wr"]:.2f}%, eqMDD={s["mdd"]:.6f} SOL.')
    if best_score:
        typ,th,s,rec=best_score
        out.append(f'Best by total-eqMDD: `{typ} MFE>={th}` -> score={(s["total"]-s["mdd"]):.6f}, total={s["total"]:.6f}, eqMDD={s["mdd"]:.6f}.')
    out.append('')
    # reason mix for best total
    if best_total:
        typ,th,s,rec=best_total
        c=collections.Counter(r.get('chosen') for r in rec)
        cr=collections.Counter(r.get('reason') for r in rec)
        out.append('Best-total exit reason mix:')
        out.append('```text')
        out.append('chosen: '+str(dict(c)))
        for k,n in cr.most_common(12):
            sub=[r for r in rec if r['reason']==k]
            out.append(f"{k:24} n={n:4d} {n/len(rec)*100:6.2f}% avg={avg([r['pnl_pct'] for r in sub]):7.3f}% total={sum(r['pnl_sol'] for r in sub): .6f} SOL")
        out.append('```')
    out.append('')
    return '\n'.join(out), hybrids, conservative

filters=[
    ('ALL paired rows', lambda r: True),
    ('CLEAN: MFE<=200', lambda r: r['mfe']<=200),
    ('CLEAN: MFE<=200 and no price_unavailable', lambda r: r['mfe']<=200 and r['reason']!='price_unavailable'),
    ('RECENT CLEAN: last 15000 state lines and MFE<=200', lambda r: r['mfe']<=200 and r['i']>=max_i-15000),
]

lines=[]
lines.append('# Z3H offline hybrid backtest — 2026-06-04')
lines.append('')
lines.append('Source: `/opt/huragan_core/state.jsonl`. No live, no restarts, no secrets.')
lines.append('')
lines.append('Important caveat: this script uses final `max_favorable_pct` and/or final `exit_reason`, so HYBRID results are diagnostic upper-bounds, not deployable live logic. Deployable Z3H needs online triggers from sampler snapshots.')
lines.append('')
all_results=[]
for label,fn in filters:
    block,hyb,cons=evaluate_set(label,fn)
    lines.append(block)
    all_results.append((label,hyb,cons))

lines.append('## Final decision')
lines.append('')
lines.append('```text')
lines.append('1. Z3.1 remains the current clean paper baseline.')
lines.append('2. Z3 captures tail better when strong MFE appears.')
lines.append('3. Z3H is promising only if we can implement an online trigger for “strong momentum / MFE bucket” without future leak.')
lines.append('4. Do not deploy a variant based directly on final MFE; first add/simulate online features: MFE_at_30s, MFE_at_60s, reserve trend, quote stability, price_unavailable guard.')
lines.append('```')
lines.append('')
lines.append('Recommended next implementation step: add paper-only `Z3H_SHADOW` with online gates logged at 30s/60s/120s, not live trading.')
lines.append('')
lines.append('Live: NO. Fresh: SHADOW_ONLY.')

REPORT.write_text('\n'.join(lines)+"\n")
print(REPORT)
for label,hyb,cons in all_results:
    best=max(hyb+cons, key=lambda x:x[1]['total'])
    print(label, 'best', best[1]['name'], 'total', round(best[1]['total'],6), 'avg', round(best[1]['avg'],3), 'wr', round(best[1]['wr'],2), 'mdd', round(best[1]['mdd'],6))
