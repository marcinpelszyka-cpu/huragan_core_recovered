#!/usr/bin/env python3
"""Sniper Follow Backtest — backfill sniper trade events via Helius getTransactionsForAddress.

Phase 1: data collection.
Phase 2: sniper detection.
Phase 3: wallet ranking (separate script).

Environment:
  Requires Helius RPC key from /opt/huragan_core/.env (RPC_SEND_URL or RPC_URL).
  No live execution. Read-only.

Usage:
  python3 scripts/sniper_follow_backtest.py [--limit N] [--self-test]
"""

import argparse
import json
import os
import sys
import time
from collections import defaultdict
from pathlib import Path
from urllib import request, error as urllib_error

PROJECT = Path(__file__).resolve().parent.parent
STATE = PROJECT / "state.jsonl"
OUT = PROJECT / "datasets" / "sniper_trade_events.jsonl"

# Solana system accounts to exclude
SYSTEM_ACCOUNTS = {
    "11111111111111111111111111111111",
    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
    "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
    "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
    "So11111111111111111111111111111111111111112",  # WSOL mint
    "ComputeBudget111111111111111111111111111111",
    "Sysvar1111111111111111111111111111111111111",
    "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
}

PUMP_AMM_PROGRAM = "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA"


def load_api_key():
    env_path = PROJECT / ".env"
    if not env_path.exists():
        sys.exit("FATAL: .env not found")
    env = {}
    for line in env_path.read_text(errors="ignore").splitlines():
        if "=" in line and not line.strip().startswith("#"):
            k, v = line.split("=", 1)
            env[k.strip()] = v.strip().strip('"').strip("'")
    for key_var in ("RPC_SEND_URL", "RPC_URL"):
        url = env.get(key_var, "")
        for sep in ("?api-key=", "&api-key="):
            if sep in url:
                return url.split(sep)[1].split("&")[0]
    sys.exit("FATAL: no api-key found in RPC_SEND_URL or RPC_URL")


def rpc_call(api_key, method, params=None):
    url = f"https://beta.helius-rpc.com/?api-key={api_key}"
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params or []})
    req = request.Request(url, body.encode(), {"Content-Type": "application/json"})
    with request.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read())


def gfta(api_key, address, before_sig=None, limit=1000):
    """Fetch transactions for an address using getTransactionsForAddress."""
    params = [
        address,
        {
            "transactionDetails": "full",
            "sortOrder": "desc",
            "limit": min(limit, 1000),
            "filters": {"status": "succeeded"},
            "encoding": "jsonParsed",
            "maxSupportedTransactionVersion": 0,
        },
    ]
    if before_sig:
        params[1]["filters"]["signature"] = {"lt": before_sig}
    return rpc_call(api_key, "getTransactionsForAddress", params)


def parse_token_deltas(tx_data) -> list[dict]:
    """Extract per-wallet token balance changes from pre/post token balances."""
    meta = tx_data.get("meta", {})
    pre = meta.get("preTokenBalances", []) or []
    post = meta.get("postTokenBalances", []) or []
    if not pre and not post:
        return []

    # Build lookup: (accountIndex, mint) -> {pre_amount, post_amount, owner}
    lookup = {}
    for entry in pre:
        key = (entry.get("accountIndex"), entry.get("mint"))
        owner = entry.get("owner", "")
        lookup[key] = {"pre": float(entry.get("uiTokenAmount", {}).get("amount", 0)), "owner": owner}
    for entry in post:
        key = (entry.get("accountIndex"), entry.get("mint"))
        owner = entry.get("owner", "")
        amt = float(entry.get("uiTokenAmount", {}).get("amount", 0))
        if key in lookup:
            lookup[key]["post"] = amt
            if not lookup[key].get("owner"):
                lookup[key]["owner"] = owner
        else:
            lookup[key] = {"pre": 0.0, "post": amt, "owner": owner}

    deltas = []
    for (idx, mint), vals in lookup.items():
        pre_amt = vals.get("pre", 0.0)
        post_amt = vals.get("post", 0.0)
        delta = post_amt - pre_amt
        owner = vals.get("owner", "")
        if delta == 0 or not owner:
            continue
        if owner in SYSTEM_ACCOUNTS or mint in SYSTEM_ACCOUNTS:
            continue
        side = "buy" if delta > 0 else "sell"
        deltas.append({
            "account_index": idx,
            "mint": mint,
            "owner": owner,
            "token_delta_raw": abs(delta),
            "side": side,
        })
    return deltas


