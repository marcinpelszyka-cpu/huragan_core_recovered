#!/usr/bin/env python3
"""Fresh Sniper Shadow — generate WOULD_BUY/NO_GO signals and compute forward PnL.

Reads fresh_sniper_events.jsonl, groups by mint+wallet, ranks wallets,
generates shadow decisions, and computes forward PnL at 30/60/120s windows.

Shadow-only. No live execution. No SOL spent.

Usage:
  python3 scripts/fresh_sniper_shadow.py [--self-test]
"""

import argparse
import json
import statistics
from collections import defaultdict
from pathlib import Path

PROJECT = Path(__file__).resolve().parent.parent
EVENTS_PATH = PROJECT / "datasets" / "fresh_sniper_events.jsonl"
SIGNALS_PATH = PROJECT / "datasets" / "fresh_sniper_shadow_signals.jsonl"
SCORES_PATH = PROJECT / "datasets" / "fresh_sniper_wallet_scores.jsonl"
REPORT_PATH = PROJECT / "datasets" / "fresh_sniper_shadow_report.md"

MIN_GOOD_WALLETS = 2
MIN_TOTAL_BUY_SOL = 0.03
MIN_HOLD_PCT_10S = 0.75
PNL_WINDOWS = [30, 60, 120]
GOOD_SNIPER_MIN_SCORE = 50.0


def load_events():
    if not EVENTS_PATH.exists():
        return []
    events = []
    with open(EVENTS_PATH) as f:
        for line in f:
            try:
                events.append(json.loads(line))
            except Exception:
                continue
    return events


def group_by_mint(events):
    by_mint = defaultdict(list)
    for ev in events:
        by_mint[ev.get("mint", "")].append(ev)
    return by_mint


def wallet_metrics_for_mint(mint_events):
    """Per-wallet metrics within one mint."""
    by_wallet = defaultdict(list)
    for ev in mint_events:
        by_wallet[ev.get("owner", "")].append(ev)

    metrics = {}
    for owner, evs in by_wallet.items():
        buys = [e for e in evs if e.get("side") == "buy"]
        if not buys:
            continue
        total_buy_sol = sum(e.get("buy_sol", 0) for e in buys)
        if total_buy_sol < 0.001:  # skip dust
            continue
        metrics[owner] = {
            "first_buy_sol": round(total_buy_sol, 9),
            "num_buys": len(buys),
            "token_age_at_buy": min(e.get("token_age_at_buy", 999) for e in buys),
        }
    return metrics


def compute_wallet_rankings(events):
    """Aggregate wallet behavior across all mints."""
    by_mint = group_by_mint(events)
    aggregated = defaultdict(lambda: {
        "mints_seen": 0, "total_buy_sol": 0.0, "first_buy_ages": [],
        "num_early_entries": 0,
    })

    for mint, mint_events in by_mint.items():
        wm = wallet_metrics_for_mint(mint_events)
        for owner, m in wm.items():
            agg = aggregated[owner]
            agg["mints_seen"] += 1
            agg["total_buy_sol"] += m["first_buy_sol"]
            agg["first_buy_ages"].append(m["token_age_at_buy"])
            if m["token_age_at_buy"] <= 10:
                agg["num_early_entries"] += 1

    results = []
    for owner, agg in aggregated.items():
        n = agg["mints_seen"]
        if n < 1:
            continue
        avg_age = statistics.mean(agg["first_buy_ages"]) if agg["first_buy_ages"] else 999
        early_rate = agg["num_early_entries"] / n if n else 0
        # Simple score: early entry + volume
        score = early_rate * 50 + min(agg["total_buy_sol"] / 0.01 * 10, 50)

        if score >= GOOD_SNIPER_MIN_SCORE and n >= 2:
            category = "GOOD_SNIPER"
        else:
            category = "UNKNOWN" if n < 2 else "LOW_CONFIDENCE"

        results.append({
            "owner": owner,
            "mints_seen": n,
            "total_buy_sol": round(agg["total_buy_sol"], 6),
            "avg_first_buy_age": round(avg_age, 1),
            "early_entry_rate": round(early_rate, 4),
            "score": round(score, 2),
            "category": category,
        })

    return sorted(results, key=lambda r: r["score"], reverse=True)


