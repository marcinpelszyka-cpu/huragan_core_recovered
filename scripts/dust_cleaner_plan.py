#!/usr/bin/env python3
"""Solana wallet dust/WSOL cleanup planner for Huragan burner.

Read-only by default. It never signs or broadcasts transactions.

What it does:
- reads RPC URL from .env without printing it
- reads wallet public key from wallet env, or --owner
- lists SOL on main wallet
- lists SPL Token / Token-2022 accounts
- estimates reclaimable SOL from:
  * WSOL native token accounts (unwrap/close)
  * empty token accounts (close account rent)
- lists non-zero non-WSOL token dust that cannot be closed until sold/burned/transferred
- optionally asks Jupiter quote API for dust sell estimates (--quote-dust)

Executor intentionally separate: any signing/broadcast requires an explicit operator GO.
"""
from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.parse
import urllib.request
from dataclasses import dataclass, asdict
from pathlib import Path
from typing import Any

WSOL_MINT = "So11111111111111111111111111111111111111112"
TOKEN_PROGRAM = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"
TOKEN_2022_PROGRAM = "TokenzQdYwCW7DkRDm6xPp8UDxFgZk1VtZ1wwSuSBT8"
DEFAULT_WALLET_ENV = "/root/.huragan_wallets/huragan_new_wallet_20260604_003229.env"
LAMPORTS = 1_000_000_000


@dataclass
class CleanupAction:
    kind: str
    program_id: str
    token_account: str
    mint: str
    amount_raw: str
    amount_ui: float | None
    recover_lamports: int
    note: str


@dataclass
class DustToken:
    program_id: str
    token_account: str
    mint: str
    amount_raw: str
    amount_ui: float | None
    decimals: int
    rent_lamports: int
    jupiter_out_lamports: int | None = None
    jupiter_error: str | None = None


def eprint(*args: Any) -> None:
    print(*args, file=sys.stderr)


def read_env(path: str | Path) -> dict[str, str]:
    out: dict[str, str] = {}
    p = Path(path)
    if not p.exists():
        return out
    for line in p.read_text(errors="ignore").splitlines():
        s = line.strip()
        if not s or s.startswith("#") or "=" not in s:
            continue
        k, v = s.split("=", 1)
        out[k.strip()] = v.strip().strip('"').strip("'")
    return out


def project_rpc_url(project_env: str = ".env") -> str:
    env = read_env(project_env)
    for k in ("RPC_SEND_URL", "HELIUS_RPC_URL", "RPC_URL"):
        if env.get(k):
            return env[k]
    raise SystemExit("RPC URL not found in .env (RPC_SEND_URL/HELIUS_RPC_URL/RPC_URL)")


def owner_from_wallet_env(wallet_env: str) -> str:
    env = read_env(wallet_env)
    if env.get("SOLANA_PUBLIC_KEY"):
        return env["SOLANA_PUBLIC_KEY"]
    raise SystemExit(f"SOLANA_PUBLIC_KEY missing in {wallet_env}; pass --owner explicitly")


def rpc(url: str, method: str, params: list[Any]) -> Any:
    req = urllib.request.Request(
        url,
        json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode(),
        {"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        data = json.loads(resp.read())
    if "error" in data:
        raise RuntimeError(f"RPC {method} error: {data['error']}")
    return data["result"]


def get_token_accounts(url: str, owner: str, program_id: str) -> list[dict[str, Any]]:
    result = rpc(
        url,
        "getTokenAccountsByOwner",
        [owner, {"programId": program_id}, {"encoding": "jsonParsed"}],
    )
    return result.get("value", [])


def quote_jupiter(input_mint: str, amount_raw: str, slippage_bps: int = 500) -> tuple[int | None, str | None]:
    if input_mint == WSOL_MINT or amount_raw == "0":
        return None, None
    params = urllib.parse.urlencode(
        {
            "inputMint": input_mint,
            "outputMint": WSOL_MINT,
            "amount": amount_raw,
            "slippageBps": str(slippage_bps),
            "onlyDirectRoutes": "false",
        }
    )
    url = f"https://quote-api.jup.ag/v6/quote?{params}"
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "huragan-dust-cleaner/1.0"})
        with urllib.request.urlopen(req, timeout=12) as resp:
            data = json.loads(resp.read())
        out = data.get("outAmount")
        return int(out) if out is not None else None, None
    except Exception as exc:  # noqa: BLE001 - report quote issue, continue plan
        return None, str(exc)[:180]


