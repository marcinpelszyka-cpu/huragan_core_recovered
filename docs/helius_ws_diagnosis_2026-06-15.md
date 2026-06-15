# Helius WS reconnect diagnosis — 2026-06-15

## Symptom

`migration-sniper.service` receives Helius Pump AMM logs and subscription acknowledgements, but the WebSocket is closed by the remote peer roughly every 302 seconds:

```text
helius reconnect_reason=close_frame:Some(CloseFrame { code: Away, reason: Utf8Bytes(b"") }) subscribed=true subscription_id=1
```

The process immediately reconnects and resubscribes successfully.

## Current production evidence

- Active migration PID: `520997` at time of diagnosis.
- Subscription is present: `subscription_ack id=1`.
- Messages and migrations continue flowing.
- No 401/429 evidence in the inspected logs.
- Metrics show `ws_ping_sent=0 ws_pong_seen=0` while `ws_messages_seen` increases.
- Remote close code is `Away` / WebSocket 1001.

## Root cause

The current heartbeat in `src/helius_log_scout.rs` is inactivity-gated. It sends a Ping only when `ws.next()` times out. Because the Helius logs stream is busy, `ws.next()` keeps returning messages and the timeout branch never runs. Result: after the initial subscription request, the client sends no periodic client-to-server WebSocket frames.

Relevant code shape:

```rust
let msg = match timeout(Duration::from_secs(wait_secs), ws.next()).await {
    Err(_) => {
        // Ping is only sent here, after read inactivity.
        ws.send(Message::Ping(...)).await
    }
    ...
}
```

This matches production metrics:

```text
ws_ping_sent=0 ws_pong_seen=0
```

## Probe result

A separate read-only probe was run against the same Helius WS URL without printing secrets:

```text
probe_start subscribed=true ping_interval=30s duration=390s url=REDACTED
probe_ok seconds=390.0 msgs=112861 acks=1 createpool_logs=15
```

During the same kind of window, production without independent pings still closed:

```text
06:48:30 close_frame Away
```

Interpretation: independent client-side ping every 30 seconds prevents the observed 5-minute close, or at least materially improves it beyond the current failure window.

## What is not the problem

- Not missing subscription: ack present and events flow.
- Not classic rate limit: no 429/JSON-RPC subscription rejection observed.
- Not systemd restart: process remains active.
- Not fd/memory exhaustion: fd count and host resources are normal.
- Not kernel TCP keepalive: system TCP keepalive is 7200s, while close is app-layer 1001 at ~302s.

## Recommended fix

Implement a real periodic write-side heartbeat independent of inbound traffic:

1. Split WS sink/stream or use `tokio::select!` with a ping interval.
2. Send `Message::Ping` every 30s regardless of received messages.
3. Track `last_ping_sent`, `last_pong_seen`, and ping RTT.
4. If no Pong after 2–3 intervals, reconnect.
5. Keep read loop continuously polling; move expensive RPC/parsing work off the WS receive loop if needed.
6. Treat `1001 Away` as reconnectable but do not let the controlled watchdog create 180s gaps for normal provider closes.

## Minimal regression/proof test

Before/after test:

- Run service/probe for >390s.
- Expected after fix:
  - `ws_ping_sent > 0`
  - `ws_pong_seen > 0`
  - no `close_frame Away` at ~302s
  - `subscription_ack id=1`
  - messages/migrations still flow

## Safety note

Do not re-arm live until WS_GO is satisfied after the fix:

```text
reconnects <= 1 / 30m
fresh subscription active
no duplicate subscriptions
resubscribe/heartbeat OK
```
