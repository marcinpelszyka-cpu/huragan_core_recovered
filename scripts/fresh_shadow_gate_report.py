#!/usr/bin/env python3
"""Merge sniper-follow and bundler-funding signals into one shadow gate report.

Shadow-only. Does not touch runtime, wallets, services, or live config.
"""
import argparse
import json
from collections import Counter
from pathlib import Path

DEFAULT_SNIPER = "datasets/sniper_follow_signals.jsonl"
DEFAULT_BUNDLER = "datasets/fresh_bundle_risk_signals.jsonl"
DEFAULT_OUT = "datasets/fresh_shadow_gate_signals.jsonl"
DEFAULT_REPORT = "datasets/fresh_shadow_gate_report.md"


def read_jsonl(path):
    p = Path(path)
    if not p.exists():
        return []
    rows = []
    with p.open(errors="ignore") as f:
        for line in f:
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


def load_by_mint(path):
    out = {}
    for r in read_jsonl(path):
        mint = r.get("mint") or ""
        if mint:
            out[mint] = r
    return out


def repeated_bad_mother(bundler):
    for m in bundler.get("top_mother_wallets") or []:
        try:
            if int(m.get("bad_count") or 0) >= 2 and int(m.get("bad_count") or 0) >= int(m.get("good_count") or 0):
                return True
        except Exception:
            continue
    return False


def decision_for(sniper, bundler):
    sniper_passed = bool(sniper.get("passed") or sniper.get("signal") == "FOLLOW_SHADOW" or sniper.get("signal") is True)
    good_snipers = int(sniper.get("good_sniper_count") or sniper.get("good_flip_sniper_count") or 0)
    good_buy_sol = float(sniper.get("good_sniper_buy_sol") or sniper.get("good_flip_sniper_buy_sol") or sniper.get("total_good_sniper_buy_sol") or 0.0)

    cls = bundler.get("bundle_classification") or "UNKNOWN"
    risk = float(bundler.get("risk_score") or 0.0)
    follow = float(bundler.get("follow_score") or 0.0)
    shared = int(bundler.get("shared_mother_count") or 0)

    toxic_cluster = cls == "DEV_SNIPER_SUSPECT" or risk >= 70 or repeated_bad_mother(bundler)
    strong_sniper = sniper_passed and good_snipers >= 2 and good_buy_sol >= 0.03

    if toxic_cluster:
        return "AVOID_DEV_CLUSTER", "high_risk_or_repeated_bad_mother"
    if strong_sniper and follow >= 65 and risk < 45:
        return "FOLLOW_SHADOW_STRONG", "sniper_signal_plus_calibrated_low_risk_follow"
    if strong_sniper and follow >= 45 and risk < 60:
        return "FOLLOW_SHADOW_CANDIDATE", "sniper_signal_plus_moderate_follow_score"
    if shared >= 2 and risk < 60:
        return "UNKNOWN_WAIT", "shared_mother_cluster_needs_more_outcome_validation"
    return "UNKNOWN_WAIT", "insufficient_combined_signal"

def decision_v2(v1_decision: str, forward: dict) -> tuple[str, str]:
    """V2 decision: forward-confirmed strong signal.
    
    Returns (decision, reason).
    Demotes V1 strong/candidate if forward shows dump/rug/insufficient data.
    Promotes to V2 only if forward confirms positive outcome.
    """
    evaluated = bool(forward.get("evaluated"))
    label = forward.get("outcome_label", "not_evaluated")
    pnl_30s = float(forward.get("pnl_30s_pct") or 0.0)
    pnl_60s = float(forward.get("pnl_60s_pct") or 0.0)
    sell_flow_60s = float(forward.get("sell_flow_ratio_60s") or 0.0)
    
    # Forward dump/rug blocks any strong signal
    if label in ("hard_dump_30s", "hard_dump_60s", "rug_or_liquidity_collapse"):
        return "AVOID_FORWARD_DUMP", f"forward_outcome={label}"
    if sell_flow_60s >= 0.80:
        return "AVOID_FORWARD_DUMP", f"high_sell_flow_60s={sell_flow_60s:.2f}"
    
    # Insufficient data — can't confirm
    if label == "insufficient_price_data" or not evaluated:
        if v1_decision.startswith("FOLLOW_SHADOW"):
            return f"{v1_decision}_V1_FALLBACK", "forward_data_insufficient"
        return v1_decision, "forward_data_insufficient"
    
    # V1 strong → V2 if forward confirms
    if v1_decision == "FOLLOW_SHADOW_STRONG":
        if pnl_30s >= -10 and pnl_60s >= -20 and sell_flow_60s < 0.70:
            return "FOLLOW_SHADOW_STRONG_V2", f"forward_confirmed:pnl_30s={pnl_30s:.1f}%_pnl_60s={pnl_60s:.1f}%"
        else:
            return "FOLLOW_SHADOW_STRONG_V1_DEMOTED", f"forward_weak:pnl_30s={pnl_30s:.1f}%_pnl_60s={pnl_60s:.1f}%"
    
    if v1_decision == "FOLLOW_SHADOW_CANDIDATE":
        if pnl_30s >= -10 and pnl_60s >= -20:
            return "FOLLOW_SHADOW_CANDIDATE_V2", f"forward_confirmed:pnl_30s={pnl_30s:.1f}%_pnl_60s={pnl_60s:.1f}%"
        else:
            return "FOLLOW_SHADOW_CANDIDATE_V1_DEMOTED", f"forward_weak:pnl_30s={pnl_30s:.1f}%_pnl_60s={pnl_60s:.1f}%"
    
    return v1_decision, "no_forward_change"