def parse_sol_deltas(tx_data) -> dict[int, float]:
    """Extract SOL balance changes from pre/post balances (non-token)."""
    meta = tx_data.get("meta", {})
    pre_bal = meta.get("preBalances", []) or []
    post_bal = meta.get("postBalances", []) or []
    deltas = {}
    for i in range(max(len(pre_bal), len(post_bal))):
        pre = pre_bal[i] if i < len(pre_bal) else 0
        post = post_bal[i] if i < len(post_bal) else 0
        delta = post - pre
        if delta != 0:
            deltas[i] = delta / 1e9
    return deltas


def pool_vaults_from_state(pool_state: str) -> tuple[str, str]:
    """Return (base_vault, quote_vault) from state.jsonl for a given pool_state."""
    if not STATE.exists():
        return "", ""
    with open(STATE) as f:
        for line in f:
            if pool_state in line and '"pool_state"' in line:
                try:
                    r = json.loads(line)
                    if r.get("pool_state") == pool_state:
                        return r.get("pool_base_token_account", ""), r.get("pool_quote_token_account", "")
                except Exception:
                    continue
    return "", ""


def is_pool_vault(owner: str, pool_state: str, base_vault: str, quote_vault: str) -> bool:
    return owner in (pool_state, base_vault, quote_vault)


def process_pool(api_key, pool_state, mint, base_vault, quote_vault, max_tx=500):
    """Process one pool: fetch transactions, extract buy/sell events."""
    events = []
    seen_txs = set()
    before_sig = None
    fetched = 0

    while fetched < max_tx:
        try:
            resp = gfta(api_key, pool_state, before_sig=before_sig, limit=min(1000, max_tx - fetched))
        except urllib_error.HTTPError as e:
            print(f"  HTTP {e.code} for {pool_state[:12]}", file=sys.stderr)
            break
        except Exception as e:
            print(f"  Error: {e}", file=sys.stderr)
            break

        txs = resp.get("result", {}).get("data", [])
        if not txs:
            break

        for tx in txs:
            sig = tx.get("signature", "")
            if sig in seen_txs:
                continue
            seen_txs.add(sig)

            slot = tx.get("slot", 0)
            block_time = tx.get("blockTime")
            token_deltas = parse_token_deltas(tx)
            sol_deltas = parse_sol_deltas(tx)

            for td in token_deltas:
                owner = td["owner"]
                if is_pool_vault(owner, pool_state, base_vault, quote_vault):
                    continue
                sol_delta = sol_deltas.get(td["account_index"], 0.0)
                events.append({
                    "mint": td["mint"] if td["mint"] != "So11111111111111111111111111111111111111112" else mint,
                    "pool_state": pool_state,
                    "signature": sig,
                    "slot": slot,
                    "block_time": block_time,
                    "owner": owner,
                    "token_delta_raw": td["token_delta_raw"],
                    "quote_delta_sol": abs(sol_delta) if sol_delta != 0 else 0.0,
                    "side": td["side"],
                })

        fetched += len(txs)
        if len(txs) < 100:
            break
        # Paginate: use last signature as cursor
        before_sig = txs[-1].get("signature")
        time.sleep(0.15)  # rate limit

    return events


def load_completed_pools(limit=20):
    """Load pools from state.jsonl that had a terminal Z3 lifecycle."""
    if not STATE.exists():
        return []
    latest = {}
    with open(STATE) as f:
        for line in f:
            if "pool_state" not in line:
                continue
            try:
                r = json.loads(line)
            except Exception:
                continue
            ps = r.get("pool_state", "")
            if not ps:
                continue
            latest[ps] = r

    pools = []
    for ps, r in latest.items():
        status = r.get("status", "")
        if status not in ("completed", "unrecoverable_dust_or_rug", "live_failed"):
            continue
        mint = r.get("mint", "") or r.get("quote_mint", "")
        if not mint:
            continue
        pools.append({
            "pool_state": ps,
            "mint": mint,
            "base_vault": r.get("pool_base_token_account", ""),
            "quote_vault": r.get("pool_quote_token_account", ""),
            "status": status,
            "pnl_sol": r.get("net_pnl_sol", 0) or r.get("realized_pnl_sol", 0),
        })
    return sorted(pools, key=lambda p: abs(p.get("pnl_sol", 0)), reverse=True)[:limit]


