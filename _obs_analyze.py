#!/usr/bin/env python3
"""Analyze trade events for the 3 WOULD_BUY_DIP mints."""
import json
from collections import defaultdict

targets = ['HxTzVfUz8w9T', '41r33VpMbsEE', 'BknmwfF9GQGK']

by_mint = defaultdict(list)
with open('datasets/sniper_trade_events.jsonl') as f:
    for line in f:
        e = json.loads(line)
        mint = e.get('mint', '')
        for t in targets:
            if t in mint:
                by_mint[t].append(e)
                break

print("=" * 70)
print("FRESH TOKEN OBSERVATION — ENTRY TIMING ANALYSIS")
print("=" * 70)

agg_pump_pct = []
agg_dump_pct = []
agg_trough_age = []
creator_sets = []

for mint_key in targets:
    evts = by_mint.get(mint_key, [])
    if not evts:
        print(f"\n{mint_key}: NO EVENTS")
        continue

    evts.sort(key=lambda x: x.get('age_secs', 0))

    # Extract MC values
    ages = [e.get('age_secs', 0) for e in evts]
    mcs = [e.get('entry_market_cap_sol', 0) for e in evts]
    sides = [e.get('side', '?') for e in evts]
    signers = [e.get('signer', '?') for e in evts]

    peak_mc = max(mcs) if mcs else 0
    peak_idx = mcs.index(peak_mc) if peak_mc > 0 else 0

    # Trough after peak
    post_peak_mcs = mcs[peak_idx+1:] if peak_idx + 1 < len(mcs) else []
    trough_mc = min(post_peak_mcs) if post_peak_mcs else mcs[-1]
    trough_age = ages[mcs.index(trough_mc)] if trough_mc in mcs else 0

    entry_mc = mcs[0]
    pump_pct = ((peak_mc / entry_mc) - 1) * 100 if entry_mc > 0 else 0
    dump_pct = ((trough_mc / peak_mc) - 1) * 100 if peak_mc > 0 else 0

    # Creators: first 3 unique signers in first block
    first_block = [e for e in evts if e.get('age_secs', 0) == 0]
    first_signers = list(dict.fromkeys([e['signer'] for e in first_block]))[:5]

    # All unique signers
    all_signers = list(dict.fromkeys(signers))
    creator_sets.append(set(first_signers))

    # Buy count accounting
    buys = [e for e in evts if e['side'] == 'buy']
    sells = [e for e in evts if e['side'] == 'sell']
    buy_sol = sum(e.get('quote_delta_sol', 0) for e in buys)
    sell_sol = sum(e.get('quote_delta_sol', 0) for e in sells)

    agg_pump_pct.append(pump_pct)
    agg_dump_pct.append(dump_pct)
    agg_trough_age.append(trough_age)

    print(f"\n--- {mint_key}... ({len(evts)} events) ---")
    print(f"  Entry MC: {entry_mc:.1f} SOL → Peak MC: {peak_mc:.1f} SOL → Trough MC: {trough_mc:.1f} SOL")
    print(f"  Pump: +{pump_pct:.1f}%  |  Dump: {dump_pct:.1f}%  |  Trough at: {trough_age}s")
    print(f"  Buy volume: {buy_sol:.3f} SOL  |  Sell volume: {sell_sol:.3f} SOL")
    print(f"  First-block signers ({len(first_signers)}): {', '.join(first_signers[:3])}")

    # MC time series (first 15 snapshots)
    print(f"  MC time series:")
    for i, (age, mc, side) in enumerate(zip(ages, mcs, sides)):
        if i >= 15:
            break
        marker = ""
        if mc == peak_mc:
            marker = " ← PEAK"
        elif mc == trough_mc:
            marker = " ← TROUGH"
        print(f"    {age:3d}s  {mc:6.1f} SOL  {side:4s}{marker}")

# Aggregate stats
print("\n" + "=" * 70)
print("AGGREGATE STATS")
print("=" * 70)
if agg_pump_pct:
    import statistics
    print(f"  PUMP: avg={statistics.mean(agg_pump_pct):.1f}%  median={statistics.median(agg_pump_pct):.1f}%  range={min(agg_pump_pct):.1f}% to {max(agg_pump_pct):.1f}%")
    print(f"  DUMP: avg={statistics.mean(agg_dump_pct):.1f}%  median={statistics.median(agg_dump_pct):.1f}%  range={min(agg_dump_pct):.1f}% to {max(agg_dump_pct):.1f}%")
    print(f"  TROUGH AGE: avg={statistics.mean(agg_trough_age):.1f}s  median={statistics.median(agg_trough_age):.1f}s  range={min(agg_trough_age)}s to {max(agg_trough_age)}s")

# Creator overlap
if len(creator_sets) >= 2:
    common = creator_sets[0]
    for cs in creator_sets[1:]:
        common = common & cs
    print(f"  Creator overlap across all 3: {common if common else 'NONE'}")
    pairwise = []
    for i in range(len(creator_sets)):
        for j in range(i+1, len(creator_sets)):
            olap = creator_sets[i] & creator_sets[j]
            if olap:
                pairwise.append(f"{targets[i][:6]}/{targets[j][:6]}: {len(olap)} shared")
    if pairwise:
        print(f"  Pairwise overlaps: {'; '.join(pairwise)}")
    else:
        print(f"  No pairwise creator overlaps")
