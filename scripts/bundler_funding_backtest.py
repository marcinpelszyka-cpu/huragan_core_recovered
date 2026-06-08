#!/usr/bin/env python3
"""Bundler + mother-wallet funding graph backtest.

Shadow/backtest only. Reads Helius JSON-RPC data and local datasets; never signs,
never sends transactions, and never touches runtime config.
"""
import argparse
import csv
import json
import math
import os
import sys
import time
import urllib.error
import urllib.request
from collections import Counter, defaultdict
from pathlib import Path

PROJECT = Path(__file__).resolve().parent.parent
WSOL_MINT = "So11111111111111111111111111111111111111112"
SYSTEM_ACCOUNTS = {
    "11111111111111111111111111111111",
    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
    "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
    "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
    "ComputeBudget111111111111111111111111111111",
    WSOL_MINT,
}
DEFAULT_OUT_EDGES = "datasets/bundler_wallet_edges.jsonl"
DEFAULT_OUT_CLUSTERS = "datasets/bundler_clusters.csv"
DEFAULT_OUT_SIGNALS = "datasets/fresh_bundle_risk_signals.jsonl"
DEFAULT_ERRORS = "datasets/bundler_funding_errors.jsonl"


def load_dotenv_value(key, default=""):
    val = os.environ.get(key)
    if val:
        return val
    env_path = Path(".env")
    if not env_path.exists():
        return default
    try:
        for line in env_path.read_text(errors="ignore").splitlines():
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
    with path.open(errors="ignore") as f:
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
        self.errors = Counter()

    def call(self, method, params):
        self.calls += 1
        if self.sleep_s:
            time.sleep(self.sleep_s)
        body = json.dumps({"jsonrpc": "2.0", "id": self.calls, "method": method, "params": params}).encode()
        req = urllib.request.Request(self.url, data=body, headers={"Content-Type": "application/json"})
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                out = json.load(resp)
        except urllib.error.HTTPError as e:
            self.errors[str(e.code)] += 1
            raise RuntimeError(f"http_error:{e.code}")
        except Exception as e:
            self.errors["transport"] += 1
            raise RuntimeError(f"transport_error:{e}")
        if out.get("error"):
            err = out["error"]
            msg = str(err)
            if "429" in msg or "Too Many" in msg:
                self.errors["429"] += 1
            elif "401" in msg or "Unauthorized" in msg:
                self.errors["401"] += 1
            else:
                self.errors["rpc"] += 1
            raise RuntimeError(f"rpc_error:{err}")
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
    if isinstance(row, dict) and isinstance(row.get("nativeTransaction"), dict):
        return row["nativeTransaction"]
    return row if isinstance(row, dict) else {}


def tx_signature(row):
    tx = unwrap_tx(row)
    return row.get("signature") or row.get("transactionSignature") or (((tx.get("transaction") or {}).get("signatures") or [""])[0]) or ""


def tx_slot(row):
    tx = unwrap_tx(row)
    return inum(row.get("slot") or tx.get("slot"), 0)


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
    msg = ((tx.get("transaction") or {}).get("message") or {}) if isinstance(tx, dict) else {}
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
    return inum((balance.get("uiTokenAmount") or {}).get("amount"), 0)


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


def extract_mint_trade_events(row, mint, first_block_time):
    tx = unwrap_tx(row)
    bt = tx_block_time(row)
    if not bt:
        return []
    signer = primary_signer(tx)
    pre = token_balance_maps(tx, "preTokenBalances")
    post = token_balance_maps(tx, "postTokenBalances")
    events = []
    for key in set(pre) | set(post):
        account, bal_mint, owner = key
        if bal_mint != mint:
            continue
        delta = post.get(key, 0) - pre.get(key, 0)
        if delta == 0:
            continue
        actor = owner or signer
        if not actor or actor in SYSTEM_ACCOUNTS:
            continue
        if owner and signer and owner != signer:
            continue
        side = "buy" if delta > 0 else "sell"
        quote_delta_sol = abs(native_sol_delta_for(tx, signer))
        events.append({
            "mint": mint,
            "signature": tx_signature(row),
            "slot": tx_slot(row),
            "timestamp": bt,
            "age_secs": max(0, bt - first_block_time) if first_block_time else 0,
            "signer": signer,
            "owner": actor,
            "token_account": account,
            "side": side,
            "token_delta_raw": abs(delta),
            "quote_delta_sol": round(quote_delta_sol, 12),
        })
    return events


