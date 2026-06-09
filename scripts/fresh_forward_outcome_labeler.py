#!/usr/bin/env python3
"""Fresh shadow forward outcome labeler.

Shadow-only: reads local datasets, labels 30s/60s forward outcomes for fresh
shadow gate signals, and writes report artifacts. Never touches runtime, wallets,
services, or live config.
"""
import argparse
import json
import statistics
from collections import Counter, defaultdict
from pathlib import Path

DEFAULT_GATE = "datasets/fresh_shadow_gate_signals.jsonl"
DEFAULT_EVENTS = "datasets/sniper_trade_events.jsonl"
DEFAULT_BUNDLER = "datasets/fresh_bundle_risk_signals.jsonl"
DEFAULT_SNIPER = "datasets/sniper_follow_signals.jsonl"
DEFAULT_OUT = "datasets/fresh_forward_outcomes.jsonl"
DEFAULT_REPORT = "datasets/fresh_forward_outcome_report.md"
DEFAULT_SUMMARY = "datasets/fresh_forward_outcome_summary.json"
EVALUATED_DECISIONS = {"FOLLOW_SHADOW_STRONG", "FOLLOW_SHADOW_CANDIDATE"}


def fnum(v, default=0.0):
    try:
        if v is None or v == "":
            return default
        return float(v)
    except Exception:
        return default


def inum(v, default=0):
    try:
        if v is None or v == "":
            return default
        return int(float(v))
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


def write_jsonl(path, rows):
    p = Path(path)
    p.parent.mkdir(parents=True, exist_ok=True)
    with p.open("w") as f:
        for r in rows:
            f.write(json.dumps(r, separators=(",", ":"), ensure_ascii=False) + "\n")


def write_json(path, row):
    p = Path(path)
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(row, indent=2, sort_keys=True) + "\n")


def by_mint(rows):
    out = defaultdict(list)
    for r in rows:
        mint = r.get("mint") or ""
        if mint:
            out[mint].append(r)
    return out


def latest_by_mint(rows):
    out = {}
    for r in rows:
        mint = r.get("mint") or ""
        if mint:
            out[mint] = r
    return out


def event_time(e):
    return inum(e.get("timestamp") or e.get("block_time") or e.get("blockTime"), 0)


def event_age(e, signal_time):
    if e.get("age_secs") not in (None, ""):
        return inum(e.get("age_secs"), 0)
    t = event_time(e)
    return max(0, t - signal_time) if t and signal_time else 0


def implied_price(e):
    quote = fnum(e.get("quote_delta_sol") or e.get("buy_sol") or e.get("sell_sol"), 0.0)
    token = fnum(e.get("token_delta_raw") or e.get("token_amount") or e.get("amount"), 0.0)
    if quote <= 0 or token <= 0:
        return 0.0
    return quote / token


def weighted_buy_price(events, signal_time, entry_window_secs=10):
    buys = [
        e for e in events
        if (e.get("side") == "buy") and event_age(e, signal_time) <= entry_window_secs and fnum(e.get("quote_delta_sol"), 0.0) > 0 and fnum(e.get("token_delta_raw"), 0.0) > 0
    ]
    total_quote = sum(fnum(e.get("quote_delta_sol"), 0.0) for e in buys)
    total_token = sum(fnum(e.get("token_delta_raw"), 0.0) for e in buys)
    if total_quote <= 0 or total_token <= 0:
        return 0.0, len(buys)
    return total_quote / total_token, len(buys)


def last_price_in_window(events, signal_time, window_secs):
    candidates = []
    for e in events:
        age = event_age(e, signal_time)
        if age < 0 or age > window_secs:
            continue
        p = implied_price(e)
        if p > 0:
            candidates.append((age, event_time(e), p, e.get("side", "")))
    if not candidates:
        return 0.0, ""
    candidates.sort(key=lambda x: (x[0], x[1]))
    return candidates[-1][2], candidates[-1][3]


