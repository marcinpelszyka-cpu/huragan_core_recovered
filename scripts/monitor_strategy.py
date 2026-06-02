#!/usr/bin/env python3
"""
huragan_core Strategy Monitor — connects bot state.jsonl data with GMGN on-chain analysis.

Usage:
  python3 scripts/monitor_strategy.py                     # Full report (last 24h)
  python3 scripts/monitor_strategy.py --mint <address>    # Single token deep dive
  python3 scripts/monitor_strategy.py --summary           # Quick overview only
  python3 scripts/monitor_strategy.py --all-time          # Full historical analysis
"""

import json
import subprocess
import sys
import os
from collections import defaultdict
from datetime import datetime, timezone

STATE_FILE = "/root/huragan_core/state.jsonl"
GMGN_CLI = "/root/.hermes/node/bin/gmgn-cli"

def load_state():
    """Load all state entries from state.jsonl."""
    if not os.path.exists(STATE_FILE):
        print(f"❌ state.jsonl not found at {STATE_FILE}")
        return []
    with open(STATE_FILE) as f:
        return [json.loads(l) for l in f if l.strip()]

def gmgn_token_info(mint):
    """Get token info from GMGN."""
    try:
        r = subprocess.run(
            [GMGN_CLI, "token", "info", "--chain", "sol", "--address", mint, "--raw"],
            capture_output=True, text=True, timeout=15
        )
        if r.returncode == 0:
            return json.loads(r.stdout)
        return None
    except Exception as e:
        return None

def gmgn_token_security(mint):
    """Get token security from GMGN."""
    try:
        r = subprocess.run(
            [GMGN_CLI, "token", "security", "--chain", "sol", "--address", mint, "--raw"],
            capture_output=True, text=True, timeout=15
        )
        if r.returncode == 0:
            return json.loads(r.stdout)
        return None
    except Exception as e:
        return None

def format_report(results):
    """Format analysis results into a readable report."""
    total = len(results)
    alive = sum(1 for r in results if r.get("alive", False))
    dead = total - alive
    avg_profit = sum(r.get("price_change_pct", 0) for r in results) / max(total, 1)
    
    lines = []
    lines.append(f"## 📊 Huragan Core — Strategy Performance Report")
    lines.append(f"")
    lines.append(f"**Paper trades analyzed**: {total}")
    lines.append(f"**Tokens still alive**: {alive} ({alive/max(total,1)*100:.0f}%)")
    lines.append(f"**Dead/rugged**: {dead} ({dead/max(total,1)*100:.0f}%)")
    lines.append(f"**Avg price change**: {avg_profit:+.2f}%")
    lines.append(f"")
    lines.append(f"### Bot Strategy Gate Results")
    
    # Gate analysis
    sound = sum(1 for r in results if r.get("all_gates_ok", False))
    risky = sum(1 for r in results if not r.get("all_gates_ok", False))
    
    lines.append(f"- **Passed all gates**: {sound}")
    lines.append(f"- **Failed gates**: {risky}")
    lines.append(f"")
    
    # Top performers and worst performers
    performers = sorted(results, key=lambda r: r.get("price_change_pct", 0), reverse=True)
    
    lines.append(f"### 🔥 Top 5 Performers")
    lines.append(f"| Token | Price Δ | Alive | Smart $ | Rug Ratio |")
    lines.append(f"|-------|---------|-------|---------|-----------|")
    for r in performers[:5]:
        lines.append(f"| {r['mint'][:12]}... | {r.get('price_change_pct',0):+.1f}% | {'✅' if r.get('alive') else '❌'} | {r.get('smart_wallets','?')} | {r.get('rug_ratio','?')} |")
    
    lines.append(f"")
    lines.append(f"### 💀 Worst 5")
    lines.append(f"| Token | Price Δ | Alive | Rug Ratio | Notes |")
    lines.append(f"|-------|---------|-------|-----------|-------|")
    for r in reversed(performers[-5:]):
        notes = r.get("gate_fail_reason", "")
        lines.append(f"| {r['mint'][:12]}... | {r.get('price_change_pct',0):+.1f}% | {'✅' if r.get('alive') else '❌'} | {r.get('rug_ratio','?')} | {notes} |")
    
    lines.append(f"")
    lines.append(f"### 🏦 Smart Money Activity")
    smart_money_tokens = [r for r in results if r.get("smart_wallets", 0) > 0]
    lines.append(f"- {len(smart_money_tokens)} tokens have smart money interest")
    for r in sorted(smart_money_tokens, key=lambda r: r.get("smart_wallets", 0), reverse=True)[:5]:
        lines.append(f"  - {r['mint'][:12]}...: {r.get('smart_wallets',0)} smart wallets, price {r.get('price_change_pct',0):+.1f}%")
    
    return "\n".join(lines)