def load_candidates(path, events_path, limit):
    candidates = []
    seen = set()
    for r in read_jsonl(path):
        mint = r.get("mint") or ""
        if mint and mint not in seen:
            seen.add(mint)
            candidates.append({
                "mint": mint,
                "first_block_time_hint": inum(r.get("blockTime") or r.get("timestamp"), 0),
                "entry_market_cap_sol": fnum(r.get("marketCapSol") or r.get("entry_market_cap_sol"), 0.0),
            })
            if limit and len(candidates) >= limit:
                return candidates
    if candidates:
        return candidates
    by_mint = defaultdict(list)
    for e in read_jsonl(events_path):
        mint = e.get("mint") or ""
        if mint:
            by_mint[mint].append(e)
    for mint, rows in by_mint.items():
        first_bt = min((inum(r.get("timestamp") or r.get("block_time"), 0) for r in rows if r.get("timestamp") or r.get("block_time")), default=0)
        candidates.append({"mint": mint, "first_block_time_hint": first_bt, "entry_market_cap_sol": fnum(rows[0].get("entry_market_cap_sol"), 0.0)})
        if limit and len(candidates) >= limit:
            break
    return candidates


def load_wallet_classes(path):
    out = {}
    p = Path(path)
    if not p.exists():
        return out
    with p.open(newline="") as f:
        for r in csv.DictReader(f):
            wallet = r.get("wallet") or r.get("owner") or ""
            cls = r.get("classification") or r.get("category") or "UNKNOWN"
            if wallet:
                out[wallet] = cls
    return out


def load_outcomes(path="state.jsonl"):
    latest = {}
    for r in read_jsonl(path):
        mint = r.get("mint") or ""
        if not mint:
            continue
        latest[mint] = r
    out = {}
    for mint, r in latest.items():
        reason = r.get("exit_reason") or r.get("live_exit_reason") or ""
        status = r.get("status") or ""
        bad = status == "unrecoverable_dust_or_rug" or reason in {"hard_stop", "rug_guard", "price_unavailable"} or "dust_or_rug" in reason
        good = r.get("realized_pnl_sol", 0) and fnum(r.get("realized_pnl_sol")) > 0
        out[mint] = {"status": status, "exit_reason": reason, "bad_outcome": bool(bad), "good_outcome": bool(good)}
    return out


def find_funding_source_from_rows(rows, buyer, buy_time, min_sol=0.001):
    best = None
    for row in rows:
        tx = unwrap_tx(row)
        bt = tx_block_time(row)
        if not bt or bt > buy_time:
            continue
        keys = [k.get("pubkey") for k in account_keys(tx)]
        if buyer not in keys:
            continue
        buyer_delta = native_sol_delta_for(tx, buyer)
        if buyer_delta < min_sol:
            continue
        source = ""
        source_delta = 0.0
        for k in keys:
            if not k or k == buyer or k in SYSTEM_ACCOUNTS:
                continue
            d = native_sol_delta_for(tx, k)
            if d < source_delta:
                source_delta = d
                source = k
        row_best = {
            "mother_wallet": source,
            "funding_signature": tx_signature(row),
            "funding_time": bt,
            "funding_age_before_buy_secs": max(0, buy_time - bt),
            "funding_sol": round(buyer_delta, 12),
            "source_sol_delta": round(source_delta, 12),
        }
        if not best or row_best["funding_sol"] > best["funding_sol"]:
            best = row_best
    return best or {
        "mother_wallet": "",
        "funding_signature": "",
        "funding_time": 0,
        "funding_age_before_buy_secs": 0,
        "funding_sol": 0.0,
        "source_sol_delta": 0.0,
    }


