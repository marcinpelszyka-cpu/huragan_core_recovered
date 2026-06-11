#!/usr/bin/env python3
"""Z3 outcome audit for Huragan state.jsonl.

Offline-only. Reads JSONL/CSV and writes sanitized reports. It does not read .env,
does not sign, does not send transactions, and does not touch systemd.
"""
import argparse
import csv
import json
import math
import statistics
from collections import Counter, defaultdict
from pathlib import Path

TERMINAL_STATUSES = {
    "paper_completed",
    "completed",
    "live_failed",
    "live_sell_failed",
    "live_sell_failed_retryable",
    "unrecoverable_dust_or_rug",
    "quote_unsupported_shadow",
}
OPEN_STATUSES = {"holding", "live_sell_failed_retryable"}
CONTROL_VARIANTS = ["Z", "Z3", "Z3.1"]


def read_jsonl(path: Path):
    if not path.exists():
        return []
    rows = []
    with path.open() as f:
        for i, line in enumerate(f, 1):
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except Exception as e:
                print(f"WARN bad_json path={path} line={i}: {e}")
    return rows


def fnum(v, default=0.0):
    try:
        if v is None or v == "":
            return default
        x = float(v)
        if math.isnan(x) or math.isinf(x):
            return default
        return x
    except Exception:
        return default


def inum(v, default=0):
    try:
        if v is None or v == "":
            return default
        return int(float(v))
    except Exception:
        return default


def bval(v):
    if isinstance(v, bool):
        return v
    if isinstance(v, str):
        return v.lower() in {"1", "true", "yes"}
    return bool(v)


def median(xs):
    xs = [x for x in xs if isinstance(x, (int, float)) and math.isfinite(x)]
    return statistics.median(xs) if xs else 0.0


def avg(xs):
    xs = [x for x in xs if isinstance(x, (int, float)) and math.isfinite(x)]
    return sum(xs) / len(xs) if xs else 0.0


def latest_by(rows, key_fn):
    out = {}
    for r in rows:
        k = key_fn(r)
        if k:
            out[k] = r
    return out


def clean_paper_rows(rows):
    latest = latest_by(rows, lambda r: (r.get("mint"), r.get("variant_id", "")) if r.get("mint") else None)
    out = []
    for r in latest.values():
        if r.get("status") != "paper_completed":
            continue
        if bval(r.get("excluded_from_stats")):
            continue
        if r.get("exit_reason") in {"price_unavailable", "data_quality_fail", "invalid_quote"}:
            continue
        if fnum(r.get("paper_entry_sol")) <= 0 and fnum(r.get("cost_basis_sol")) <= 0:
            continue
        out.append(r)
    return out


def variant_metrics(rows):
    out = []
    by_variant = defaultdict(list)
    for r in rows:
        by_variant[r.get("variant_id", "")].append(r)
    for variant in sorted(by_variant):
        rs = by_variant[variant]
        pnls_pct = [fnum(r.get("net_pnl_pct")) for r in rs]
        pnls_sol = [fnum(r.get("net_pnl_sol")) for r in rs]
        wins = [x for x in pnls_sol if x > 0]
        exits = Counter(r.get("exit_reason", "") or "unknown" for r in rs)
        out.append({
            "variant_id": variant,
            "n": len(rs),
            "wr_pct": round(len(wins) / len(rs) * 100, 4) if rs else 0.0,
            "avg_pnl_pct": round(avg(pnls_pct), 6),
            "median_pnl_pct": round(median(pnls_pct), 6),
            "total_sol": round(sum(pnls_sol), 12),
            "avg_mfe_pct": round(avg([fnum(r.get("max_favorable_pct")) for r in rs]), 6),
            "median_mfe_pct": round(median([fnum(r.get("max_favorable_pct")) for r in rs]), 6),
            "max_hold": exits.get("max_hold", 0),
            "hard_stop": exits.get("hard_stop", 0),
            "early_no_momentum": exits.get("early_no_momentum", 0),
            "profit_protect": exits.get("profit_protect", 0),
            "breakeven_floor": exits.get("breakeven_floor", 0),
            "trailing_stop": exits.get("trailing_stop", 0),
        })
    return out


