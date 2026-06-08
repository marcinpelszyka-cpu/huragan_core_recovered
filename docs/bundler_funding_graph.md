# Bundler + Mother Wallet Funding Graph v1

## Cel

Moduł wykrywa, czy early buyers fresh tokena wyglądają jak niezależni snajperzy, czy jak skoordynowany bundle zasilany z jednej matki. V1 jest tylko shadow/backtest: nie podpisuje i nie wysyła transakcji.

## Dane

Źródła:

- Helius JSON-RPC `getTransactionsForAddress` dla mintu i buyer walletów.
- `preTokenBalances` / `postTokenBalances` do early buy/sell detection.
- `preBalances` / `postBalances` do wykrycia inbound SOL funding.
- `datasets/sniper_wallet_scores.csv` do rozpoznania znanych GOOD/FAST_DUMPER walletów.
- `state.jsonl` do korelacji z `hard_stop`, `rug`, `dust_or_rug` i zyskownymi wynikami.

Funding source V1:

```text
największy inbound SOL transfer do buyer walleta w oknie 60 min przed first buy
```

## Komendy

Self-test:

```bash
python3 scripts/bundler_funding_backtest.py --self-test
```

Smoke:

```bash
python3 scripts/bundler_funding_backtest.py --limit-mints 5 --dry-run
```

Pełny shadow run:

```bash
python3 scripts/bundler_funding_backtest.py \
  --rpc-env-key RPC_SEND_URL \
  --limit-mints 500 \
  --funding-lookback-min 60 \
  --early-window-sec 10 \
  --rpc-sleep 0.35 \
  --rpc-retries 3 \
  --rpc-backoff 1.5
```

## Outputy

```text
datasets/bundler_wallet_edges.jsonl
datasets/bundler_clusters.csv
datasets/fresh_bundle_risk_signals.jsonl
datasets/bundler_funding_errors.jsonl
```

`fresh_bundle_risk_signals.jsonl` zawiera minimum:

```text
mint
early_buyer_count
shared_mother_count
top_mother_wallets
bundle_classification
bundle_score
mother_score
risk_score
follow_score
live_allowed=false
```

## Klasyfikacje

```text
INDEPENDENT_BUYERS
BUNDLE_POSSIBLE
BUNDLE_LIKELY
SHARED_MOTHER_CLUSTER
DEV_SNIPER_SUSPECT
GOOD_SNIPER_CLUSTER
UNKNOWN
```

Nie każdy bundler jest zły. V1 ma rozróżnić:

```text
GOOD_SNIPER_CLUSTER / profitable bundle to observe
vs
DEV_SNIPER_SUSPECT / shared mother cluster to avoid
```

## Acceptance

```text
processed_mints >= 500
early_buyer_clusters >= 100
terminal 429/error pct < 5% after retry/backoff; retry_errors reported separately
top mother wallets repeat across multiple mints
risk_score correlates with hard_stop/rug/dust outcomes
follow_score correlates with positive 30s/60s forward outcome
zero live trades
```

## Bezpieczne użycie RPC URL

Nie przekazywać pełnego RPC URL przez `--rpc-url`, bo będzie widoczny w `ps`. Na VPS używać:

```bash
python3 scripts/bundler_funding_backtest.py --rpc-env-key RPC_SEND_URL ...
```

## Safety

- Fresh pozostaje `SHADOW_ONLY`.
- Z3/Sender/canary nie są zmieniane przez ten moduł.
- `live_allowed` zawsze `false`.
- Nie wkładać private key do runtime dla tego modułu.
- Hermes może odpalać backtest po ops locku, ale nie edytuje kodu.