def classify_cluster(early_buyers, edges, wallet_classes, outcome):
    buyer_count = len({b["owner"] for b in early_buyers})
    if buyer_count == 0:
        return "UNKNOWN", 0.0, 0.0, 0.0, []

    slots = [b.get("slot", 0) for b in early_buyers if b.get("slot")]
    times = [b.get("timestamp", 0) for b in early_buyers if b.get("timestamp")]
    buys = [b.get("quote_delta_sol", 0.0) for b in early_buyers if b.get("quote_delta_sol", 0.0) > 0]
    slot_span = max(slots) - min(slots) if len(slots) >= 2 else 999999
    time_span = max(times) - min(times) if len(times) >= 2 else 999999
    avg_buy = sum(buys) / len(buys) if buys else 0.0
    similar_buys = sum(1 for x in buys if avg_buy and abs(x - avg_buy) / avg_buy <= 0.35)

    mother_counts = Counter(e.get("mother_wallet") for e in edges if e.get("mother_wallet"))
    top_mothers = [{"mother_wallet": m, "buyer_count": c} for m, c in mother_counts.most_common(5)]
    shared_mother_count = top_mothers[0]["buyer_count"] if top_mothers else 0

    bundle_score = 0.0
    if buyer_count >= 2:
        bundle_score += 20
    if buyer_count >= 3:
        bundle_score += 15
    if slot_span <= 1 or time_span <= 2:
        bundle_score += 25
    elif time_span <= 5:
        bundle_score += 15
    if buys and similar_buys >= max(2, math.ceil(len(buys) * 0.6)):
        bundle_score += 15
    if shared_mother_count >= 2:
        bundle_score += 25
    bundle_score = min(100.0, bundle_score)

    mother_score = min(100.0, shared_mother_count / max(1, buyer_count) * 100.0)
    good_buyers = sum(1 for b in early_buyers if wallet_classes.get(b["owner"]) in {"GOOD_SNIPER", "GOOD_FLIP_SNIPER"})
    fast_dumpers = sum(1 for b in early_buyers if wallet_classes.get(b["owner"]) in {"FAST_DUMPER", "DEV_SNIPER_SUSPECT"})
    bad_outcome = bool(outcome.get("bad_outcome"))
    good_outcome = bool(outcome.get("good_outcome"))

    risk_score = 0.0
    risk_score += mother_score * 0.45
    risk_score += min(35.0, fast_dumpers * 15.0)
    if bad_outcome:
        risk_score += 20.0
    risk_score = min(100.0, risk_score)

    follow_score = 0.0
    follow_score += min(50.0, good_buyers * 20.0)
    follow_score += max(0.0, 30.0 - mother_score * 0.3)
    if good_outcome:
        follow_score += 20.0
    if risk_score >= 60:
        follow_score *= 0.4
    follow_score = min(100.0, follow_score)

    if shared_mother_count >= 3:
        cls = "DEV_SNIPER_SUSPECT" if risk_score >= 60 or bad_outcome else "SHARED_MOTHER_CLUSTER"
    elif shared_mother_count >= 2:
        cls = "SHARED_MOTHER_CLUSTER"
    elif bundle_score >= 65:
        cls = "BUNDLE_LIKELY"
    elif bundle_score >= 40:
        cls = "BUNDLE_POSSIBLE"
    elif good_buyers >= 2 and risk_score < 50:
        cls = "GOOD_SNIPER_CLUSTER"
    elif buyer_count >= 2:
        cls = "INDEPENDENT_BUYERS"
    else:
        cls = "UNKNOWN"
    return cls, round(bundle_score, 4), round(mother_score, 4), round(risk_score, 4), top_mothers, round(follow_score, 4)


def write_clusters_csv(path, clusters):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    fields = [
        "mint", "early_buyer_count", "shared_mother_count", "bundle_classification",
        "bundle_score", "mother_score", "risk_score", "follow_score", "top_mother_wallets",
        "bad_outcome", "good_outcome",
    ]
    with path.open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=fields)
        w.writeheader()
        for r in clusters:
            w.writerow({k: json.dumps(r.get(k)) if k == "top_mother_wallets" else r.get(k, "") for k in fields})


