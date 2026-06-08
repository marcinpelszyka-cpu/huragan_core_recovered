#!/usr/bin/env python3
"""Fresh Sniper Collector — discover fresh Pump tokens near target MC and extract early buyers.

Phase 1: Discover fresh tokens via PumpPortal REST API
Phase 2: For each matching token, fetch early transactions via Helius gTFA
Phase 3: Extract early buyer wallet data

Environment:
  Requires Helius RPC key from /opt/huragan_core/.env (RPC_SEND_URL or RPC_URL).
  Read-only. No live execution.

Usage:
  python3 scripts/fresh_sniper_collector.py [--limit N] [--self-test]
"""

import argparse
import json
import sys
import time
from collections import defaultdict
from pathlib import Path
from urllib import request as urlrequest, error as urllib_error

PROJECT = Path(__file__).resolve().parent.parent
PUMPPORTAL_API = "https://frontend-api.pump.fun"
DATASET_DIR = PROJECT / "datasets"
OUT = DATASET_DIR / "fresh_sniper_events.jsonl"

# Target: ~3500 USD MC ≈ 22-28 SOL (price varies)
MC_TARGET_SOL = 25.0
MC_BAND_PCT = 40  # ±40% band
MC_MIN = MC_TARGET_SOL * (1 - MC_BAND_PCT / 100)  # 15 SOL
MC_MAX = MC_TARGET_SOL * (1 + MC_BAND_PCT / 100)  # 35 SOL
MAX_AGE_SECS = 60

SYSTEM_ACCOUNTS = {
    "11111111111111111111111111111111",
    "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA",
    "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb",
    "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL",
    "So11111111111111111111111111111111111111112",
    "ComputeBudget111111111111111111111111111111",
    "pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA",
}


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
    sys.exit("FATAL: no api-key found")


def fetch_fresh_tokens(limit=50):
    """Fetch recently graduated tokens from PumpPortal."""
    url = f"{PUMPPORTAL_API}/coins?limit={limit}&sort=created&includeNsfw=false"
    try:
        req = urlrequest.Request(url, headers={"User-Agent": "huragan/1.0"})
        with urlrequest.urlopen(req, timeout=15) as resp:
            data = json.loads(resp.read())
    except Exception as e:
        print(f"PumpPortal API error: {e}", file=sys.stderr)
        return []

    tokens = []
    now = time.time()
    for coin in data if isinstance(data, list) else data.get("coins", []):
        mint = coin.get("mint", "")
        if not mint:
            continue
        created_at = coin.get("created_timestamp", 0) / 1000 if coin.get("created_timestamp") else 0
        age = now - created_at if created_at else 999
        mc_sol = float(coin.get("market_cap", 0) or 0)
        pool_state = coin.get("raydium_pool", "") or coin.get("bonding_curve", "") or ""

        tokens.append({
            "mint": mint,
            "pool_state": pool_state,
            "market_cap_sol": mc_sol,
            "age_secs": round(age, 1),
            "created_at": int(created_at),
            "name": coin.get("name", ""),
            "symbol": coin.get("symbol", ""),
            "description": coin.get("description", ""),
        })

    return tokens


def filter_fresh_targets(tokens):
    """Filter tokens to those within MC band and age window."""
    return [
        t for t in tokens
        if MC_MIN <= t["market_cap_sol"] <= MC_MAX
        and 0 < t["age_secs"] <= MAX_AGE_SECS
        and t["pool_state"]  # must have a pool
    ]


def rpc_call(api_key, method, params=None):
    url = f"https://beta.helius-rpc.com/?api-key={api_key}"
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params or []})
    req = urlrequest.Request(url, body.encode(), {"Content-Type": "application/json"})
    with urlrequest.urlopen(req, timeout=30) as resp:
        return json.loads(resp.read())


