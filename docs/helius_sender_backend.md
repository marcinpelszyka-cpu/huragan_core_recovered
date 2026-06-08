# Helius Sender Backend v1

## Purpose

`LIVE_SEND_BACKEND=helius_sender` is a submit-only backend for one controlled Z3 full lifecycle canary. Gatekeeper/RPC remains responsible for detection, quotes, preflight, blockhashes and confirmation status.

## Why separate backend

Helius Sender requires:

```text
skipPreflight=true
tip transfer instruction
compute unit price instruction
base64 sendTransaction
```

Therefore Sender is never the default trading path and is not used for Fresh shadow or multi-position.

Docs: https://www.helius.dev/docs/sending-transactions/sender

## Default v1 config

```env
LIVE_SEND_BACKEND=helius_sender
HELIUS_SENDER_ENDPOINT=https://sender.helius-rpc.com/fast?swqos_only=true
HELIUS_SENDER_TIP_LAMPORTS=5000
HELIUS_SENDER_CU_LIMIT=250000
HELIUS_SENDER_CU_PRICE_MICRO_LAMPORTS=200000
HELIUS_SENDER_MAX_PER_DAY=2
```

`swqos_only=true` is the v1 default because the dual-route 200000 lamport minimum tip is too expensive for a 0.003 SOL canary.

## Safety

Sender is allowed only when the standard canary guards pass:

```text
AMM_LIVE_CANARY=true
LIVE_VARIANT=Z3
MAX_TRADES_PER_RUN=1
BUY_AMOUNT_SOL<=0.003
LIVE_AUTO_SELL_ENABLED=true
LIVE_SELL_SEND_ENABLED=true
PUMPPORTAL_ENABLED=false
JITO_TIP_LAMPORTS=0
EMERGENCY_JITO_TIP_LAMPORTS=0
```

Rollback always restores:

```env
LIVE_SEND_BACKEND=rpc
PAPER_MODE=true
LIVE_SEND_ENABLED=false
SOLANA_PRIVATE_KEY_BASE58 absent
```

## Canary arm syntax

RPC backend:

```bash
/opt/huragan_core/scripts/huragan_canary_arm.sh 8500 false rpc
```

Sender backend:

```bash
/opt/huragan_core/scripts/huragan_canary_arm.sh 8500 false helius_sender
```

No Sender canary should run without an explicit GO.
