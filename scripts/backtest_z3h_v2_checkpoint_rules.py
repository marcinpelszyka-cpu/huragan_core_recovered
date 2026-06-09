#!/usr/bin/env python3
"""
Offline Z3H_V2 checkpoint-rule backtest.

Safety: reads state.jsonl only and writes a markdown report. No runtime/config changes.
"""
import json
import math
import statistics
import collections
from dataclasses import dataclass
from pathlib import Path

STATE = Path('/opt/huragan_core/state.jsonl')
REPORT = Path('/opt/huragan_core/reports/z3h_v2_checkpoint_backtest_2026-06-05.md')
START_LINE = 46803
VARIANTS = ['Z3', 'Z3.1', 'Z3H_SHADOW']
STAKE_SOL = 0.003


def f(x):
    try:
        return float(x or 0.0)
    except Exception:
        return 0.0


def avg(xs):
    return sum(xs) / len(xs) if xs else 0.0


def med(xs):
    return statistics.median(xs) if xs else 0.0


def q(xs, p):
    if not xs:
        return 0.0
    xs = sorted(xs)
    k = (len(xs) - 1) * p
    lo = math.floor(k)
    hi = math.ceil(k)
    if lo == hi:
        return xs[lo]
    return xs[lo] * (hi - k) + xs[hi] * (k - lo)


def mdd_from_pcts(pcts):
    eq = peak = dd = 0.0
    for p in pcts:
        eq += STAKE_SOL * p / 100.0
        peak = max(peak, eq)
        dd = max(dd, peak - eq)
    return dd


def clean(r):
    return (
        r.get('status') == 'paper_completed'
        and f(r.get('max_favorable_pct')) <= 200
        and abs(f(r.get('net_pnl_pct'))) <= 300
        and r.get('exit_reason') not in ('price_unavailable', 'invalid_quote')
    )


def load_rows():
    rows = []
    with STATE.open() as fh:
        for i, line in enumerate(fh, 1):
            if not line.strip():
                continue
            try:
                r = json.loads(line)
            except Exception:
                continue
            if i >= START_LINE:
                rows.append((i, r))
    return rows


def metrics_from_pcts(pcts):
    return {
        'n': len(pcts),
        'wr': (sum(x > 0 for x in pcts) / len(pcts) * 100.0) if pcts else 0.0,
        'avg': avg(pcts),
        'med': med(pcts),
        'p25': q(pcts, 0.25),
        'p75': q(pcts, 0.75),
        'total_sol': sum(STAKE_SOL * x / 100.0 for x in pcts),
        'mdd_sol': mdd_from_pcts(pcts),
    }


def metrics(items):
    return metrics_from_pcts([f(r.get('net_pnl_pct')) for _, r in items])


def chunk_stats(pcts, size=50):
    chunks = []
    for idx in range(0, len(pcts), size):
        ch = pcts[idx:idx + size]
        if len(ch) < 20:
            continue
        m = metrics_from_pcts(ch)
        m['chunk'] = idx // size + 1
        chunks.append(m)
    return chunks


@dataclass
class Candidate:
    family: str
    rule: str
    pcts: list
    touched_tail: int
    rejected_baseline: int
    rejected_baseline_before: list
    rejected_baseline_after: list
    promoted_false: int = 0
    detected_tail: int = 0

    def summary(self, base):
        m = metrics_from_pcts(self.pcts)
        chunks = chunk_stats(self.pcts)
        pos_chunks = sum(c['avg'] > 0 for c in chunks)
        worst_chunk = min([c['avg'] for c in chunks], default=0.0)
        tail_touch_pct = self.touched_tail / max(1, base['tail_n']) * 100.0
        hard_reject = (
            tail_touch_pct > 10.0
            or m['total_sol'] <= base['z3h']['total_sol']
            or m['wr'] < base['z3h']['wr'] - 3.0
            or m['med'] < base['z3h']['med'] - 3.0
            or m['mdd_sol'] > base['z3h']['mdd_sol']
        )
        score = (
            (m['total_sol'] - base['z3h']['total_sol'])
            - 3.0 * max(0.0, tail_touch_pct - 10.0) / 100.0
            - 2.0 * max(0.0, m['mdd_sol'] - base['z3h']['mdd_sol'])
            + 0.001 * pos_chunks
        )
        return {
            **m,
            'pcts': self.pcts,
            'family': self.family,
            'rule': self.rule,
            'delta_sol': m['total_sol'] - base['z3h']['total_sol'],
            'delta_avg': m['avg'] - base['z3h']['avg'],
            'delta_wr': m['wr'] - base['z3h']['wr'],
            'touched_tail': self.touched_tail,
            'tail_touch_pct': tail_touch_pct,
            'rejected_baseline': self.rejected_baseline,
            'rejected_before_avg': avg(self.rejected_baseline_before),
            'rejected_after_avg': avg(self.rejected_baseline_after),
            'positive_chunks': pos_chunks,
            'chunk_count': len(chunks),
            'worst_chunk_avg': worst_chunk,
            'hard_reject': hard_reject,
            'score': score,
            'detected_tail': self.detected_tail,
            'promoted_false': self.promoted_false,
        }