def sell_flow_ratio(events, signal_time, window_secs):
    buys = sells = 0.0
    for e in events:
        age = event_age(e, signal_time)
        if age < 0 or age > window_secs:
            continue
        q = fnum(e.get("quote_delta_sol"), 0.0)
        if e.get("side") == "buy":
            buys += q
        elif e.get("side") == "sell":
            sells += q
    denom = max(1e-12, buys + sells)
    return sells / denom, buys, sells


def label_from_pnl(pnl30, pnl60, sell_ratio60, event_count):
    if event_count <= 0:
        return "no_trade_data"
    if pnl30 is None and pnl60 is None:
        return "insufficient_price_data"
    vals = [v for v in [pnl30, pnl60] if v is not None]
    worst = min(vals) if vals else None
    best = max(vals) if vals else None
    if worst is not None and (worst <= -80.0 or (worst <= -60.0 and sell_ratio60 >= 0.8)):
        return "rug_or_liquidity_collapse"
    if pnl30 is not None and pnl30 <= -40.0:
        return "hard_dump_30s"
    if pnl60 is not None and pnl60 <= -40.0:
        return "hard_dump_60s"
    if pnl30 is not None and pnl30 >= 25.0:
        return "forward_win_30s"
    if pnl60 is not None and pnl60 >= 25.0:
        return "forward_win_60s"
    if best is not None and worst is not None and worst > -20.0 and best < 25.0:
        return "flat_or_noise"
    return "insufficient_price_data"


def evaluate_signal(gate_row, events, bundler, sniper, entry_window_secs=10):
    mint = gate_row.get("mint") or ""
    decision = gate_row.get("decision") or "UNKNOWN_WAIT"
    out = {
        "mint": mint,
        "decision": decision,
        "live_allowed": False,
        "bundle_classification": gate_row.get("bundle_classification") or bundler.get("bundle_classification") or "UNKNOWN",
        "risk_score": fnum(gate_row.get("risk_score"), fnum(bundler.get("risk_score"), 0.0)),
        "follow_score": fnum(gate_row.get("follow_score"), fnum(bundler.get("follow_score"), 0.0)),
        "good_sniper_count": inum(gate_row.get("good_sniper_count"), inum(sniper.get("good_sniper_count"), 0)),
        "good_flip_sniper_count": inum(gate_row.get("good_flip_sniper_count"), inum(sniper.get("good_flip_sniper_count"), 0)),
    }
    if decision not in EVALUATED_DECISIONS:
        out.update({"outcome_label": "not_evaluated", "evaluated": False})
        return out

    events = sorted(events, key=lambda e: (event_time(e), e.get("signature", "")))
    if not events:
        out.update({"outcome_label": "no_trade_data", "evaluated": True, "event_count": 0})
        return out

    signal_time = min((event_time(e) for e in events if event_time(e)), default=0)
    entry_price, entry_trades = weighted_buy_price(events, signal_time, entry_window_secs=entry_window_secs)
    p30, side30 = last_price_in_window(events, signal_time, 30)
    p60, side60 = last_price_in_window(events, signal_time, 60)
    sell_ratio60, buy_sol60, sell_sol60 = sell_flow_ratio(events, signal_time, 60)

    pnl30 = ((p30 - entry_price) / entry_price * 100.0) if entry_price > 0 and p30 > 0 else None
    pnl60 = ((p60 - entry_price) / entry_price * 100.0) if entry_price > 0 and p60 > 0 else None
    label = label_from_pnl(pnl30, pnl60, sell_ratio60, len(events)) if entry_price > 0 else "insufficient_price_data"
    out.update({
        "evaluated": True,
        "outcome_label": label,
        "signal_time": signal_time,
        "event_count": len(events),
        "entry_trade_count": entry_trades,
        "entry_price_proxy": entry_price,
        "price_30s": p30,
        "price_60s": p60,
        "price_30s_side": side30,
        "price_60s_side": side60,
        "pnl_30s_pct": None if pnl30 is None else round(pnl30, 6),
        "pnl_60s_pct": None if pnl60 is None else round(pnl60, 6),
        "buy_sol_60s": round(buy_sol60, 12),
        "sell_sol_60s": round(sell_sol60, 12),
        "sell_flow_ratio_60s": round(sell_ratio60, 6),
    })
    return out


def risk_bucket(v):
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


