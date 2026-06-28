#!/usr/bin/env python3
"""Paper-only quote validation metadata report.

Reads state.jsonl and TASK_07 classifier artifacts. Does not change runtime.
For rows produced before the validator fields existed, uses classifier fallback so
historical audit artifacts remain useful.
"""
from __future__ import annotations
import argparse, json, statistics
from pathlib import Path
from typing import Any

OUT=Path('artifacts/task_07_price_quote_validator_implementation')
TERMINAL={'paper_completed','completed'}

def read_jsonl(p:Path):
    if not p.exists(): return []
    out=[]
    for line in p.read_text(errors='ignore').splitlines():
        if not line.strip(): continue
        try: out.append(json.loads(line))
        except Exception: pass
    return out

def f(v:Any,d=0.0):
    try: return float(v)
    except Exception: return d

def reason(r): return r.get('exit_reason') or r.get('terminal_reason') or r.get('live_exit_reason') or 'unknown'
def pnl(r): return f(r.get('net_pnl_pct'), f(r.get('realized_pnl_pct'),0.0))
def mfe(r): return f(r.get('max_favorable_pct'),0.0)
def dd(r): return f(r.get('max_drawdown_pct'),0.0)

def load_classifier(base:Path):
    m={}
    for r in read_jsonl(base/'classified_trades.jsonl'):
        key=(r.get('line_no'), r.get('mint'), r.get('variant'))
        m[key]=r
    return m

def classify(row, line_no, clf):
    c=clf.get((line_no,row.get('mint'),row.get('variant_id')))
    has_native=any(k in row for k in ['quote_valid','quote_unavailable','quote_invalid','quote_artifact','valuation_uncertain','exit_design_eligible'])
    if has_native:
        q={k:row.get(k,False) for k in ['quote_valid','quote_unavailable','quote_stale','quote_invalid','quote_artifact','valuation_uncertain','exit_affected','metrics_eligible','exit_design_eligible']}
        codes=row.get('quote_reason_codes') or []
    elif c:
        bucket=c.get('data_quality_bucket')
        q={
            'quote_valid': bucket in {'clean_trade','clean_runner','real_mfe_giveback_problem','real_rug_or_death'},
            'quote_unavailable': bucket in {'price_unavailable_profitable','price_unavailable_exit_affected'},
            'quote_stale': False,
            'quote_invalid': bucket=='invalid_quote_anomaly',
            'quote_artifact': bucket=='quote_artifact',
            'valuation_uncertain': bucket in {'price_unavailable_profitable','price_unavailable_exit_affected','invalid_quote_anomaly','quote_artifact'},
            'exit_affected': bucket=='price_unavailable_exit_affected',
            'metrics_eligible': bool(c.get('clean_metrics_eligible')),
            'exit_design_eligible': bool(c.get('exit_design_eligible')),
        }
        codes=c.get('reason_codes') or [bucket]
    else:
        q={'quote_valid':False,'quote_unavailable':reason(row)=='price_unavailable','quote_stale':False,'quote_invalid':reason(row)=='invalid_quote','quote_artifact':False,'valuation_uncertain':True,'exit_affected':reason(row)=='price_unavailable','metrics_eligible':False,'exit_design_eligible':False}
        codes=['legacy_unclassified']
    return {
        'line_no':line_no,'mint':row.get('mint',''),'variant':row.get('variant_id',''),'exit_reason':reason(row),
        'pnl_pct':pnl(row),'mfe_pct':mfe(row),'drawdown_pct':dd(row),'hold_secs':row.get('hold_secs'),
        'last_valid_value_sol':row.get('last_valid_value_sol', row.get('last_valid_quote_sol',0.0)),
        'last_valid_quote_age_secs':row.get('last_valid_quote_age_secs',0),
        'quote_error_count':row.get('quote_error_count',0),'quote_retry_count':row.get('quote_retry_count',0),
        'quote_failure_stage':row.get('quote_failure_stage',''),'quote_source':row.get('quote_source',''),
        'fallback_used':row.get('fallback_used',False),'fallback_reason':row.get('fallback_reason',''),
        'quote_reason_codes':codes, **q,
    }

def metrics(rows):
    ps=[r['pnl_pct'] for r in rows]
    if not ps: return {'count':0,'wr':'0/0','avg_pnl_pct':0,'median_pnl_pct':0}
    return {'count':len(rows),'wr':f"{sum(x>0 for x in ps)}/{len(ps)}",'avg_pnl_pct':round(statistics.mean(ps),6),'median_pnl_pct':round(statistics.median(ps),6)}