def candidate_early(z3h_clean, family, checkpoint, pnl_thr, mfe_thr):
    pcts = []
    touched_tail = 0
    rejected = 0
    before = []
    after = []
    pnl_key = f'z3h_pnl_{checkpoint}s'
    mfe_key = f'z3h_mfe_{checkpoint}s'
    for _, r in z3h_clean:
        mode = r.get('z3h_selected_mode') or ''
        final = f(r.get('net_pnl_pct'))
        sim = final
        if mode == 'baseline_z31' and f(r.get(pnl_key)) <= pnl_thr and f(r.get(mfe_key)) <= mfe_thr:
            sim = f(r.get(pnl_key))
            rejected += 1
            before.append(final)
            after.append(sim)
        pcts.append(sim)
    return Candidate(
        family=family,
        rule=f'{checkpoint}s baseline if pnl_{checkpoint}<={pnl_thr} and mfe_{checkpoint}<={mfe_thr} -> exit pnl_{checkpoint}',
        pcts=pcts,
        touched_tail=touched_tail,
        rejected_baseline=rejected,
        rejected_baseline_before=before,
        rejected_baseline_after=after,
    )


def candidates_sequential(z3h_clean):
    out = []
    p30s = [-10, -5, 0]
    m30s = [3, 5, 8]
    p60s = [-10, -5, 0, 2.5]
    m60s = [5, 8, 10, 15]
    p120s = [-5, 0, 2.5, 5]
    m120s = [8, 10, 15, 20]
    # 30 AND 60 -> exit at 60
    for p30 in p30s:
        for m30 in m30s:
            for p60 in p60s:
                for m60 in m60s:
                    pcts=[]; rejected=0; before=[]; after=[]
                    for _,r in z3h_clean:
                        final=f(r.get('net_pnl_pct')); sim=final
                        if (r.get('z3h_selected_mode')=='baseline_z31'
                            and f(r.get('z3h_pnl_30s')) <= p30 and f(r.get('z3h_mfe_30s')) <= m30
                            and f(r.get('z3h_pnl_60s')) <= p60 and f(r.get('z3h_mfe_60s')) <= m60):
                            sim=f(r.get('z3h_pnl_60s')); rejected+=1; before.append(final); after.append(sim)
                        pcts.append(sim)
                    out.append(Candidate('D_seq_30_60', f'30(pnl<={p30},mfe<={m30}) AND 60(pnl<={p60},mfe<={m60}) -> exit60', pcts, 0, rejected, before, after))
    # 60 AND 120 -> exit at 120
    for p60 in p60s:
        for m60 in m60s:
            for p120 in p120s:
                for m120 in m120s:
                    pcts=[]; rejected=0; before=[]; after=[]
                    for _,r in z3h_clean:
                        final=f(r.get('net_pnl_pct')); sim=final
                        if (r.get('z3h_selected_mode')=='baseline_z31'
                            and f(r.get('z3h_pnl_60s')) <= p60 and f(r.get('z3h_mfe_60s')) <= m60
                            and f(r.get('z3h_pnl_120s')) <= p120 and f(r.get('z3h_mfe_120s')) <= m120):
                            sim=f(r.get('z3h_pnl_120s')); rejected+=1; before.append(final); after.append(sim)
                        pcts.append(sim)
                    out.append(Candidate('D_seq_60_120', f'60(pnl<={p60},mfe<={m60}) AND 120(pnl<={p120},mfe<={m120}) -> exit120', pcts, 0, rejected, before, after))
    return out


def tail_detection(z3h_clean):
    rows=[]
    grids=[
        ('60s', 'z3h_pnl_60s', 'z3h_mfe_60s', [10,15,20], [20,25,30]),
        ('120s', 'z3h_pnl_120s', 'z3h_mfe_120s', [15,20,25], [25,30,35]),
    ]
    tail_total=sum(1 for _,r in z3h_clean if r.get('z3h_selected_mode')=='tail_z3')
    base_total=sum(1 for _,r in z3h_clean if r.get('z3h_selected_mode')=='baseline_z31')
    for label,pk,mk,pgrid,mgrid in grids:
        for pt in pgrid:
            for mt in mgrid:
                detected=0; false=0
                for _,r in z3h_clean:
                    if f(r.get(pk))>=pt and f(r.get(mk))>=mt:
                        if r.get('z3h_selected_mode')=='tail_z3': detected+=1
                        elif r.get('z3h_selected_mode')=='baseline_z31': false+=1
                rows.append({
                    'rule': f'{label} pnl>={pt} mfe>={mt}',
                    'detected_tail': detected,
                    'tail_recall': detected/max(1,tail_total)*100,
                    'false_baseline': false,
                    'false_rate': false/max(1,base_total)*100,
                })
    return sorted(rows, key=lambda x:(-x['tail_recall'], x['false_rate']))


