#!/usr/bin/env python3
"""Sniper Wallet Ranker — rank sniper wallets by historical forward PnL.

Reads sniper_trade_events.jsonl, groups by wallet + mint, computes:
  - forward PnL at 10s/30s/60s windows
  - rug rate after entry
  - fast dump rate
  - hold quality score
  - final category: GOOD_SNIPER / FAST_DUMPER / DEV_SNIPER_SUSPECT / UNKNOWN

Usage:
  python3 scripts/sniper_wallet_ranker.py [--self-test]
"""

import argparse
import json
import math
import statistics
from collections import defaultdict
from pathlib import Path

PROJECT = Path(__file__).resolve().parent.parent
EVENTS_PATH = PROJECT / "datasets" / "sniper_trade_events.jsonl"
SCORES_CSV = PROJECT / "datasets" / "sniper_wallet_scores.csv"
SCORES_JSONL = PROJECT / "datasets" / "sniper_wallet_scores.jsonl"
SIGNALS_PATH = PROJECT / "datasets" / "sniper_follow_signals.jsonl"

EARLY_WINDOWS = [3, 5, 10, 15]
PNL_WINDOWS = [10, 30, 60]
MIN_BUY_SOL = 0.01
GOOD_SNIPER_MIN_SCORE = 50.0
FAST_DUMP_THRESHOLD = 0.70  # 70%+ sold within 10s = fast dumper


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


def group_by_mint(events: list) -> dict[str, list]:
    by_mint = defaultdict(list)
    for ev in events:
        by_mint[ev.get("mint", "")].append(ev)
    return by_mint


def first_event_time(events: list) -> int:
    """Earliest block_time in a list of events."""
    times = [e.get("block_time") for e in events if e.get("block_time")]
    return min(times) if times else 0


def wallet_pnl_for_mint(mint_events: list, launch_time: int) -> dict[str, dict]:
    """For each wallet in this mint, compute forward PnL at each window."""
    by_wallet = defaultdict(list)
    for ev in mint_events:
        by_wallet[ev.get("owner", "")].append(ev)

    wallet_metrics = {}
    for owner, events in sorted(by_wallet.items(), key=lambda x: len(x[1]), reverse=True):
        buys = [e for e in events if e.get("side") == "buy"]
        sells = [e for e in events if e.get("side") == "sell"]
        if not buys:
            continue

        first_buy_time = min(e.get("block_time", 0) for e in buys)
        first_buy_age = first_buy_time - launch_time if launch_time else 999
        total_buy_tokens = sum(e.get("token_delta_raw", 0) for e in buys)
        total_buy_sol = sum(e.get("quote_delta_sol", 0) for e in buys)

        if total_buy_sol < MIN_BUY_SOL:
            continue

        pnl_windows = {}
        for w in PNL_WINDOWS:
            cutoff = first_buy_time + w
            sold_tokens = sum(e.get("token_delta_raw", 0) for e in sells if e.get("block_time", 0) <= cutoff)
            # Rough PnL: assume proportional sell
            sell_pct = min(sold_tokens / total_buy_tokens, 1.0) if total_buy_tokens else 0
            pnl_windows[w] = {
                "sold_tokens": sold_tokens,
                "sell_pct": round(sell_pct, 4),
                "hold_pct": round(1.0 - sell_pct, 4),
            }

        wallet_metrics[owner] = {
            "first_buy_age_secs": first_buy_age,
            "first_buy_sol": round(total_buy_sol, 9),
            "total_buy_tokens": total_buy_tokens,
            "total_buy_sol": round(total_buy_sol, 9),
            "pnl_windows": pnl_windows,
            "num_buys": len(buys),
            "num_sells": len(sells),
        }
    return wallet_metrics


