#!/usr/bin/env python3
"""Cron analysis: find repeat dumpers and WOULD_BUY_DIP creators."""
import json
from collections import defaultdict

creators = defaultdict(lambda: {'mints': set(), 'buy_at_t0': False, 'sold_in_300s': False, 'total_sell_sol': 0.0})

with open('datasets/sniper_trade_events.jsonl') as f:
    for line in f:
        try:
            e = json.loads(line)
        except:
            continue
        signer = e.get('signer', '')
        mint = e.get('mint', '')
        age_s = e.get('age_secs', 0)
        side = e.get('side', 'buy')
        sol_amount = float(e.get('quote_delta_sol', 0) or 0)
        
        if age_s is not None and age_s <= 1 and side == 'buy':
            creators[signer]['buy_at_t0'] = True
            creators[signer]['mints'].add(mint)
        elif side == 'buy' and age_s is not None:
            creators[signer]['mints'].add(mint)
        
        if side == 'sell' and age_s is not None and age_s <= 300:
            creators[signer]['sold_in_300s'] = True
            creators[signer]['total_sell_sol'] += sol_amount
            creators[signer]['mints'].add(mint)

dual = [(addr, len(d['mints']), d['total_sell_sol']) for addr, d in creators.items() if d['buy_at_t0'] and d['sold_in_300s'] and len(d['mints']) >= 2]
dual.sort(key=lambda x: -x[1])
print('=== Buy@t0 + Dump within 300s (≥2 mints) ===')
for addr, mints, sold in dual[:10]:
    print(f'  {addr[:12]}...  mints={mints}  sold={sold:.3f} SOL')

wbd = set()
with open('datasets/fresh_3500_shadow_signals.jsonl') as f:
    for line in f:
        e = json.loads(line)
        if e.get('signal') == 'WOULD_BUY_DIP' and e.get('creator'):
            wbd.add(e['creator'])

print()
print('=== WOULD_BUY_DIP creators ===')
for addr in wbd:
    c = creators.get(addr, {})
    print(f'  {addr[:12]}...  buy@t0={c.get("buy_at_t0")}  dumped={c.get("sold_in_300s")}  mints={len(c.get("mints",set()))}  sold={c.get("total_sell_sol",0):.3f} SOL')
dup_set = set(a[0] for a in dual)
print(f'  ↑ Any in repeat-dumper set? {"YES" if wbd & dup_set else "NO"}')

# Also show repeat dumpers active in WOULD_BUY_DIP mints
wbd_mints = set()
with open('datasets/fresh_3500_shadow_signals.jsonl') as f:
    for line in f:
        e = json.loads(line)
        if e.get('signal') == 'WOULD_BUY_DIP' and e.get('mint'):
            wbd_mints.add(e['mint'])

print()
print('=== Repeat dumpers active in WOULD_BUY_DIP mints ===')
for addr, mints, sold in dual:
    overlap = set()
    with open('datasets/sniper_trade_events.jsonl') as f:
        for line in f:
            try:
                e = json.loads(line)
            except:
                continue
            if e.get('signer') == addr and e.get('mint') in wbd_mints:
                overlap.add(e['mint'])
    if overlap:
        print(f'  {addr[:12]}...  mints={mints}  in_WBD={len(overlap)}  sold={sold:.3f} SOL')
