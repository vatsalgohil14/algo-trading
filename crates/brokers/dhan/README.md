# dhan

Async Rust client for the [DhanHQ v2 API](https://dhanhq.co/docs/v2/). It is a
standalone broker SDK with no knowledge of strategies, so it can be reused by any
part of the system (live trader, backtest data loader, tools).

## What's covered

| Area | Methods |
|---|---|
| Orders | `place_order`, `modify_order`, `cancel_order`, `place_slice_order`, `order_book`, `get_order`, `get_order_by_correlation_id`, `trade_book`, `trades_for_order` |
| Super orders (entry + target + SL, trailing) | `place_super_order`, `modify_super_order`, `cancel_super_order`, `super_order_book` |
| Portfolio | `positions`, `holdings`, `convert_position`, `exit_all_positions` |
| Funds & margin | `fund_limit`, `margin_for`, `margin_for_many` |
| Broker-side risk | `set_kill_switch`, `kill_switch_status`, `set_pnl_exit`, `stop_pnl_exit`, `pnl_exit_status` |
| Quotes (REST) | `ltp`, `ohlc`, `quote` (up to 1000 instruments) |
| History | `daily_candles`, `intraday_candles`, `intraday_candles_range` (auto-splits >90 days) |
| Statements | `ledger`, `trade_history`, `trade_history_all` (with per-trade charges) |
| Account | `profile`, `renew_token`, `set_static_ip`, `modify_static_ip`, `static_ips` |
| Tokens | `auth::DhanAuth` — PIN+TOTP flow (with local TOTP generation) and API-key consent flow |
| Live market feed | `feed::MarketFeed` — binary WebSocket, ticker / quote / full (5-level depth) |
| Live order updates | `order_update::OrderUpdateStream` — JSON WebSocket |
| Instruments | `instruments::InstrumentMaster` — symbol → security ID, tick size, circuits, ASM/GSM flags |

Not covered yet (not needed for intraday cash equity): forever orders, conditional
triggers, option chain, expired options data, EDIS, 20/200-level depth feeds.

## Safety behaviour

- **Orders are never retried automatically.** If a place/modify/cancel call fails
  and `err.outcome_unknown()` is true, the order may exist: look it up with
  `get_order_by_correlation_id` before trying again. Always set a correlation ID.
- **Reads are retried** (GET only) on 5xx, 429 and network errors, with backoff.
- **Client-side rate limiting** matches Dhan's published limits per category
  (orders 10/s, 250/min, 1000/h, 7000/day; quotes 1/s; data 5/s; others 20/s). If
  a slot would take longer than `max_wait` (5 s), the call fails fast with
  `RateLimitedLocally` instead of stalling a trading decision.
- **Local validation** rejects obviously bad orders before they leave the process
  (zero quantity, LIMIT without price, SL without trigger, super-order legs on the
  wrong side, invalid correlation IDs, ...).
- **Lenient parsing**: numbers sent as strings, `""`/`"NA"`/`null`, unknown enum
  values and Dhan's own typos (`availabelBalance`, `totalQuatity`) don't break
  responses. Unknown statuses become `OrderStatus::Unknown`.
- **Money is `Decimal`**, never `f64`. Feed prices stay `f32` on the hot path; use
  `feed::packet::to_price` to convert.
- **Secrets** (`Secret`) never print in `Debug` output and are zeroed on drop.
- **WebSockets** reconnect with jittered backoff, replay all subscriptions, detect
  silent connections (no data/ping for 30 s), and emit `Connected` /
  `Disconnected` events so the engine can stop trading on stale data.
- The library is built under `deny(unwrap_used, expect_used, panic, indexing_slicing)`.

## Quick start

```rust
use dhan::*;

let client = DhanClient::from_env()?; // DHAN_CLIENT_ID, DHAN_ACCESS_TOKEN
let profile = client.profile().await?;

// Intraday long on RELIANCE with a broker-side stop and target in one call.
let order = PlaceSuperOrderRequest::limit(
    TransactionType::Buy,
    ExchangeSegment::NseEq,
    SecurityId(2885),
    10,
    Decimal::new(142_805, 2), // entry ₹1428.05
    Decimal::new(144_500, 2), // target
    Decimal::new(141_500, 2), // stop
    ProductType::Intraday,
)
.with_correlation_id("orb-2885-0001");
let ack = client.place_super_order(&order).await?;
```

## Examples

```bash
cargo run -p dhan --example instruments -- RELIANCE HDFCBANK   # no credentials needed
cargo run -p dhan --example account_check                      # read-only
cargo run -p dhan --example live_feed                          # needs Data API plan
cargo run -p dhan --example order_updates                      # read-only
```

The credential examples read `DHAN_CLIENT_ID` and `DHAN_ACCESS_TOKEN` from the
environment. None of the examples place orders.

## Verify against a live account before trading

These points come from ambiguities in Dhan's documentation and have only been
tested against mocks:

1. `margin_for_many` — the docs show two different request shapes; this uses
   `includeOrder` / `scripList`.
2. `intraday_candles` sends `interval` as a number (the docs' sample shows a string).
3. `renew_token` assumes the response has the same shape as token generation.
4. Feed `ltt` (last trade time) is passed through raw; confirm whether it is UTC
   epoch or IST-shifted before using it for bar timing.
5. Order placement needs the machine's static IP whitelisted (`set_static_ip`).

## Tests

```bash
cargo test -p dhan
```

Unit tests plus integration tests: REST against a mock server (`wiremock`),
binary feed packets built byte by byte from the spec (including truncated and
garbage input), order-update samples from the docs, and real instrument-master rows.
