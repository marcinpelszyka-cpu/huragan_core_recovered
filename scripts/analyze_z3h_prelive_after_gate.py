#!/usr/bin/env python3
"""Analyze Z3H pre-live safety gate results from state.jsonl.

Read-only against runtime/config. Writes JSON/Markdown reports unless --no-write.
"""
import argparse
import json
import math
from collections import Counter, defaultdict
from datetime import datetime, timezone
from pathlib import Path
from statistics import mean, median

TIMESTAMP_FIELDS = ["completed_at", "captured_at", "updated_at", "created_at", "observed_at", "timestamp"]
DEFAULT_VARIANTS = ["Z3", "Z3.1", "Z3H_SHADOW", "Z3H_V2_SHADOW"]
DEFAULT_Z3H_VARIANTS = ["Z3H_SHADOW", "Z3H_V2_SHADOW"]
EXCLUDED_EXIT_REASONS = {"price_unavailable", "invalid_quote", "data_quality_fail"}


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
        return int(v)
    except Exception:
        try:
            return int(float(v))
        except Exception:
            return default


def pct(part, total):
    return 100.0 * part / total if total else 0.0


def stats(vals):
    vals = [fnum(v) for v in vals if v is not None]
    if not vals:
        return {"n": 0, "avg": 0.0, "median": 0.0, "p25": 0.0, "p75": 0.0, "min": 0.0, "max": 0.0}
    vals = sorted(vals)
    def q(qv):
        if not vals:
            return 0.0
        idx = int(round((len(vals) - 1) * qv))
        return vals[max(0, min(len(vals)-1, idx))]
    return {
        "n": len(vals),
        "avg": mean(vals),
        "median": median(vals),
        "p25": q(0.25),
        "p75": q(0.75),
        "min": vals[0],
        "max": vals[-1],
    }


def mdd_sol(rows):
    equity = 0.0
    peak = 0.0
    mdd = 0.0
    for r in rows:
        equity += fnum(r.get("net_pnl_sol"))
        peak = max(peak, equity)
        mdd = max(mdd, peak - equity)
    return mdd


def reason_family(reason):
    reason = (reason or "").strip()
    if not reason:
        return "empty"
    if reason == "would_trade":
        return "would_trade"
    return reason.split(":", 1)[0]


def is_gate_decision_row(r):
    br = (r.get("prelive_blocked_reason") or "").strip()
    minr = fnum(r.get("prelive_min_quote_reserve_ui"))
    return r.get("status") == "prelive_liquidity_shadow" or bool(br) or minr > 0


def is_clean_completed(r):
    if r.get("status") != "paper_completed":
        return False
    if r.get("excluded_from_stats") is True:
        return False
    if fnum(r.get("paper_entry_sol")) <= 0:
        return False
    if r.get("prelive_would_trade") is not True:
        return False
    if (r.get("exit_reason") or "") in EXCLUDED_EXIT_REASONS:
        return False
    if abs(fnum(r.get("net_pnl_pct"))) > 300:
        return False
    if fnum(r.get("max_favorable_pct")) > 200:
        return False
    return True


def metric_block(rows):
    rows = list(rows)
    pnls = [fnum(r.get("net_pnl_pct")) for r in rows]
    mfes = [fnum(r.get("max_favorable_pct")) for r in rows]
    dds = [fnum(r.get("max_drawdown_pct")) for r in rows]
    holds = [fnum(r.get("hold_secs")) for r in rows]
    return {
        "n": len(rows),
        "wr_pct": pct(sum(1 for x in pnls if x > 0), len(pnls)),
        "avg_pnl_pct": mean(pnls) if pnls else 0.0,
        "median_pnl_pct": median(pnls) if pnls else 0.0,
        "p25_pnl_pct": stats(pnls)["p25"],
        "p75_pnl_pct": stats(pnls)["p75"],
        "total_sol": sum(fnum(r.get("net_pnl_sol")) for r in rows),
        "mdd_sol": mdd_sol(rows),
        "avg_mfe_pct": mean(mfes) if mfes else 0.0,
        "median_mfe_pct": median(mfes) if mfes else 0.0,
        "avg_drawdown_pct": mean(dds) if dds else 0.0,
        "median_hold_secs": median(holds) if holds else 0.0,
    }


def load_rows(path):
    rows = []
    total = 0
    with Path(path).open() as f:
        for line_no, line in enumerate(f, 1):
            total = line_no
            try:
                r = json.loads(line)
            except Exception:
                continue
            r["_line"] = line_no
            rows.append(r)
    return rows, total


def detect_since_line(rows):
    for r in rows:
        if is_gate_decision_row(r):
            return r["_line"]
    return None


def filter_rows(rows, since_line):
    if since_line:
        return [r for r in rows if r["_line"] >= since_line]
    return rows


