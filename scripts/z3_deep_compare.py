#!/usr/bin/env python3
import json, statistics, math, collections, itertools
from pathlib import Path

STATE=Path('/opt/huragan_core/state.jsonl')
REPORT=Path('/opt/huragan_core/reports/z3_vs_z31_deep_2026-06-04.md')
REPORT.parent.mkdir(parents=True, exist_ok=True)
VARIANTS=['Z','Z3','Z3.1']

def pct(x): return f"{x:.2f}%"
def sol(x): return f"{x:.6f} SOL"
def med(xs): return statistics.median(xs) if xs else 0.0
def avg(xs): return sum(xs)/len(xs) if xs else 0.0
def q(xs,p):
    if not xs: return 0.0
    xs=sorted(xs); i=(len(xs)-1)*p; lo=math.floor(i); hi=math.ceil(i)
    if lo==hi: return xs[lo]
    return xs[lo]*(hi-i)+xs[hi]*(i-lo)
def max_drawdown_equity(pnls):
    eq=0.0; peak=0.0; mdd=0.0
    for x in pnls:
        eq+=x
        if eq>peak: peak=eq
        dd=peak-eq
        if dd>mdd: mdd=dd
    return mdd

def normalize_reason(r): return r or 'unknown'

rows=[]
with STATE.open() as f:
    for i,line in enumerate(f,1):
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
            'reason': normalize_reason(r.get('exit_reason')),
            'entry_reserve': float(r.get('entry_quote_reserve_raw') or 0.0),
            'min_reserve': float(r.get('min_quote_reserve_raw') or 0.0),
        })

by={v:[r for r in rows if r['variant']==v] for v in VARIANTS}

lines=[]
lines.append('# Z3 vs Z3.1 deep comparison — 2026-06-04')
lines.append('')
lines.append('Źródło: `/opt/huragan_core/state.jsonl`. Filtr: `status=paper_completed`, `excluded_from_stats != true`.')
lines.append('Bez live, bez sekretów, bez restartów.')
lines.append('')

lines.append('## 1. Summary metrics')
lines.append('')
lines.append('```text')
for v in VARIANTS:
    rs=by[v]
    pn=[r['pnl_pct'] for r in rs]; solp=[r['pnl_sol'] for r in rs]; mfe=[r['mfe'] for r in rs]; mdd=[r['mdd_pct'] for r in rs]; hold=[r['hold'] for r in rs]
    wins=sum(1 for x in pn if x>0)
    losses=sum(1 for x in pn if x<0)
    lines.append(f"{v:4} n={len(rs):5d} WR={wins/len(rs)*100 if rs else 0:6.2f}% avg={avg(pn):7.3f}% median={med(pn):7.3f}% total={sum(solp): .6f} SOL")
    lines.append(f"     q10={q(pn,0.10):7.3f}% q25={q(pn,0.25):7.3f}% q75={q(pn,0.75):7.3f}% q90={q(pn,0.90):7.3f}%")
    lines.append(f"     loss_n={losses:4d} avg_loss={avg([x for x in pn if x<0]):7.3f}% win_n={wins:4d} avg_win={avg([x for x in pn if x>0]):7.3f}%")
    lines.append(f"     MFE_avg={avg(mfe):7.3f}% MFE_med={med(mfe):7.3f}% maxDD_avg={avg(mdd):7.3f}% hold_med={med(hold):6.1f}s equity_MDD={max_drawdown_equity(solp):.6f} SOL")
lines.append('```')
lines.append('')

lines.append('## 2. Exit reasons')
lines.append('')
for v in VARIANTS:
    c=collections.Counter(r['reason'] for r in by[v])
    total=sum(c.values()) or 1
    lines.append(f'### {v}')
    lines.append('```text')
    for k,n in c.most_common():
        subset=[r for r in by[v] if r['reason']==k]
        lines.append(f"{k:24} n={n:5d} {n/total*100:6.2f}% avg={avg([r['pnl_pct'] for r in subset]):7.3f}% med={med([r['pnl_pct'] for r in subset]):7.3f}% total={sum(r['pnl_sol'] for r in subset): .6f} SOL")
    lines.append('```')
    lines.append('')

