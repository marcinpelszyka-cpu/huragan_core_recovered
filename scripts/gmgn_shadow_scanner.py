#!/usr/bin/env python3
"""
GMGN Shadow Scanner — paper-only Solana new-pair shadow scanner.

For every fresh candidate it pulls GMGN token info + security, applies a
conservative safety gate, and appends a structured signal row to
gmgn_shadow_signals.jsonl.  NEVER trades.  NEVER touches the live bot.

Gate (paper shadow):
    liquidity_ok  : liquidity_usd >= MIN_LIQUIDITY_USD
    security_ok   : no top10 > MAX_TOP10_HOLDER_PCT, renounced, holder floor
    signal:
        WATCH  -> passes both gates
        SKIP   -> fails one or more gates
        ALERT  -> smart money + KOL overlap and renounced (rare)

Usage:
    python3 scripts/gmgn_shadow_scanner.py
    python3 scripts/gmgn_shadow_scanner.py --once           # single batch, no loop
    python3 scripts/gmgn_shadow_scanner.py --limit 50       # cap mints this run
"""

import argparse
import json
import os
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

# ----- configuration --------------------------------------------------------

ROOT = Path("/opt/huragan_core")
GMGN_CLI = "/root/.hermes/node/bin/gmgn-cli"
CANDIDATE_FILE = ROOT / "fresh_momentum_candidates.jsonl"
SIGNAL_FILE = ROOT / "gmgn_shadow_signals.jsonl"
STATE_FILE = ROOT / "state.jsonl"  # already-traded mints to avoid re-scanning

CHAIN = "sol"
MIN_LIQUIDITY_USD = 5_000.0
MAX_TOP10_HOLDER_PCT = 0.40      # 40 %
MAX_RUG_RATIO = 0.30             # 30 %
MIN_HOLDERS = 25
SCAN_INTERVAL_SECS = 300         # 5 min
PER_MINT_TIMEOUT = 12            # seconds
BATCH_LIMIT = 80

# ----- helpers --------------------------------------------------------------

def run_gmgn(*args, timeout=PER_MINT_TIMEOUT):
    """Run gmgn-cli and return parsed JSON or None."""
    try:
        r = subprocess.run(
            [GMGN_CLI, *args, "--chain", CHAIN, "--raw"],
            capture_output=True, text=True, timeout=timeout,
        )
        if r.returncode != 0 or not r.stdout.strip():
            return None
        try:
            return json.loads(r.stdout)
        except json.JSONDecodeError:
            return None
    except subprocess.TimeoutExpired:
        return None
    except Exception:
        return None


def fnum(x, default=0.0):
    try:
        if x in (None, "", "null"):
            return default
        return float(x)
    except (TypeError, ValueError):
        return default


def collect_candidates(limit):
    """Return list of mints, prioritising freshest and never-before-seen."""
    seen_in_signals = set()
    if SIGNAL_FILE.exists():
        with SIGNAL_FILE.open() as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    row = json.loads(line)
                    mint = row.get("token")
                    if mint:
                        seen_in_signals.add(mint)
                except json.JSONDecodeError:
                    continue

    seen_in_state = set()
    if STATE_FILE.exists():
        with STATE_FILE.open() as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    row = json.loads(line)
                    mint = row.get("mint")
                    if mint:
                        seen_in_state.add(mint)
                except json.JSONDecodeError:
                    continue

    if not CANDIDATE_FILE.exists():
        return []

    mints = []
    with CANDIDATE_FILE.open() as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except json.JSONDecodeError:
                continue
            mint = row.get("mint")
            if not mint:
                continue
            mints.append(mint)

    # dedupe, reverse so tail-most (freshest) win
    seen = set()
    deduped = []
    for m in reversed(mints):
        if m in seen:
            continue
        seen.add(m)
        deduped.append(m)
    return deduped[:limit]