def stats(rows):
    n = len(rows)
    labels = Counter(r.get("outcome_label") for r in rows)
    pnl30 = [fnum(r.get("pnl_30s_pct")) for r in rows if r.get("pnl_30s_pct") is not None]
    pnl60 = [fnum(r.get("pnl_60s_pct")) for r in rows if r.get("pnl_60s_pct") is not None]
    wins = sum(labels.get(x, 0) for x in ["forward_win_30s", "forward_win_60s"])
    bad = sum(labels.get(x, 0) for x in ["hard_dump_30s", "hard_dump_60s", "rug_or_liquidity_collapse"])
    return {
        "count": n,
        "labels": dict(labels),
        "win_rate": round(wins / max(1, n), 4),
        "bad_rate": round(bad / max(1, n), 4),
        "avg_pnl_30s_pct": round(sum(pnl30) / len(pnl30), 6) if pnl30 else None,
        "median_pnl_30s_pct": round(statistics.median(pnl30), 6) if pnl30 else None,
        "avg_pnl_60s_pct": round(sum(pnl60) / len(pnl60), 6) if pnl60 else None,
        "median_pnl_60s_pct": round(statistics.median(pnl60), 6) if pnl60 else None,
    }


def write_report(path, rows, summary):
    p = Path(path)
    p.parent.mkdir(parents=True, exist_ok=True)
    with p.open("w") as f:
        f.write("# Fresh Forward Outcome Report\n\n")
        f.write("Shadow-only forward labels for fresh shadow gate decisions.\n\n")
        f.write(f"- rows: {summary['rows']}\n")
        f.write(f"- evaluated: {summary['evaluated']}\n")
        f.write(f"- live_allowed: false\n\n")
        f.write("## Outcomes by decision\n\n| Decision | Count | Win rate | Bad rate | Avg 30s | Median 30s | Avg 60s | Median 60s |\n|---|---:|---:|---:|---:|---:|---:|---:|\n")
        for dec, s in summary["by_decision"].items():
            f.write(f"| {dec} | {s['count']} | {s['win_rate']:.2%} | {s['bad_rate']:.2%} | {s['avg_pnl_30s_pct']} | {s['median_pnl_30s_pct']} | {s['avg_pnl_60s_pct']} | {s['median_pnl_60s_pct']} |\n")
        f.write("\n## Outcomes by bundle class\n\n| Class | Count | Win rate | Bad rate | Labels |\n|---|---:|---:|---:|---|\n")
        for cls, s in summary["by_bundle_class"].items():
            f.write(f"| {cls} | {s['count']} | {s['win_rate']:.2%} | {s['bad_rate']:.2%} | {json.dumps(s['labels'], sort_keys=True)} |\n")
        f.write("\n## Outcomes by risk bucket\n\n| Bucket | Count | Win rate | Bad rate | Labels |\n|---|---:|---:|---:|---|\n")
        for b, s in summary["by_risk_bucket"].items():
            f.write(f"| {b} | {s['count']} | {s['win_rate']:.2%} | {s['bad_rate']:.2%} | {json.dumps(s['labels'], sort_keys=True)} |\n")
        f.write("\n## Top FOLLOW_SHADOW_STRONG rows\n\n| Mint | Label | PnL 30s | PnL 60s | Risk | Follow | Events |\n|---|---|---:|---:|---:|---:|---:|\n")
        strong = [r for r in rows if r.get("decision") == "FOLLOW_SHADOW_STRONG"]
        for r in sorted(strong, key=lambda x: (x.get("pnl_60s_pct") is None, -(x.get("pnl_60s_pct") or -999999)))[:40]:
            f.write(f"| {r['mint'][:12]}... | {r.get('outcome_label')} | {r.get('pnl_30s_pct')} | {r.get('pnl_60s_pct')} | {r.get('risk_score'):.1f} | {r.get('follow_score'):.1f} | {r.get('event_count', 0)} |\n")
        f.write("\n## Notes\n\n- Labels are proxy labels from GTFA/sniper trade events, not live execution results.\n- no_trade_data and insufficient_price_data are not counted as losses.\n- This report does not authorize canary/live.\n")


