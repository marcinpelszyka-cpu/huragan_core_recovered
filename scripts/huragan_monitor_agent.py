#!/usr/bin/env python3
"""Huragan 2h monitor agent.

Refreshes datasets, runs market_supervisor, compares current 2h strategy metrics
with the previous monitor run, and writes a compact Hermes-style report.
No trading, no send path, analytics only.
"""

import argparse
import csv
import json
import os
import subprocess
import sys
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path


def run(cmd, cwd):
    proc = subprocess.run(cmd, cwd=cwd, text=True, capture_output=True)
    if proc.returncode != 0:
        raise RuntimeError(
            f"command failed ({proc.returncode}): {' '.join(cmd)}\nSTDOUT:\n{proc.stdout}\nSTDERR:\n{proc.stderr}"
        )
    return proc.stdout.strip()


def read_json(path):
    try:
        with open(path) as f:
            return json.load(f)
    except FileNotFoundError:
        return None
    except json.JSONDecodeError:
        return None


def read_csv(path):
    if not path.exists():
        return []
    with open(path, newline="") as f:
        return list(csv.DictReader(f))


def read_state_jsonl(path):
    rows = []
    if not path.exists():
        return rows
    with open(path) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError:
                continue
    return rows


def fnum(value, default=0.0):
    try:
        if value in (None, ""):
            return default
        return float(value)
    except (TypeError, ValueError):
        return default


def pct(part, total):
    return 100.0 * part / total if total else 0.0


def median(values):
    values = sorted(v for v in values if isinstance(v, (int, float)))
    if not values:
        return 0.0
    n = len(values)
    mid = n // 2
    if n % 2:
        return values[mid]
    return (values[mid - 1] + values[mid]) / 2.0


def latest_terminal_by_mint_variant(state_rows, window_mins):
    cutoff = None
    now = datetime.now(timezone.utc).timestamp()
    if window_mins > 0:
        cutoff = now - window_mins * 60
    latest = {}
    for row in state_rows:
        if row.get("status") != "paper_completed":
            continue
        mint = row.get("mint")
        variant = row.get("variant_id")
        if not mint or not variant:
            continue
        # Existing state rows do not consistently include wall-clock timestamps.
        # Use append order as source of truth and window by latest N if timestamps are absent.
        latest[(mint, variant)] = row
    values = list(latest.values())
    if cutoff is not None:
        timed = []
        for row in values:
            ts = fnum(row.get("completed_at") or row.get("captured_at") or row.get("updated_at"), 0.0)
            if ts and ts >= cutoff:
                timed.append(row)
        if timed:
            values = timed
        else:
            # Fallback for timestamp-less state: cap to recent lifecycle volume.
            values = values[-1200:]
    return values


def variant_metrics(rows, variant):
    xs = [r for r in rows if r.get("variant_id") == variant and fnum(r.get("paper_entry_sol")) > 0]
    pnls = [fnum(r.get("net_pnl_pct")) for r in xs if not r.get("excluded_from_stats")]
    clean = len(pnls)
    reasons = Counter(r.get("exit_reason", "") for r in xs)
    return {
        "variant": variant,
        "completed": len(xs),
        "clean": clean,
        "wr_pct": round(pct(sum(1 for p in pnls if p > 0), clean), 4),
        "avg_pnl_pct": round(sum(pnls) / clean, 6) if clean else 0.0,
        "median_pnl_pct": round(median(pnls), 6),
        "total_sol": round(sum(fnum(r.get("net_pnl_sol")) for r in xs if not r.get("excluded_from_stats")), 9),
        "price_unavailable": reasons.get("price_unavailable", 0),
        "profit_protect": reasons.get("profit_protect", 0),
        "early_no_momentum": reasons.get("early_no_momentum", 0),
        "hard_stop": reasons.get("hard_stop", 0),
        "max_hold": reasons.get("max_hold", 0),
        "trailing_stop": reasons.get("trailing_stop", 0),
    }


