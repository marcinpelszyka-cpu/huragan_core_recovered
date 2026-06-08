#!/usr/bin/env python3
"""Fresh sniper follow backtest/shadow dataset builder.

Shadow/backfill only: reads Helius RPC data and local PumpPortal fresh candidates;
never signs or sends transactions.
"""
import argparse
import csv
import json
import math
import os
import sys
import time
import urllib.request
from collections import defaultdict
from pathlib import Path

WSOL_MINT = "So11111111111111111111111111111111111111112"
USDC_MINT = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v"
DEFAULT_OUT_EVENTS = "datasets/sniper_trade_events.jsonl"
DEFAULT_OUT_SCORES = "datasets/sniper_wallet_scores.csv"
DEFAULT_OUT_SIGNALS = "datasets/sniper_follow_signals.jsonl"
DEFAULT_ERRORS = "datasets/sniper_follow_errors.jsonl"


def load_dotenv_value(key, default=""):
    val = os.environ.get(key)
    if val:
        return val
    env_path = Path(".env")
    if not env_path.exists():
        return default
    try:
        for line in env_path.read_text().splitlines():
            line = line.strip()
            if not line or line.startswith("#") or "=" not in line:
                continue
            k, v = line.split("=", 1)
            if k.strip() == key:
                return v.strip().strip('"').strip("'")
    except Exception:
        return default
    return default


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


def read_jsonl(path):
    path = Path(path)
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
                print(f"WARN bad_json path={path} line={i}: {e}", file=sys.stderr)
    return rows


def write_jsonl(path, rows):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w") as f:
        for row in rows:
            f.write(json.dumps(row, separators=(",", ":"), ensure_ascii=False) + "\n")


def append_jsonl(path, row):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a") as f:
        f.write(json.dumps(row, separators=(",", ":"), ensure_ascii=False) + "\n")


class Rpc:
    def __init__(self, url, sleep_s=0.0):
        self.url = url
        self.sleep_s = sleep_s
        self.calls = 0

    def call(self, method, params):
        self.calls += 1
        if self.sleep_s:
            time.sleep(self.sleep_s)
        body = json.dumps({"jsonrpc": "2.0", "id": self.calls, "method": method, "params": params}).encode()
        req = urllib.request.Request(self.url, data=body, headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=60) as resp:
            out = json.load(resp)
        if out.get("error"):
            raise RuntimeError(f"rpc_error:{out['error']}")
        return out.get("result")


def gtfa_fetch(rpc, address, *, start_time=None, end_time=None, limit=1000, max_pages=1):
    rows = []
    token = None
    for _ in range(max_pages):
        opts = {
            "transactionDetails": "full",
            "sortOrder": "asc",
            "limit": limit,
            "filters": {"status": "succeeded"},
        }
        if start_time is not None or end_time is not None:
            block_time = {}
            if start_time is not None:
                block_time["gte"] = int(start_time)
            if end_time is not None:
                block_time["lte"] = int(end_time)
            opts["filters"]["blockTime"] = block_time
        if token:
            opts["paginationToken"] = token
        res = rpc.call("getTransactionsForAddress", [address, opts]) or {}
        data = res.get("data") if isinstance(res, dict) else res
        if not data:
            break
        rows.extend(data)
        token = res.get("paginationToken") if isinstance(res, dict) else None
        if not token:
            break
    return rows


def unwrap_tx(row):
    if not isinstance(row, dict):
        return {}
    if "nativeTransaction" in row and isinstance(row.get("nativeTransaction"), dict):
        return row.get("nativeTransaction") or {}
    return row


def tx_signature(row):
    tx = unwrap_tx(row)
    return (
        row.get("signature")
        or row.get("transactionSignature")
        or (((tx.get("transaction") or {}).get("signatures") or [None])[0])
        or ""
    )


def tx_block_time(row):
    tx = unwrap_tx(row)
    return inum(row.get("blockTime") or row.get("timestamp") or tx.get("blockTime"), 0)


