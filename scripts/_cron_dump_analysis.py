#!/usr/bin/env python3
"""Cron-safe dump timing analysis"""
import json, sys

events_by_mint = {}
with open("datasets/sniper_trade_events.jsonl") as f:
    for line in f:
        if not line.strip():
            continue
        e = json.loads(line)
        mint = e["mint"]
        events_by_mint.setdefault(mint, []).append(e)

targets = [
    "BknmwfF9GQGK66ZpKNZ326NPYJzaxuRS9TfXWbLypump",
    "41r33VpMbsEEgjZjeyF8k5mG7no4QeNZ4PmXzbFgpump",
    "HxTzVfUz8w9TATtJrKg2Ya9AuyS5YGwbcW1r2rWpump",
    "2CJzmaLDgxdygpCx4chVtd2fi34CpoTL1UgpuGnjpump",
    "66vzmuH21WSvZLV4gKUh6bihgBzoJyVmJ8aKVbippump",
    "A5quQ2j88x3XZ8kLqnExdyKuJnyyVUzoenQnV5XHpump",
    "FN2u9xnb5LUwTH8qB4GyUXg88hbwF7GZrC8b8ptSpump",
]

print("=== DUMP TIMING ANALYSIS ===\n")
dump_ages = []  # first sell age per mint (trough_age_s equivalent)
all_pump_pct = []
all_dump_pct = []

for mint in targets:
    events = events_by_mint.get(mint, [])
    if not events:
        print(f"{mint[:16]}... NO EVENTS")
        continue

    buys = [e for e in events if e["side"] == "buy"]
    sells = [e for e in events if e["side"] == "sell"]
    buy_times = sorted([e["age_secs"] for e in buys])
    sell_times = sorted([e["age_secs"] for e in sells])
    buy_quotes = [e["quote_delta_sol"] for e in buys]
    sell_quotes = [e["quote_delta_sol"] for e in sells]
    total_buy_sol = sum(buy_quotes)
    total_sell_sol = sum(sell_quotes)

    first_sell = sell_times[0] if sell_times else None
    last_sell = sell_times[-1] if sell_times else None

    big_sells = sorted([(e["age_secs"], e["quote_delta_sol"], e["signer"][:12])
                          for e in sells],
                         key=lambda x: -x[1])[:3]

    print(f"Mint: {mint[:20]}...")
    print(f"  Buys={len(buys)}({total_buy_sol:.4f}SOL) Sells={len(sells)}({total_sell_sol:.4f}SOL)")
    print(f"  Buy timing: first={buy_times[0] if buy_times else '?'}s")
    print(f"  Sell timing: first={first_sell}s last={last_sell}s window={sell_times[:6]}")
    if big_sells:
        print(f"  Top dumps: {[(f'{t}s', f'{q:.4f}SOL', s) for t,q,s in big_sells]}")
    print(f"  Net flow: {total_buy_sol - total_sell_sol:+.4f} SOL")

    if first_sell is not None:
        dump_ages.append(first_sell)

print(f"\n=== SUMMARY ===")
if dump_ages:
    dump_ages.sort()
    print(f"First-sell ages: {dump_ages}")
    print(f"Median first-sell (trough): {dump_ages[len(dump_ages)//2]}s")

# Signer analysis
all_signers = set()
for mint in targets:
    for e in events_by_mint.get(mint, []):
        all_signers.add(e["signer"])

signer_mints = {}
for mint in targets:
    seen = set()
    for e in events_by_mint.get(mint, []):
        if e["signer"] not in seen:
            signer_mints[e["signer"]] = signer_mints.get(e["signer"], 0) + 1
            seen.add(e["signer"])

multi = [(s, c) for s, c in signer_mints.items() if c > 1]
multi.sort(key=lambda x: -x[1])
print(f"\nMulti-mint signers: {[(s[:12], c) for s,c in multi[:10]]}")
print(f"Unique signers across {len(targets)} signal mints: {len(all_signers)}")
