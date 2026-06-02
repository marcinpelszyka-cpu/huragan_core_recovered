# Huragan Core — recovered project

Ten katalog jest rekonstrukcją po utracie serwera `/root/huragan_core`.

Stan odzysku:

- odtworzone moduły: `main`, `engine`, `executor`, `state`, `strategy`, `paper_amm`,
  `position_manager`, `fresh_momentum`, `market_supervisor`, `scout`, `helius_log_scout`;
- sekrety nie zostały przeniesione;
- domyślnie działa wyłącznie `PAPER_MODE=true` / shadow;
- live execution jest zabezpieczone i w tej rekonstrukcji nie wysyła transakcji, dopóki właściwe
  Pump AMM instruction builders nie zostaną ponownie zweryfikowane na nowym serwerze.

## Build

```bash
cargo build --release --bin huragan_core
cargo build --release --bin market_supervisor
```

## Safe paper run

```bash
cp .env.example .env
# uzupełnij RPC_URL, RPC_WS_URL, opcjonalnie PUMPPORTAL_API_KEY
PAPER_MODE=true LIVE_ARMED=false ./target/release/huragan_core
```

## Fresh Momentum only

```bash
FRESH_MOMENTUM_CAPTURE=only PAPER_MODE=true LIVE_ARMED=false ./target/release/huragan_core
```

## Supervisor

```bash
./target/release/market_supervisor \
  --state ./state.jsonl \
  --live-state ./state.jsonl \
  --window-mins 120 \
  --output ./agents_decision.json
```

## Deployment notes

1. Zainstaluj Rust stable.
2. Skopiuj repo.
3. Utwórz `.env` z `.env.example`.
4. Nie kopiuj starych prywatnych kluczy z rozmów/logów; wygeneruj lub wklej świadomie lokalnie.
5. Startuj najpierw `PAPER_MODE=true`.
6. Live można przywracać dopiero po ponownym podpięciu zweryfikowanych AMM builders i testach dry-run.