def mfe_band_rows(rows, variant="Z3"):
    bands = [
        ("<0", None, 0),
        ("0-20", 0, 20),
        ("20-30", 20, 30),
        ("30-60", 30, 60),
        ("60-100", 60, 100),
        ("100-150", 100, 150),
        ("150+", 150, None),
    ]
    rs = [r for r in rows if r.get("variant_id") == variant]
    out = []
    for name, lo, hi in bands:
        xs = []
        for r in rs:
            mfe = fnum(r.get("max_favorable_pct"))
            if lo is None and mfe < hi:
                xs.append(r)
            elif hi is None and mfe >= lo:
                xs.append(r)
            elif lo is not None and hi is not None and lo <= mfe < hi:
                xs.append(r)
        exits = Counter(r.get("exit_reason", "") or "unknown" for r in xs)
        out.append({
            "variant_id": variant,
            "mfe_band": name,
            "n": len(xs),
            "wr_pct": round(sum(1 for r in xs if fnum(r.get("net_pnl_sol")) > 0) / len(xs) * 100, 4) if xs else 0.0,
            "avg_pnl_pct": round(avg([fnum(r.get("net_pnl_pct")) for r in xs]), 6),
            "median_pnl_pct": round(median([fnum(r.get("net_pnl_pct")) for r in xs]), 6),
            "total_sol": round(sum(fnum(r.get("net_pnl_sol")) for r in xs), 12),
            "top_exit": exits.most_common(1)[0][0] if exits else "",
            "profit_protect": exits.get("profit_protect", 0),
            "early_no_momentum": exits.get("early_no_momentum", 0),
            "hard_stop": exits.get("hard_stop", 0),
            "max_hold": exits.get("max_hold", 0),
        })
    return out


def is_live_row(r):
    status = r.get("status", "")
    # Paper lifecycle also emits transient status=holding rows. Treat rows as live
    # only when they carry live-specific terminal states/fields or source=live.
    if r.get("source") == "live":
        return True
    if status in {"completed", "live_failed", "live_sell_failed", "live_sell_failed_retryable", "unrecoverable_dust_or_rug"}:
        return True
    if r.get("sell_signature") or r.get("live_exit_reason") or r.get("live_sell_family"):
        return True
    tx = str(r.get("tx_signature", ""))
    return tx.startswith("LIVE_")


def latest_live_rows(rows):
    live = [r for r in rows if is_live_row(r)]
    latest = latest_by(live, lambda r: r.get("mint"))
    return list(latest.values())


def open_live_blockers(rows):
    # Latest by mint+variant prevents stale paper holding rows from blocking when
    # a later paper_completed row for the same lifecycle exists. Then restrict
    # blockers to rows that are actually live.
    latest = latest_by(rows, lambda r: (r.get("mint"), r.get("variant_id", "")) if r.get("mint") else None)
    return [r for r in latest.values() if r.get("status") in OPEN_STATUSES and is_live_row(r)]


def live_outcome_category(r):
    status = r.get("status", "")
    reason = r.get("exit_reason", "") or r.get("live_exit_reason", "")
    pnl = fnum(r.get("realized_pnl_sol") or r.get("net_pnl_sol"))
    label = r.get("diagnostic_label", "")
    if label == "RPC_PREFLIGHT_FALSE_REJECTION":
        return "rpc_preflight_false_rejection"
    if label == "POOL_LEVEL_REJECTED":
        return "pool_level_rejected"
    if label == "ONCHAIN_DIAGNOSTIC_TEST":
        return "diagnostic_onchain_test"
    if reason.startswith("diagnostic_daily_limit_reached"):
        return "diagnostic_daily_limit_reached"
    if status == "completed":
        return "completed_profit" if pnl > 0 else "completed_loss"
    if status == "live_failed" and reason.startswith("live_entry_"):
        return "live_failed_entry_gate"
    if status == "unrecoverable_dust_or_rug":
        return "unrecoverable_dust_or_rug"
    return status or "unknown"


def canary_rows(rows):
    live = latest_live_rows(rows)
    out = []
    for r in live:
        status = r.get("status", "")
        if status not in {"completed", "live_failed", "live_sell_failed", "live_sell_failed_retryable", "unrecoverable_dust_or_rug", "holding"}:
            continue
        out.append({
            "mint": r.get("mint", ""),
            "status": status,
            "outcome_category": live_outcome_category(r),
            "buy_tx": r.get("tx_signature", ""),
            "sell_tx": r.get("sell_signature", ""),
            "exit_reason": r.get("live_exit_reason") or r.get("exit_reason", ""),
            "hold_secs": inum(r.get("hold_secs")),
            "pnl_sol": round(fnum(r.get("realized_pnl_sol") or r.get("net_pnl_sol")), 12),
            "pnl_pct": round(fnum(r.get("net_pnl_pct")), 6),
            "remaining_tokens": inum(r.get("remaining_tokens")),
            "terminal": status not in OPEN_STATUSES,
        })
    return out


def write_csv(path: Path, rows, fields):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields, extrasaction="ignore")
        w.writeheader()
        for r in rows:
            w.writerow(r)