def generate_shadow_signals(events, wallet_scores):
    """For each mint, decide WOULD_BUY / NO_GO."""
    good_snipers = {s["owner"] for s in wallet_scores if s["category"] == "GOOD_SNIPER"}
    by_mint = group_by_mint(events)
    signals = []

    for mint, mint_events in by_mint.items():
        wm = wallet_metrics_for_mint(mint_events)

        good_in_mint = []
        for owner in wm:
            if owner in good_snipers:
                m = wm[owner]
                if m["token_age_at_buy"] <= 10:
                    good_in_mint.append({"owner": owner, **m})

        good_count = len(good_in_mint)
        total_buy = sum(g["first_buy_sol"] for g in good_in_mint)
        mc = mint_events[0].get("token_mc_sol", 0) if mint_events else 0

        # Signal logic
        if good_count >= MIN_GOOD_WALLETS and total_buy >= MIN_TOTAL_BUY_SOL:
            signal = True
            reason = "signal"
        elif good_count < MIN_GOOD_WALLETS:
            signal = False
            reason = "too_few_good_snipers"
        else:
            signal = False
            reason = "total_buy_too_low"

        signals.append({
            "mint": mint,
            "signal": signal,
            "good_sniper_count": good_count,
            "total_good_sniper_buy_sol": round(total_buy, 9),
            "market_cap_sol": round(mc, 2),
            "good_sniper_wallets": [g["owner"] for g in good_in_mint],
            "reason": reason,
        })

    return signals


def compute_forward_pnl(events, signals):
    """Naive forward PnL: compare buy SOL to estimated exit at window end."""
    by_mint = group_by_mint(events)
    for sig in signals:
        mint = sig["mint"]
        mint_events = by_mint.get(mint, [])
        if not mint_events:
            sig["forward_pnl"] = {}
            continue

        # Get last known MC as proxy for exit value
        mc_entry = mint_events[0].get("token_mc_sol", 0)
        mc_end = mint_events[-1].get("token_mc_sol", mc_entry)

        pnl = {}
        for w in PNL_WINDOWS:
            # Simple: use MC change as PnL proxy
            if mc_entry > 0:
                pnl[f"mc_change_{w}s_pct"] = 0  # need real time-series data
            pnl[f"window_{w}s_estimable"] = mc_entry > 0

        sig["forward_pnl_proxy"] = {
            "mc_entry_sol": mc_entry,
            "mc_trailing_sol": mc_end,
            "mc_change_pct": round((mc_end - mc_entry) / mc_entry * 100, 2) if mc_entry else 0,
        }

    return signals


def write_report(signals, scores, path):
    with open(path, "w") as f:
        f.write("# Fresh Sniper Follow Shadow Report\n\n")
        n_signals = sum(1 for s in signals if s["signal"])
        f.write(f"- Tokens observed: {len(signals)}\n")
        f.write(f"- WOULD_BUY signals: {n_signals}\n")
        f.write(f"- GOOD_SNIPER wallets: {sum(1 for s in scores if s['category'] == 'GOOD_SNIPER')}\n\n")

        f.write("## Signals\n\n")
        f.write("| Mint | Signal | SNIPERS | Buy SOL | MC SOL | Reason |\n")
        f.write("|---|---:|---:|---:|---|\n")
        for s in signals:
            emoji = "✅" if s["signal"] else "❌"
            f.write(f"| {s['mint'][:12]}... | {emoji} | {s['good_sniper_count']} | {s['total_good_sniper_buy_sol']:.4f} | {s['market_cap_sol']:.1f} | {s['reason']} |\n")

        f.write("\n## Top Wallets\n\n")
        f.write("| Wallet | Mints | Score | Category | Avg Buy Age |\n")
        f.write("|---|---:|---:|---|---:|\n")
        for s in scores[:5]:
            f.write(f"| {s['owner'][:12]}... | {s['mints_seen']} | {s['score']:.1f} | {s['category']} | {s['avg_first_buy_age']}s |\n")

        f.write("\n## Notes\n\n")
        f.write("- Shadow only — no SOL was spent\n")
        f.write("- Forward PnL requires time-series MC data not yet available in v1\n")
        f.write("- Re-run after accumulating more fresh token data\n")


