#!/usr/bin/env python3
"""Fresh 3.5k MC Entry Gate v2 — uses sniper trade events from Helius gTFA.

Detects: pump → dump → bounce pattern for fresh pump.fun tokens.
Reconstructs market cap from cumulative SOL flow.
Paper-only, read-only.

Config:
  TARGET_MC_SOL  — target entry market cap (~25 SOL ≈ $3.5k)
  MAX_AGE_SECS   — max token age for entry signal
  MIN_PUMP_PCT   — min pump above entry MC to qualify
  MAX_DUMP_RECOVERY — MC must recover to within this % of entry

Usage:
  python3 scripts/fresh_3500_entry_gate.py
"""

import json
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path

PROJECT = Path(__file__).resolve().parent.parent

EVENTS_PATH = PROJECT / "datasets" / "sniper_trade_events.jsonl"
OUTPUT_SIGNALS = PROJECT / "datasets" / "fresh_3500_shadow_signals.jsonl"
OUTPUT_REPORT = PROJECT / "datasets" / "fresh_3500_entry_gate_report.md"

TARGET_MC_SOL = 25.0
MC_BAND_MIN = 15.0
MC_BAND_MAX = 35.0
MAX_AGE_SECS = 60
MIN_PUMP_PCT = 0.10    # 10% pump from entry
MIN_DUMP_PCT = 0.03    # 3% drop from peak to qualify as dump
FORWARD_WINDOWS = [30, 60, 120]


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


def analyze():
    events = load_events()
    by_mint = defaultdict(list)
    for ev in events:
        by_mint[ev.get("mint", "")].append(ev)

    results = []
    for mint, mint_events in by_mint.items():
        mint_events.sort(key=lambda e: e.get("age_secs", 999))

        # Estimate entry MC from early buys
        entry_buys = sum(
            abs(e.get("quote_delta_sol", 0))
            for e in mint_events
            if e.get("side") == "buy" and e.get("age_secs", 99) <= 5
        )
        entry_mc = TARGET_MC_SOL + entry_buys
        if entry_mc < MC_BAND_MIN or entry_mc > MC_BAND_MAX:
            continue

        # Reconstruct MC curve from cumulative SOL flow
        cum_sol = entry_mc
        timeseries = []
        for ev in mint_events:
            delta = ev.get("quote_delta_sol", 0)
            side = ev.get("side", "")
            if side == "buy":
                cum_sol += abs(delta)
            elif side == "sell":
                cum_sol -= abs(delta)
            timeseries.append({
                "age": ev.get("age_secs", 0),
                "mc": max(cum_sol, 0.01),
            })

        if not timeseries:
            continue

        peak_mc = max(t["mc"] for t in timeseries)
        peak_age = next((t["age"] for t in timeseries if t["mc"] >= peak_mc * 0.99), 0)

        # MC at 60s
        mc_60 = next(
            (t["mc"] for t in timeseries if t["age"] >= MAX_AGE_SECS),
            timeseries[-1]["mc"] if timeseries else entry_mc,
        )

        pump_pct = (peak_mc - entry_mc) / entry_mc if entry_mc else 0
        if pump_pct < MIN_PUMP_PCT:
            continue

        # Check for dump after peak before 60s
        post_peak = [t for t in timeseries if peak_age <= t["age"] <= MAX_AGE_SECS]
        trough_mc = min((t["mc"] for t in post_peak), default=peak_mc)
        trough_age = next((t["age"] for t in post_peak if t["mc"] <= trough_mc * 1.01), 0)
        dump_pct = (peak_mc - trough_mc) / peak_mc if peak_mc else 0

        # Signal
        recovery_to_entry = mc_60 / entry_mc if entry_mc else 0
        if dump_pct >= MIN_DUMP_PCT and recovery_to_entry <= 1.10:
            signal = "WOULD_BUY_DIP"
        elif dump_pct < MIN_DUMP_PCT:
            signal = "SKIP_NO_DUMP"
        elif recovery_to_entry < 0.80:
            signal = "SKIP_RUG"
        else:
            signal = "SKIP_OTHER"

        # Forward PnL (from MC at 60s to future windows)
        forward = {}
        for w in FORWARD_WINDOWS:
            target_age = MAX_AGE_SECS + w
            future_mc = next(
                (t["mc"] for t in timeseries if t["age"] >= target_age),
                timeseries[-1]["mc"],
            )
            pnl = (future_mc - mc_60) / mc_60 * 100 if mc_60 else 0
            forward[f"pnl_{w}s"] = round(pnl, 2)

        creator = mint_events[0].get("signer", "") if mint_events else ""

        results.append({
            "mint": mint,
            "creator": creator,
            "entry_mc_sol": round(entry_mc, 4),
            "peak_mc_sol": round(peak_mc, 4),
            "peak_age_s": peak_age,
            "mc_at_60s": round(mc_60, 4),
            "trough_mc_sol": round(trough_mc, 4),
            "trough_age_s": trough_age,
            "pump_pct": round(pump_pct * 100, 2),
            "dump_pct": round(dump_pct * 100, 2),
            "recovery_ratio": round(recovery_to_entry, 3),
            "signal": signal,
            **forward,
        })

    return results