def write_report(out,summary,rows):
    lines=['# TASK_07 Price Quote Validator Implementation Report','', 'Mode: **PAPER/VALIDATION ONLY**. No live, no real buy/sell, no policy change, no restart, no TASK_08.','', '## Summary','']
    for k,v in summary.items():
        if not isinstance(v,dict): lines.append(f'- {k}: **{v}**')
    lines += ['', '## Metrics Eligible', '']
    for k,v in summary['metrics_eligible_metrics'].items(): lines.append(f'- {k}: {v}')
    lines += ['', '## Exit Design Eligible', '']
    for k,v in summary['exit_design_eligible_metrics'].items(): lines.append(f'- {k}: {v}')
    lines += ['', '## Example rows', '', '| mint | variant | reason | pnl% | mfe% | valid | unavailable | invalid | artifact | exit_design | codes |','|---|---|---|---:|---:|---|---|---|---|---|---|']
    for r in rows[:40]:
        lines.append(f"| {r['mint'][:8]} | {r['variant']} | {r['exit_reason']} | {r['pnl_pct']:.3f} | {r['mfe_pct']:.3f} | {r['quote_valid']} | {r['quote_unavailable']} | {r['quote_invalid']} | {r['quote_artifact']} | {r['exit_design_eligible']} | {','.join(map(str,r['quote_reason_codes']))} |")
    lines += ['', '## Final guard', '', 'This report summarizes validation metadata only. No trading behavior changes.']
    (out/'validator_report.md').write_text('\n'.join(lines)+'\n')

def main():
    ap=argparse.ArgumentParser(); ap.add_argument('--state',default='state.jsonl'); ap.add_argument('--session',default='datasets/task07_v2_session.json'); ap.add_argument('--classifier-dir',default='artifacts/task_07_data_quality_classifier'); ap.add_argument('--out-dir',default=str(OUT)); args=ap.parse_args()
    out=Path(args.out_dir); out.mkdir(parents=True,exist_ok=True)
    start=0; sp=Path(args.session)
    if sp.exists(): start=json.loads(sp.read_text())['state_start_line']
    clf=load_classifier(Path(args.classifier_dir))
    rows=[]
    for idx,line in enumerate(Path(args.state).read_text(errors='ignore').splitlines()[start:], start=start+1):
        try: r=json.loads(line)
        except Exception: continue
        if str(r.get('variant_id','')).startswith(('Z3','Z3.1')) and r.get('status') in TERMINAL:
            rows.append(classify(r,idx,clf))
    summary={
        'total_completed':len(rows),
        'quote_valid_count':sum(1 for r in rows if r['quote_valid']),
        'quote_unavailable_count':sum(1 for r in rows if r['quote_unavailable']),
        'quote_invalid_count':sum(1 for r in rows if r['quote_invalid']),
        'quote_artifact_count':sum(1 for r in rows if r['quote_artifact']),
        'valuation_uncertain_count':sum(1 for r in rows if r['valuation_uncertain']),
        'exit_affected_count':sum(1 for r in rows if r['exit_affected']),
        'metrics_eligible_count':sum(1 for r in rows if r['metrics_eligible']),
        'exit_design_eligible_count':sum(1 for r in rows if r['exit_design_eligible']),
        'price_unavailable_profitable_count':sum(1 for r in rows if r['quote_unavailable'] and r['pnl_pct']>0 and not r['quote_invalid'] and not r['quote_artifact']),
        'price_unavailable_exit_affected_count':sum(1 for r in rows if r['exit_affected'] and r['quote_unavailable']),
        'native_validator_rows_seen':sum(1 for r in rows if r['quote_source'] or r['last_valid_quote_age_secs'] or r['quote_error_count']),
        'metrics_eligible_metrics':metrics([r for r in rows if r['metrics_eligible']]),
        'exit_design_eligible_metrics':metrics([r for r in rows if r['exit_design_eligible']]),
    }
    files={
        'quote_validation_examples.jsonl':rows[:1000],
        'price_unavailable_classified.jsonl':[r for r in rows if r['quote_unavailable']],
        'invalid_quote_examples.jsonl':[r for r in rows if r['quote_invalid'] or r['quote_artifact']],
        'exit_design_eligible_sample.jsonl':[r for r in rows if r['exit_design_eligible']][:1000],
    }
    for name,data in files.items():
        with (out/name).open('w') as f:
            for r in data: f.write(json.dumps(r,sort_keys=True)+'\n')
    (out/'validator_summary.json').write_text(json.dumps(summary,indent=2,sort_keys=True)+'\n')
    write_report(out,summary,rows)
    print(json.dumps(summary,indent=2,sort_keys=True))
if __name__=='__main__': main()