def gfta_transactions(api_key, address, limit=200):
    """Fetch transactions for pool_state via Helius getTransactionsForAddress."""
    params = [
        address,
        {
            "transactionDetails": "full",
            "sortOrder": "asc",
            "limit": min(limit, 1000),
            "filters": {"status": "succeeded"},
            "encoding": "jsonParsed",
            "maxSupportedTransactionVersion": 0,
        },
    ]
    return rpc_call(api_key, "getTransactionsForAddress", params)


def parse_early_buyers(tx_data) -> list[dict]:
    """Extract buyers from pre/post token balances."""
    meta = tx_data.get("meta", {})
    pre = meta.get("preTokenBalances", []) or []
    post = meta.get("postTokenBalances", []) or []

    lookup = {}
    for entry in pre:
        key = (entry.get("accountIndex"), entry.get("mint"))
        lookup[key] = {
            "pre": float(entry.get("uiTokenAmount", {}).get("amount", 0)),
            "owner": entry.get("owner", ""),
        }
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

    buyers = []
    for (idx, token_mint), vals in lookup.items():
        post_amt = vals.get("post", 0.0)
        pre_amt = vals.get("pre", 0.0)
        delta = post_amt - pre_amt
        owner = vals.get("owner", "")
        if delta <= 0 or not owner:
            continue
        if owner in SYSTEM_ACCOUNTS or token_mint in SYSTEM_ACCOUNTS:
            continue
        buyers.append({
            "account_index": idx,
            "mint": token_mint,
            "owner": owner,
            "token_delta_raw": delta,
        })
    return buyers


def sol_delta_for_account(tx_data, account_index: int) -> float:
    """SOL delta for an account index."""
    meta = tx_data.get("meta", {})
    pre_bal = meta.get("preBalances", []) or []
    post_bal = meta.get("postBalances", []) or []
    pre = pre_bal[account_index] if account_index < len(pre_bal) else 0
    post = post_bal[account_index] if account_index < len(post_bal) else 0
    return (post - pre) / 1e9


def process_fresh_token(api_key, token_info, pool_state_addr, base_vault, quote_vault):
    """Process one fresh token: fetch transactions, extract early buyers."""
    events = []
    try:
        resp = gfta_transactions(api_key, pool_state_addr, limit=100)
        txs = resp.get("result", {}).get("data", [])
    except Exception as e:
        print(f"  gTFA error: {e}", file=sys.stderr)
        return events

    pool_vaults = {pool_state_addr, base_vault, quote_vault}

    for tx in txs:
        sig = tx.get("signature", "")
        slot = tx.get("slot", 0)
        block_time = tx.get("blockTime")

        buyers = parse_early_buyers(tx)
        for b in buyers:
            owner = b["owner"]
            if owner in pool_vaults:
                continue
            sol_delta = sol_delta_for_account(tx, b["account_index"])
            events.append({
                "mint": token_info["mint"],
                "pool_state": pool_state_addr,
                "signature": sig,
                "slot": slot,
                "block_time": block_time,
                "owner": owner,
                "token_delta_raw": b["token_delta_raw"],
                "buy_sol": round(abs(sol_delta), 9) if sol_delta < 0 else 0.001,
                "side": "buy",
                "token_age_at_buy": round(token_info["age_secs"], 1),
                "token_mc_sol": token_info["market_cap_sol"],
            })

    return events


def resolve_pool(api_key, mint):
    """Resolve pool_state for a fresh token via Helius getTokenAccounts."""
    try:
        resp = rpc_call(api_key, "getTokenAccountsByOwner", [
            mint,
            {"programId": "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"},
            {"encoding": "jsonParsed"},
        ])
    except Exception:
        return {"pool_state": "", "base_vault": "", "quote_vault": ""}

    result = {"pool_state": "", "base_vault": "", "quote_vault": ""}
    # For Pump tokens, the pool is typically a PDA from pAMM
    # We fall back to searching by the mint's largest holder
    try:
        resp = rpc_call(api_key, "getTokenLargestAccounts", [mint])
        accounts = resp.get("result", {}).get("value", [])
        if accounts:
            # The pool/liquidity account is usually the largest holder
            result["pool_state"] = accounts[0].get("address", "")
    except Exception:
        pass

    return result