def evaluate(mint):
    """Return dict signal row or None if gmgn returned nothing useful."""
    info = run_gmgn("token", "info", "--address", mint) or {}
    sec = run_gmgn("token", "security", "--address", mint) or {}

    if not info and not sec:
        return None

    symbol = info.get("symbol") or sec.get("symbol") or ""
    name = info.get("name") or sec.get("name") or ""

    liquidity_usd = fnum(info.get("liquidity") or info.get("liquidity_usd"))
    market_cap_usd = fnum(info.get("market_cap_usd") or info.get("market_cap"))
    volume_1h_usd = fnum(info.get("volume_1h_usd") or info.get("volume_1h"))
    volume_24h_usd = fnum(info.get("volume_24h_usd") or info.get("volume_24h"))
    holder_count = int(fnum(info.get("holder_count") or sec.get("holder_count")))

    # top10 holder pct
    top10 = sec.get("top_10_holder_rate") or sec.get("top10_holder_pct")
    if top10 is None:
        holders = sec.get("holders") or []
        if holders:
            top10 = sum(fnum(h.get("amount") or h.get("pct") or 0) for h in holders[:10])
        else:
            top10 = 0.0
    top10_pct = float(top10)
    if top10_pct > 1.5:  # looks like a percent not a fraction
        top10_pct = top10_pct / 100.0

    # rug ratio
    rug_ratio = float(fnum(sec.get("rugged_num") or sec.get("rug_ratio")))
    if rug_ratio > 1.5:  # already a percent
        rug_ratio = rug_ratio / 100.0

    # renounce / mint authority
    renounced = bool(sec.get("renounced") or sec.get("mint_authority_renounced"))

    # smart money / KOL overlap — left at 0 for shadow; cross-ref later
    smart_money_count = int(fnum(sec.get("smart_money_count") or 0))
    kol_count = int(fnum(sec.get("kol_count") or 0))

    age_secs = int(fnum(info.get("age_secs") or sec.get("age_secs") or 0))
    created_ts = fnum(info.get("created_timestamp") or sec.get("open_timestamp"))
    if not age_secs and created_ts:
        age_secs = max(0, int(time.time() - created_ts))

    # ----- gate -------------------------------------------------------------
    reasons = []
    liquidity_ok = liquidity_usd >= MIN_LIQUIDITY_USD
    if not liquidity_ok:
        reasons.append(f"thin_liquidity:{liquidity_usd:.0f}")

    if not renounced:
        reasons.append("not_renounced")
    if top10_pct > 0.60:
        reasons.append(f"top10_very_high:{top10_pct*100:.2f}%")
    elif top10_pct > MAX_TOP10_HOLDER_PCT:
        reasons.append(f"top10_high:{top10_pct*100:.2f}%")
    if rug_ratio > MAX_RUG_RATIO:
        reasons.append(f"rug_ratio_high:{rug_ratio*100:.2f}%")
    if holder_count < MIN_HOLDERS:
        reasons.append(f"few_holders:{holder_count}")
    if age_secs and age_secs < 60:
        reasons.append("very_fresh")

    security_ok = (
        renounced
        and top10_pct <= MAX_TOP10_HOLDER_PCT
        and rug_ratio <= MAX_RUG_RATIO
        and holder_count >= MIN_HOLDERS
    )

    if liquidity_ok and security_ok and smart_money_count and kol_count:
        signal = "ALERT"
    elif liquidity_ok and security_ok:
        signal = "WATCH"
    else:
        signal = "SKIP"

    # risk labels for downstream consumers
    if top10_pct > 0.60 or holder_count < 50:
        holder_risk = "high"
    elif top10_pct > 0.40 or holder_count < 150:
        holder_risk = "medium"
    else:
        holder_risk = "low"

    if rug_ratio > 0.20:
        rug_risk = "high"
    elif rug_ratio > 0.10:
        rug_risk = "medium"
    else:
        rug_risk = "low"

    return {
        "source": "gmgn",
        "token": mint,
        "symbol": symbol,
        "name": name,
        "chain": CHAIN,
        "signal": signal,
        "liquidity_ok": liquidity_ok,
        "security_ok": security_ok,
        "smart_money_count": smart_money_count,
        "kol_count": kol_count,
        "holder_count": holder_count,
        "top10_holder_pct": round(top10_pct, 4),
        "rug_ratio": round(rug_ratio, 4),
        "holder_risk": holder_risk,
        "rug_risk": rug_risk,
        "liquidity_usd": round(liquidity_usd, 4),
        "market_cap_usd": round(market_cap_usd, 4),
        "volume_1h_usd": round(volume_1h_usd, 4),
        "volume_24h_usd": round(volume_24h_usd, 4),
        "age_secs": age_secs,
        "reason": reasons,
        "paper_only": True,
        "_captured_at": datetime.now(timezone.utc).isoformat(),
    }


def scan_batch(limit):
    mints = collect_candidates(limit)
    written = 0
    errors = 0
    SIGNAL_FILE.touch(exist_ok=True)
    with SIGNAL_FILE.open("a") as out:
        for mint in mints:
            try:
                row = evaluate(mint)
            except Exception as e:
                errors += 1
                print(f"  ! {mint[:8]}  eval error: {e}", file=sys.stderr)
                continue
            if not row:
                errors += 1
                continue
            out.write(json.dumps(row) + "\n")
            out.flush()
            written += 1
            sig = row["signal"]
            liq = row["liquidity_usd"]
            mc = row["market_cap_usd"]
            holders = row["holder_count"]
            print(f"  {sig:5s}  {row['symbol'] or '-':8s}  liq=${liq:>10.0f}  mc=${mc:>10.0f}  holders={holders:>5d}  {mint[:10]}…")
    return written, errors, len(mints)


def main():
    ap = argparse.ArgumentParser(description="GMGN paper-only shadow scanner")
    ap.add_argument("--once", action="store_true", help="single batch, no loop")
    ap.add_argument("--limit", type=int, default=BATCH_LIMIT, help="max mints per run")
    ap.add_argument("--interval", type=int, default=SCAN_INTERVAL_SECS, help="seconds between batches")
    args = ap.parse_args()

    if not os.path.exists(GMGN_CLI):
        print(f"gmgn-cli not found at {GMGN_CLI}", file=sys.stderr)
        return 2

    print(f"GMGN Shadow Scanner — chain={CHAIN}  paper_only=True")
    print(f"  candidates: {CANDIDATE_FILE}")
    print(f"  signals:    {SIGNAL_FILE}")
    print(f"  limits:     liq>={MIN_LIQUIDITY_USD}  top10<={MAX_TOP10_HOLDER_PCT*100:.0f}%  rug<={MAX_RUG_RATIO*100:.0f}%  holders>={MIN_HOLDERS}")
    print()

    if args.once:
        w, e, n = scan_batch(args.limit)
        print(f"\n-> wrote {w} signals ({e} errors) from {n} candidates")
        return 0

    while True:
        try:
            w, e, n = scan_batch(args.limit)
            print(f"\n[{datetime.now(timezone.utc).isoformat()}] wrote {w} ({e} errors) from {n} candidates — sleeping {args.interval}s")
        except KeyboardInterrupt:
            print("\nstopped.")
            return 0
        except Exception as e:
            print(f"\n! batch crashed: {e}", file=sys.stderr)
        time.sleep(args.interval)


if __name__ == "__main__":
    sys.exit(main())