def merge(sniper_by_mint, bundler_by_mint, forward_by_mint=None):
    rows = []
    if forward_by_mint is None:
        forward_by_mint = {}
    for mint in sorted(set(sniper_by_mint) | set(bundler_by_mint)):
        sniper = sniper_by_mint.get(mint, {})
        bundler = bundler_by_mint.get(mint, {})
        forward = forward_by_mint.get(mint, {})
        v1_decision, v1_reason = decision_for(sniper, bundler)
        v2_decision, v2_reason = decision_v2(v1_decision, forward)
        rows.append({
            "mint": mint,
            "v1_decision": v1_decision,
            "decision": v2_decision,
            "v1_reason": v1_reason,
            "reason": v2_reason,
            "forward_confirmed": bool(forward.get("evaluated")),
            "forward_outcome_label": forward.get("outcome_label", "not_evaluated"),
            "pnl_30s_pct": float(forward.get("pnl_30s_pct") or 0.0),
            "pnl_60s_pct": float(forward.get("pnl_60s_pct") or 0.0),
            "sell_flow_ratio_60s": float(forward.get("sell_flow_ratio_60s") or 0.0),
            "live_allowed": False,
            "sniper_signal": sniper.get("signal", "NO_SIGNAL"),
            "sniper_passed": bool(sniper.get("passed") or sniper.get("signal") == "FOLLOW_SHADOW" or sniper.get("signal") is True),
            "good_sniper_count": int(sniper.get("good_sniper_count") or 0),
            "good_flip_sniper_count": int(sniper.get("good_flip_sniper_count") or 0),
            "good_sniper_buy_sol": float(sniper.get("good_sniper_buy_sol") or 0.0),
            "good_flip_sniper_buy_sol": float(sniper.get("good_flip_sniper_buy_sol") or 0.0),
            "bundle_classification": bundler.get("bundle_classification", "UNKNOWN"),
            "early_buyer_count": int(bundler.get("early_buyer_count") or 0),
            "shared_mother_count": int(bundler.get("shared_mother_count") or 0),
            "top_mother_wallets": bundler.get("top_mother_wallets") or [],
            "bundle_score": float(bundler.get("bundle_score") or 0.0),
            "mother_score": float(bundler.get("mother_score") or 0.0),
            "risk_score": float(bundler.get("risk_score") or 0.0),
            "follow_score": float(bundler.get("follow_score") or 0.0),
        })
    return rows


def write_report(path, rows):
    p = Path(path)
    p.parent.mkdir(parents=True, exist_ok=True)
    decisions = Counter(r["decision"] for r in rows)
    v1_decisions = Counter(r.get("v1_decision", "") for r in rows)
    classes = Counter(r["bundle_classification"] for r in rows)
    with p.open("w") as f:
        f.write("# Fresh Shadow Gate Report — V2 Forward-Confirmed\n\n")
        f.write("Shadow-only combined decision from sniper-follow + bundler + forward outcomes.\\n\n")
        f.write(f"- mints: {len(rows)}\n\n")
        f.write("## V2 Decisions (forward-confirmed)\n\n")
        f.write(f"- FOLLOW_SHADOW_STRONG_V2: {decisions.get('FOLLOW_SHADOW_STRONG_V2', 0)}\n")
        f.write(f"- FOLLOW_SHADOW_CANDIDATE_V2: {decisions.get('FOLLOW_SHADOW_CANDIDATE_V2', 0)}\n")
        f.write(f"- FOLLOW_SHADOW_STRONG_V1_DEMOTED: {decisions.get('FOLLOW_SHADOW_STRONG_V1_DEMOTED', 0)}\n")
        f.write(f"- AVOID_FORWARD_DUMP: {decisions.get('AVOID_FORWARD_DUMP', 0)}\n")
        f.write(f"- AVOID_DEV_CLUSTER: {decisions.get('AVOID_DEV_CLUSTER', 0)}\n")
        f.write(f"- UNKNOWN_WAIT: {decisions.get('UNKNOWN_WAIT', 0)}\n")
        f.write("- live_allowed: false for all rows\n\n")
        f.write("## V1 → V2 comparison\n\n")
        v1_strong = v1_decisions.get("FOLLOW_SHADOW_STRONG", 0)
        v2_strong = decisions.get("FOLLOW_SHADOW_STRONG_V2", 0)
        f.write(f"- V1 STRONG: {v1_strong} → V2 STRONG: {v2_strong}\n")
        f.write(f"- Demoted by forward: {v1_strong - v2_strong}\n")
        f.write(f"- Blocked by forward dump/rug: {decisions.get('AVOID_FORWARD_DUMP', 0)}\n\n")
        f.write("## Bundle classes\n\n")
        f.write("| Class | Count |\n|---|---:|\n")
        for cls, n in classes.most_common():
            f.write(f"| {cls} | {n} |\n")
        f.write("\n## Top decisions\n\n")
        f.write("| Mint | Decision | Snipers | Bundle | Shared mothers | Risk | Follow | Reason |\n")
        f.write("|---|---|---:|---|---:|---:|---:|---|\n")
        order = {"FOLLOW_SHADOW_STRONG": 0, "FOLLOW_SHADOW_CANDIDATE": 1, "AVOID_DEV_CLUSTER": 2, "UNKNOWN_WAIT": 3}
        for r in sorted(rows, key=lambda x: (order.get(x["decision"], 9), -x["follow_score"], -x["risk_score"]))[:80]:
            snipers = max(r.get("good_sniper_count", 0), r.get("good_flip_sniper_count", 0))
            f.write(
                f"| {r['mint'][:12]}... | {r['decision']} | {snipers} | {r['bundle_classification']} | "
                f"{r['shared_mother_count']} | {r['risk_score']:.1f} | {r['follow_score']:.1f} | {r['reason']} |\n"
            )
        f.write("\n## Notes\n\n")
        f.write("- This is not a live gate. It is selection research only.\n")
        f.write("- If risk_score is zero-heavy, improve risk calibration before live.\n")