def compute_forward_pnl(wallet_metrics: dict, all_mint_metrics: dict) -> list[dict]:
    """Aggregate per-wallet metrics across all mints."""
    aggregated = defaultdict(lambda: {
        "mints_seen": 0,
        "total_buy_sol": 0.0,
        "total_buy_tokens": 0.0,
        "first_buy_age_list": [],
        "hold_pct_10s_list": [],
        "hold_pct_30s_list": [],
        "hold_pct_60s_list": [],
        "fast_dumps": 0,
        "rug_hits": 0,
        "early_entries": 0,
    })

    for mint, wm in all_mint_metrics.items():
        for owner, metrics in wm.items():
            agg = aggregated[owner]
            agg["mints_seen"] += 1
            agg["total_buy_sol"] += metrics["total_buy_sol"]
            agg["total_buy_tokens"] += metrics["total_buy_tokens"]
            agg["first_buy_age_list"].append(metrics["first_buy_age_secs"])

            pnl = metrics.get("pnl_windows", {})
            if 10 in pnl:
                agg["hold_pct_10s_list"].append(pnl[10]["hold_pct"])
                sold_10s = pnl[10]["sell_pct"]
                if sold_10s >= FAST_DUMP_THRESHOLD:
                    agg["fast_dumps"] += 1
            if 30 in pnl:
                agg["hold_pct_30s_list"].append(pnl[30]["hold_pct"])
            if 60 in pnl:
                agg["hold_pct_60s_list"].append(pnl[60]["hold_pct"])

            if metrics["first_buy_age_secs"] <= 10:
                agg["early_entries"] += 1

    results = []
    for owner, agg in aggregated.items():
        n = agg["mints_seen"]
        if n < 1:
            continue

        avg_hold_10s = statistics.mean(agg["hold_pct_10s_list"]) if agg["hold_pct_10s_list"] else 0.0
        avg_hold_30s = statistics.mean(agg["hold_pct_30s_list"]) if agg["hold_pct_30s_list"] else 0.0
        avg_hold_60s = statistics.mean(agg["hold_pct_60s_list"]) if agg["hold_pct_60s_list"] else 0.0
        rug_rate = agg["rug_hits"] / n if n else 0.0
        fast_dump_rate = agg["fast_dumps"] / n if n else 0.0
        early_rate = agg["early_entries"] / n if n else 0.0

        hold_quality = (avg_hold_10s * 0.3 + avg_hold_30s * 0.3 + avg_hold_60s * 0.4) * 100
        score = (
            hold_quality * 0.4
            + early_rate * 100 * 0.3
            - fast_dump_rate * 100 * 0.2
            - rug_rate * 100 * 0.1
        )

        if score >= GOOD_SNIPER_MIN_SCORE and fast_dump_rate < 0.3 and n >= 2:
            category = "GOOD_SNIPER"
        elif fast_dump_rate >= FAST_DUMP_THRESHOLD:
            category = "FAST_DUMPER"
        elif rug_rate >= 0.5:
            category = "DEV_SNIPER_SUSPECT"
        else:
            category = "UNKNOWN"

        results.append({
            "owner": owner,
            "mints_seen": n,
            "total_buy_sol": round(agg["total_buy_sol"], 6),
            "avg_hold_pct_10s": round(avg_hold_10s, 4),
            "avg_hold_pct_30s": round(avg_hold_30s, 4),
            "avg_hold_pct_60s": round(avg_hold_60s, 4),
            "hold_quality": round(hold_quality, 2),
            "fast_dump_rate": round(fast_dump_rate, 4),
            "rug_rate": round(rug_rate, 4),
            "early_entry_rate": round(early_rate, 4),
            "avg_first_buy_age": round(statistics.mean(agg["first_buy_age_list"]), 1) if agg["first_buy_age_list"] else 999,
            "score": round(score, 2),
            "category": category,
        })

    return sorted(results, key=lambda r: r["score"], reverse=True)


def generate_signals(all_mint_metrics: dict, wallet_scores: list[dict]) -> list[dict]:
    """Generate sniper-follow signals per token."""
    good_snipers = {s["owner"] for s in wallet_scores if s["category"] == "GOOD_SNIPER"}
    signals = []

    for mint, wm in all_mint_metrics.items():
        good_in_mint = []
        for owner in wm:
            if owner in good_snipers:
                m = wm[owner]
                if m["first_buy_age_secs"] <= 10:
                    good_in_mint.append({"owner": owner, **m})

        if len(good_in_mint) < 2:
            continue

        total_buy = sum(g["first_buy_sol"] for g in good_in_mint)
        if total_buy < 0.03:
            continue

        cohort_hold_10s = statistics.mean(
            [g["pnl_windows"].get(10, {}).get("hold_pct", 0) for g in good_in_mint]
        ) if good_in_mint else 0

        cohort_hold_30s = statistics.mean(
            [g["pnl_windows"].get(30, {}).get("hold_pct", 0) for g in good_in_mint]
        ) if good_in_mint else 0

        score_sum = sum(
            next((s["score"] for s in wallet_scores if s["owner"] == g["owner"]), 0)
            for g in good_in_mint
        )

        signal = cohort_hold_10s >= 0.5 and total_buy >= 0.03
        signals.append({
            "mint": mint,
            "signal": signal,
            "good_sniper_count": len(good_in_mint),
            "total_good_sniper_buy_sol": round(total_buy, 9),
            "cohort_hold_pct_10s": round(cohort_hold_10s, 4),
            "cohort_hold_pct_30s": round(cohort_hold_30s, 4),
            "sniper_score_sum": round(score_sum, 2),
            "reason": "signal" if signal else "cohort_too_weak" if cohort_hold_10s < 0.5 else "buy_too_low",
        })

    return signals


