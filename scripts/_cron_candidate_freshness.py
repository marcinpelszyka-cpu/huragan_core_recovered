#!/usr/bin/env python3
"""Check candidate JSONL freshness and dump timing stats."""
import json, os, time

cpath = 'fresh_momentum_candidates.jsonl'
mtime = os.path.getmtime(cpath)
now = time.time()
age_min = (now - mtime) / 60

lines = []
with open(cpath) as f:
    for line in f:
        try:
            lines.append(json.loads(line))
        except:
            pass

print(f'File: {cpath}')
print(f'  Modified: {time.strftime("%Y-%m-%d %H:%M:%SZ", time.gmtime(mtime))}')
print(f'  Age: {age_min:.1f} min ago')
print(f'  Lines: {len(lines)}')

# Dump timing from trade events (using correct field names)
print()
te_path = 'datasets/sniper_trade_events.jsonl'
first_sells = {}  # mint → first_sell_age
with open(te_path) as f:
    for line in f:
        try:
            e = json.loads(line)
        except:
            continue
        mint = e.get('mint', '')
        side = e.get('side', 'buy')
        age_s = e.get('age_secs')
        sol = float(e.get('quote_delta_sol', 0) or 0)
        if side == 'sell' and age_s is not None and sol > 0:
            if mint not in first_sells or age_s < first_sells[mint]:
                first_sells[mint] = age_s

fs_vals = sorted(first_sells.values()) if first_sells else []
print(f'=== Dump timing (from trade events, side=sell) ===')
print(f'  Mints with sells: {len(first_sells)}')
if fs_vals:
    print(f'  First-sell ages: min={min(fs_vals)}s  median={fs_vals[len(fs_vals)//2]}s  max={max(fs_vals)}s')
    buckets = {5: 0, 10: 0, 20: 0, 999: 0}
    for a in fs_vals:
        if a <= 5: buckets[5] += 1
        elif a <= 10: buckets[10] += 1
        elif a <= 20: buckets[20] += 1
        else: buckets[999] += 1
    print(f'  0-5s: {buckets[5]}  6-10s: {buckets[10]}  11-20s: {buckets[20]}  20s+: {buckets[999]}')

# Trough ages for WOULD_BUY_DIP
print()
print(f'=== WOULD_BUY_DIP trough timing ===')
with open('datasets/fresh_3500_shadow_signals.jsonl') as f:
    for line in f:
        e = json.loads(line)
        if e.get('signal') == 'WOULD_BUY_DIP':
            mint = e.get('mint', '')[:16]
            creator = e.get('creator', '')[:12] if e.get('creator') else '?'
            print(f'  {mint}... pump={e.get("pump_pct",0):.1f}% dump={e.get("dump_pct",0):.1f}% trough={e.get("trough_age_s","?")}s recovery={e.get("recovery_ratio",0):.3f}x creator={creator}...')
