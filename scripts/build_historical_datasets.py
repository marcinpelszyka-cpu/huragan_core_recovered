#!/usr/bin/env python3
import argparse, csv, json, math, statistics
from collections import defaultdict
from pathlib import Path

PROFIT_PCT_MIN_DEFAULT = 20.0
FRESH_MOONSHOT_PCT_DEFAULT = 100.0
MAX_SANE_MFE_PCT_DEFAULT = 1000.0


def read_jsonl(path: Path):
    if not path.exists():
        return []
    out = []
    with path.open() as f:
        for i, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            try:
                out.append(json.loads(line))
            except Exception as e:
                print(f"WARN bad_json path={path} line={i}: {e}")
    return out


def read_csv(path: Path):
    if not path.exists():
        return []
    with path.open(newline='') as f:
        return list(csv.DictReader(f))


def fnum(v, default=0.0):
    try:
        if v is None or v == "":
            return default
        x = float(v)
        if math.isnan(x) or math.isinf(x):
            return default
        return x
    except Exception:
        return default


def inum(v, default=0):
    try:
        if v is None or v == "":
            return default
        return int(float(v))
    except Exception:
        return default


def latest_by(rows, key_fn):
    out = {}
    for r in rows:
        k = key_fn(r)
        if k:
            out[k] = r
    return out


def median(xs):
    xs = [x for x in xs if isinstance(x, (int, float))]
    return statistics.median(xs) if xs else 0.0


def avg(xs):
    xs = [x for x in xs if isinstance(x, (int, float))]
    return sum(xs) / len(xs) if xs else 0.0


def write_csv(path: Path, rows, fields):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('w', newline='') as f:
        w = csv.DictWriter(f, fieldnames=fields, extrasaction='ignore')
        w.writeheader()
        for r in rows:
            w.writerow(r)


def build_migration(rows, gtfa_rows, profit_pct_min, max_sane_mfe_pct):
    gtfa_by_mint = latest_by(gtfa_rows, lambda r: r.get('mint'))
    terminal = [r for r in rows if r.get('status') in ('paper_completed', 'quote_unsupported_shadow', 'holding')]
    latest = latest_by(terminal, lambda r: (r.get('mint'), r.get('variant_id', '')) if r.get('mint') else None)
    clean = []
    for r in latest.values():
        if r.get('status') != 'paper_completed':
            continue
        if r.get('excluded_from_stats'):
            continue
        if r.get('exit_reason') in {'price_unavailable', 'data_quality_fail', 'invalid_quote'}:
            continue
        if fnum(r.get('paper_entry_sol')) <= 0:
            continue
        clean.append(r)

    by_mint = defaultdict(list)
    for r in clean:
        by_mint[r.get('mint')].append(r)

    winners, quote_spike_suspects, mint_summary = [], [], []
    for mint, rs in by_mint.items():
        best = max(rs, key=lambda r: fnum(r.get('net_pnl_pct')))
        gtfa = gtfa_by_mint.get(mint, {})
        best_pct = fnum(best.get('net_pnl_pct'))
        paper_mfe = max(fnum(r.get('max_favorable_pct')) for r in rs)
        gtfa_mfe = fnum(gtfa.get('mfe_pct'), default=paper_mfe)
        best_mfe = gtfa_mfe if gtfa else paper_mfe
        quote_spike_suspect = bool(gtfa.get('quote_spike_suspect')) or best_mfe > max_sane_mfe_pct
        row = {
            'dataset': 'migration_amm',
            'mint': mint,
            'best_variant': best.get('variant_id', ''),
            'best_net_pnl_pct': round(best_pct, 6),
            'best_net_pnl_sol': round(fnum(best.get('net_pnl_sol')), 12),
            'best_mfe_pct': round(best_mfe, 6),
            'paper_mfe_pct': round(paper_mfe, 6),
            'gtfa_mfe_pct': round(gtfa_mfe, 6) if gtfa else '',
            'best_hold_secs': inum(best.get('hold_secs')),
            'best_exit_reason': best.get('exit_reason', ''),
            'quote_symbol': best.get('quote_symbol', ''),
            'source': best.get('source', ''),
            'pool_state': best.get('pool_state', ''),
            'base_mint': best.get('base_mint', ''),
            'quote_mint': best.get('quote_mint', ''),
            'pool_base_token_account': best.get('pool_base_token_account', ''),
            'pool_quote_token_account': best.get('pool_quote_token_account', ''),
            'creator_address': best.get('creator_address', ''),
            'entry_quote_reserve_raw': best.get('entry_quote_reserve_raw', 0),
            'min_quote_reserve_raw': best.get('min_quote_reserve_raw', 0),
            'max_drawdown_pct': round(fnum(gtfa.get('max_drawdown_pct'), fnum(best.get('max_drawdown_pct'))), 6),
            'advanced_gate_passed': bool(best.get('advanced_gate_passed')),
            'advanced_gate_reason': best.get('advanced_gate_reason', ''),
            'variant_count': len(rs),
            'quote_spike_suspect': quote_spike_suspect,
            'gtfa_enriched': bool(gtfa),
        }
        mint_summary.append(row)
        if quote_spike_suspect:
            quote_spike_suspects.append(row)
        elif best_pct >= profit_pct_min or (0 <= best_mfe <= max_sane_mfe_pct and best_mfe >= profit_pct_min):
            winners.append(row)

    variant_rows = []
    for variant in sorted({r.get('variant_id','') for r in clean if r.get('variant_id')}):
        rs = [r for r in clean if r.get('variant_id') == variant]
        wins = [r for r in rs if fnum(r.get('net_pnl_sol')) > 0]
        variant_rows.append({
            'variant_id': variant,
            'n': len(rs),
            'wr_pct': round(len(wins) / len(rs) * 100, 4) if rs else 0.0,
            'avg_pnl_pct': round(avg([fnum(r.get('net_pnl_pct')) for r in rs]), 6),
            'median_pnl_pct': round(median([fnum(r.get('net_pnl_pct')) for r in rs]), 6),
            'total_sol': round(sum(fnum(r.get('net_pnl_sol')) for r in rs), 12),
            'avg_mfe_pct': round(avg([fnum(r.get('max_favorable_pct')) for r in rs]), 6),
        })
    return clean, mint_summary, winners, quote_spike_suspects, variant_rows


