#!/usr/bin/env python3
"""Helius getTransactionsForAddress backfill for Huragan migration/fresh datasets.

Shadow/backfill only: reads RPC_URL and writes JSONL datasets; never signs or sends txs.
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
CHECKPOINTS = [0, 10, 30, 60, 120, 300]


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


def read_csv(path):
    path = Path(path)
    if not path.exists():
        return []
    with path.open(newline="") as f:
        return list(csv.DictReader(f))


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


def gtfa_fetch(rpc, address, *, details="full", start_time=None, end_time=None, limit=1000, max_pages=1):
    rows = []
    token = None
    for _ in range(max_pages):
        opts = {
            "transactionDetails": details,
            "sortOrder": "asc",
            "limit": limit,
            "filters": {"status": "succeeded"},
        }
        if start_time is not None or end_time is not None:
            bt = {}
            if start_time is not None:
                bt["gte"] = int(start_time)
            if end_time is not None:
                bt["lte"] = int(end_time)
            opts["filters"]["blockTime"] = bt
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
    # Helius gTFA rows may contain transaction/meta directly or under transaction.
    if "transaction" in row and ("meta" in row or isinstance(row.get("transaction"), dict)):
        return row
    if "nativeTransaction" in row:
        return row.get("nativeTransaction") or {}
    return row


def account_pubkey(key):
    if isinstance(key, str):
        return key
    if isinstance(key, dict):
        return key.get("pubkey") or key.get("account") or ""
    return ""


def account_keys(tx):
    msg = (((tx.get("transaction") or {}).get("message") or {}) if isinstance(tx, dict) else {})
    return [account_pubkey(k) for k in (msg.get("accountKeys") or [])]


def token_balances_by_account(tx, field="postTokenBalances"):
    keys = account_keys(tx)
    meta = tx.get("meta") or {}
    out = {}
    for b in meta.get(field) or []:
        idx = inum(b.get("accountIndex"), -1)
        acct = keys[idx] if 0 <= idx < len(keys) else b.get("account") or ""
        mint = b.get("mint") or ""
        amt = fnum(((b.get("uiTokenAmount") or {}).get("uiAmountString")) or ((b.get("uiTokenAmount") or {}).get("uiAmount")))
        raw = (b.get("uiTokenAmount") or {}).get("amount")
        out[acct] = {"mint": mint, "ui": amt, "raw": raw, "owner": b.get("owner", "")}
    return out


def tx_signature(row):
    return row.get("signature") or row.get("transactionSignature") or (((row.get("transaction") or {}).get("signatures") or [None])[0])


def tx_block_time(row):
    return inum(row.get("blockTime") or row.get("timestamp"), 0)


def migration_price_from_tx(row, base_vault, quote_vault):
    tx = unwrap_tx(row)
    bals = token_balances_by_account(tx, "postTokenBalances")
    base = bals.get(base_vault)
    quote = bals.get(quote_vault)
    if not base or not quote:
        return None
    base_ui = fnum(base.get("ui"))
    quote_ui = fnum(quote.get("ui"))
    if base_ui <= 0 or quote_ui <= 0:
        return None
    return base_ui / quote_ui


def value_at_or_before(points, age):
    prev = None
    for a, v in points:
        if a <= age:
            prev = v
        else:
            break
    return prev


def summarize_price_path(points):
    if not points:
        return {f"price_{s}s": None for s in CHECKPOINTS} | {
            "max_price_300s": None,
            "min_price_300s": None,
            "mfe_pct": 0.0,
            "max_drawdown_pct": 0.0,
            "dumped_below_entry": False,
            "quote_spike_suspect": False,
        }
    points = sorted(points)
    entry = points[0][1]
    prices_300 = [v for a, v in points if a <= 300 and v and v > 0]
    max_p = max(prices_300) if prices_300 else entry
    min_p = min(prices_300) if prices_300 else entry
    mfe = ((max_p / entry) - 1.0) * 100.0 if entry > 0 else 0.0
    dd = ((min_p / max_p) - 1.0) * 100.0 if max_p > 0 else 0.0
    out = {f"price_{s}s": value_at_or_before(points, s) for s in CHECKPOINTS}
    out.update({
        "max_price_300s": max_p,
        "min_price_300s": min_p,
        "mfe_pct": round(mfe, 6),
        "max_drawdown_pct": round(dd, 6),
        "dumped_below_entry": bool(min_p < entry),
        "quote_spike_suspect": bool(mfe > 1000.0),
    })
    return out


def backfill_migration(rpc, rows, args):
    n = 0
    for r in rows:
        if args.limit_mints and n >= args.limit_mints:
            break
        mint = r.get("mint") or ""
        pool = r.get("pool_state") or ""
        base_vault = r.get("pool_base_token_account") or ""
        quote_vault = r.get("pool_quote_token_account") or ""
        if not pool or not base_vault or not quote_vault:
            append_jsonl(args.errors, {"mode": "migration", "mint": mint, "reason": "missing_pool_or_vault"})
            continue
        try:
            txs = gtfa_fetch(rpc, pool, details=args.transaction_details, limit=args.page_limit, max_pages=args.max_pages)
            if not txs:
                append_jsonl(args.errors, {"mode": "migration", "mint": mint, "pool_state": pool, "reason": "no_transactions"})
                continue
            first_bt = tx_block_time(txs[0])
            points = []
            first_sig = tx_signature(txs[0])
            for tx in txs:
                bt = tx_block_time(tx)
                if not bt or not first_bt:
                    continue
                age = bt - first_bt
                if age < 0 or age > 300:
                    continue
                p = migration_price_from_tx(tx, base_vault, quote_vault)
                if p is not None:
                    points.append((age, p))
            summary = summarize_price_path(points)
            out = {
                "dataset": "migration_gtfa",
                "mint": mint,
                "pool_state": pool,
                "quote_symbol": r.get("quote_symbol", ""),
                "base_mint": r.get("base_mint", ""),
                "quote_mint": r.get("quote_mint", ""),
                "first_tx_signature": first_sig,
                "first_block_time": first_bt,
                "tx_count_5m": sum(1 for tx in txs if first_bt and 0 <= tx_block_time(tx) - first_bt <= 300),
            }
            out.update(summary)
            append_jsonl(args.out, out)
            n += 1
        except Exception as e:
            append_jsonl(args.errors, {"mode": "migration", "mint": mint, "pool_state": pool, "reason": str(e)})
    return n


def classify_fresh(row):
    if row.get("trade_stream_missing") or inum(row.get("buy_count_60s")) + inum(row.get("sell_count_60s")) == 0:
        return "no_trade_data"
    max_change = fnum(row.get("max_change_pct"))
    entry = fnum(row.get("entry_market_cap_sol"))
    max_mc = fnum(row.get("max_mc_300s"))
    mc60 = fnum(row.get("mc_60s"))
    if max_change >= 100.0 or (entry > 0 and max_mc >= entry * 2.0):
        return "moonshot_100k_or_2x"
    if max_change >= 50.0 or (entry > 0 and max_mc >= entry + 40.0):
        return "pump_40k_or_50pct"
    if entry > 0 and mc60 > 0 and mc60 <= entry * 0.5:
        return "rug_60s"
    return "flat"


def backfill_fresh(rpc, rows, args):
    n = 0
    for r in rows:
        if args.limit_mints and n >= args.limit_mints:
            break
        mint = r.get("mint") or ""
        if not mint:
            continue
        creator = r.get("traderPublicKey") or r.get("creator") or ""
        entry_mc = fnum(r.get("marketCapSol") or r.get("entry_market_cap_sol"))
        try:
            txs = gtfa_fetch(rpc, mint, details=args.transaction_details, limit=args.page_limit, max_pages=args.max_pages)
            first_bt = tx_block_time(txs[0]) if txs else inum(r.get("blockTime"))
            first_sig = tx_signature(txs[0]) if txs else r.get("signature", "")
            # gTFA against mint often gives tx presence but not PumpPortal MC. Keep explicit no_trade_data unless parsed events exist.
            out = {
                "dataset": "fresh_gtfa",
                "mint": mint,
                "creator": creator,
                "symbol": r.get("symbol", ""),
                "name": r.get("name", ""),
                "create_signature": first_sig,
                "create_block_time": first_bt,
                "quote_mint": r.get("quote_mint") or r.get("quoteMint") or WSOL_MINT,
                "quote_symbol": "USDC" if (r.get("quote_mint") or r.get("quoteMint")) == USDC_MINT else "WSOL",
                "quote_decimals": 6 if (r.get("quote_mint") or r.get("quoteMint")) == USDC_MINT else 9,
                "fresh_pair": "fresh_USDC" if (r.get("quote_mint") or r.get("quoteMint")) == USDC_MINT else "fresh_WSOL",
                "entry_market_cap_sol": entry_mc,
                "mc_10s": None,
                "mc_30s": None,
                "mc_60s": None,
                "mc_120s": None,
                "mc_300s": None,
                "max_mc_300s": entry_mc if entry_mc > 0 else None,
                "max_change_pct": 0.0,
                "buy_count_60s": 0,
                "sell_count_60s": 0,
                "unique_buyers_60s": 0,
                "net_flow_sol_60s": 0.0,
                "trade_stream_missing": True,
                "tx_count_5m": sum(1 for tx in txs if first_bt and 0 <= tx_block_time(tx) - first_bt <= 300),
            }
            out["label"] = classify_fresh(out)
            append_jsonl(args.out, out)
            n += 1
        except Exception as e:
            append_jsonl(args.errors, {"mode": "fresh", "mint": mint, "reason": str(e)})
    return n


def self_test():
    sample = {
        "blockTime": 100,
        "signature": "sig0",
        "transaction": {"message": {"accountKeys": ["payer", "base", "quote"]}},
        "meta": {"postTokenBalances": [
            {"accountIndex": 1, "mint": WSOL_MINT, "uiTokenAmount": {"uiAmountString": "10"}},
            {"accountIndex": 2, "mint": "Token", "uiTokenAmount": {"uiAmountString": "1000"}},
        ]},
    }
    assert abs(migration_price_from_tx(sample, "base", "quote") - 0.01) < 1e-12
    path = summarize_price_path([(0, 1.0), (10, 1.5), (60, 0.4), (300, 2.1)])
    assert path["price_10s"] == 1.5
    assert round(path["mfe_pct"], 6) == 110.0
    assert path["dumped_below_entry"] is True
    assert classify_fresh({"trade_stream_missing": True}) == "no_trade_data"
    assert classify_fresh({"buy_count_60s": 2, "max_change_pct": 120}) == "moonshot_100k_or_2x"
    assert classify_fresh({"buy_count_60s": 2, "entry_market_cap_sol": 20, "mc_60s": 8}) == "rug_60s"
    print("SELF_TEST_OK")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", choices=["migration", "fresh"], required=False)
    ap.add_argument("--input", default=None)
    ap.add_argument("--out", default=None)
    ap.add_argument("--errors", default="datasets/gtfa_backfill_errors.jsonl")
    ap.add_argument("--rpc-url", default=os.environ.get("RPC_URL", ""))
    ap.add_argument("--transaction-details", choices=["full", "signatures"], default="full")
    ap.add_argument("--limit-mints", type=int, default=0)
    ap.add_argument("--page-limit", type=int, default=1000)
    ap.add_argument("--max-pages", type=int, default=1)
    ap.add_argument("--rpc-sleep", type=float, default=0.0)
    ap.add_argument("--self-test", action="store_true")
    args = ap.parse_args()

    if args.self_test:
        self_test()
        return
    if not args.mode:
        raise SystemExit("--mode required unless --self-test")
    if not args.rpc_url:
        raise SystemExit("RPC_URL missing; pass --rpc-url or source .env")
    if args.out is None:
        args.out = "datasets/migration_gtfa_lifecycle.jsonl" if args.mode == "migration" else "datasets/fresh_gtfa_lifecycle.jsonl"
    if args.input is None:
        args.input = "datasets/migration_profit_winners.csv" if args.mode == "migration" else "fresh_momentum_candidates.jsonl"

    rpc = Rpc(args.rpc_url, sleep_s=args.rpc_sleep)
    rows = read_csv(args.input) if args.mode == "migration" else read_jsonl(args.input)
    if args.mode == "migration":
        count = backfill_migration(rpc, rows, args)
    else:
        count = backfill_fresh(rpc, rows, args)
    print(json.dumps({"mode": args.mode, "input_rows": len(rows), "written": count, "out": args.out, "errors": args.errors}, indent=2))


if __name__ == "__main__":
    main()