def write_report(path: Path, metrics, bands, canaries, blockers, risk_status, state_path):
    path.parent.mkdir(parents=True, exist_ok=True)
    z3 = next((r for r in metrics if r.get("variant_id") == "Z3"), None)
    z = next((r for r in metrics if r.get("variant_id") == "Z"), None)
    z31 = next((r for r in metrics if r.get("variant_id") == "Z3.1"), None)
    with path.open("w") as f:
        f.write("# Z3 Outcome Audit\n\n")
        f.write(f"Input state: `{state_path}`\n\n")
        f.write("## Risk Manager\n\n")
        f.write(f"- status: **{risk_status['status']}**\n")
        f.write(f"- daily PnL: {risk_status['daily_pnl']:.9f} SOL\n")
        f.write(f"- daily trades: {risk_status['daily_trades']}\n")
        f.write(f"- consecutive losses: {risk_status['consecutive_losses']}\n")
        f.write(f"- gate pass rate: {risk_status['gate_pass_rate']:.1f}% ({risk_status['gate_passed']}/{risk_status['gate_total']})\n\n")
        f.write("## Decision gate\n\n")
        if blockers:
            f.write(f"- **NO_GO**: open live blockers = {len(blockers)}\n")
            for b in blockers:
                f.write(f"  - mint={b.get('mint','')} status={b.get('status','')} reason={b.get('exit_reason','')}\n")
        else:
            f.write("- **OK**: open live blockers = 0\n")
        if z3:
            f.write(f"- Z3: n={z3['n']} WR={z3['wr_pct']:.1f}% avg={z3['avg_pnl_pct']:.2f}% median={z3['median_pnl_pct']:.2f}% total={z3['total_sol']:.9f} SOL\n")
        if z and z3:
            f.write(f"- Z3 vs Z total delta: {z3['total_sol'] - z['total_sol']:.9f} SOL\n")
        if z31 and z3:
            f.write(f"- Z3 vs Z3.1 total delta: {z3['total_sol'] - z31['total_sol']:.9f} SOL\n")
        f.write("\n## Variant metrics\n\n")
        f.write("| Variant | n | WR | Avg % | Median % | Total SOL | early | hard | protect | max_hold |\n")
        f.write("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n")
        for r in [x for x in metrics if x.get("variant_id") in CONTROL_VARIANTS]:
            f.write(f"| {r['variant_id']} | {r['n']} | {r['wr_pct']:.1f}% | {r['avg_pnl_pct']:.2f} | {r['median_pnl_pct']:.2f} | {r['total_sol']:.9f} | {r['early_no_momentum']} | {r['hard_stop']} | {r['profit_protect']} | {r['max_hold']} |\n")
        f.write("\n## Z3 MFE bands\n\n")
        f.write("| MFE band | n | WR | Avg % | Median % | Total SOL | Top exit | protect | early | hard | max_hold |\n")
        f.write("|---|---:|---:|---:|---:|---:|---|---:|---:|---:|---:|\n")
        for r in bands:
            f.write(f"| {r['mfe_band']} | {r['n']} | {r['wr_pct']:.1f}% | {r['avg_pnl_pct']:.2f} | {r['median_pnl_pct']:.2f} | {r['total_sol']:.9f} | {r['top_exit']} | {r['profit_protect']} | {r['early_no_momentum']} | {r['hard_stop']} | {r['max_hold']} |\n")
        f.write("\n## Live canary ledger\n\n")
        if not canaries:
            f.write("No live canary rows found.\n")
        else:
            counts = Counter(r.get("outcome_category", "unknown") for r in canaries)
            f.write("Outcome counts:\n")
            for key in ["completed_profit", "completed_loss", "live_failed_entry_gate", "diagnostic_onchain_test", "rpc_preflight_false_rejection", "pool_level_rejected", "diagnostic_daily_limit_reached", "unrecoverable_dust_or_rug", "live_failed", "live_sell_failed_retryable"]:
                if counts.get(key):
                    f.write(f"- {key}: {counts[key]}\n")
            f.write("\n| Mint | Category | Status | Exit | Hold s | PnL SOL | PnL % | Remaining | Sell tx |\n")
            f.write("|---|---|---|---|---:|---:|---:|---:|---|\n")
            for r in canaries:
                sell = "yes" if r.get("sell_tx") else ""
                f.write(f"| {r['mint']} | {r['outcome_category']} | {r['status']} | {r['exit_reason']} | {r['hold_secs']} | {r['pnl_sol']:.9f} | {r['pnl_pct']:.2f} | {r['remaining_tokens']} | {sell} |\n")
        f.write("\n## Notes\n\n")
        f.write("- This is an outcome audit, not a tick-level re-simulation.\n")
        f.write("- True parameter sweep requires per-position quote/value time series. Terminal rows alone cannot prove alternate exits without bias.\n")
        f.write("- No secrets, no .env, no signing, no live execution.\n")