def load_fresh_metrics(dataset_dir, root):
    fresh_summary = read_csv(dataset_dir / "fresh_all_mint_summary.csv")
    v2_candidates = sum(1 for _ in open(root / "fresh_lifecycle_v2_candidates.jsonl")) if (root / "fresh_lifecycle_v2_candidates.jsonl").exists() else 0
    v2_snapshots = sum(1 for _ in open(root / "fresh_lifecycle_v2_snapshots.jsonl")) if (root / "fresh_lifecycle_v2_snapshots.jsonl").exists() else 0
    no_trade = 0
    for r in fresh_summary:
        if (r.get("label") == "no_trade_data") or str(r.get("trade_stream_missing", "")).lower() == "true":
            no_trade += 1
    return {
        "tracked_mints": len(fresh_summary),
        "no_trade_data": no_trade,
        "no_trade_data_pct": round(pct(no_trade, len(fresh_summary)), 4),
        "v2_candidates": v2_candidates,
        "v2_snapshots": v2_snapshots,
        "trade_stream_available": len(fresh_summary) > 0 and no_trade < len(fresh_summary),
    }


def diff_metrics(current, previous):
    if not previous:
        return {}
    out = {}
    prev_by_variant = {m.get("variant"): m for m in previous.get("variant_metrics", [])}
    for m in current.get("variant_metrics", []):
        prev = prev_by_variant.get(m.get("variant"))
        if not prev:
            continue
        out[m["variant"]] = {
            "wr_pct_delta": round(m.get("wr_pct", 0.0) - prev.get("wr_pct", 0.0), 4),
            "avg_pnl_pct_delta": round(m.get("avg_pnl_pct", 0.0) - prev.get("avg_pnl_pct", 0.0), 6),
            "median_pnl_pct_delta": round(m.get("median_pnl_pct", 0.0) - prev.get("median_pnl_pct", 0.0), 6),
            "total_sol_delta": round(m.get("total_sol", 0.0) - prev.get("total_sol", 0.0), 9),
        }
    return out


def build_alerts(snapshot, previous):
    alerts = []
    z3 = next((m for m in snapshot["variant_metrics"] if m["variant"] == "Z3"), None)
    if z3:
        pu_pct = pct(z3["price_unavailable"], max(z3["completed"], 1))
        if pu_pct > 30.0:
            alerts.append(f"RPC issue: Z3 price_unavailable={pu_pct:.1f}% > 30%")
        if previous:
            prev_z3 = next((m for m in previous.get("variant_metrics", []) if m.get("variant") == "Z3"), None)
            if prev_z3 and prev_z3.get("avg_pnl_pct", 0.0) > 0:
                drop = (prev_z3["avg_pnl_pct"] - z3["avg_pnl_pct"]) / prev_z3["avg_pnl_pct"]
                if drop > 0.5:
                    alerts.append(f"Strategy degradation: Z3 avg dropped {drop*100:.1f}% vs previous window")
    fresh = snapshot.get("fresh_metrics", {})
    prev_fresh = previous.get("fresh_metrics", {}) if previous else {}
    if fresh.get("trade_stream_available") and not prev_fresh.get("trade_stream_available"):
        alerts.append("Fresh data available: trade stream is no longer 100% no_trade_data")
    return alerts