def fresh_label(row):
    label = row.get('label') or row.get('exit_label') or row.get('final_exit_label') or ''
    if label:
        return label
    if row.get('trade_stream_missing') in (True, 'True', 'true', '1'):
        return 'no_trade_data'
    max_change = fnum(row.get('max_change_pct'))
    if max_change >= 100.0:
        return 'moonshot_100k_or_2x'
    if max_change >= 50.0:
        return 'pump_40k_or_50pct'
    entry = fnum(row.get('entry_market_cap_sol'))
    mc60 = fnum(row.get('mc_60s') or row.get('current_market_cap_sol'))
    if entry > 0 and mc60 > 0 and mc60 <= entry * 0.5:
        return 'rug_60s'
    return 'flat'


def normalize_fresh_row(mint, c, snapshots, gtfa):
    ss = sorted(snapshots, key=lambda r: inum(r.get('age_secs')))
    final = ss[-1] if ss else {}
    entry = fnum(gtfa.get('entry_market_cap_sol') or final.get('entry_market_cap_sol') or c.get('marketCapSol'))
    max_mc = max(
        [fnum(gtfa.get('max_mc_300s')), fnum(final.get('max_market_cap_sol')), entry]
        + [fnum(s.get('max_market_cap_sol')) for s in ss]
    )
    final_mc = fnum(gtfa.get('mc_300s') or final.get('current_market_cap_sol'))
    max_change_pct = fnum(gtfa.get('max_change_pct'))
    if max_change_pct == 0.0 and entry > 0 and max_mc > 0:
        max_change_pct = (max_mc / entry - 1.0) * 100.0
    max_buy_count = max([inum(gtfa.get('buy_count_60s'))] + [inum(s.get('buy_count')) for s in ss] + [0])
    max_sell_count = max([inum(gtfa.get('sell_count_60s'))] + [inum(s.get('sell_count')) for s in ss] + [0])
    max_buyers = max([inum(gtfa.get('unique_buyers_60s'))] + [inum(s.get('unique_buyers')) for s in ss] + [0])
    max_net_flow = max([fnum(gtfa.get('net_flow_sol_60s'))] + [fnum(s.get('net_flow_sol')) for s in ss] + [0.0])
    trade_stream_missing = bool(gtfa.get('trade_stream_missing')) if gtfa else all(inum(s.get('buy_count')) + inum(s.get('sell_count')) == 0 for s in ss)
    row = {
        'dataset': 'fresh_launch',
        'mint': mint,
        'name': c.get('name') or final.get('name', '') or gtfa.get('name', ''),
        'symbol': c.get('symbol') or final.get('symbol', '') or gtfa.get('symbol', ''),
        'creator': c.get('traderPublicKey') or final.get('creator', '') or gtfa.get('creator', ''),
        'entry_market_cap_sol': round(entry, 6),
        'max_market_cap_sol': round(max_mc, 6),
        'final_market_cap_sol': round(final_mc, 6),
        'max_change_pct': round(max_change_pct, 6),
        'final_change_pct': round(((final_mc / entry - 1.0) * 100.0) if entry > 0 and final_mc > 0 else 0.0, 6),
        'max_buy_count': max_buy_count,
        'max_sell_count': max_sell_count,
        'max_unique_buyers': max_buyers,
        'max_net_flow_sol': round(max_net_flow, 9),
        'final_exit_label': fresh_label(gtfa or final or {}),
        'last_age_secs': inum(final.get('age_secs')),
        'create_sol_amount': c.get('solAmount', ''),
        'create_market_cap_sol': c.get('marketCapSol', ''),
        'trade_stream_missing': trade_stream_missing,
        'gtfa_enriched': bool(gtfa),
    }
    row['final_exit_label'] = fresh_label(row)
    return row