def build_for_mint(rpc, candidate, args, wallet_classes, outcomes):
    mint = candidate["mint"]
    hint = inum(candidate.get("first_block_time_hint"), 0)
    start = hint - 5 if hint else None
    end = hint + args.max_age_secs if hint else None
    txs = gtfa_fetch(rpc, mint, start_time=start, end_time=end, limit=args.page_limit, max_pages=args.max_pages)
    first_bt = hint or (tx_block_time(txs[0]) if txs else 0)
    events = []
    for tx in txs:
        events.extend(extract_mint_trade_events(tx, mint, first_bt))
    early_buyers = [e for e in events if e["side"] == "buy" and e["age_secs"] <= args.early_window_sec and e["quote_delta_sol"] >= args.min_buy_sol]
    by_owner = {}
    for e in sorted(early_buyers, key=lambda r: (r.get("timestamp", 0), r.get("signature", ""))):
        by_owner.setdefault(e["owner"], e)
    early_unique = list(by_owner.values())

    edges = []
    for b in early_unique:
        buyer = b["owner"]
        buy_time = b["timestamp"]
        fund_rows = gtfa_fetch(
            rpc,
            buyer,
            start_time=buy_time - args.funding_lookback_min * 60,
            end_time=buy_time,
            limit=args.funding_page_limit,
            max_pages=args.funding_max_pages,
        )
        funding = find_funding_source_from_rows(fund_rows, buyer, buy_time, min_sol=args.min_funding_sol)
        edge = {
            "mint": mint,
            "buyer_wallet": buyer,
            "buy_signature": b["signature"],
            "buy_slot": b.get("slot", 0),
            "buy_time": buy_time,
            "buy_age_secs": b.get("age_secs", 0),
            "buy_sol": b.get("quote_delta_sol", 0.0),
            "buyer_classification": wallet_classes.get(buyer, "UNKNOWN"),
            **funding,
        }
        edges.append(edge)

    outcome = outcomes.get(mint, {})
    cls, bundle_score, mother_score, risk_score, top_mothers, follow_score = classify_cluster(early_unique, edges, wallet_classes, outcome)
    signal = {
        "mint": mint,
        "early_buyer_count": len(early_unique),
        "shared_mother_count": top_mothers[0]["buyer_count"] if top_mothers else 0,
        "top_mother_wallets": top_mothers,
        "bundle_classification": cls,
        "bundle_score": bundle_score,
        "mother_score": mother_score,
        "risk_score": risk_score,
        "follow_score": follow_score,
        "bad_outcome": bool(outcome.get("bad_outcome")),
        "good_outcome": bool(outcome.get("good_outcome")),
        "exit_reason": outcome.get("exit_reason", ""),
        "status": outcome.get("status", ""),
        "live_allowed": False,
    }
    return events, edges, signal


