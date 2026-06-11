import json

# Read the shadow signals for full detail
with open('datasets/fresh_3500_shadow_signals.jsonl') as f:
    results = []
    for line in f:
        results.append(json.loads(line))

print(f"Total results: {len(results)}")
print()

# Count by signal type
from collections import Counter
counts = Counter(r['signal'] for r in results)
for sig, n in counts.most_common():
    print(f"  {sig}: {n}")

# Now show all results with full detail
print()
for r in results:
    mint = r['mint'][:12]
    creator = r.get('creator', '')[:12]
    signal = r['signal']
    pump = r.get('pump_pct', 0)
    dump = r.get('dump_pct', 0)
    trough = r.get('trough_age_s', 0)
    entry_mc = r.get('entry_mc_sol', 0)
    peak_mc = r.get('peak_mc_sol', 0)
    mc_60 = r.get('mc_at_60s', 0)
    recov = r.get('recovery_ratio', 0)
    pnl_30 = r.get('pnl_30s', 0)
    pnl_60 = r.get('pnl_60s', 0)
    pnl_120 = r.get('pnl_120s', 0)
    print(f"{signal:20s} mint={mint}... creator={creator}... pump={pump:5.1f}% dump={dump:5.1f}% trough={trough:.0f}s entry_mc={entry_mc:.1f} peak_mc={peak_mc:.1f} mc60={mc_60:.1f} rec={recov:.2f}x pnl_30={pnl_30}% pnl_60={pnl_60}% pnl_120={pnl_120}%")

# Stats on WOULD_BUY_DIP
would_buy = [r for r in results if r['signal'] == 'WOULD_BUY_DIP']
if would_buy:
    print()
    print("=== WOULD_BUY_DIP STATS ===")
    pumps = [r['pump_pct'] for r in would_buy]
    dumps = [r['dump_pct'] for r in would_buy]
    troughs = [r['trough_age_s'] for r in would_buy]
    creators = [r['creator'][:8] for r in would_buy]
    import statistics
    print(f"  avg pump: {statistics.mean(pumps):.1f}%")
    print(f"  avg dump: {statistics.mean(dumps):.1f}%")
    print(f"  median trough_age: {statistics.median(troughs):.1f}s")
    print(f"  creators: {set(creators)}")