def format_report(snapshot, previous, decision_doc, deltas, alerts):
    z3 = next((m for m in snapshot["variant_metrics"] if m["variant"] == "Z3"), {})
    fresh = snapshot.get("fresh_metrics", {})
    decision = (decision_doc or {}).get("decision", "UNKNOWN")
    live_allowed = (decision_doc or {}).get("live_allowed", False)
    z3_delta = deltas.get("Z3", {})
    lines = []
    lines.append("# 📊 HURAGAN REPORT — 2h window")
    lines.append("")
    lines.append(f"Generated: `{snapshot['generated_at']}`")
    lines.append(f"Decision: `{decision}` live_allowed={str(live_allowed).lower()}")
    lines.append("")
    lines.append("## Z3")
    lines.append(
        f"WR={z3.get('wr_pct',0):.2f}% avg={z3.get('avg_pnl_pct',0):+.2f}% "
        f"median={z3.get('median_pnl_pct',0):+.2f}% total={z3.get('total_sol',0):+.6f} SOL "
        f"completed={z3.get('completed',0)}"
    )
    if z3_delta:
        lines.append(
            f"Change vs last: WR {z3_delta.get('wr_pct_delta',0):+.2f}pp, "
            f"avg {z3_delta.get('avg_pnl_pct_delta',0):+.2f}pp, "
            f"median {z3_delta.get('median_pnl_pct_delta',0):+.2f}pp, "
            f"total {z3_delta.get('total_sol_delta',0):+.6f} SOL"
        )
    lines.append(
        f"Exits: profit_protect={z3.get('profit_protect',0)}, "
        f"early_no_momentum={z3.get('early_no_momentum',0)}, hard_stop={z3.get('hard_stop',0)}, "
        f"price_unavailable={z3.get('price_unavailable',0)}"
    )
    lines.append("")
    lines.append("## Variant spread")
    lines.append("| Variant | Completed | WR | Avg | Median | Total SOL | Profit protect | Early no momentum |")
    lines.append("|---|---:|---:|---:|---:|---:|---:|---:|")
    for m in snapshot["variant_metrics"]:
        lines.append(
            f"| {m['variant']} | {m['completed']} | {m['wr_pct']:.1f}% | {m['avg_pnl_pct']:+.2f}% | "
            f"{m['median_pnl_pct']:+.2f}% | {m['total_sol']:+.6f} | {m['profit_protect']} | {m['early_no_momentum']} |"
        )
    lines.append("")
    lines.append("## Fresh")
    lines.append(
        f"tracked={fresh.get('tracked_mints',0)}, no_trade_data={fresh.get('no_trade_data_pct',0):.1f}%, "
        f"v2_candidates={fresh.get('v2_candidates',0)}, v2_snapshots={fresh.get('v2_snapshots',0)}"
    )
    lines.append("")
    lines.append("## Alerts")
    if alerts:
        for a in alerts:
            lines.append(f"- ⚠️ {a}")
    else:
        lines.append("- none")
    return "\n".join(lines) + "\n"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--root", default="/opt/huragan_core")
    ap.add_argument("--window-mins", type=int, default=120)
    ap.add_argument("--dataset-dir", default="datasets")
    ap.add_argument("--snapshot", default="/opt/huragan_core/monitor_agent_snapshot.json")
    ap.add_argument("--output-json", default="/opt/huragan_core/monitor_agent_report.json")
    ap.add_argument("--output-md", default="/tmp/huragan_monitor_report.md")
    args = ap.parse_args()

    root = Path(args.root)
    dataset_dir = root / args.dataset_dir
    decision_path = root / "agents_decision.json"
    supervisor_report = "/tmp/market_supervisor_report.md"

    run(["python3", "scripts/build_historical_datasets.py", "--out-dir", str(dataset_dir)], root)
    run([
        str(root / "target/release/market_supervisor"),
        "--state", str(root / "state.jsonl"),
        "--live-state", str(root / "state.jsonl"),
        "--dataset-dir", str(dataset_dir),
        "--window-mins", str(args.window_mins),
        "--output", str(decision_path),
        "--report", supervisor_report,
    ], root)

    state_rows = latest_terminal_by_mint_variant(read_state_jsonl(root / "state.jsonl"), args.window_mins)
    snapshot = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "window_mins": args.window_mins,
        "variant_metrics": [variant_metrics(state_rows, v) for v in ["Z", "Z3", "Z3.1"]],
        "fresh_metrics": load_fresh_metrics(dataset_dir, root),
    }
    previous = read_json(args.snapshot) or {}
    decision_doc = read_json(decision_path) or {}
    deltas = diff_metrics(snapshot, previous)
    alerts = build_alerts(snapshot, previous)
    snapshot["deltas"] = deltas
    snapshot["alerts"] = alerts
    snapshot["decision"] = decision_doc.get("decision")
    snapshot["live_allowed"] = decision_doc.get("live_allowed")

    report = format_report(snapshot, previous, decision_doc, deltas, alerts)
    Path(args.output_json).write_text(json.dumps(snapshot, indent=2, sort_keys=True) + "\n")
    Path(args.output_md).write_text(report)
    Path(args.snapshot).write_text(json.dumps(snapshot, indent=2, sort_keys=True) + "\n")
    print(report)


if __name__ == "__main__":
    try:
        main()
    except Exception as e:
        print(f"HURAGAN_MONITOR_ERROR: {e}", file=sys.stderr)
        sys.exit(1)