def account_key_info(key):
    if isinstance(key, str):
        return {"pubkey": key, "signer": False}
    if isinstance(key, dict):
        return {"pubkey": key.get("pubkey") or key.get("account") or "", "signer": bool(key.get("signer"))}
    return {"pubkey": "", "signer": False}


def account_keys(tx):
    msg = (((tx.get("transaction") or {}).get("message") or {}) if isinstance(tx, dict) else {})
    return [account_key_info(k) for k in (msg.get("accountKeys") or [])]


def primary_signer(tx):
    keys = account_keys(tx)
    for k in keys:
        if k.get("signer") and k.get("pubkey"):
            return k["pubkey"]
    return keys[0]["pubkey"] if keys else ""


def native_sol_delta_for(tx, pubkey):
    keys = account_keys(tx)
    idx = next((i for i, k in enumerate(keys) if k.get("pubkey") == pubkey), -1)
    if idx < 0:
        return 0.0
    meta = tx.get("meta") or {}
    pre = meta.get("preBalances") or []
    post = meta.get("postBalances") or []
    if idx >= len(pre) or idx >= len(post):
        return 0.0
    return (fnum(post[idx]) - fnum(pre[idx])) / 1e9


def raw_token_amount(balance):
    amt = (balance.get("uiTokenAmount") or {}).get("amount")
    return inum(amt, 0)


def token_balance_maps(tx, field):
    keys = account_keys(tx)
    out = {}
    for b in (tx.get("meta") or {}).get(field) or []:
        idx = inum(b.get("accountIndex"), -1)
        account = keys[idx]["pubkey"] if 0 <= idx < len(keys) else b.get("account") or ""
        owner = b.get("owner") or ""
        mint = b.get("mint") or ""
        out[(account, mint, owner)] = raw_token_amount(b)
    return out


def extract_mint_trade_events(row, mint, first_block_time, entry_market_cap_sol=0.0):
    tx = unwrap_tx(row)
    bt = tx_block_time(row)
    if not bt:
        return []
    signer = primary_signer(tx)
    pre = token_balance_maps(tx, "preTokenBalances")
    post = token_balance_maps(tx, "postTokenBalances")
    keys = set(pre.keys()) | set(post.keys())
    events = []
    for key in keys:
        account, bal_mint, owner = key
        if bal_mint != mint:
            continue
        delta = post.get(key, 0) - pre.get(key, 0)
        if delta == 0:
            continue
        actor = owner or signer
        if not actor:
            continue
        side = "buy" if delta > 0 else "sell"
        native_delta = native_sol_delta_for(tx, signer)
        quote_delta_sol = abs(native_delta) if native_delta else 0.0
        events.append({
            "mint": mint,
            "timestamp": bt,
            "age_secs": max(0, bt - first_block_time) if first_block_time else 0,
            "signature": tx_signature(row),
            "signer": signer,
            "owner": actor,
            "token_account": account,
            "side": side,
            "token_delta_raw": abs(delta),
            "quote_delta_sol": round(quote_delta_sol, 12),
            "entry_market_cap_sol": entry_market_cap_sol,
        })
    return events


def classify_wallet(sample_count, win_rate, avg_pnl_sol, fast_dump_rate, avg_hold_ratio_60s, dev_suspect_rate):
    if sample_count <= 0:
        return "UNKNOWN"
    if dev_suspect_rate >= 0.5 and sample_count >= 2:
        return "DEV_SNIPER_SUSPECT"
    if fast_dump_rate >= 0.5 and sample_count >= 2:
        return "FAST_DUMPER"
    if sample_count >= 3 and win_rate >= 0.55 and avg_pnl_sol > 0 and avg_hold_ratio_60s >= 0.35:
        return "GOOD_SNIPER"
    return "UNKNOWN"