lines.append('## 3. Profit capture / giveback')
lines.append('')
lines.append('giveback = MFE - realized PnL, tylko gdy MFE > 0. Wyższy giveback = strategia oddaje więcej ruchu.')
lines.append('')
lines.append('```text')
for v in VARIANTS:
    rs=by[v]
    gb=[max(0.0, r['mfe']-r['pnl_pct']) for r in rs if r['mfe']>0]
    cap=[(r['pnl_pct']/r['mfe']*100.0) for r in rs if r['mfe']>0]
    pp=[r for r in rs if r['reason']=='profit_protect']
    eno=[r for r in rs if r['reason']=='early_no_momentum']
    hs=[r for r in rs if r['reason']=='hard_stop']
    lines.append(f"{v:4} giveback_avg={avg(gb):7.3f}% giveback_med={med(gb):7.3f}% capture_avg={avg(cap):7.2f}% capture_med={med(cap):7.2f}%")
    lines.append(f"     profit_protect n={len(pp):4d} total={sum(r['pnl_sol'] for r in pp): .6f} SOL avg={avg([r['pnl_pct'] for r in pp]):7.3f}%")
    lines.append(f"     early_no_momentum n={len(eno):4d} total={sum(r['pnl_sol'] for r in eno): .6f} SOL avg={avg([r['pnl_pct'] for r in eno]):7.3f}%")
    lines.append(f"     hard_stop n={len(hs):4d} total={sum(r['pnl_sol'] for r in hs): .6f} SOL avg={avg([r['pnl_pct'] for r in hs]):7.3f}%")
lines.append('```')
lines.append('')

# Paired comparison by mint+pool; if duplicates per variant keep latest completed line per variant for pair.
g=collections.defaultdict(dict)
for r in rows:
    key=(r['mint'], r['pool'])
    # latest line wins if duplicate
    if r['variant'] not in g[key] or r['i'] > g[key][r['variant']]['i']:
        g[key][r['variant']]=r
pairs31=[]; pairs3z=[]
for key,d in g.items():
    if 'Z3' in d and 'Z3.1' in d: pairs31.append((key,d['Z3'],d['Z3.1']))
    if 'Z' in d and 'Z3' in d: pairs3z.append((key,d['Z'],d['Z3']))

def paired_block(label, pairs, a, b):
    dif=[pb['pnl_pct']-pa['pnl_pct'] for _,pa,pb in pairs]
    difsol=[pb['pnl_sol']-pa['pnl_sol'] for _,pa,pb in pairs]
    better=sum(1 for x in dif if x>0)
    worse=sum(1 for x in dif if x<0)
    tie=len(dif)-better-worse
    lines.append(f'## Paired comparison: {label}')
    lines.append('')
    lines.append('Same `mint+pool`, latest completed row per variant.')
    lines.append('')
    lines.append('```text')
    lines.append(f'pairs={len(pairs)}')
    lines.append(f'{b}_better={better} ({better/len(pairs)*100 if pairs else 0:.2f}%) {a}_better={worse} ({worse/len(pairs)*100 if pairs else 0:.2f}%) tie={tie}')
    lines.append(f'diff_avg={avg(dif):.3f}% diff_median={med(dif):.3f}% diff_q25={q(dif,0.25):.3f}% diff_q75={q(dif,0.75):.3f}%')
    lines.append(f'diff_total_sol={sum(difsol):.6f} SOL')
    lines.append('```')
    lines.append('')
paired_block('Z3.1 - Z3', pairs31, 'Z3', 'Z3.1')
paired_block('Z3 - Z', pairs3z, 'Z', 'Z3')