def format_single_token_report(mint, info_data, security_data):
    """Format a deep dive report for a single token."""
    d = info_data.get("data", {}) if info_data else {}
    s = security_data.get("data", {}) if security_data else {}
    
    lines = []
    lines.append(f"## 🪙 Token Analysis: {d.get('symbol','?')} ({mint[:16]}...)")
    lines.append(f"")
    
    # Price info
    price = d.get("price", {}) if isinstance(d.get("price"), dict) else {"price": d.get("price", 0)}
    p = price.get("price", 0)
    mcap = d.get("market_cap", 0)
    liq = d.get("liquidity", 0)
    vol = price.get("volume_1h", price.get("volume_24h", 0))
    holders = d.get("holder_count", 0)
    
    price_1m = price.get("price_1m", 0)
    price_5m = price.get("price_5m", 0) 
    price_1h = price.get("price_1h", 0)
    
    change_1m = ((p / price_1m) - 1) * 100 if price_1m and price_1m > 0 else 0
    change_5m = ((p / price_5m) - 1) * 100 if price_5m and price_5m > 0 else 0
    change_1h = ((p / price_1h) - 1) * 100 if price_1h and price_1h > 0 else 0
    
    lines.append(f"**Price**: ${p:.8f} | **MCap**: ${mcap:,.0f} | **Liq**: ${liq:,.0f}")
    lines.append(f"**Volume**: ${vol:,.0f} | **Holders**: {holders}")
    lines.append(f"**Δ**: 1m: {change_1m:+.1f}% | 5m: {change_5m:+.1f}% | 1h: {change_1h:+.1f}%")
    lines.append(f"")
    
    # Security
    rug = s.get("rug_ratio", d.get("rug_ratio", "?"))
    renounced_mint = d.get("renounced_mint", s.get("renounced_mint", "?"))
    renounced_freeze = d.get("renounced_freeze_account", s.get("renounced_freeze_account", "?"))
    is_honeypot = s.get("is_honeypot", "?")
    creator_token_status = d.get("creator_token_status", s.get("creator_token_status", "?"))
    top10 = d.get("top_10_holder_rate", s.get("top_10_holder_rate", 0))
    cto = d.get("cto_flag", "?")
    
    lines.append(f"**Security**: rug_ratio={rug} | renounced_mint={renounced_mint} | honeypot={is_honeypot}")
    lines.append(f"**Dev**: {creator_token_status} | **CTO**: {cto} | **Top10 holder**: {top10*100 if isinstance(top10, float) else top10}%")
    lines.append(f"")
    
    # Smart money
    wallet_tags = d.get("wallet_tags_stat", {})
    smart = wallet_tags.get("smart_wallets", wallet_tags.get("smart_degen_count", 0))
    renowned = wallet_tags.get("renowned_wallets", wallet_tags.get("renowned_count", 0))
    sniper = wallet_tags.get("sniper_wallets", 0)
    rat = wallet_tags.get("rat_trader_wallets", 0)
    
    lines.append(f"**Smart Money**: {smart} | **KOL**: {renowned} | **Snipers**: {sniper} | **Rats**: {rat}")
    lines.append(f"")
    
    # Bot strategy assessment
    gates = []
    if renounced_mint == 1 or renounced_mint == "1":
        gates.append("✅ renounced")
    else:
        gates.append("❌ NOT renounced")
    if is_honeypot == False or is_honeypot == "false":
        gates.append("✅ not honeypot")
    elif is_honeypot == True or is_honeypot == "true":
        gates.append("❌ HONEYPOT")
    if isinstance(top10, float) and top10 <= 0.35:
        gates.append(f"✅ top10={top10*100:.1f}%")
    elif isinstance(top10, float):
        gates.append(f"⚠️ top10={top10*100:.1f}% > 35%")
    if isinstance(rug, float) and rug < 0.3:
        gates.append(f"✅ rug={rug:.2f}")
    elif isinstance(rug, float):
        gates.append(f"⚠️ rug={rug:.2f} > 0.3")
    if creator_token_status in ("creator_close", "cto"):
        gates.append("✅ dev closed/CTO")
    elif creator_token_status == "creator_hold":
        gates.append("⚠️ dev still holds")
    
    lines.append(f"**Bot Strategy Assessment**:")
    for g in gates:
        lines.append(f"  {g}")
    
    return "\n".join(lines)

