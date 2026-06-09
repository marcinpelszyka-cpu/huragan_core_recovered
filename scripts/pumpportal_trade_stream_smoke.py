#!/usr/bin/env python3
"""
PumpPortal Trade Stream Smoke Test — tests whether subscribeTokenTrade
delivers real trade events. Requires PUMPPORTAL_API_KEY in .env.

Usage:
  cd /opt/huragan_core && source .venv/bin/activate
  python3 scripts/pumpportal_trade_stream_smoke.py

Connects to PumpPortal, gets a fresh token, subscribes to trades,
reports whether any arrive. If 0 trades: wallet likely needs SOL.
"""
import json, os, sys, time, asyncio
from datetime import datetime, timezone
from pathlib import Path

try:
    import websockets
except Exception as e:
    print(f"missing websockets: {e}")
    sys.exit(2)

# Optional dotenv without printing secrets
env_path = Path(__file__).parent.parent / ".env"
if env_path.exists():
    try:
        import dotenv
        dotenv.load_dotenv(env_path)
    except Exception:
        for line in env_path.read_text().splitlines():
            if line and not line.startswith('#') and '=' in line:
                k, v = line.split('=', 1)
                os.environ.setdefault(k, v)

WS_URL = "wss://pumpportal.fun/api/data"
TEST_DURATION = int(os.environ.get("PUMPPORTAL_SMOKE_DURATION", "60"))

async def smoke_test():
    api_key = os.environ.get("PUMPPORTAL_API_KEY", "")
    url = f"{WS_URL}?api-key={api_key}" if api_key else WS_URL
    print("=" * 60)
    print("PUMPPORTAL TRADE STREAM SMOKE TEST")
    print(f"  API key: {'SET' if api_key else 'NOT SET'}")
    print(f"  Test duration: {TEST_DURATION}s")
    print("=" * 60)
    async with websockets.connect(url, ping_interval=20, ping_timeout=10) as ws:
        print("WebSocket connected")
        await ws.send(json.dumps({"method": "subscribeNewToken"}))
        fresh_mint = None
        deadline = time.time() + 30
        while time.time() < deadline and fresh_mint is None:
            try:
                msg = await asyncio.wait_for(ws.recv(), timeout=5)
                data = json.loads(msg)
                if data.get("txType") == "create" and data.get("mint"):
                    fresh_mint = data["mint"]
                    print(f"Create event: mint={fresh_mint} mc_sol={data.get('marketCapSol')}")
            except asyncio.TimeoutError:
                print(".", end="", flush=True)
            except Exception as e:
                print(f"warn recv create: {e}")
        if not fresh_mint:
            print("NO_CREATE_EVENT")
            return 1
        await ws.send(json.dumps({"method": "subscribeTokenTrade", "keys": [fresh_mint]}))
        trade_count = 0
        buy_count = 0
        sell_count = 0
        errors = []
        deadline = time.time() + TEST_DURATION
        while time.time() < deadline:
            try:
                msg = await asyncio.wait_for(ws.recv(), timeout=2)
                data = json.loads(msg)
                if data.get("errors") or data.get("error"):
                    errors.append(data)
                    print("ERROR_MSG", json.dumps(data)[:500])
                if data.get("txType") in ("buy", "sell") and data.get("mint") == fresh_mint:
                    trade_count += 1
                    buy_count += 1 if data.get("txType") == "buy" else 0
                    sell_count += 1 if data.get("txType") == "sell" else 0
                    if trade_count <= 3:
                        print("TRADE", json.dumps(data)[:500])
            except asyncio.TimeoutError:
                continue
            except Exception as e:
                print(f"warn recv trade: {e}")
        print("RESULT", json.dumps({"mint": fresh_mint, "trade_count": trade_count, "buy_count": buy_count, "sell_count": sell_count, "errors": errors[:3]}))
        return 0

if __name__ == "__main__":
    raise SystemExit(asyncio.run(smoke_test()))
