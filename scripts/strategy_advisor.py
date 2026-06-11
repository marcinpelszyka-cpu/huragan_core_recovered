#!/usr/bin/env python3
"""Strategy Advisor v1 - rekomendacje na podstawie danych z canary.

Czyta: state.jsonl, datasets/reserve_bucket_summary.json
Generuje: CONTINUE_CANARY_SERIES, STOP_SERIES, KEEP_GATE_100, TEST_GATE_75, NO_GO_SCALE
"""

import json
import sys
from pathlib import Path
from datetime import datetime, timezone

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

def load_buckets(path: Path):
    if not path.exists():
        return []
    with path.open() as f:
        return json.load(f)

def check_risk(rows):
    """Returns (risk_status, blockers)"""
    today = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    
    today_rows = [r for r in rows if r.get("live_send_day") == today]
    daily_pnl = sum(r.get("realized_pnl_sol", 0) for r in today_rows)
    daily_trades = len([r for r in today_rows if r.get("status") in ("completed", "live_failed")])
    has_sell_failed = any(r.get("status") == "live_sell_failed_retryable" for r in today_rows)
    
    sorted_rows = sorted(
        [r for r in rows if r.get("status") in ("completed", "live_failed", "unrecoverable_dust_or_rug")],
        key=lambda r: r.get("timestamp", "")
    )
    consecutive = 0
    for r in reversed(sorted_rows):
        s = r.get("status")
        if s == "completed" and r.get("realized_pnl_sol", 0) > 0:
            break
        elif s in ("completed", "live_failed", "unrecoverable_dust_or_rug"):
            consecutive += 1
        else:
            break
    
    # Deduplicate: only latest row per (mint, variant_id)
    from collections import defaultdict as dd
    latest = {}
    for r in rows:
        if r.get("mint"):
            k = (r["mint"], r.get("variant_id", ""))
            if k not in latest or r.get("timestamp", "") >= latest[k].get("timestamp", ""):
                latest[k] = r
    latest_rows = list(latest.values())
    
    open_blockers = len([r for r in latest_rows if r.get("status") in ("holding", "live_sell_failed_retryable") and r.get("remaining_tokens", 0) > 0 and r.get("live_send_day", "")])
    
    blockers = []
    if daily_pnl <= -0.01:
        blockers.append(f"daily_loss_limit (pnl={daily_pnl:.6f})")
    if daily_trades >= 10:
        blockers.append(f"daily_trade_limit ({daily_trades}/10)")
    if consecutive >= 3:
        blockers.append(f"consecutive_loss_limit ({consecutive}/3)")
    if has_sell_failed:
        blockers.append("live_sell_failed_today")
    if open_blockers > 0:
        blockers.append(f"open_blockers ({open_blockers})")
    
    if blockers:
        return "NO_GO:" + ",".join(blockers), blockers
    
    return "GO", []