def analyze_state_tokens(state_entries, mint_filter=None, max_tokens=10):
    """Analyze bot state tokens through GMGN."""
    # Get unique mints for completed trades
    seen = set()
    unique_mints = []
    for l in reversed(state_entries):
        if l.get("status") in ("paper_completed", "paper_partial_sold") and l["mint"] not in seen:
            if mint_filter and l["mint"] != mint_filter:
                continue
            seen.add(l["mint"])
            unique_mints.append(l["mint"])
        if len(unique_mints) >= max_tokens and not mint_filter:
            break
    
    if not unique_mints:
        print("❌ No completed trades found")
        return []
    
    print(f"📡 Analyzing {len(unique_mints)} tokens via GMGN...")
    results = []
    for i, mint in enumerate(unique_mints):
        print(f"  [{i+1}/{len(unique_mints)}] {mint[:16]}...", end=" ", flush=True)
        info = gmgn_token_info(mint)
        security = gmgn_token_security(mint)
        
        result = {"mint": mint, "alive": info is not None}
        
        if info:
            d = info.get("data", {})
            price_data = d.get("price", {}) if isinstance(d.get("price"), dict) else {"price": d.get("price", 0)}
            p = price_data.get("price", 0)
            price_1h = price_data.get("price_1h", 0)
            
            if p and price_1h:
                result["price_change_pct"] = ((p / price_1h) - 1) * 100
            else:
                result["price_change_pct"] = 0
            
            result["price"] = p
            result["market_cap"] = d.get("market_cap", 0)
            result["liquidity"] = d.get("liquidity", 0)
            result["holder_count"] = d.get("holder_count", 0)
            result["top10_holder_rate"] = d.get("top_10_holder_rate", 0)
            result["creator_token_status"] = d.get("creator_token_status", "?")
            result["cto_flag"] = d.get("cto_flag", "?")
            
            wallet_tags = d.get("wallet_tags_stat", {})
            result["smart_wallets"] = wallet_tags.get("smart_wallets", wallet_tags.get("smart_degen_count", 0))
            result["renowned_wallets"] = wallet_tags.get("renowned_wallets", wallet_tags.get("renowned_count", 0))
            
            # Calculate gate assessment
            renounced = d.get("renounced_mint", "?")
            top10 = d.get("top_10_holder_rate", 0)
            creator_status = d.get("creator_token_status", "")
            result["rug_ratio"] = d.get("rug_ratio", "?")
            
            fails = []
            if renounced != 1 and renounced != "1":
                fails.append("not_renounced")
            if "creator_hold" in creator_status and d.get("cto_flag") != 1:
                fails.append("dev_holds")
            
            result["all_gates_ok"] = len(fails) == 0
            result["gate_fail_reason"] = ", ".join(fails) if fails else ""
            print(f"✅ ${p:.8f}")
        else:
            result["price_change_pct"] = -100
            result["alive"] = False
            print("❌ dead/rugged")
        
        results.append(result)
    
    return results

def main():
    state = load_state()
    if not state:
        print("❌ No state data found")
        sys.exit(1)
    
    # Check for --mint flag (single token deep dive)
    if "--mint" in sys.argv:
        idx = sys.argv.index("--mint")
        if idx + 1 < len(sys.argv):
            mint = sys.argv[idx + 1]
            print(f"🔍 Deep dive: {mint}")
            info = gmgn_token_info(mint)
            security = gmgn_token_security(mint)
            print(format_single_token_report(mint, info, security))
            return
    
    # Determine how many tokens to analyze
    if "--all-time" in sys.argv:
        max_tokens = 999
    elif "--summary" in sys.argv:
        max_tokens = 0
    else:
        max_tokens = 15  # Default: last 15 completed trades
    
    results = analyze_state_tokens(state, max_tokens=max_tokens)
    
    if not results:
        return
    
    print("\n" + "=" * 60)
    print(format_report(results))
    print("=" * 60)

if __name__ == "__main__":
    main()
