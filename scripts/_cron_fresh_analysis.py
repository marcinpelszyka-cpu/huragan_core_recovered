import json, statistics, os

# Read signals
signals = []
sig_path = 'datasets/sniper_follow_signals.jsonl'
if os.path.exists(sig_path):
    with open(sig_path) as f:
        for line in f:
            signals.append(json.loads(line))

print(f"Total signals: {len(signals)}")
print()

# Read trade events for timing analysis
events = []
ev_path = 'datasets/sniper_trade_events.jsonl'
if os.path.exists(ev_path):
    with open(ev_path) as f:
        for line in f:
            events.append(json.loads(line))

print(f"Total trade events: {len(events)}")

# Collect pump/dump stats from events
pump_pcts = []
dump_pcts = []
trough_ages = []
creators = {}

for e in events:
    pump = e.get('pump_pct', 0)
    dump = e.get('dump_pct', 0)
    trough_age = e.get('trough_age_s')
    creator = e.get('creator_address', '')
    if pump and pump > 0:
        pump_pcts.append(pump)
    if dump and dump > 0:
        dump_pcts.append(dump)
    if trough_age is not None:
        trough_ages.append(trough_age)
    if creator:
        creators[creator[:8]] = creators.get(creator[:8], 0) + 1

print()
print(f"=== PUMP/DUMP STATS ===")
print(f"avg_pump_pct: {statistics.mean(pump_pcts):.1f}% (n={len(pump_pcts)})" if pump_pcts else "no pump data")
print(f"avg_dump_pct: {statistics.mean(dump_pcts):.1f}% (n={len(dump_pcts)})" if dump_pcts else "no dump data")
print(f"median_trough_age_s: {statistics.median(trough_ages):.1f}s (n={len(trough_ages)})" if trough_ages else "no trough data")
if trough_ages:
    trough_ages.sort()
    print(f"trough_age_range: {trough_ages[0]}-{trough_ages[-1]}s")
    print(f"trough_age_p25: {trough_ages[len(trough_ages)//4]:.1f}s")
    print(f"trough_age_p75: {trough_ages[3*len(trough_ages)//4]:.1f}s")

print()
print(f"=== TOP CREATORS (by event count) ===")
for creator, count in sorted(creators.items(), key=lambda x: -x[1])[:10]:
    print(f"  {creator}...: {count} events")

# Signal details
print()
print(f"=== SIGNALS ===")
for s in signals:
    mint = s.get('token_address','')[:14]
    creator = s.get('creator_address','')[:14]
    pump_pct = s.get('pump_pct', 0)
    dump_pct = s.get('dump_pct', 0)
    trough_age = s.get('trough_age_s', 'N/A')
    mc_peak = s.get('mc_peak_sol', 0)
    mc_trough = s.get('mc_trough_sol', 0)
    signal_type = s.get('signal_type', 'N/A')
    confidence = s.get('confidence', 0)
    wallet = s.get('wallet_address','')[:10]
    print(f"  mint={mint} wallet={wallet} pump={pump_pct:.1f}% dump={dump_pct:.1f}% trough={trough_age}s peak={mc_peak:.1f} trough={mc_trough:.1f} type={signal_type} conf={confidence:.2f}")

# Entry gate data
gate_path = 'datasets/fresh_3500_entry_gate_report.md'
if os.path.exists(gate_path):
    print()
    print(f"=== ENTRY GATE REPORT ===")
    with open(gate_path) as f:
        print(f.read())
