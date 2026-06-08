# Fresh Sniper Follow Layer v1

## Cel

Warstwa identyfikuje portfele, które kupują fresh tokeny w pierwszych sekundach, klasyfikuje ich zachowanie i generuje sygnał `FOLLOW_SHADOW`. V1 jest tylko shadow/backtest: nie podpisuje, nie wysyła transakcji i nie zmienia Z3/live canary.

## Źródła danych

- PumpPortal `subscribeNewToken`: wykrywanie fresh mintów i market cap w SOL.
- Helius JSON-RPC `getTransactionsForAddress`: historia transakcji mintu w pierwszych 60 sekundach.
- Pola Solana tx używane do klasyfikacji:
  - `preTokenBalances`
  - `postTokenBalances`
  - `preBalances`
  - `postBalances`
  - signer / owner token account

Nie dodajemy Helius Rust SDK do core w v1, bo projekt używa `solana-client = 2`; SDK może wymagać innego stacku Solany. Aktualny v1 używa deterministycznego JSON-RPC przez `reqwest` / Python `urllib`.

## Backtest

Smoke test:

```bash
python scripts/sniper_follow_backtest.py --self-test
python scripts/sniper_follow_backtest.py --limit-mints 5 --dry-run
```

Pełniejszy run:

```bash
python scripts/sniper_follow_backtest.py \
  --input fresh_momentum_candidates.jsonl \
  --limit-mints 50 \
  --rpc-sleep 0.1
```

Wyjścia:

```text
datasets/sniper_trade_events.jsonl
datasets/sniper_wallet_scores.csv
datasets/sniper_follow_signals.jsonl
datasets/sniper_follow_errors.jsonl
```

Minimalne pola eventu:

```text
mint, timestamp, age_secs, signature, signer, owner, side, token_delta_raw, quote_delta_sol, entry_market_cap_sol
```

Klasy walletów:

```text
GOOD_SNIPER
FAST_DUMPER
DEV_SNIPER_SUSPECT
UNKNOWN
```

## Shadow runtime

Osobny tryb runtime:

```bash
SNIPER_SHADOW_CAPTURE=only cargo run --release --bin huragan_core
```

Na VPS używać dopiero po osobnym systemd/timer planie. Tryb zapisuje:

```text
sniper_follow_shadow.jsonl
sniper_follow_shadow_errors.jsonl
```

## Sygnał v1

Domyślne warunki:

```text
token age <= 60s
early sniper window = 10s
min sniper buy = 0.01 SOL
>=2 early sniper wallets
total early sniper buy >=0.03 SOL
hold ratio po 10s >=75%
```

`FOLLOW_SHADOW` nie oznacza zgody na live. Przed live musi być osobny plan i osobna seria canary.

## GO / NO-GO

GO do kolejnego etapu dopiero gdy:

```text
>=50 tokenów w sample
>=20 FOLLOW_SHADOW signals
forward PnL 30s/60s lepszy niż baseline fresh
rug rate niższy niż obecne Z3 canary
zero live trades podczas testu
```

NO-GO:

```text
brak trade data
429/401 RPC pattern
sygnały tylko na lookahead score bez rolling validation
rug rate podobny lub gorszy niż Z3
```

## Safety

- Fresh pozostaje `SHADOW_ONLY`.
- Multi-position pozostaje forbidden.
- Nie używać `BHuhRD...p2Q1` do tradingu.
- Nie wkładać private key do runtime dla tego modułu.
- Nie dodawać live buy bez osobnego planu.
