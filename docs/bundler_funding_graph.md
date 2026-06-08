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

Funding source V1 default:

```text
największy inbound SOL transfer do buyer walleta w oknie 60 min przed first buy
```

Opcjonalnie można użyć Helius Wallet API `funded-by`:

```text
GET /v1/wallet/{wallet}/funded-by
```

To zwraca oryginalnego fundera walleta (`funder`, `amount`, `signature`, `timestamp`, `funderType`). Jest to przydatne do wykrywania “matki”, ale API jest beta i kosztuje 100 credits/call, więc nie jest defaultem. Tryb `hybrid` próbuje Wallet API najpierw, a gdy nie ma wyniku, wraca do tańszego `getTransactionsForAddress`.

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

Run z Wallet API tylko gdy świadomie akceptujesz credit cost:

```bash
python3 scripts/bundler_funding_backtest.py \
  --rpc-env-key RPC_SEND_URL \
  --funding-source-method hybrid \
  --limit-mints 100 \
  --funding-lookback-min 60 \
  --early-window-sec 10 \
  --rpc-sleep 0.75 \
  --wallet-api-sleep 0.1 \
  --max-wallet-api-buyers-per-mint 8
```

Jeśli API key nie jest w URL, podaj nazwę zmiennej z `.env` bez wpisywania sekretu w CLI:

```bash
python3 scripts/bundler_funding_backtest.py \
  --rpc-env-key RPC_SEND_URL \
  --funding-source-method hybrid \
  --helius-api-key-env HELIUS_API_KEY \
  --limit-mints 100
```

## Outputy

```text
datasets/bundler_wallet_edges.jsonl
datasets/bundler_clusters.csv
datasets/fresh_bundle_risk_signals.jsonl
datasets/bundler_funding_errors.jsonl
```

`bundler_wallet_edges.jsonl` przy trybie Wallet API dodaje pola:

```text
funding_source_method=wallet-api|gtfa|hybrid_gtfa_fallback
wallet_api_funder_type
wallet_api_funder_name_present
wallet_api_within_lookback
```

Domyślnie `hybrid` ogranicza płatne Wallet API lookupi do 8 buyerów na mint:

```text
--max-wallet-api-buyers-per-mint 8
```

To jest celowe. Jeden `funded-by` kosztuje 100 credits, więc pełny run bez limitu może niepotrzebnie przepalić limit na jednym hałaśliwym tokenie.

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