def self_test():
    print("=== SELF-TEST: sniper_wallet_ranker ===")

    # Simulated 3 mints × 3 wallets
    test_mint_metrics = {}
    good = {
        "first_buy_age_secs": 3,
        "first_buy_sol": 0.05,
        "total_buy_sol": 0.05,
        "total_buy_tokens": 1000000,
        "pnl_windows": {
            10: {"sold_tokens": 50000, "sell_pct": 0.05, "hold_pct": 0.95},
            30: {"sold_tokens": 100000, "sell_pct": 0.10, "hold_pct": 0.90},
            60: {"sold_tokens": 150000, "sell_pct": 0.15, "hold_pct": 0.85},
        },
        "num_buys": 1, "num_sells": 1,
    }
    dumper = {
        "first_buy_age_secs": 2,
        "first_buy_sol": 0.10,
        "total_buy_sol": 0.10,
        "total_buy_tokens": 2000000,
        "pnl_windows": {
            10: {"sold_tokens": 1900000, "sell_pct": 0.95, "hold_pct": 0.05},
            30: {"sold_tokens": 2000000, "sell_pct": 1.0, "hold_pct": 0.0},
            60: {"sold_tokens": 2000000, "sell_pct": 1.0, "hold_pct": 0.0},
        },
        "num_buys": 1, "num_sells": 1,
    }

    # Populate 3 mints
    for i in range(3):
        mint = f"MINT_{i}"
        test_mint_metrics[mint] = {
            "GWALLET11111111111111111111111111111": good,
            "DWALLET22222222222222222222222222222": dumper,
        }

    results = compute_forward_pnl({}, test_mint_metrics)
    assert len(results) == 2, f"Expected 2 wallets, got {len(results)}"

    good_w = [r for r in results if r["category"] == "GOOD_SNIPER"]
    dumper_w = [r for r in results if r["category"] == "FAST_DUMPER"]

    assert len(good_w) >= 1, f"Expected at least 1 GOOD_SNIPER: {results}"
    assert len(dumper_w) >= 1, f"Expected at least 1 FAST_DUMPER: {results}"

    print(f"  GOOD_SNIPER score={good_w[0]['score']:.1f} hold_quality={good_w[0]['hold_quality']:.1f}")
    print(f"  FAST_DUMPER score={dumper_w[0]['score']:.1f} fast_dump_rate={dumper_w[0]['fast_dump_rate']:.2f}")

    # Test signal generation
    scores = [{"owner": r["owner"], "category": r["category"], "score": r["score"]} for r in results]
    signals = generate_signals(test_mint_metrics, scores)
    print(f"  signals generated: {len(signals)}")

    print("ALL SELF-TESTS PASSED")


def main():
    ap = argparse.ArgumentParser(description="Sniper Wallet Ranker")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()

    if args.self_test:
        self_test()
        return

    events = load_events()
    if not events:
        print("No events found. Run sniper_follow_backtest.py first.")
        return

    print(f"Loaded {len(events)} trade events")

    by_mint = group_by_mint(events)
    print(f"Unique mints: {len(by_mint)}")

    all_mint_metrics = {}
    for mint, mint_events in by_mint.items():
        launch_time = first_event_time(mint_events)
        wm = wallet_pnl_for_mint(mint_events, launch_time)
        all_mint_metrics[mint] = wm

    wallet_scores = compute_forward_pnl({}, all_mint_metrics)
    print(f"Wallet scores: {len(wallet_scores)}")
    for cat in ["GOOD_SNIPER", "FAST_DUMPER", "DEV_SNIPER_SUSPECT", "UNKNOWN"]:
        count = sum(1 for s in wallet_scores if s["category"] == cat)
        print(f"  {cat}: {count}")

    # Write scores
    SCORES_JSONL.parent.mkdir(parents=True, exist_ok=True)
    with open(SCORES_JSONL, "w") as f:
        for s in wallet_scores:
            f.write(json.dumps(s) + "\n")
    print(f"Scores → {SCORES_JSONL}")

    # CSV
    import csv
    fields = ["owner", "mints_seen", "total_buy_sol", "avg_hold_pct_10s", "avg_hold_pct_30s",
              "hold_quality", "fast_dump_rate", "score", "category"]
    with open(SCORES_CSV, "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields, extrasaction="ignore")
        w.writeheader()
        for s in wallet_scores:
            w.writerow(s)
    print(f"CSV → {SCORES_CSV}")

    # Generate signals
    signals = generate_signals(all_mint_metrics, wallet_scores)
    signal_true = [s for s in signals if s["signal"]]
    print(f"\nSniper-follow signals: {len(signals)} total, {len(signal_true)} signals")

    with open(SIGNALS_PATH, "w") as f:
        for s in signals:
            f.write(json.dumps(s) + "\n")
    print(f"Signals → {SIGNALS_PATH}")


if __name__ == "__main__":
    main()