def self_test():
    print("=== SELF-TEST: fresh_sniper_collector ===")
    print("MC target: {} SOL, band: ±{}%, window: <{}s".format(MC_TARGET_SOL, MC_BAND_PCT, MAX_AGE_SECS))
    print(f"MC range: {MC_MIN:.1f} – {MC_MAX:.1f} SOL")

    # Test filtering logic
    test_tokens = [
        {"mint": "A", "pool_state": "P1", "market_cap_sol": 25.0, "age_secs": 30},
        {"mint": "B", "pool_state": "P2", "market_cap_sol": 10.0, "age_secs": 20},  # too low MC
        {"mint": "C", "pool_state": "P3", "market_cap_sol": 30.0, "age_secs": 90},  # too old
        {"mint": "D", "pool_state": "", "market_cap_sol": 22.0, "age_secs": 15},     # no pool
        {"mint": "E", "pool_state": "P5", "market_cap_sol": 20.0, "age_secs": 5},    # good
    ]
    matches = filter_fresh_targets(test_tokens)
    mints = [t["mint"] for t in matches]
    assert mints == ["A", "E"], f"Expected [A, E], got {mints}"
    print(f"  filter: OK ({len(matches)}/{len(test_tokens)} passed)")

    print("ALL SELF-TESTS PASSED")


def main():
    ap = argparse.ArgumentParser(description="Fresh Sniper Collector")
    ap.add_argument("--limit", type=int, default=30, help="Max tokens to scan")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--target-mc", type=float, default=MC_TARGET_SOL, help="Target MC in SOL")
    args = ap.parse_args()

    if args.self_test:
        self_test()
        return

    mc_target = args.target_mc
    mc_min = mc_target * (1 - MC_BAND_PCT / 100)
    mc_max = mc_target * (1 + MC_BAND_PCT / 100)

    api_key = load_api_key()
    print(f"API key loaded ({len(api_key)} chars)")

    # 1. Discover fresh tokens
    print(f"\nFetching fresh tokens from PumpPortal...")
    tokens = fetch_fresh_tokens(args.limit * 2)
    print(f"Fetched {len(tokens)} tokens")

    targets = filter_fresh_targets(tokens)
    print(f"Targets matching MC [{mc_min:.0f}-{mc_max:.0f}] SOL, age < {MAX_AGE_SECS}s: {len(targets)}")
    for t in targets[:5]:
        print(f"  {t['mint'][:12]}... MC={t['market_cap_sol']:.1f} SOL age={t['age_secs']}s")

    if not targets:
        print("No targets found. PumpPortal may not have fresh tokens in this MC band right now.")
        return

    # 2. For each target, resolve pool and collect early buyers
    all_events = []
    for i, t in enumerate(targets[:args.limit]):
        print(f"\n[{i+1}/{min(len(targets), args.limit)}] {t['mint'][:12]}...")

        if t["pool_state"]:
            ps = t["pool_state"]
        else:
            pool = resolve_pool(api_key, t["mint"])
            ps = pool["pool_state"] or ""
            if not ps:
                print(f"  skip: no pool state")
                continue

        events = process_fresh_token(api_key, t, ps, "", "")
        all_events.extend(events)
        print(f"  → {len(events)} early buys")
        time.sleep(0.1)

    # Write output
    DATASET_DIR.mkdir(parents=True, exist_ok=True)
    with open(OUT, "w") as f:
        for ev in all_events:
            f.write(json.dumps(ev) + "\n")

    print(f"\nWrote {len(all_events)} events → {OUT}")
    buyers = len({e["owner"] for e in all_events})
    print(f"Unique early buyers: {buyers}")


if __name__ == "__main__":
    main()