def top_examples(rows, key, n=5):
    c = Counter((r.get(key) or "") for r in rows)
    return [{"reason": k, "count": v} for k, v in c.most_common(n)]


def build_report(args):
    rows, total_lines = load_rows(args.state)
    warnings = []
    mode = "all"
    since_line = args.since_line
    auto_line = detect_since_line(rows)

    timestamp_rows_seen = sum(1 for r in rows if any(r.get(f) for f in TIMESTAMP_FIELDS))
    if args.activated_at and timestamp_rows_seen == 0:
        warnings.append("state rows have no timestamp fields; activated_at ignored, using auto/since-line fallback")
        mode = "timestamp_unavailable_fallback_line"
    if since_line:
        mode = "since_line"
    elif args.auto_since_gate and auto_line:
        since_line = auto_line
        mode = "auto_since_gate"
    elif args.activated_at:
        mode = "activated_at_unimplemented_no_timestamps"

    after = filter_rows(rows, since_line)
    gate_rows = [r for r in after if is_gate_decision_row(r)]
    blocked = [r for r in gate_rows if r.get("prelive_would_trade") is False and r.get("status") == "prelive_liquidity_shadow"]
    would = [r for r in gate_rows if r.get("prelive_would_trade") is True and (r.get("prelive_blocked_reason") or "") == "would_trade"]

    unique_seen = {r.get("mint") for r in gate_rows if r.get("mint")}
    unique_blocked = {r.get("mint") for r in blocked if r.get("mint")}
    unique_would = {r.get("mint") for r in would if r.get("mint")}

    blocked_by_family = defaultdict(list)
    for r in blocked:
        blocked_by_family[reason_family(r.get("prelive_blocked_reason") or r.get("exit_reason"))].append(r)
    blocked_breakdown = []
    for fam, rs in sorted(blocked_by_family.items(), key=lambda kv: len(kv[1]), reverse=True):
        blocked_breakdown.append({
            "reason_family": fam,
            "rows": len(rs),
            "unique_mints": len({r.get("mint") for r in rs if r.get("mint")}),
            "pct_of_blocked_rows": pct(len(rs), len(blocked)),
            "quote_reserve_ui": stats([r.get("quote_reserve_ui") for r in rs]),
            "raw_examples": top_examples(rs, "prelive_blocked_reason"),
        })

    lifecycle_counts = []
    for v in args.variants:
        vr = [r for r in after if r.get("variant_id") == v and r.get("prelive_would_trade") is True]
        c = Counter(r.get("status") for r in vr)
        lifecycle_counts.append({
            "variant": v,
            "paper_entry": c.get("paper_entry", 0),
            "paper_completed": c.get("paper_completed", 0),
            "paper_partial_sold": c.get("paper_partial_sold", 0),
            "paper_lifecycle_orphaned_restart": c.get("paper_lifecycle_orphaned_restart", 0),
            "clean_completed": sum(1 for r in vr if is_clean_completed(r)),
            "price_unavailable": sum(1 for r in vr if r.get("exit_reason") == "price_unavailable"),
            "invalid_quote": sum(1 for r in vr if r.get("exit_reason") == "invalid_quote"),
        })

    z3h_metrics = []
    mode_split = []
    for v in args.z3h_variants:
        clean = [r for r in after if r.get("variant_id") == v and is_clean_completed(r)]
        mb = metric_block(clean)
        mb["variant"] = v
        z3h_metrics.append(mb)
        by_mode = defaultdict(list)
        for r in clean:
            by_mode[(r.get("z3h_selected_mode") or "unknown")].append(r)
        for mode_name, rs in sorted(by_mode.items()):
            b = metric_block(rs)
            b.update({
                "variant": v,
                "mode": mode_name,
                "avg_hold_secs": mean([fnum(r.get("hold_secs")) for r in rs]) if rs else 0.0,
                "exit_reasons": [{"reason": k, "count": c} for k, c in Counter(r.get("exit_reason") or "" for r in rs).most_common()],
            })
            mode_split.append(b)

    clean_by_variant_mint = defaultdict(dict)
    for r in after:
        if r.get("variant_id") in args.variants and is_clean_completed(r):
            clean_by_variant_mint[r.get("mint")][r.get("variant_id")] = r
    paired_mints = [m for m, d in clean_by_variant_mint.items() if all(v in d for v in args.variants)]
    paired_variants = []
    baseline = {}
    for v in args.variants:
        rs = [clean_by_variant_mint[m][v] for m in paired_mints]
        b = metric_block(rs)
        if v == "Z3.1":
            baseline = b
        b["variant"] = v
        paired_variants.append(b)
    for b in paired_variants:
        b["delta_vs_z31"] = {
            "avg_pnl_pct": b["avg_pnl_pct"] - baseline.get("avg_pnl_pct", 0.0),
            "median_pnl_pct": b["median_pnl_pct"] - baseline.get("median_pnl_pct", 0.0),
            "wr_pct": b["wr_pct"] - baseline.get("wr_pct", 0.0),
            "total_sol": b["total_sol"] - baseline.get("total_sol", 0.0),
        }

    report = {
        "schema_version": "z3h_prelive_after_gate.v1",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "source": {"state_path": str(Path(args.state).resolve()), "state_lines_total": total_lines},
        "activation_filter": {
            "mode": mode,
            "since_line": since_line,
            "activated_at": args.activated_at,
            "timestamp_fields_checked": TIMESTAMP_FIELDS,
            "timestamp_rows_seen": timestamp_rows_seen,
            "auto_detected_first_gate_line": auto_line,
            "warnings": warnings,
        },
        "filters": {
            "variants": args.variants,
            "z3h_variants": args.z3h_variants,
            "clean_rules": {
                "status": "paper_completed",
                "excluded_from_stats": False,
                "paper_entry_sol_gt": 0,
                "prelive_would_trade": True,
                "excluded_exit_reasons": sorted(EXCLUDED_EXIT_REASONS),
                "abs_net_pnl_pct_lte": 300,
                "max_favorable_pct_lte": 200,
            },
        },
        "gate_overview": {
            "rows_after_filter": len(after),
            "gate_decision_rows": len(gate_rows),
            "unique_mints_seen": len(unique_seen),
            "would_trade_rows": len(would),
            "blocked_rows": len(blocked),
            "would_trade_unique_mints": len(unique_would),
            "blocked_unique_mints": len(unique_blocked),
            "would_trade_pct_rows": pct(len(would), len(gate_rows)),
            "blocked_pct_rows": pct(len(blocked), len(gate_rows)),
            "min_quote_reserve_ui_config": max([fnum(r.get("prelive_min_quote_reserve_ui")) for r in gate_rows] or [0.0]),
            "quote_reserve_ui": stats([r.get("quote_reserve_ui") for r in gate_rows]),
        },
        "blocked_reason_breakdown": blocked_breakdown,
        "would_trade_lifecycle_counts": lifecycle_counts,
        "z3h_clean_metrics": z3h_metrics,
        "z3h_mode_split": mode_split,
        "paired_comparison": {"paired_mints": len(paired_mints), "variants": paired_variants},
        "samples": {
            "last_blocked": [
                {"line": r["_line"], "mint": r.get("mint"), "reason": r.get("prelive_blocked_reason") or r.get("exit_reason"), "quote_reserve_ui": fnum(r.get("quote_reserve_ui")), "min_quote_reserve_ui": fnum(r.get("prelive_min_quote_reserve_ui")), "quote_symbol": r.get("quote_symbol")}
                for r in blocked[-10:]
            ],
            "last_would_trade_z3h_completed": [
                {"line": r["_line"], "mint": r.get("mint"), "variant": r.get("variant_id"), "mode": r.get("z3h_selected_mode"), "exit_reason": r.get("exit_reason"), "net_pnl_pct": fnum(r.get("net_pnl_pct")), "max_favorable_pct": fnum(r.get("max_favorable_pct")), "quote_reserve_ui": fnum(r.get("quote_reserve_ui"))}
                for r in [x for x in after if x.get("variant_id") in args.z3h_variants and is_clean_completed(x)][-10:]
            ],
        },
    }
    return report


