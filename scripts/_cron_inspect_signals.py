import json

# Inspect first signal
with open('datasets/sniper_follow_signals.jsonl') as f:
    first = json.loads(f.readline())
    print("=== SIGNAL KEYS ===")
    for k, v in sorted(first.items()):
        val = str(v)[:200]
        print(f"  {k}: {val}")
    print(f"\nTotal keys: {len(first)}")

# Inspect first event  
with open('datasets/sniper_trade_events.jsonl') as f:
    first = json.loads(f.readline())
    print("\n=== EVENT KEYS ===")
    for k, v in sorted(first.items()):
        val = str(v)[:200]
        print(f"  {k}: {val}")
    print(f"\nTotal keys: {len(first)}")

# Count events per mint
mints = {}
with open('datasets/sniper_trade_events.jsonl') as f:
    for line in f:
        e = json.loads(line)
        mint = e.get('token_address', e.get('mint', ''))[:16]
        if mint not in mints:
            mints[mint] = 0
        mints[mint] += 1

print(f"\n=== MINT COVERAGE ({len(mints)} mints) ===")
for mint, count in sorted(mints.items(), key=lambda x: -x[1])[:20]:
    print(f"  {mint}: {count} events")