def main():
    results = analyze()
    results.sort(key=lambda r: r.get("pump_pct", 0), reverse=True)

    counts = defaultdict(int)
    for r in results:
        counts[r["signal"]] += 1

    would_buy = [r for r in results if r["signal"] == "WOULD_BUY_DIP"]
    n_buy = len(would_buy)

    lines = []
    lines.append("# Fresh 3.5k MC Entry Gate v2 — Trade Events")
    lines.append(f"\nGenerated: {datetime.now(timezone.utc).isoformat()}")
    lines.append(f"\n## Config")
    lines.append(f"- target_mc: {MC_BAND_MIN}-{MC_BAND_MAX} SOL")
    lines.append(f"- max_age: {MAX_AGE_SECS}s")
    lines.append(f"- min_pump: {MIN_PUMP_PCT*100:.0f}%")
    lines.append(f"\n## Summary")
    lines.append(f"- mints_analyzed: {len(results)}")
    for sig, n in sorted(counts.items()):
        lines.append(f"- {sig}: {n}")

    if n_buy > 0:
        avg_30 = sum(r.get("pnl_30s", 0) for r in would_buy) / n_buy
        avg_60 = sum(r.get("pnl_60s", 0) for r in would_buy) / n_buy
        avg_120 = sum(r.get("pnl_120s", 0) for r in would_buy) / n_buy
        lines.append(f"- avg_forward_pnl: 30s={avg_30:.1f}% 60s={avg_60:.1f}% 120s={avg_120:.1f}%")
        wins = sum(1 for r in would_buy if r.get("pnl_30s", -999) > 0)
        lines.append(f"- win_rate_30s: {wins}/{n_buy} ({wins/n_buy*100:.0f}%)")

        lines.append(f"\n## WOULD_BUY_DIP signals")
        lines.append(f"| Mint | Entry MC | Peak MC | Dump % | Recovery | PnL 30s | PnL 60s | PnL 120s |")
        lines.append(f"|------|----------|---------|--------|----------|---------|---------|----------|")
        for r in would_buy[:30]:
            lines.append(
                f"| {r['mint'][:12]}... | {r['entry_mc_sol']:.1f} | {r['peak_mc_sol']:.1f} | "
                f"{r['dump_pct']:.1f}% | {r['recovery_ratio']:.2f}x | "
                f"{r.get('pnl_30s','-')}% | {r.get('pnl_60s','-')}% | {r.get('pnl_120s','-')}% |"
            )

    OUTPUT_REPORT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT_REPORT.write_text("\n".join(lines))

    with open(OUTPUT_SIGNALS, "w") as f:
        for r in results:
            f.write(json.dumps(r) + "\n")

    print(f"mints={len(results)} signals={dict(counts)} would_buy={n_buy}")
    print(f"report: {OUTPUT_REPORT}")


if __name__ == "__main__":
    main()