def build_fresh(candidates, snapshots, v2_snapshots, gtfa_rows, moonshot_pct_min):
    cand_by_mint = latest_by(candidates, lambda r: r.get('mint'))
    gtfa_by_mint = latest_by(gtfa_rows, lambda r: r.get('mint'))
    by_mint = defaultdict(list)
    for s in snapshots + v2_snapshots:
        if s.get('mint'):
            by_mint[s.get('mint')].append(s)
    for mint in gtfa_by_mint:
        by_mint.setdefault(mint, [])

    rows, winners, rugs = [], [], []
    for mint, ss in by_mint.items():
        row = normalize_fresh_row(mint, cand_by_mint.get(mint, {}), ss, gtfa_by_mint.get(mint, {}))
        rows.append(row)
        if row['final_exit_label'] == 'rug_60s':
            rugs.append(row)
        if fnum(row['max_change_pct']) >= moonshot_pct_min or row['final_exit_label'] == 'moonshot_100k_or_2x':
            winners.append(row)
    return rows, winners, rugs


def write_report(path, migration_clean, migration_winners, quote_spike_suspects, variant_rows, fresh_rows, fresh_winners, fresh_rugs, args):
    path.parent.mkdir(parents=True, exist_ok=True)
    no_trade = sum(1 for r in fresh_rows if r.get('trade_stream_missing'))
    gtfa_migration = sum(1 for r in migration_winners if r.get('gtfa_enriched'))
    usdc = sum(1 for r in migration_clean if r.get('quote_symbol') == 'USDC')
    with path.open('w') as f:
        f.write('# Historical Profit Token Dataset Report\n\n')
        f.write('## Scope\n')
        f.write(f'- Migration rows clean: {len(migration_clean)}\n')
        f.write(f'- Migration sane winner mints MFE/net >= {args.profit_pct_min:.1f}%: {len(migration_winners)}\n')
        f.write(f'- Migration quote spike suspects excluded: {len(quote_spike_suspects)}\n')
        f.write(f'- Migration winners with gTFA enrichment: {gtfa_migration}\n')
        f.write(f'- USDC pool observations: {usdc}\n')
        f.write(f'- Fresh tracked mints: {len(fresh_rows)}\n')
        f.write(f'- Fresh moonshot mints max_change >= {args.fresh_moonshot_pct_min:.1f}%: {len(fresh_winners)}\n')
        f.write(f'- Fresh rug cases: {len(fresh_rugs)}\n')
        f.write(f'- Fresh missing trade stream: {no_trade}/{len(fresh_rows)}\n\n')
        f.write('## Migration variant metrics\n')
        for r in sorted(variant_rows, key=lambda x: x['avg_pnl_pct'], reverse=True):
            f.write(f"- {r['variant_id']}: n={r['n']} WR={r['wr_pct']:.1f}% avg={r['avg_pnl_pct']:.2f}% median={r['median_pnl_pct']:.2f}% total={r['total_sol']:.6f} SOL avg_mfe={r['avg_mfe_pct']:.2f}%\n")
        f.write('\n## Top migration winners\n')
        for r in sorted(migration_winners, key=lambda x: fnum(x['best_mfe_pct']), reverse=True)[:25]:
            f.write(f"- {r['mint']} variant={r['best_variant']} net={r['best_net_pnl_pct']:.2f}% mfe={r['best_mfe_pct']:.2f}% hold={r['best_hold_secs']}s exit={r['best_exit_reason']} quote={r['quote_symbol']} gtfa={r['gtfa_enriched']}\n")
        f.write('\n## Top fresh moonshots\n')
        for r in sorted(fresh_winners, key=lambda x: fnum(x['max_change_pct']), reverse=True)[:25]:
            f.write(f"- {r['mint']} {r['symbol']} entry_mc={r['entry_market_cap_sol']:.2f} max_mc={r['max_market_cap_sol']:.2f} change={r['max_change_pct']:.2f}% buyers={r['max_unique_buyers']} flow={r['max_net_flow_sol']:.4f} label={r['final_exit_label']}\n")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--state', default='state.jsonl')
    ap.add_argument('--fresh-candidates', default='fresh_momentum_candidates.jsonl')
    ap.add_argument('--fresh-snapshots', default='fresh_lifecycle_snapshots.jsonl')
    ap.add_argument('--fresh-v2-snapshots', default='fresh_lifecycle_v2_snapshots.jsonl')
    ap.add_argument('--migration-gtfa', default='datasets/migration_gtfa_lifecycle.jsonl')
    ap.add_argument('--fresh-gtfa', default='datasets/fresh_gtfa_lifecycle.jsonl')
    ap.add_argument('--out-dir', default='datasets')
    ap.add_argument('--profit-pct-min', type=float, default=PROFIT_PCT_MIN_DEFAULT)
    ap.add_argument('--fresh-moonshot-pct-min', type=float, default=FRESH_MOONSHOT_PCT_DEFAULT)
    ap.add_argument('--max-sane-mfe-pct', type=float, default=MAX_SANE_MFE_PCT_DEFAULT)
    args = ap.parse_args()

    out = Path(args.out_dir)
    state_rows = read_jsonl(Path(args.state))
    cand_rows = read_jsonl(Path(args.fresh_candidates))
    snap_rows = read_jsonl(Path(args.fresh_snapshots))
    v2_snap_rows = read_jsonl(Path(args.fresh_v2_snapshots))
    migration_gtfa = read_jsonl(Path(args.migration_gtfa))
    fresh_gtfa = read_jsonl(Path(args.fresh_gtfa))

    migration_clean, migration_summary, migration_winners, quote_spike_suspects, variant_rows = build_migration(state_rows, migration_gtfa, args.profit_pct_min, args.max_sane_mfe_pct)
    fresh_rows, fresh_winners, fresh_rugs = build_fresh(cand_rows, snap_rows, v2_snap_rows, fresh_gtfa, args.fresh_moonshot_pct_min)

    migration_fields = ['dataset','mint','best_variant','best_net_pnl_pct','best_net_pnl_sol','best_mfe_pct','paper_mfe_pct','gtfa_mfe_pct','best_hold_secs','best_exit_reason','quote_symbol','source','pool_state','base_mint','quote_mint','pool_base_token_account','pool_quote_token_account','creator_address','entry_quote_reserve_raw','min_quote_reserve_raw','max_drawdown_pct','advanced_gate_passed','advanced_gate_reason','variant_count','quote_spike_suspect','gtfa_enriched']
    fresh_fields = ['dataset','mint','name','symbol','creator','entry_market_cap_sol','max_market_cap_sol','final_market_cap_sol','max_change_pct','final_change_pct','max_buy_count','max_sell_count','max_unique_buyers','max_net_flow_sol','final_exit_label','last_age_secs','create_sol_amount','create_market_cap_sol','trade_stream_missing','gtfa_enriched']
    variant_fields = ['variant_id','n','wr_pct','avg_pnl_pct','median_pnl_pct','total_sol','avg_mfe_pct']

    write_csv(out / 'migration_all_mint_summary.csv', migration_summary, migration_fields)
    write_csv(out / 'migration_profit_winners.csv', sorted(migration_winners, key=lambda r: fnum(r['best_mfe_pct']), reverse=True), migration_fields)
    write_csv(out / 'migration_quote_spike_suspects.csv', sorted(quote_spike_suspects, key=lambda r: fnum(r['best_mfe_pct']), reverse=True), migration_fields)
    write_csv(out / 'migration_variant_metrics.csv', variant_rows, variant_fields)
    write_csv(out / 'fresh_all_mint_summary.csv', fresh_rows, fresh_fields)
    write_csv(out / 'fresh_moonshot_winners.csv', sorted(fresh_winners, key=lambda r: fnum(r['max_change_pct']), reverse=True), fresh_fields)
    write_csv(out / 'fresh_rug_cases.csv', sorted(fresh_rugs, key=lambda r: fnum(r['max_change_pct'])), fresh_fields)
    write_report(out / 'historical_dataset_report.md', migration_clean, migration_winners, quote_spike_suspects, variant_rows, fresh_rows, fresh_winners, fresh_rugs, args)

    print(json.dumps({
        'state_rows': len(state_rows),
        'migration_clean_rows': len(migration_clean),
        'migration_winners': len(migration_winners),
        'migration_quote_spike_suspects': len(quote_spike_suspects),
        'migration_gtfa_rows': len(migration_gtfa),
        'fresh_candidates': len(cand_rows),
        'fresh_snapshots': len(snap_rows) + len(v2_snap_rows),
        'fresh_gtfa_rows': len(fresh_gtfa),
        'fresh_tracked_mints': len(fresh_rows),
        'fresh_moonshot_winners': len(fresh_winners),
        'fresh_rug_cases': len(fresh_rugs),
        'out_dir': str(out),
    }, indent=2))

if __name__ == '__main__':
    main()