# Regime buckets by Z3 MFE and entry reserve where comparable
lines.append('## 4. Bucket analysis: where Z3 or Z3.1 wins')
lines.append('')
if pairs31:
    buckets=[('MFE<25', lambda z3,z31: z3['mfe']<25), ('25<=MFE<50', lambda z3,z31: 25<=z3['mfe']<50), ('50<=MFE<100', lambda z3,z31: 50<=z3['mfe']<100), ('MFE>=100', lambda z3,z31: z3['mfe']>=100)]
    lines.append('```text')
    for name,fn in buckets:
        sub=[(k,z3,z31) for k,z3,z31 in pairs31 if fn(z3,z31)]
        if not sub: continue
        dif=[z31['pnl_pct']-z3['pnl_pct'] for _,z3,z31 in sub]
        lines.append(f'{name:12} n={len(sub):4d} z31_minus_z3_avg={avg(dif):7.3f}% med={med(dif):7.3f}% z31_better={sum(1 for x in dif if x>0)/len(dif)*100:6.2f}%')
    lines.append('```')
    lines.append('')

lines.append('## 5. Interpretation')
lines.append('')
# programmatic conclusion from paired diff and total
z3_total=sum(r['pnl_sol'] for r in by['Z3']); z31_total=sum(r['pnl_sol'] for r in by['Z3.1'])
pair_diff=sum((z31['pnl_sol']-z3['pnl_sol']) for _,z3,z31 in pairs31) if pairs31 else 0
z31_wr=sum(1 for r in by['Z3.1'] if r['pnl_pct']>0)/len(by['Z3.1'])*100 if by['Z3.1'] else 0
z3_wr=sum(1 for r in by['Z3'] if r['pnl_pct']>0)/len(by['Z3'])*100 if by['Z3'] else 0
if z3_total > z31_total and pair_diff < 0:
    winner='Z3'
elif z31_total > z3_total and pair_diff > 0:
    winner='Z3.1'
else:
    winner='mixed'
lines.append(f'Automatyczny werdykt po total + paired diff: `{winner}`.')
lines.append('')
lines.append('- Jeśli wybierać tylko po `total_sol` z całego state: porównać tabelę summary.')
lines.append('- Jeśli wybierać po paired same mint/pool: patrzeć na sekcję `Paired comparison: Z3.1 - Z3`.')
lines.append('- `profit_protect`, `early_no_momentum`, `hard_stop` występują praktycznie tylko w Z3, więc Z3 jest aktywniejszym risk managerem.')
lines.append('- Z3.1 zwykle ma prostszy profil: mniej aktywnych wczesnych wyjść, wyższy median/WR w niektórych oknach, ale może oddawać inny fragment ogona.')
lines.append('')
lines.append('## 6. Recommendation')
lines.append('')
if winner=='Z3':
    lines.append('Rekomendacja: `Z3` pozostaje paper winnerem, ale obserwować `Z3.1` jako benchmark, bo ma lepszy WR/median w części okien.')
elif winner=='Z3.1':
    lines.append('Rekomendacja: `Z3.1` jest kandydatem na paper winnera, ale nie wyłączać Z3 — Z3 dostarcza risk-management signals (`profit_protect`, `early_no_momentum`, `hard_stop`) do hybrydy.')
else:
    lines.append('Rekomendacja: brak czystego zwycięzcy; budować hybrydę: Z3 early risk controls + Z3.1 exit/hold profile tam, gdzie paired bucket pokazuje przewagę.')
lines.append('')
lines.append('Live: NIE. Fresh: SHADOW_ONLY. Następny krok po tej analizie: PumpPortal trade stream smoke/funding requirement.')

REPORT.write_text('\n'.join(lines)+"\n")
print(REPORT)
print('rows_completed', len(rows))
for v in VARIANTS:
    print(v, len(by[v]), sum(r['pnl_sol'] for r in by[v]), avg([r['pnl_pct'] for r in by[v]]), med([r['pnl_pct'] for r in by[v]]))
print('pairs_z31_z3', len(pairs31), 'pair_diff_sol_z31_minus_z3', sum(z31['pnl_sol']-z3['pnl_sol'] for _,z3,z31 in pairs31) if pairs31 else 0)
print('pairs_z3_z', len(pairs3z), 'pair_diff_sol_z3_minus_z', sum(z3['pnl_sol']-z['pnl_sol'] for _,z,z3 in pairs3z) if pairs3z else 0)