def short(addr: str) -> str:
    return f"{addr[:6]}...{addr[-4:]}" if len(addr) > 12 else addr


def analyze(url: str, owner: str, quote_dust: bool) -> dict[str, Any]:
    main_lamports = rpc(url, "getBalance", [owner])["value"]
    actions: list[CleanupAction] = []
    dust: list[DustToken] = []
    totals = {
        "main_lamports": main_lamports,
        "wsol_lamports": 0,
        "empty_account_rent_lamports": 0,
        "closeable_accounts": 0,
        "nonzero_dust_accounts": 0,
        "dust_quote_lamports": 0,
    }

    for program_id in (TOKEN_PROGRAM, TOKEN_2022_PROGRAM):
        try:
            accounts = get_token_accounts(url, owner, program_id)
        except Exception as exc:  # token-2022 may fail on old RPCs
            eprint(f"WARN: token accounts scan failed for {program_id}: {exc}")
            continue
        for item in accounts:
            pubkey = item["pubkey"]
            account = item["account"]
            lamports = int(account.get("lamports") or 0)
            parsed = account.get("data", {}).get("parsed", {})
            info = parsed.get("info", {})
            mint = info.get("mint", "")
            token_amount = info.get("tokenAmount", {})
            amount_raw = str(token_amount.get("amount", "0"))
            amount_int = int(amount_raw or "0")
            decimals = int(token_amount.get("decimals") or 0)
            amount_ui = token_amount.get("uiAmount")

            if mint == WSOL_MINT:
                # Closing native WSOL ATA unwraps amount + rent to owner.
                actions.append(
                    CleanupAction(
                        kind="unwrap_wsol_close_account",
                        program_id=program_id,
                        token_account=pubkey,
                        mint=mint,
                        amount_raw=amount_raw,
                        amount_ui=amount_ui,
                        recover_lamports=lamports,
                        note="close native WSOL account; recovers WSOL amount plus rent",
                    )
                )
                totals["wsol_lamports"] += lamports
                totals["closeable_accounts"] += 1
                continue

            if amount_int == 0:
                actions.append(
                    CleanupAction(
                        kind="close_empty_token_account",
                        program_id=program_id,
                        token_account=pubkey,
                        mint=mint,
                        amount_raw=amount_raw,
                        amount_ui=amount_ui,
                        recover_lamports=lamports,
                        note="empty token account; close to recover rent",
                    )
                )
                totals["empty_account_rent_lamports"] += lamports
                totals["closeable_accounts"] += 1
            else:
                d = DustToken(
                    program_id=program_id,
                    token_account=pubkey,
                    mint=mint,
                    amount_raw=amount_raw,
                    amount_ui=amount_ui,
                    decimals=decimals,
                    rent_lamports=lamports,
                )
                if quote_dust:
                    # Be gentle to public quote API.
                    time.sleep(0.15)
                    q, err = quote_jupiter(mint, amount_raw)
                    d.jupiter_out_lamports = q
                    d.jupiter_error = err
                    if q:
                        totals["dust_quote_lamports"] += q
                dust.append(d)
                totals["nonzero_dust_accounts"] += 1

    totals["recoverable_lamports"] = totals["wsol_lamports"] + totals["empty_account_rent_lamports"]
    totals["total_after_cleanup_est_lamports"] = main_lamports + totals["recoverable_lamports"]
    return {
        "owner": owner,
        "owner_short": short(owner),
        "totals": totals,
        "actions": [asdict(a) for a in actions],
        "dust_tokens": [asdict(d) for d in dust],
    }