def check_gate(buckets):
    """Returns gate recommendation"""
    b100 = next((b for b in buckets if b["bucket"] == "100-200"), None)
    b75 = next((b for b in buckets if b["bucket"] == "75-100"), None)
    b200 = next((b for b in buckets if b["bucket"] == "200-500"), None)
    
    # Sprawdź 100+ SOL bucket
    b100_ok = b100 and b100.get("count", 0) > 0 and b100.get("total_pnl_sol", 0) > 0
    b75_ok = b75 and b75.get("count", 0) > 0 and b75.get("total_pnl_sol", 0) > 0
    
    # Wysokie hard_stop_rate lub price_unavailable = zagrożenie
    b100_risk = b100 and (b100.get("hard_stop_rate", 0) > 20 or b100.get("price_unavailable_rate", 0) > 30)
    b75_risk = b75 and (b75.get("hard_stop_rate", 0) > 30 or b75.get("price_unavailable_rate", 0) > 40)
    
    if b100_ok and not b100_risk:
        return "KEEP_GATE_100", {
            "reason": f"100+ bucket: {b100['count']} trades, {b100['win_rate']:.1f}% WR, +{b100['total_pnl_sol']:.6f} SOL",
            "bucket_100": b100,
        }
    
    if b75_ok and not b75_risk and b100 and b100.get("count", 0) < 20:
        return "TEST_GATE_75", {
            "reason": f"75-100 bucket lepszy niż 100-200 (za mało danych na 100+): {b75['count']} trades, {b75['win_rate']:.1f}% WR",
            "bucket_75": b75,
            "bucket_100": b100,
        }
    
    if b200 and b200.get("count", 0) > 5 and b200.get("win_rate", 0) > 70 and b200.get("total_pnl_sol", 0) > 0:
        return "TEST_GATE_150", {
            "reason": f"200+ bucket lepsza jakość: {b200['count']} trades, {b200['win_rate']:.1f}% WR, +{b200['total_pnl_sol']:.6f} SOL",
            "bucket_200": b200,
        }
    
    return "NO_GO_SCALE", {
        "reason": f"Za mało danych lub bucket 100+ stratny. b100={b100.get('count',0) if b100 else 0} trades, b75={b75.get('count',0) if b75 else 0} trades",
        "bucket_100": b100,
        "bucket_75": b75,
    }

def check_series(rows):
    """Count canary trades at gate 100+"""
    canaries_100 = []
    for r in rows:
        if r.get("status") != "completed":
            continue
        reserve = r.get("entry_quote_reserve_raw", 0)
        if reserve > 0 and reserve / 1e9 >= 100:
            canaries_100.append(r)
    
    total_pnl = sum(r.get("realized_pnl_sol", 0) for r in canaries_100)
    
    if len(canaries_100) < 20:
        return "NO_GO_SCALE", f"Za mało canary po 100 SOL: {len(canaries_100)}/20 (net={total_pnl:.6f} SOL)"
    
    wins = [r for r in canaries_100 if r.get("realized_pnl_sol", 0) > 0]
    losses = [r for r in canaries_100 if r.get("realized_pnl_sol", 0) < 0]
    
    avg_win = sum(r.get("realized_pnl_sol", 0) for r in wins) / len(wins) if wins else 0
    avg_loss = sum(r.get("realized_pnl_sol", 0) for r in losses) / len(losses) if losses else 0
    
    if avg_loss != 0 and abs(avg_loss) > abs(avg_win) * 3:
        return "STOP_SERIES", f"avg_loss ({avg_loss:.6f}) >> avg_win ({avg_win:.6f}), seria niestabilna"
    
    return "CONTINUE_CANARY_SERIES", f"{len(canaries_100)} canary po 100+, net={total_pnl:.6f} SOL, avg_win={avg_win:.6f}, avg_loss={avg_loss:.6f}"

def main():
    state_path = Path("state.jsonl")
    buckets_path = Path("datasets/reserve_bucket_summary.json")
    
    if not state_path.exists():
        print(f"ERROR: {state_path} not found", file=sys.stderr)
        sys.exit(1)
    
    rows = load_state(state_path)
    buckets = load_buckets(buckets_path)
    
    risk, blockers = check_risk(rows)
    gate, gate_info = check_gate(buckets)
    series, series_info = check_series(rows)
    
    print("=== STRATEGY ADVISOR v1 ===")
    print(f"RISK_MANAGER: {risk}")
    print(f"GATE_RECOMMENDATION: {gate}")
    print(f"  → {gate_info.get('reason', '')}")
    print(f"SERIES_STATUS: {series}")
    print(f"  → {series_info}")
    print()
    
    if "NO_GO" in risk:
        print("⛔ STOP - Risk manager blokuje")
    elif "NO_GO_SCALE" in gate:
        print(f"⚠️  {gate} - " + gate_info.get("reason", ""))
    else:
        print(f"✅ {gate} + {series}")

if __name__ == "__main__":
    main()