def risk_manager_status(rows):
    """Compute LiveRiskManager v1 status from state.jsonl."""
    from datetime import datetime, timezone
    today = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    live = [r for r in rows if is_live_row(r)]
    latest = list(latest_by(live, lambda r: (r.get("mint"), r.get("variant_id", "")) if r.get("mint") else None).values())

    today_rows = [r for r in latest if r.get("live_send_day") == today]
    daily_pnl = sum(fnum(r.get("realized_pnl_sol") or r.get("net_pnl_sol")) for r in today_rows)
    daily_trades = sum(1 for r in today_rows if r.get("status") in {"completed", "live_failed", "unrecoverable_dust_or_rug"})
    has_sell_failed = any(r.get("status") == "live_sell_failed_retryable" for r in today_rows)

    sorted_rows = sorted(latest, key=lambda r: r.get("timestamp", ""))
    consecutive = 0
    for r in reversed(sorted_rows):
        if r.get("status") in {"completed", "unrecoverable_dust_or_rug"}:
            if fnum(r.get("realized_pnl_sol") or r.get("net_pnl_sol")) <= 0 or r.get("status") == "unrecoverable_dust_or_rug":
                consecutive += 1
            else:
                break
        elif r.get("status") == "live_failed":
            consecutive += 1
        else:
            break

    gate_passed = sum(1 for r in latest if r.get("quote_reserve_ui", 0) >= 100)
    gate_total = len([r for r in latest if r.get("quote_reserve_ui", 0) > 0])
    gate_rate = (gate_passed / gate_total * 100) if gate_total > 0 else 0.0

    blockers = []
    if daily_pnl <= -0.01:
        blockers.append("daily_loss_limit")
    if daily_trades >= 10:
        blockers.append("daily_trade_limit")
    if consecutive >= 3:
        blockers.append("consecutive_loss_limit")
    if has_sell_failed:
        blockers.append("live_sell_failed_today")
    if len(open_live_blockers(rows)) > 0:
        blockers.append("open_blockers")

    status = "NO_GO:" + ",".join(blockers) if blockers else "GO"
    return {
        "status": status,
        "daily_pnl": daily_pnl,
        "daily_trades": daily_trades,
        "consecutive_losses": consecutive,
        "gate_passed": gate_passed,
        "gate_total": gate_total,
        "gate_pass_rate": gate_rate,
        "blockers": blockers,
    }


def main():
    ap = argparse.ArgumentParser(description="Offline Z3 outcome audit")
    ap.add_argument("--state", default="state.jsonl")
    ap.add_argument("--out-dir", default="datasets")
    args = ap.parse_args()

    state_path = Path(args.state)
    out = Path(args.out_dir)
    rows = read_jsonl(state_path)
    if not rows:
        raise SystemExit(f"no state rows found: {state_path}")

    clean = clean_paper_rows(rows)
    metrics = variant_metrics(clean)
    bands = mfe_band_rows(clean, "Z3")
    canaries = canary_rows(rows)
    blockers = open_live_blockers(rows)
    risk = risk_manager_status(rows)

    write_csv(out / "z3_variant_outcome_metrics.csv", metrics, [
        "variant_id", "n", "wr_pct", "avg_pnl_pct", "median_pnl_pct", "total_sol",
        "avg_mfe_pct", "median_mfe_pct", "max_hold", "hard_stop", "early_no_momentum",
        "profit_protect", "breakeven_floor", "trailing_stop",
    ])
    write_csv(out / "z3_mfe_band_metrics.csv", bands, [
        "variant_id", "mfe_band", "n", "wr_pct", "avg_pnl_pct", "median_pnl_pct",
        "total_sol", "top_exit", "profit_protect", "early_no_momentum", "hard_stop", "max_hold",
    ])
    write_csv(out / "z3_live_canary_ledger.csv", canaries, [
        "mint", "outcome_category", "status", "buy_tx", "sell_tx", "exit_reason", "hold_secs", "pnl_sol", "pnl_pct", "remaining_tokens", "terminal",
    ])
    write_report(out / "z3_outcome_audit.md", metrics, bands, canaries, blockers, risk, state_path)

    z3 = next((r for r in metrics if r.get("variant_id") == "Z3"), None)
    print(f"rows={len(rows)} clean_paper={len(clean)} canaries={len(canaries)} open_live_blockers={len(blockers)}")
    print(f"risk_manager={risk['status']} daily_pnl={risk['daily_pnl']:.9f} daily_trades={risk['daily_trades']} consecutive_losses={risk['consecutive_losses']}")
    if z3:
        print(f"Z3 n={z3['n']} WR={z3['wr_pct']:.1f}% avg={z3['avg_pnl_pct']:.2f}% median={z3['median_pnl_pct']:.2f}% total={z3['total_sol']:.9f} SOL")
    print(f"wrote {out / 'z3_outcome_audit.md'}")


if __name__ == "__main__":
    main()