def score_wallets(events, early_window_secs=10, dump_window_secs=30):
    by_wallet_mint = defaultdict(list)
    for e in events:
        wallet = e.get("owner") or e.get("signer") or ""
        if wallet:
            by_wallet_mint[(wallet, e["mint"])].append(e)

    per_wallet = defaultdict(list)
    for (wallet, mint), rows in by_wallet_mint.items():
        rows = sorted(rows, key=lambda r: (r.get("timestamp", 0), r.get("signature", "")))
        buys = [r for r in rows if r["side"] == "buy"]
        sells = [r for r in rows if r["side"] == "sell"]
        first_buy = min((r["age_secs"] for r in buys), default=None)
        if first_buy is None or first_buy > early_window_secs:
            continue
        bought = sum(r["token_delta_raw"] for r in buys)
        sold = sum(r["token_delta_raw"] for r in sells)
        bought_sol = sum(r["quote_delta_sol"] for r in buys)
        sold_sol = sum(r["quote_delta_sol"] for r in sells)
        sold_fast = sum(r["token_delta_raw"] for r in sells if r["age_secs"] <= dump_window_secs)
        hold_ratio = max(0.0, (bought - sold) / bought) if bought > 0 else 0.0
        fast_dump_ratio = sold_fast / bought if bought > 0 else 0.0
        pnl = sold_sol - bought_sol
        # Funding graph is not available in this V1 dataset. Keep this explicit.
        dev_suspect = False
        per_wallet[wallet].append({
            "mint": mint,
            "first_buy_age_secs": first_buy,
            "buy_sol": bought_sol,
            "sell_sol": sold_sol,
            "pnl_sol": pnl,
            "hold_ratio_60s": hold_ratio,
            "fast_dump_ratio_30s": fast_dump_ratio,
            "dev_suspect": dev_suspect,
        })

    scores = []
    for wallet, rows in per_wallet.items():
        sample_count = len(rows)
        wins = sum(1 for r in rows if r["pnl_sol"] > 0)
        win_rate = wins / sample_count if sample_count else 0.0
        avg_pnl = sum(r["pnl_sol"] for r in rows) / sample_count if sample_count else 0.0
        fast_dump_rate = sum(1 for r in rows if r["fast_dump_ratio_30s"] >= 0.5) / sample_count if sample_count else 0.0
        avg_hold = sum(r["hold_ratio_60s"] for r in rows) / sample_count if sample_count else 0.0
        dev_rate = sum(1 for r in rows if r["dev_suspect"]) / sample_count if sample_count else 0.0
        cls = classify_wallet(sample_count, win_rate, avg_pnl, fast_dump_rate, avg_hold, dev_rate)
        scores.append({
            "wallet": wallet,
            "classification": cls,
            "sample_count": sample_count,
            "win_rate": round(win_rate, 6),
            "avg_pnl_sol": round(avg_pnl, 12),
            "fast_dump_rate": round(fast_dump_rate, 6),
            "avg_hold_ratio_60s": round(avg_hold, 6),
            "dev_sniper_suspect_rate": round(dev_rate, 6),
        })
    scores.sort(key=lambda r: (r["classification"] != "GOOD_SNIPER", -r["sample_count"], -r["avg_pnl_sol"], r["wallet"]))
    return scores


