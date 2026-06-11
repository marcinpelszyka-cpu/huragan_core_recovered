import json

signals = []
with open('/opt/huragan_core/gmgn_shadow_signals.jsonl') as f:
    for line in f:
        try: signals.append(json.loads(line))
        except: pass

# Last 50 (this run)
recent = signals[-50:]

from collections import Counter
types = Counter(s.get('signal','?') for s in recent)
print(f"This run (last 50): {dict(types)}")

# Show sample with actual data
for s in recent[:3]:
    print(json.dumps(s, indent=2)[:300])
    print("---")