def summarize(rows):
    evaluated = [r for r in rows if r.get("evaluated")]
    by_decision = {k: stats([r for r in rows if r.get("decision") == k]) for k in sorted(set(r.get("decision") for r in rows))}
    by_bundle = {k: stats([r for r in rows if r.get("bundle_classification") == k]) for k in sorted(set(r.get("bundle_classification") for r in rows))}
    buckets = {b: stats([r for r in rows if risk_bucket(r.get("risk_score")) == b]) for b in ["00-20", "20-40", "40-60", "60-80", "80-100"]}
    return {
        "rows": len(rows),
        "evaluated": len(evaluated),
        "labels": dict(Counter(r.get("outcome_label") for r in rows)),
        "by_decision": by_decision,
        "by_bundle_class": by_bundle,
        "by_risk_bucket": buckets,
        "live_allowed": False,
    }


def run(args):
    gate = read_jsonl(args.gate)
    events_by_mint = by_mint(read_jsonl(args.events))
    bundler = latest_by_mint(read_jsonl(args.bundler))
    sniper = latest_by_mint(read_jsonl(args.sniper))
    rows = [
        evaluate_signal(g, events_by_mint.get(g.get("mint") or "", []), bundler.get(g.get("mint") or "", {}), sniper.get(g.get("mint") or "", {}), entry_window_secs=args.entry_window_secs)
        for g in gate
    ]
    summary = summarize(rows)
    write_jsonl(args.out, rows)
    write_json(args.summary, summary)
    write_report(args.report, rows, summary)
    return {"rows": len(rows), "evaluated": summary["evaluated"], "labels": summary["labels"], "out": args.out, "report": args.report, "summary": args.summary, "live_allowed": False}


def self_test():
    events = [
        {"mint": "M1", "timestamp": 100, "age_secs": 0, "side": "buy", "quote_delta_sol": 1.0, "token_delta_raw": 100.0},
        {"mint": "M1", "timestamp": 130, "age_secs": 30, "side": "buy", "quote_delta_sol": 1.5, "token_delta_raw": 100.0},
        {"mint": "M1", "timestamp": 160, "age_secs": 60, "side": "sell", "quote_delta_sol": 1.6, "token_delta_raw": 100.0},
    ]
    gate = {"mint": "M1", "decision": "FOLLOW_SHADOW_STRONG", "bundle_classification": "GOOD_SNIPER_CLUSTER", "risk_score": 10, "follow_score": 80}
    row = evaluate_signal(gate, events, {}, {})
    assert row["outcome_label"] == "forward_win_30s", row
    bad_events = [
        {"mint": "M2", "timestamp": 100, "age_secs": 0, "side": "buy", "quote_delta_sol": 1.0, "token_delta_raw": 100.0},
        {"mint": "M2", "timestamp": 130, "age_secs": 30, "side": "sell", "quote_delta_sol": 0.1, "token_delta_raw": 100.0},
    ]
    bad = evaluate_signal({"mint": "M2", "decision": "FOLLOW_SHADOW_CANDIDATE"}, bad_events, {}, {})
    assert bad["outcome_label"] == "rug_or_liquidity_collapse", bad
    skip = evaluate_signal({"mint": "M3", "decision": "UNKNOWN_WAIT"}, [], {}, {})
    assert skip["outcome_label"] == "not_evaluated", skip
    print("SELF_TEST_OK")


def main():
    ap = argparse.ArgumentParser(description="Label forward outcomes for fresh shadow gate signals. Shadow-only.")
    ap.add_argument("--gate", default=DEFAULT_GATE)
    ap.add_argument("--events", default=DEFAULT_EVENTS)
    ap.add_argument("--bundler", default=DEFAULT_BUNDLER)
    ap.add_argument("--sniper", default=DEFAULT_SNIPER)
    ap.add_argument("--out", default=DEFAULT_OUT)
    ap.add_argument("--report", default=DEFAULT_REPORT)
    ap.add_argument("--summary", default=DEFAULT_SUMMARY)
    ap.add_argument("--entry-window-secs", type=int, default=10)
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()
    if args.self_test:
        self_test()
        return
    print(json.dumps(run(args), indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