def fmt_m(m):
    return f"n={m['n']} WR={m['wr']:.2f}% avg={m['avg']:+.3f}% med={m['med']:+.3f}% total={m['total_sol']:+.6f} SOL MDD={m['mdd_sol']:.6f}"


def main():
    rows = load_rows()
    by_variant = {v: [] for v in VARIANTS}
    for i, r in rows:
        v = r.get('variant_id')
        if v in by_variant and clean(r):
            by_variant[v].append((i, r))
    z3h_clean = by_variant['Z3H_SHADOW']
    completed_z3h = [(i,r) for i,r in rows if r.get('variant_id')=='Z3H_SHADOW' and r.get('status')=='paper_completed']

    # Sanity assertions: fail loudly if dataset is not fit for this backtest.
    assert len(z3h_clean) >= 300, f'not enough clean Z3H rows: {len(z3h_clean)}'
    for key in ['z3h_pnl_30s','z3h_pnl_60s','z3h_pnl_120s','z3h_mfe_30s','z3h_mfe_60s','z3h_mfe_120s']:
        missing = sum(1 for _,r in completed_z3h if key not in r)
        assert missing == 0, f'missing {key} on {missing} completed rows'

    base_metrics = {v: metrics(items) for v, items in by_variant.items()}
    base = {
        'z3h': base_metrics['Z3H_SHADOW'],
        'tail_n': sum(1 for _,r in z3h_clean if r.get('z3h_selected_mode')=='tail_z3'),
    }

    candidates = []
    for pnl_thr in [-20,-15,-10,-7.5,-5,-2.5,0]:
        for mfe_thr in [0,3,5,8,10,15]:
            candidates.append(candidate_early(z3h_clean, 'A_30s', 30, pnl_thr, mfe_thr))
    for pnl_thr in [-20,-15,-10,-7.5,-5,-2.5,0,2.5,5]:
        for mfe_thr in [3,5,8,10,12,15,20]:
            candidates.append(candidate_early(z3h_clean, 'B_60s', 60, pnl_thr, mfe_thr))
    for pnl_thr in [-20,-15,-10,-7.5,-5,-2.5,0,2.5,5]:
        for mfe_thr in [5,8,10,12,15,20,25]:
            candidates.append(candidate_early(z3h_clean, 'C_120s', 120, pnl_thr, mfe_thr))
    candidates.extend(candidates_sequential(z3h_clean))

    summaries = [c.summary(base) for c in candidates]
    accepted = [s for s in summaries if not s['hard_reject']]
    top_all = sorted(summaries, key=lambda x: (x['score'], x['delta_sol']), reverse=True)[:20]
    top_ok = sorted(accepted, key=lambda x: (x['score'], x['delta_sol']), reverse=True)[:20]
    tail_rows = tail_detection(z3h_clean)

    best = top_ok[0] if top_ok else None
    decision = 'REJECT_V2_FILTERS'
    reason = 'No candidate passed hard gates.'
    if best and best['delta_sol'] > 0 and best['rejected_baseline'] > 0:
        decision = 'IMPLEMENT_Z3H_V2_SHADOW_PAPER_ONLY'
        reason = 'Best candidate improves clean total SOL while preserving tail and passing hard gates.'
    elif best:
        decision = 'COLLECT_MORE_OR_KEEP_Z3H'
        reason = 'Candidate passes hard gates but does not materially improve baseline.'

    lines=[]
    lines.append('# Z3H_V2 checkpoint-rule offline backtest — 2026-06-05')
    lines.append('')
    lines.append('## Safety')
    lines.append('')
    lines.append('Offline only. Read `state.jsonl`; wrote this report. No `.env`, no service restart, no runtime strategy change, no live.')
    lines.append('')
    lines.append('## Dataset')
    lines.append('')
    lines.append(f'```text\nSTART_LINE={START_LINE}\nZ3H clean={len(z3h_clean)}\nZ3 clean={len(by_variant["Z3"])}\nZ3.1 clean={len(by_variant["Z3.1"])}\n```')
    lines.append('')
    lines.append('## Baseline variant metrics, clean')
    lines.append('')
    lines.append('```text')
    for v in VARIANTS:
        lines.append(f'{v}: {fmt_m(base_metrics[v])}')
    lines.append('```')
    lines.append('')
    lines.append('## Z3H mode metrics, clean')
    lines.append('')
    for mode in ['baseline_z31','tail_z3']:
        items=[(i,r) for i,r in z3h_clean if r.get('z3h_selected_mode')==mode]
        lines.append(f'```text\n{mode}: {fmt_m(metrics(items))}\n```')
    lines.append('')
    lines.append('## Best accepted candidates')
    lines.append('')
    if top_ok:
        lines.append('| rank | family | delta SOL | n | WR | avg | med | MDD | reject baseline | rejected before->after avg | worst chunk | rule |')
        lines.append('|---:|---|---:|---:|---:|---:|---:|---:|---:|---|---:|---|')
        for idx,s in enumerate(top_ok[:15],1):
            lines.append(f"| {idx} | {s['family']} | {s['delta_sol']:+.6f} | {s['n']} | {s['wr']:.2f}% | {s['avg']:+.3f}% | {s['med']:+.3f}% | {s['mdd_sol']:.6f} | {s['rejected_baseline']} | {s['rejected_before_avg']:+.3f}% -> {s['rejected_after_avg']:+.3f}% | {s['worst_chunk_avg']:+.3f}% | `{s['rule']}` |")
    else:
        lines.append('No accepted candidates.')
    lines.append('')
    lines.append('## Top candidates including hard rejects')
    lines.append('')
    lines.append('| rank | hard_reject | family | delta SOL | WR | avg | med | MDD | tail touched | reject baseline | rule |')
    lines.append('|---:|---|---|---:|---:|---:|---:|---:|---:|---:|---|')
    for idx,s in enumerate(top_all[:15],1):
        lines.append(f"| {idx} | {s['hard_reject']} | {s['family']} | {s['delta_sol']:+.6f} | {s['wr']:.2f}% | {s['avg']:+.3f}% | {s['med']:+.3f}% | {s['mdd_sol']:.6f} | {s['tail_touch_pct']:.2f}% | {s['rejected_baseline']} | `{s['rule']}` |")
    lines.append('')
    lines.append('## Tail detection sanity')
    lines.append('')
    lines.append('| rank | rule | tail recall | detected tail | false baseline | false baseline rate |')
    lines.append('|---:|---|---:|---:|---:|---:|')
    for idx,row in enumerate(tail_rows[:12],1):
        lines.append(f"| {idx} | `{row['rule']}` | {row['tail_recall']:.2f}% | {row['detected_tail']} | {row['false_baseline']} | {row['false_rate']:.2f}% |")
    lines.append('')
    lines.append('## Chunk comparison')
    lines.append('')
    current_pcts=[f(r.get('net_pnl_pct')) for _,r in z3h_clean]
    best_pcts=best['pcts'] if best else current_pcts
    lines.append('| chunk | current avg | current total SOL | best avg | best total SOL |')
    lines.append('|---:|---:|---:|---:|---:|')
    cur_chunks=chunk_stats(current_pcts); best_chunks=chunk_stats(best_pcts)
    for c,b in zip(cur_chunks,best_chunks):
        lines.append(f"| {c['chunk']} | {c['avg']:+.3f}% | {c['total_sol']:+.6f} | {b['avg']:+.3f}% | {b['total_sol']:+.6f} |")
    lines.append('')
    lines.append('## Decision')
    lines.append('')
    lines.append(f'```text\nwinner={best["rule"] if best else "NONE"}\ndecision={decision}\nreason={reason}\n```')
    if best:
        lines.append('')
        lines.append('Best candidate metrics:')
        lines.append('')
        lines.append(f"```text\nfamily={best['family']}\ndelta_sol={best['delta_sol']:+.6f}\nWR={best['wr']:.2f}%\navg={best['avg']:+.3f}%\nmedian={best['med']:+.3f}%\nMDD={best['mdd_sol']:.6f}\nrejected_baseline={best['rejected_baseline']}\nrejected_before_avg={best['rejected_before_avg']:+.3f}%\nrejected_after_avg={best['rejected_after_avg']:+.3f}%\npositive_chunks={best['positive_chunks']}/{best['chunk_count']}\nworst_chunk_avg={best['worst_chunk_avg']:+.3f}%\n```")
    REPORT.write_text('\n'.join(lines)+'\n')
    print(f'wrote {REPORT}')
    print(f'decision={decision}')
    if best:
        print(f"winner={best['rule']}")
        print(f"delta_sol={best['delta_sol']:+.6f} avg={best['avg']:+.3f} wr={best['wr']:.2f} med={best['med']:+.3f} rejected={best['rejected_baseline']}")


if __name__ == '__main__':
    main()
