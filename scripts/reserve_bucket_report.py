#!/usr/bin/env python3
"""Reserve Bucket Report - analiza jakości wejść po wielkości pool SOL.

Czyta state.jsonl, grupuje entry po quote_reserve_sol w buckety:
- <25, 25-50, 50-75, 75-100, 100-200, 200-500, 500+

Dla każdego bucketu liczy:
- count, avg_pnl_pct, median_pnl_pct, total_pnl_sol
- win_rate, hard_stop_rate, early_no_momentum_rate
- profit_protect_rate, max_hold_rate, price_unavailable_rate

Output: datasets/reserve_bucket_report.md, datasets/reserve_bucket_summary.json
"""

import json
import sys
from pathlib import Path
from collections import defaultdict
from statistics import median

BUCKETS = [
    ("<25", 0, 25),
    ("25-50", 25, 50),
    ("50-75", 50, 75),
    ("75-100", 75, 100),
    ("100-200", 100, 200),
    ("200-500", 200, 500),
    ("500+", 500, 999999),
]

def load_state(path: Path):
    rows = []
    with path.open() as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    return rows

def get_bucket(reserve_sol: float):
    for name, lo, hi in BUCKETS:
        if lo <= reserve_sol < hi:
            return name
    return "500+"

def analyze_buckets(rows):
    buckets = defaultdict(list)
    
    for r in rows:
        # Live completed: source=helius_migration + status=completed + realized_pnl_sol
        # Paper completed: status=completed + net_pnl_sol
        if r.get("status") != "completed":
            continue
            
        pnl = r.get("realized_pnl_sol", 0) or r.get("net_pnl_sol", 0)
        if pnl is None:
            pnl = 0
            
        # Entry reserve: prefer entry_quote_reserve_raw (lamports / 1e9)
        reserve_sol = 0
        if r.get("entry_quote_reserve_raw") and r["entry_quote_reserve_raw"] > 0:
            reserve_sol = r["entry_quote_reserve_raw"] / 1e9
        elif r.get("quote_reserve_ui") and r["quote_reserve_ui"] > 0:
            reserve_sol = r["quote_reserve_ui"]
            
        if reserve_sol <= 0:
            continue
            
        bucket = get_bucket(float(reserve_sol))
        buckets[bucket].append({
            "pnl_sol": float(pnl),
            "pnl_pct": float(r.get("net_pnl_pct", r.get("realized_pnl_pct", 0)) or 0),
            "exit_reason": r.get("live_exit_reason") or r.get("exit_reason", "unknown"),
        })
    
    results = []
    for name, lo, hi in BUCKETS:
        entries = buckets.get(name, [])
        count = len(entries)
        
        if count == 0:
            results.append({
                "bucket": name,
                "count": 0,
                "avg_pnl_pct": 0,
                "median_pnl_pct": 0,
                "total_pnl_sol": 0,
                "win_rate": 0,
                "hard_stop_rate": 0,
                "early_no_momentum_rate": 0,
                "profit_protect_rate": 0,
                "max_hold_rate": 0,
                "price_unavailable_rate": 0,
            })
            continue
        
        pnls_sol = [e["pnl_sol"] for e in entries]
        pnls_pct = [e["pnl_pct"] for e in entries]
        wins = [p for p in pnls_sol if p > 0]
        
        exit_reasons = [e["exit_reason"] for e in entries]
        
        results.append({
            "bucket": name,
            "count": count,
            "avg_pnl_pct": sum(pnls_pct) / count,
            "median_pnl_pct": median(pnls_pct) if pnls_pct else 0,
            "total_pnl_sol": sum(pnls_sol),
            "win_rate": len(wins) / count * 100,
            "hard_stop_rate": exit_reasons.count("hard_stop") / count * 100,
            "early_no_momentum_rate": exit_reasons.count("early_no_momentum") / count * 100,
            "profit_protect_rate": exit_reasons.count("profit_protect") / count * 100,
            "max_hold_rate": exit_reasons.count("max_hold") / count * 100,
            "price_unavailable_rate": exit_reasons.count("price_unavailable") / count * 100,
        })
    
    return results

def write_markdown(results, path: Path):
    path.parent.mkdir(parents=True, exist_ok=True)
    
    with path.open("w") as f:
        f.write("# Reserve Bucket Analysis\n\n")
        f.write("Analiza jakości wejść po wielkości pool SOL.\n\n")
        f.write("## Tabela wyników\n\n")
        f.write("| Bucket | Count | Avg PnL % | Median % | Total SOL | Win % | HardStop % | EarlyNoMom % | ProfitProt % | MaxHold % | PriceUnavail % |\n")
        f.write("|--------|-------|-----------|----------|-----------|-------|------------|--------------|--------------|-----------|----------------|\n")
        
        for r in results:
            f.write(f"| {r['bucket']} | {r['count']} | {r['avg_pnl_pct']:.1f}% | {r['median_pnl_pct']:.1f}% | {r['total_pnl_sol']:.6f} | {r['win_rate']:.1f}% | {r['hard_stop_rate']:.1f}% | {r['early_no_momentum_rate']:.1f}% | {r['profit_protect_rate']:.1f}% | {r['max_hold_rate']:.1f}% | {r['price_unavailable_rate']:.1f}% |\n")
        
        f.write("\n## Interpretacja\n\n")
        
        # Znajdź bucket 100-200
        b100 = next((r for r in results if r["bucket"] == "100-200"), None)
        b75 = next((r for r in results if r["bucket"] == "75-100"), None)
        
        if b100 and b100["count"] > 0:
            f.write(f"**Bucket 100-200 SOL**: {b100['count']} trades, {b100['win_rate']:.1f}% win rate, {b100['total_pnl_sol']:.6f} SOL\n\n")
        
        if b75 and b75["count"] > 0:
            f.write(f"**Bucket 75-100 SOL**: {b75['count']} trades, {b75['win_rate']:.1f}% win rate, {b75['total_pnl_sol']:.6f} SOL\n\n")
        
        f.write("## Uwagi\n\n")
        f.write("- Dane tylko dla completed trades z realnym PnL\n")
        f.write("- HardStopRate + PriceUnavailableRate = ryzyko bucketu\n")
        f.write("- Wysoki MaxHoldRate = strategia działa (tail_z3)\n")

def write_json(results, path: Path):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w") as f:
        json.dump(results, f, indent=2)

def main():
    state_path = Path("state.jsonl")
    if not state_path.exists():
        print(f"ERROR: {state_path} not found", file=sys.stderr)
        sys.exit(1)
    
    rows = load_state(state_path)
    results = analyze_buckets(rows)
    
    write_markdown(results, Path("datasets/reserve_bucket_report.md"))
    write_json(results, Path("datasets/reserve_bucket_summary.json"))
    
    print(f"Analyzed {len(rows)} rows")
    print(f"Generated: datasets/reserve_bucket_report.md")
    print(f"Generated: datasets/reserve_bucket_summary.json")

if __name__ == "__main__":
    main()
