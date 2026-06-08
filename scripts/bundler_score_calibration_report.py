#!/usr/bin/env python3
"""Report calibration quality for GTFA bundler/funding risk scores.

Shadow-only. Reads local datasets and writes report artifacts; never touches
runtime, wallets, services, or live config.
"""
import argparse
import json
from collections import Counter, defaultdict
from pathlib import Path

DEFAULT_SIGNALS = "datasets/fresh_bundle_risk_signals.jsonl"
DEFAULT_EDGES = "datasets/bundler_wallet_edges.jsonl"
DEFAULT_SNIPER = "datasets/sniper_follow_signals.jsonl"
DEFAULT_STATE = "state.jsonl"
DEFAULT_REPORT = "datasets/bundler_score_calibration_report.md"
DEFAULT_SUMMARY = "datasets/bundler_score_calibration_summary.json"


def fnum(v, default=0.0):
    try:
        return float(v) if v not in (None, "") else default
    except Exception:
        return default


def read_jsonl(path):
    p = Path(path)
    if not p.exists():
        return []
    rows = []
    with p.open(errors="ignore") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                rows.append(json.loads(line))
            except Exception:
                continue
    return rows


def write_json(path, row):
    p = Path(path)
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(row, indent=2, sort_keys=True) + "\n")


def bucket(v):
    v = fnum(v)
    if v < 20:
        return "00-20"
    if v < 40:
        return "20-40"
    if v < 60:
        return "40-60"
    if v < 80:
        return "60-80"
    return "80-100"


def load_outcomes(path):
    latest = {}
    for r in read_jsonl(path):
        mint = r.get("mint") or ""
        if mint:
            latest[mint] = r
    out = {}
    for mint, r in latest.items():
        reason = r.get("exit_reason") or r.get("live_exit_reason") or ""
        status = r.get("status") or ""
        pnl = fnum(r.get("realized_pnl_sol") or r.get("net_pnl_sol"), 0.0)
        bad = status == "unrecoverable_dust_or_rug" or reason in {"hard_stop", "rug_guard", "price_unavailable"} or "dust_or_rug" in reason or pnl < -0.0005
        good = pnl > 0.00005
        out[mint] = {"bad": bad, "good": good, "pnl": pnl, "exit_reason": reason, "status": status}
    return out


def enrich(signals, outcomes):
    rows = []
    for s in signals:
        mint = s.get("mint") or ""
        o = outcomes.get(mint, {})
        r = dict(s)
        r["outcome_bad"] = bool(s.get("bad_outcome") or o.get("bad"))
        r["outcome_good"] = bool(s.get("good_outcome") or o.get("good"))
        r["outcome_pnl"] = fnum(s.get("realized_pnl_sol"), fnum(o.get("pnl"), 0.0))
        r["outcome_reason"] = s.get("exit_reason") or o.get("exit_reason", "")
        rows.append(r)
    return rows


def rate_stats(rows, pred):
    sub = [r for r in rows if pred(r)]
    n = len(sub)
    bad = sum(1 for r in sub if r.get("outcome_bad"))
    good = sum(1 for r in sub if r.get("outcome_good"))
    pnl = sum(fnum(r.get("outcome_pnl"), 0.0) for r in sub)
    return {"count": n, "bad": bad, "good": good, "bad_rate": round(bad / max(1, n), 4), "good_rate": round(good / max(1, n), 4), "pnl_sum": round(pnl, 9)}


def mother_tables(edges, outcomes):
    by_mother = defaultdict(set)
    for e in edges:
        m = e.get("mother_wallet") or ""
        mint = e.get("mint") or ""
        if m and mint:
            by_mother[m].add(mint)
    rows = []
    for mother, mints in by_mother.items():
        bad = sum(1 for mint in mints if outcomes.get(mint, {}).get("bad"))
        good = sum(1 for mint in mints if outcomes.get(mint, {}).get("good"))
        pnl = sum(fnum(outcomes.get(mint, {}).get("pnl"), 0.0) for mint in mints)
        rows.append({"mother_wallet": mother, "mint_count": len(mints), "bad_count": bad, "good_count": good, "pnl_sum": round(pnl, 9)})
    bad_top = sorted(rows, key=lambda r: (-r["bad_count"], -r["mint_count"], r["pnl_sum"]))[:15]
    good_top = sorted(rows, key=lambda r: (-r["good_count"], -r["pnl_sum"], -r["mint_count"]))[:15]
    return bad_top, good_top