def build_signals(events, scores, *, early_window_secs=10, min_buy_sol=0.01, min_good_snipers=2, min_total_buy_sol=0.03, min_hold_ratio=0.75):
    class_by_wallet = {r["wallet"]: r["classification"] for r in scores}
    by_mint = defaultdict(list)
    for e in events:
        by_mint[e["mint"]].append(e)
    signals = []
    for mint, rows in sorted(by_mint.items()):
        early_buys = [
            r for r in rows
            if r["side"] == "buy" and r["age_secs"] <= early_window_secs and r["quote_delta_sol"] >= min_buy_sol
        ]
        good = [r for r in early_buys if class_by_wallet.get(r.get("owner") or r.get("signer"), "UNKNOWN") == "GOOD_SNIPER"]
        good_wallets = sorted({r.get("owner") or r.get("signer") for r in good if r.get("owner") or r.get("signer")})
        total_buy = sum(r["quote_delta_sol"] for r in good)
        bought_by_good = sum(r["token_delta_raw"] for r in good)
        sold_by_good_10s = sum(
            r["token_delta_raw"]
            for r in rows
            if r["side"] == "sell" and r["age_secs"] <= early_window_secs and (r.get("owner") or r.get("signer")) in set(good_wallets)
        )
        hold_ratio = max(0.0, (bought_by_good - sold_by_good_10s) / bought_by_good) if bought_by_good else 0.0
        passed = len(good_wallets) >= min_good_snipers and total_buy >= min_total_buy_sol and hold_ratio >= min_hold_ratio
        signals.append({
            "mint": mint,
            "signal": "FOLLOW_SHADOW" if passed else "NO_SIGNAL",
            "passed": passed,
            "age_limit_secs": 60,
            "early_window_secs": early_window_secs,
            "target_market_cap_note": "around_3500_if_source_available_else_sol_bucket",
            "entry_market_cap_sol": next((r.get("entry_market_cap_sol", 0.0) for r in rows if r.get("entry_market_cap_sol", 0.0)), 0.0),
            "good_sniper_count": len(good_wallets),
            "good_sniper_wallets": good_wallets[:10],
            "good_sniper_buy_sol": round(total_buy, 12),
            "good_sniper_hold_ratio_10s": round(hold_ratio, 6),
            "reason": "good_snipers_follow" if passed else "insufficient_good_sniper_signal",
            "live_allowed": False,
        })
    return signals


def write_scores_csv(path, scores):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fields = ["wallet", "classification", "sample_count", "win_rate", "avg_pnl_sol", "fast_dump_rate", "avg_hold_ratio_60s", "dev_sniper_suspect_rate"]
    with path.open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields)
        w.writeheader()
        for row in scores:
            w.writerow({k: row.get(k, "") for k in fields})


def fresh_candidates(path):
    rows = read_jsonl(path)
    out = []
    seen = set()
    for r in rows:
        mint = r.get("mint") or ""
        if not mint or mint in seen:
            continue
        seen.add(mint)
        out.append(r)
    return out


def run(args):
    if not args.rpc_url:
        if args.dry_run:
            return {
                "input": args.input,
                "processed_mints": 0,
                "trade_events": 0,
                "wallet_scores": 0,
                "shadow_signals": 0,
                "dry_run": True,
                "rpc_missing": True,
                "note": "RPC_URL required for real backfill; dry-run performed config smoke only",
                "out_events": args.out_events,
                "out_scores": args.out_scores,
                "out_signals": args.out_signals,
                "errors": args.errors,
            }
        raise SystemExit("RPC_URL missing; pass --rpc-url or source .env")
    rpc = Rpc(args.rpc_url, sleep_s=args.rpc_sleep)
    candidates = fresh_candidates(args.input)
    events = []
    processed = 0
    for row in candidates:
        if args.limit_mints and processed >= args.limit_mints:
            break
        mint = row.get("mint") or ""
        if not mint:
            continue
        entry_mc = fnum(row.get("marketCapSol") or row.get("entry_market_cap_sol"))
        first_bt_hint = inum(row.get("blockTime") or row.get("timestamp"), 0)
        try:
            start_time = first_bt_hint - 5 if first_bt_hint else None
            end_time = first_bt_hint + args.max_age_secs if first_bt_hint else None
            txs = gtfa_fetch(rpc, mint, start_time=start_time, end_time=end_time, limit=args.page_limit, max_pages=args.max_pages)
            first_bt = first_bt_hint or (tx_block_time(txs[0]) if txs else 0)
            for tx in txs:
                events.extend(extract_mint_trade_events(tx, mint, first_bt, entry_mc))
            processed += 1
        except Exception as e:
            append_jsonl(args.errors, {"mode": "sniper_follow", "mint": mint, "reason": str(e)})

    scores = score_wallets(events, args.early_window_secs, args.dump_window_secs)
    signals = build_signals(
        events,
        scores,
        early_window_secs=args.early_window_secs,
        min_buy_sol=args.min_buy_sol,
        min_good_snipers=args.min_good_snipers,
        min_total_buy_sol=args.min_total_buy_sol,
        min_hold_ratio=args.min_hold_ratio,
    )

    if not args.dry_run:
        write_jsonl(args.out_events, events)
        write_scores_csv(args.out_scores, scores)
        write_jsonl(args.out_signals, signals)

    return {
        "input": args.input,
        "processed_mints": processed,
        "trade_events": len(events),
        "wallet_scores": len(scores),
        "shadow_signals": sum(1 for s in signals if s.get("passed")),
        "dry_run": args.dry_run,
        "out_events": args.out_events,
        "out_scores": args.out_scores,
        "out_signals": args.out_signals,
        "errors": args.errors,
    }