def run(args):
    if args.dry_run and not args.rpc_url:
        return {"dry_run": True, "rpc_missing": True, "processed_mints": 0, "live_allowed": False}
    if not args.rpc_url:
        raise SystemExit("RPC_URL missing; pass --rpc-url or source .env")
    rpc = Rpc(args.rpc_url, sleep_s=args.rpc_sleep)
    candidates = load_candidates(args.input, args.events_input, args.limit_mints)
    wallet_classes = load_wallet_classes(args.wallet_scores)
    outcomes = load_outcomes(args.state)
    all_edges = []
    signals = []
    processed = 0
    early_clusters = 0
    for c in candidates:
        if args.limit_mints and processed >= args.limit_mints:
            break
        try:
            _events, edges, signal = build_for_mint(rpc, c, args, wallet_classes, outcomes)
            all_edges.extend(edges)
            signals.append(signal)
            processed += 1
            if signal["early_buyer_count"] >= 2:
                early_clusters += 1
        except Exception as e:
            if not args.dry_run:
                append_jsonl(args.errors, {"mode": "bundler_funding", "mint": c.get("mint", ""), "reason": str(e)})

    clusters = signals
    if not args.dry_run:
        write_jsonl(args.out_edges, all_edges)
        write_clusters_csv(args.out_clusters, clusters)
        write_jsonl(args.out_signals, signals)
    err_total = sum(rpc.errors.values())
    return {
        "processed_mints": processed,
        "early_buyer_clusters": early_clusters,
        "funding_edges": len(all_edges),
        "signals": len(signals),
        "shared_mother_clusters": sum(1 for s in signals if s.get("shared_mother_count", 0) >= 2),
        "rpc_calls": rpc.calls,
        "errors_total": err_total,
        "errors": dict(rpc.errors),
        "error_pct": round(err_total / max(1, rpc.calls) * 100, 4),
        "dry_run": args.dry_run,
        "live_allowed": False,
        "out_edges": args.out_edges,
        "out_clusters": args.out_clusters,
        "out_signals": args.out_signals,
    }


def self_test():
    buyers = [
        {"owner": "B1", "slot": 10, "timestamp": 100, "quote_delta_sol": 0.01},
        {"owner": "B2", "slot": 10, "timestamp": 101, "quote_delta_sol": 0.011},
        {"owner": "B3", "slot": 11, "timestamp": 101, "quote_delta_sol": 0.012},
    ]
    edges = [{"buyer_wallet": b["owner"], "mother_wallet": "MOTHER"} for b in buyers]
    cls, *_ = classify_cluster(buyers, edges, {}, {})
    assert cls in {"SHARED_MOTHER_CLUSTER", "DEV_SNIPER_SUSPECT"}, cls

    indep_edges = [{"buyer_wallet": "A", "mother_wallet": "MA"}, {"buyer_wallet": "B", "mother_wallet": "MB"}]
    indep_buyers = [
        {"owner": "A", "slot": 1, "timestamp": 100, "quote_delta_sol": 0.02},
        {"owner": "B", "slot": 20, "timestamp": 130, "quote_delta_sol": 0.05},
    ]
    cls, *_ = classify_cluster(indep_buyers, indep_edges, {}, {})
    assert cls == "INDEPENDENT_BUYERS", cls

    unknown_cls, *_ = classify_cluster([], [], {}, {})
    assert unknown_cls == "UNKNOWN"
    print("SELF_TEST_OK")


def main():
    ap = argparse.ArgumentParser(description="Build bundler/mother-wallet funding graph datasets. Shadow-only.")
    ap.add_argument("--input", default="fresh_momentum_candidates.jsonl")
    ap.add_argument("--events-input", default="datasets/sniper_trade_events.jsonl")
    ap.add_argument("--wallet-scores", default="datasets/sniper_wallet_scores.csv")
    ap.add_argument("--state", default="state.jsonl")
    ap.add_argument("--rpc-url", default=load_dotenv_value("RPC_URL", ""))
    ap.add_argument("--out-edges", default=DEFAULT_OUT_EDGES)
    ap.add_argument("--out-clusters", default=DEFAULT_OUT_CLUSTERS)
    ap.add_argument("--out-signals", default=DEFAULT_OUT_SIGNALS)
    ap.add_argument("--errors", default=DEFAULT_ERRORS)
    ap.add_argument("--limit-mints", type=int, default=0)
    ap.add_argument("--page-limit", type=int, default=1000)
    ap.add_argument("--max-pages", type=int, default=1)
    ap.add_argument("--funding-page-limit", type=int, default=100)
    ap.add_argument("--funding-max-pages", type=int, default=1)
    ap.add_argument("--max-age-secs", type=int, default=60)
    ap.add_argument("--early-window-sec", type=int, default=10)
    ap.add_argument("--funding-lookback-min", type=int, default=60)
    ap.add_argument("--min-buy-sol", type=float, default=0.01)
    ap.add_argument("--min-funding-sol", type=float, default=0.001)
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