def write_report(path, summary, bad_mothers, good_mothers):
    p = Path(path)
    p.parent.mkdir(parents=True, exist_ok=True)
    with p.open("w") as f:
        f.write("# Bundler Score Calibration Report\n\n")
        f.write("Shadow-only GTFA risk/follow calibration. No live permission.\n\n")
        f.write(f"- signals: {summary['signals']}\n")
        f.write(f"- edges: {summary['edges']}\n")
        f.write(f"- live_allowed: false\n\n")
        f.write("## Risk buckets\n\n| Bucket | Count | Bad | Bad rate | Good | Good rate | PnL sum |\n|---|---:|---:|---:|---:|---:|---:|\n")
        for b, s in summary["risk_buckets"].items():
            f.write(f"| {b} | {s['count']} | {s['bad']} | {s['bad_rate']:.2%} | {s['good']} | {s['good_rate']:.2%} | {s['pnl_sum']:.9f} |\n")
        f.write("\n## Bundle classes\n\n| Class | Count | Bad rate | Good rate | PnL sum |\n|---|---:|---:|---:|---:|\n")
        for cls, s in summary["classes"].items():
            f.write(f"| {cls} | {s['count']} | {s['bad_rate']:.2%} | {s['good_rate']:.2%} | {s['pnl_sum']:.9f} |\n")
        f.write("\n## Shadow decision precision proxy\n\n| Decision | Count | Bad rate | Good rate | PnL sum |\n|---|---:|---:|---:|---:|\n")
        for dec, s in summary["decision_proxy"].items():
            f.write(f"| {dec} | {s['count']} | {s['bad_rate']:.2%} | {s['good_rate']:.2%} | {s['pnl_sum']:.9f} |\n")
        f.write("\n## Top repeated bad mother wallets\n\n| Mother | Mints | Bad | Good | PnL sum |\n|---|---:|---:|---:|---:|\n")
        for r in bad_mothers:
            f.write(f"| {r['mother_wallet'][:12]}... | {r['mint_count']} | {r['bad_count']} | {r['good_count']} | {r['pnl_sum']:.9f} |\n")
        f.write("\n## Top repeated good mother wallets\n\n| Mother | Mints | Bad | Good | PnL sum |\n|---|---:|---:|---:|---:|\n")
        for r in good_mothers:
            f.write(f"| {r['mother_wallet'][:12]}... | {r['mint_count']} | {r['bad_count']} | {r['good_count']} | {r['pnl_sum']:.9f} |\n")
        f.write("\n## Notes\n\n- Wallet API is not required; GTFA is the source of truth for this report.\n- Use this as selection research only; it does not authorize canary/live.\n")


def main():
    ap = argparse.ArgumentParser(description="Build GTFA bundler score calibration report.")
    ap.add_argument("--signals", default=DEFAULT_SIGNALS)
    ap.add_argument("--edges", default=DEFAULT_EDGES)
    ap.add_argument("--sniper", default=DEFAULT_SNIPER)
    ap.add_argument("--state", default=DEFAULT_STATE)
    ap.add_argument("--report", default=DEFAULT_REPORT)
    ap.add_argument("--summary", default=DEFAULT_SUMMARY)
    args = ap.parse_args()

    signals = read_jsonl(args.signals)
    edges = read_jsonl(args.edges)
    outcomes = load_outcomes(args.state)
    rows = enrich(signals, outcomes)

    risk_buckets = {b: rate_stats(rows, lambda r, b=b: bucket(r.get("risk_score")) == b) for b in ["00-20", "20-40", "40-60", "60-80", "80-100"]}
    classes = {cls: rate_stats(rows, lambda r, cls=cls: (r.get("bundle_classification") or "UNKNOWN") == cls) for cls in sorted(set(r.get("bundle_classification") or "UNKNOWN" for r in rows))}
    decision_proxy = {
        "avoid_dev_cluster": rate_stats(rows, lambda r: fnum(r.get("risk_score")) >= 70 or r.get("bundle_classification") == "DEV_SNIPER_SUSPECT"),
        "follow_strong_candidate": rate_stats(rows, lambda r: fnum(r.get("follow_score")) >= 65 and fnum(r.get("risk_score")) < 45),
        "follow_candidate": rate_stats(rows, lambda r: fnum(r.get("follow_score")) >= 45 and fnum(r.get("risk_score")) < 60),
    }
    bad_mothers, good_mothers = mother_tables(edges, outcomes)
    summary = {
        "signals": len(rows),
        "edges": len(edges),
        "risk_buckets": risk_buckets,
        "classes": classes,
        "decision_proxy": decision_proxy,
        "top_bad_mothers": bad_mothers,
        "top_good_mothers": good_mothers,
        "live_allowed": False,
    }
    write_json(args.summary, summary)
    write_report(args.report, summary, bad_mothers, good_mothers)
    print(json.dumps({"signals": len(rows), "edges": len(edges), "report": args.report, "summary": args.summary, "live_allowed": False}, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
