import json

# Check age_secs distribution
ages = []
with open('datasets/sniper_trade_events.jsonl') as f:
    for line in f:
        e = json.loads(line)
        age = e.get('age_secs', 0)
        ages.append(age)

import statistics
ages.sort()
print(f"Event count: {len(ages)}")
print(f"age_secs range: {ages[0]} - {ages[-1]}")
print(f"median: {statistics.median(ages):.0f}")
print(f"mean: {statistics.mean(ages):.0f}")
print(f"p90: {ages[int(len(ages)*0.9)]}")
print(f"p95: {ages[int(len(ages)*0.95)]}")
print(f"count <=60s: {sum(1 for a in ages if a <= 60)}/{len(ages)}")
print(f"count >60s: {sum(1 for a in ages if a > 60)}/{len(ages)}")
print(f"count >120s: {sum(1 for a in ages if a > 120)}/{len(ages)}")

# Per-mint age range
from collections import defaultdict
mint_ages = defaultdict(list)
with open('datasets/sniper_trade_events.jsonl') as f:
    for line in f:
        e = json.loads(line)
        mint_ages[e.get('mint','')].append(e.get('age_secs', 0))

print("\n=== Per-mint age coverage ===")
for mint, ages_list in sorted(mint_ages.items(), key=lambda x: -len(x[1])):
    ages_list.sort()
    print(f"  {mint[:16]}: n={len(ages_list)} age_range={ages_list[0]}-{ages_list[-1]}s max_bucket={ages_list[-1]}s")

# Also check what the 150-event mint looks like in detail
print("\n=== Largest mint detail (66vzmu) ===")
with open('datasets/sniper_trade_events.jsonl') as f:
    for line in f:
        e = json.loads(line)
        if e.get('mint','').startswith('66vzmu'):
            side = e.get('side','')
            age = e.get('age_secs', 0)
            delta = e.get('quote_delta_sol', 0)
            owner = e.get('signer','')[:8]
            print(f"    age={age:4.0f}s side={side:4s} delta={delta:+.4f} SOL owner={owner}...")
            break