def self_test():
    # Test 1: V1 strong, no forward data → V1_FALLBACK
    sniper = {"M1": {"mint": "M1", "signal": "FOLLOW_SHADOW", "passed": True, "good_sniper_count": 2, "good_sniper_buy_sol": 0.04}}
    bundler = {"M1": {"mint": "M1", "bundle_classification": "GOOD_SNIPER_CLUSTER", "risk_score": 0, "follow_score": 70, "early_buyer_count": 3, "shared_mother_count": 0}}
    rows = merge(sniper, bundler)
    assert rows[0]["v1_decision"] == "FOLLOW_SHADOW_STRONG", rows[0]
    assert rows[0]["decision"] == "FOLLOW_SHADOW_STRONG_V1_FALLBACK", rows[0]["decision"]  # no forward data

    # Test 2: V1 strong + forward win → V2 STRONG
    forward = {"M1": {"evaluated": True, "outcome_label": "forward_win_30s", "pnl_30s_pct": 73.0, "pnl_60s_pct": 51.0, "sell_flow_ratio_60s": 0.39}}
    rows = merge(sniper, bundler, forward)
    assert rows[0]["decision"] == "FOLLOW_SHADOW_STRONG_V2", rows[0]["decision"]

    # Test 3: V1 strong + forward dump → AVOID_FORWARD_DUMP
    forward_dump = {"M1": {"evaluated": True, "outcome_label": "hard_dump_60s", "pnl_30s_pct": -50.0, "pnl_60s_pct": -80.0, "sell_flow_ratio_60s": 0.95}}
    rows = merge(sniper, bundler, forward_dump)
    assert rows[0]["decision"] == "AVOID_FORWARD_DUMP", rows[0]["decision"]

    # Test 4: Dev cluster still blocked
    bundler["M1"]["bundle_classification"] = "DEV_SNIPER_SUSPECT"
    bundler["M1"]["risk_score"] = 80
    rows = merge(sniper, bundler)
    assert rows[0]["v1_decision"] == "AVOID_DEV_CLUSTER", rows[0]["v1_decision"]

    print("SELF_TEST_OK")


def main():
    ap = argparse.ArgumentParser(description="Merge fresh sniper and bundler funding signals into shadow gate decisions (V2 forward-confirmed).")
    ap.add_argument("--sniper", default=DEFAULT_SNIPER)
    ap.add_argument("--bundler", default=DEFAULT_BUNDLER)
    ap.add_argument("--forward", default="datasets/fresh_forward_outcomes.jsonl", help="Forward outcome labels (V2 enrichment)")
    ap.add_argument("--out", default=DEFAULT_OUT)
    ap.add_argument("--report", default=DEFAULT_REPORT)
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()
    if args.self_test:
        self_test()
        return
    forward_by_mint = load_by_mint(args.forward) if Path(args.forward).exists() else {}
    rows = merge(load_by_mint(args.sniper), load_by_mint(args.bundler), forward_by_mint)
    write_jsonl(args.out, rows)
    write_report(args.report, rows)
    print(json.dumps({
        "mints": len(rows),
        "decisions": dict(Counter(r["decision"] for r in rows)),
        "out": args.out,
        "report": args.report,
        "live_allowed": False,
    }, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