def self_test():
    tx = {
        "blockTime": 100,
        "signature": "sig1",
        "transaction": {"message": {"accountKeys": [{"pubkey": "Wallet", "signer": True}, "Ata"]}},
        "meta": {
            "preBalances": [10_000_000_000, 0],
            "postBalances": [9_980_000_000, 0],
            "preTokenBalances": [{"accountIndex": 1, "mint": "Mint", "owner": "Wallet", "uiTokenAmount": {"amount": "0"}}],
            "postTokenBalances": [{"accountIndex": 1, "mint": "Mint", "owner": "Wallet", "uiTokenAmount": {"amount": "1000"}}],
        },
    }
    events = extract_mint_trade_events(tx, "Mint", 95, 25.0)
    assert len(events) == 1
    assert events[0]["side"] == "buy"
    assert events[0]["age_secs"] == 5
    assert abs(events[0]["quote_delta_sol"] - 0.02) < 1e-12
    scores = score_wallets(events * 3, early_window_secs=10)
    assert scores and scores[0]["wallet"] == "Wallet"
    research_scores = [{"wallet": "Wallet", "classification": "GOOD_SNIPER"}]
    signals = build_signals(events * 3, research_scores, min_good_snipers=1, min_total_buy_sol=0.01)
    assert signals and signals[0]["passed"] is True
    print("SELF_TEST_OK")


def main():
    ap = argparse.ArgumentParser(description="Build Fresh Sniper Follow shadow/backtest datasets from Helius gTFA.")
    ap.add_argument("--input", default="fresh_momentum_candidates.jsonl")
    ap.add_argument("--rpc-url", default=load_dotenv_value("RPC_URL", ""))
    ap.add_argument("--out-events", default=DEFAULT_OUT_EVENTS)
    ap.add_argument("--out-scores", default=DEFAULT_OUT_SCORES)
    ap.add_argument("--out-signals", default=DEFAULT_OUT_SIGNALS)
    ap.add_argument("--errors", default=DEFAULT_ERRORS)
    ap.add_argument("--limit-mints", type=int, default=0)
    ap.add_argument("--page-limit", type=int, default=1000)
    ap.add_argument("--max-pages", type=int, default=1)
    ap.add_argument("--max-age-secs", type=int, default=60)
    ap.add_argument("--early-window-secs", type=int, default=10)
    ap.add_argument("--dump-window-secs", type=int, default=30)
    ap.add_argument("--min-buy-sol", type=float, default=0.01)
    ap.add_argument("--min-good-snipers", type=int, default=2)
    ap.add_argument("--min-total-buy-sol", type=float, default=0.03)
    ap.add_argument("--min-hold-ratio", type=float, default=0.75)
    ap.add_argument("--rpc-sleep", type=float, default=0.0)
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()
    if args.self_test:
        self_test()
        return
    print(json.dumps(run(args), indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