def md_table(headers, rows):
    out = ["| " + " | ".join(headers) + " |", "| " + " | ".join(["---"] * len(headers)) + " |"]
    for row in rows:
        out.append("| " + " | ".join(str(x) for x in row) + " |")
    return "\n".join(out)


def render_md(report):
    go = report["gate_overview"]
    lines = [
        f"# Z3H prelive after-gate report — {report['generated_at']}",
        "",
        f"state: `{report['source']['state_path']}`",
        f"lines: `{report['source']['state_lines_total']}`",
        f"filter: `{report['activation_filter']['mode']}` since_line=`{report['activation_filter']['since_line']}`",
        "",
    ]
    for w in report["activation_filter"].get("warnings", []):
        lines.append(f"WARNING: {w}")
    lines += [
        "## Gate overview",
        "",
        md_table(["metric", "value"], [
            ["gate_decision_rows", go["gate_decision_rows"]],
            ["unique_mints_seen", go["unique_mints_seen"]],
            ["would_trade_rows", go["would_trade_rows"]],
            ["blocked_rows", go["blocked_rows"]],
            ["would_trade_pct_rows", f"{go['would_trade_pct_rows']:.2f}%"],
            ["blocked_pct_rows", f"{go['blocked_pct_rows']:.2f}%"],
            ["quote_reserve_median", f"{go['quote_reserve_ui']['median']:.6f}"],
            ["quote_reserve_min", f"{go['quote_reserve_ui']['min']:.6f}"],
            ["quote_reserve_max", f"{go['quote_reserve_ui']['max']:.6f}"],
        ]),
        "",
        "## Blocked reasons",
        "",
    ]
    lines.append(md_table(["reason", "rows", "unique_mints", "pct", "median_reserve"], [
        [b["reason_family"], b["rows"], b["unique_mints"], f"{b['pct_of_blocked_rows']:.2f}%", f"{b['quote_reserve_ui']['median']:.6f}"]
        for b in report["blocked_reason_breakdown"]
    ] or [["none", 0, 0, "0.00%", "0.000000"]]))
    lines += ["", "## Lifecycle counts", ""]
    lines.append(md_table(["variant", "entry", "completed", "clean", "orphaned"], [
        [x["variant"], x["paper_entry"], x["paper_completed"], x["clean_completed"], x["paper_lifecycle_orphaned_restart"]]
        for x in report["would_trade_lifecycle_counts"]
    ]))
    lines += ["", "## Z3H clean metrics", ""]
    lines.append(md_table(["variant", "n", "WR", "avg", "median", "total SOL", "MDD SOL", "median MFE"], [
        [x["variant"], x["n"], f"{x['wr_pct']:.2f}%", f"{x['avg_pnl_pct']:.3f}%", f"{x['median_pnl_pct']:.3f}%", f"{x['total_sol']:.6f}", f"{x['mdd_sol']:.6f}", f"{x['median_mfe_pct']:.3f}%"]
        for x in report["z3h_clean_metrics"]
    ]))
    lines += ["", "## Z3H mode split", ""]
    lines.append(md_table(["variant", "mode", "n", "WR", "avg", "median", "total SOL"], [
        [x["variant"], x["mode"], x["n"], f"{x['wr_pct']:.2f}%", f"{x['avg_pnl_pct']:.3f}%", f"{x['median_pnl_pct']:.3f}%", f"{x['total_sol']:.6f}"]
        for x in report["z3h_mode_split"]
    ] or [["none", "none", 0, "0.00%", "0.000%", "0.000%", "0.000000"]]))
    pc = report["paired_comparison"]
    lines += ["", f"## Paired comparison — paired_mints={pc['paired_mints']}", ""]
    lines.append(md_table(["variant", "n", "WR", "avg", "median", "total SOL", "delta avg vs Z3.1"], [
        [x["variant"], x["n"], f"{x['wr_pct']:.2f}%", f"{x['avg_pnl_pct']:.3f}%", f"{x['median_pnl_pct']:.3f}%", f"{x['total_sol']:.6f}", f"{x['delta_vs_z31']['avg_pnl_pct']:.3f}%"]
        for x in pc["variants"]
    ]))
    lines += ["", "## Samples — last blocked", ""]
    lines.append("```json\n" + json.dumps(report["samples"]["last_blocked"], indent=2, ensure_ascii=False) + "\n```")
    lines += ["", "## Samples — last would_trade Z3H completed", ""]
    lines.append("```json\n" + json.dumps(report["samples"]["last_would_trade_z3h_completed"], indent=2, ensure_ascii=False) + "\n```")
    return "\n".join(lines) + "\n"


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--state", default="/opt/huragan_core/state.jsonl")
    p.add_argument("--reports-dir", default="/opt/huragan_core/reports")
    p.add_argument("--since-line", type=int, default=None)
    p.add_argument("--activated-at", default=None)
    p.add_argument("--auto-since-gate", action=argparse.BooleanOptionalAction, default=True)
    p.add_argument("--variants", nargs="+", default=DEFAULT_VARIANTS)
    p.add_argument("--z3h-variants", nargs="+", default=DEFAULT_Z3H_VARIANTS)
    p.add_argument("--no-write", action="store_true")
    args = p.parse_args()

    report = build_report(args)
    if args.no_write:
        print(json.dumps(report, indent=2, ensure_ascii=False))
        return
    outdir = Path(args.reports_dir)
    outdir.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).strftime("%Y-%m-%d_%H%M")
    json_path = outdir / f"z3h_prelive_after_gate_{stamp}.json"
    md_path = outdir / f"z3h_prelive_after_gate_{stamp}.md"
    json_path.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n")
    md_path.write_text(render_md(report))
    print(f"json={json_path}")
    print(f"md={md_path}")
    print(f"gate_rows={report['gate_overview']['gate_decision_rows']} would_trade={report['gate_overview']['would_trade_rows']} blocked={report['gate_overview']['blocked_rows']}")


if __name__ == "__main__":
    main()