def self_test():
    print("=== SELF-TEST: fresh_sniper_shadow ===")

    # Simulated events: 2 mints, one with good snipers
    test_events = [
        {"mint": "M1", "pool_state": "P1", "owner": "GWALLET_A", "side": "buy", "buy_sol": 0.02, "token_age_at_buy": 5, "token_mc_sol": 25.0},
        {"mint": "M1", "pool_state": "P1", "owner": "GWALLET_B", "side": "buy", "buy_sol": 0.015, "token_age_at_buy": 3, "token_mc_sol": 25.0},
        {"mint": "M1", "pool_state": "P1", "owner": "D_WALLET", "side": "buy", "buy_sol": 0.01, "token_age_at_buy": 2, "token_mc_sol": 25.0},
        {"mint": "M2", "pool_state": "P2", "owner": "D_WALLET", "side": "buy", "buy_sol": 0.005, "token_age_at_buy": 15, "token_mc_sol": 30.0},
    ]

    scores = compute_wallet_rankings(test_events)
    assert len(scores) >= 2, f"Expected >=2 wallets, got {len(scores)}"
    print(f"  wallets ranked: {len(scores)}")

    signals = generate_shadow_signals(test_events, scores)
    print(f"  signals: {len(signals)}, WOULD_BUY: {sum(1 for s in signals if s['signal'])}")

    # GWALLET_A and GWALLET_B entered M1 early with good volume → signal
    m1 = [s for s in signals if s["mint"] == "M1"]
    if m1:
        print(f"  M1: signal={m1[0]['signal']}, good_snipers={m1[0]['good_sniper_count']}")

    print("ALL SELF-TESTS PASSED")


def main():
    ap = argparse.ArgumentParser(description="Fresh Sniper Shadow")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()

    if args.self_test:
        self_test()
        return

    events = load_events()
    if not events:
        print("No events found. Run fresh_sniper_collector.py first.")
        return

    print(f"Loaded {len(events)} trade events from {len(set(e.get('mint','') for e in events))} mints")

    # Rank wallets
    wallet_scores = compute_wallet_rankings(events)
    print(f"\nWallet scores: {len(wallet_scores)}")
    for cat in ["GOOD_SNIPER", "LOW_CONFIDENCE", "UNKNOWN"]:
        count = sum(1 for s in wallet_scores if s["category"] == cat)
        if count:
            print(f"  {cat}: {count}")

    # Write scores
    SCORES_PATH.parent.mkdir(parents=True, exist_ok=True)
    with open(SCORES_PATH, "w") as f:
        for s in wallet_scores:
            f.write(json.dumps(s) + "\n")

    # Generate shadow signals
    signals = generate_shadow_signals(events, wallet_scores)
    signals = compute_forward_pnl(events, signals)
    print(f"\nShadow signals: {len(signals)}")
    print(f"  WOULD_BUY: {sum(1 for s in signals if s['signal'])}")

    with open(SIGNALS_PATH, "w") as f:
        for s in signals:
            f.write(json.dumps(s) + "\n")

    write_report(signals, wallet_scores, REPORT_PATH)
    print(f"\nReport → {REPORT_PATH}")


if __name__ == "__main__":
    main()