def self_test():
    print("=== SELF-TEST: sniper_follow_backtest ===")
    print("Testing token delta parser...")

    # Simulated tx with 2 accounts: SOL vault and sniper wallet
    fake_tx = {
        "meta": {
            "preTokenBalances": [
                {"accountIndex": 0, "mint": "So11111111111111111111111111111111111111112", "owner": "PoolVaultAAAA", "uiTokenAmount": {"amount": "85000000000"}},
                {"accountIndex": 1, "mint": "MintAAAAAAAABBBBBBBBBBBBBBBBCCCCCCCCCCCC", "owner": "Sniper111111", "uiTokenAmount": {"amount": "0"}},
            ],
            "postTokenBalances": [
                {"accountIndex": 0, "mint": "So11111111111111111111111111111111111111112", "owner": "PoolVaultAAAA", "uiTokenAmount": {"amount": "85300000000"}},
                {"accountIndex": 1, "mint": "MintAAAAAAAABBBBBBBBBBBBBBBBCCCCCCCCCCCC", "owner": "Sniper111111", "uiTokenAmount": {"amount": "500000000000"}},
            ],
            "preBalances": [100000000, 5000000000],
            "postBalances": [97000000, 4997000000],
        }
    }

    deltas = parse_token_deltas(fake_tx)
    # Pool WSOL vault gets filtered (system account), sniper token deltas remain
    assert len(deltas) >= 1, f"Expected at least 1 delta, got {len(deltas)}"
    
    # Sniper buy: tokens increased
    sniper = [d for d in deltas if d["owner"] == "Sniper111111"]
    assert len(sniper) == 1, f"Sniper not found: {deltas}"
    assert sniper[0]["side"] == "buy", f"Expected buy, got {sniper[0]['side']}"
    assert sniper[0]["token_delta_raw"] == 500000000000

    # WSOL should be filtered (system account via mint)
    pool_wsol = [d for d in deltas if "So1111" in d.get("mint", "")]
    assert len(pool_wsol) == 0, f"WSOL mint should be excluded, got {pool_wsol}"

    print("  token delta parser: OK")

    # Test pool vault detection
    assert is_pool_vault("PoolVaultAAAA", "PoolVaultAAAA", "BaseVault", "QuoteVault") is True
    assert is_pool_vault("Sniper111111", "PoolVaultAAAA", "BaseVault", "QuoteVault") is False
    print("  pool vault detection: OK")

    print("ALL SELF-TESTS PASSED")


def main():
    ap = argparse.ArgumentParser(description="Sniper Follow Backtest")
    ap.add_argument("--limit", type=int, default=20, help="Max pools to backfill")
    ap.add_argument("--self-test", action="store_true", help="Run self-test only")
    ap.add_argument("--pool-state", type=str, help="Single pool state to analyze")
    ap.add_argument("--mint", type=str, help="Mint for single-pool mode")
    args = ap.parse_args()

    if args.self_test:
        self_test()
        return

    api_key = load_api_key()
    print(f"Helius API key loaded ({len(api_key)} chars)")

    OUT.parent.mkdir(parents=True, exist_ok=True)

    if args.pool_state and args.mint:
        pools = [{
            "pool_state": args.pool_state,
            "mint": args.mint,
            "base_vault": "",
            "quote_vault": "",
            "status": "manual",
            "pnl_sol": 0,
        }]
    else:
        pools = load_completed_pools(args.limit)

    print(f"Processing {len(pools)} pools...")

    all_events = []
    for i, pool in enumerate(pools):
        ps = pool["pool_state"]
        mint = pool["mint"]
        bv, qv = pool.get("base_vault"), pool.get("quote_vault")
        print(f"[{i+1}/{len(pools)}] Pool {ps[:12]}... mint={mint[:12]}")
        events = process_pool(api_key, ps, mint, bv or "", qv or "", max_tx=500)
        all_events.extend(events)
        print(f"  → {len(events)} events")
        time.sleep(0.1)

    # Write output
    with open(OUT, "w") as f:
        for ev in all_events:
            f.write(json.dumps(ev) + "\n")

    print(f"\nWrote {len(all_events)} events → {OUT}")

    # Quick stats
    buyers = len({e["owner"] for e in all_events if e["side"] == "buy"})
    sellers = len({e["owner"] for e in all_events if e["side"] == "sell"})
    print(f"Unique buyers: {buyers}, unique sellers: {sellers}")


if __name__ == "__main__":
    main()