def write_outputs(plan: dict[str, Any], out_json: str, out_md: str) -> None:
    Path(out_json).parent.mkdir(parents=True, exist_ok=True)
    Path(out_md).parent.mkdir(parents=True, exist_ok=True)
    Path(out_json).write_text(json.dumps(plan, indent=2, sort_keys=True) + "\n")
    t = plan["totals"]
    lines = [
        "# Dust Cleaner Plan",
        "",
        f"owner: `{plan['owner_short']}`",
        "",
        "## Totals",
        "",
        f"- main SOL: `{t['main_lamports'] / LAMPORTS:.9f}`",
        f"- WSOL/native accounts recoverable: `{t['wsol_lamports'] / LAMPORTS:.9f}` SOL",
        f"- empty ATA rent recoverable: `{t['empty_account_rent_lamports'] / LAMPORTS:.9f}` SOL",
        f"- total close/unwrap recoverable: `{t['recoverable_lamports'] / LAMPORTS:.9f}` SOL",
        f"- estimated main after cleanup: `{t['total_after_cleanup_est_lamports'] / LAMPORTS:.9f}` SOL",
        f"- closeable accounts: `{t['closeable_accounts']}`",
        f"- non-zero dust accounts: `{t['nonzero_dust_accounts']}`",
    ]
    if t.get("dust_quote_lamports"):
        lines.append(f"- Jupiter dust quote total: `{t['dust_quote_lamports'] / LAMPORTS:.9f}` SOL")
    lines += ["", "## Actions", ""]
    for a in plan["actions"][:200]:
        lines.append(
            f"- `{a['kind']}` {short(a['token_account'])} mint={short(a['mint'])} recover≈`{a['recover_lamports']/LAMPORTS:.9f}` SOL"
        )
    if len(plan["actions"]) > 200:
        lines.append(f"- ... {len(plan['actions']) - 200} more")
    lines += ["", "## Non-zero dust tokens", ""]
    for d in plan["dust_tokens"]:
        q = d.get("jupiter_out_lamports")
        qtxt = f" quote≈`{q/LAMPORTS:.9f}` SOL" if q else (f" quote_error=`{d.get('jupiter_error')}`" if d.get("jupiter_error") else " quote=not_checked")
        lines.append(
            f"- mint={short(d['mint'])} amount=`{d['amount_ui']}` rent=`{d['rent_lamports']/LAMPORTS:.9f}` SOL{qtxt}"
        )
    lines += [
        "",
        "## Safety",
        "",
        "This file is a plan only. Closing/selling requires a separate signed transaction and explicit operator GO.",
    ]
    Path(out_md).write_text("\n".join(lines) + "\n")


def main() -> int:
    ap = argparse.ArgumentParser(description="Read-only Solana dust/WSOL cleanup planner")
    ap.add_argument("--wallet-env", default=DEFAULT_WALLET_ENV)
    ap.add_argument("--owner", help="Owner pubkey; overrides --wallet-env public key")
    ap.add_argument("--project-env", default=".env")
    ap.add_argument("--quote-dust", action="store_true", help="Quote non-zero dust via Jupiter API")
    ap.add_argument("--out-json", default="datasets/dust_cleaner_plan.json")
    ap.add_argument("--out-md", default="datasets/dust_cleaner_plan.md")
    args = ap.parse_args()

    url = project_rpc_url(args.project_env)
    owner = args.owner or owner_from_wallet_env(args.wallet_env)
    plan = analyze(url, owner, args.quote_dust)
    write_outputs(plan, args.out_json, args.out_md)

    t = plan["totals"]
    print("DUST CLEANER PLAN")
    print(f"owner={plan['owner_short']}")
    print(f"main_SOL={t['main_lamports']/LAMPORTS:.9f}")
    print(f"recoverable_SOL={t['recoverable_lamports']/LAMPORTS:.9f}")
    print(f"after_cleanup_est_SOL={t['total_after_cleanup_est_lamports']/LAMPORTS:.9f}")
    print(f"closeable_accounts={t['closeable_accounts']} nonzero_dust={t['nonzero_dust_accounts']}")
    if args.quote_dust:
        print(f"dust_quote_SOL={t['dust_quote_lamports']/LAMPORTS:.9f}")
    print(f"wrote={args.out_json} {args.out_md}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
