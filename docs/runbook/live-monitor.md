# Live monitor

Streams live quotes and 5-level market depth from Dhan for every stock in the universe, builds 1-minute candles, and computes each indicator two ways. It saves all of it to QuestDB as it arrives. It places no orders.

## Run it

You need the Dhan Data API plan, and `.env` filled in as in [Dhan login](dhan-token.md).

```bash
scripts/live_monitor.sh
```

The script loads `.env`, starts QuestDB if needed, builds the tool and runs it until Ctrl-C. Options go straight to the tool:

```bash
scripts/live_monitor.sh --show RELIANCE,TCS --print-every 10
```

| Option | Default | Meaning |
|---|---|---|
| `--universe` | `data/universe.csv` | Stocks to stream; written by `dhan-history` |
| `--indicators` | `ema(9),ema(20),vwap,rsi(14),orb(15).high,orb(15).low` | Any specs from [Building blocks](../building-blocks.md#indicators-available) |
| `--show` | all | Only print these symbols; all are still recorded |
| `--print-every` | `15` | Seconds between tables |
| `--warmup-days` | `10` | Days of history used to warm up indicators |
| `--minutes` | until Ctrl-C | Stop after this many minutes |
| `--no-store` | off | Don't write to QuestDB |

## Reading the table

Each indicator column shows `live/closed`.

- **Live** is the value as if the current minute's candle closed now. It moves with every trade and can change until the minute ends. Use it for watching only.
- **Closed** is the value after the last finished candle. It changes once a minute and is exactly what a strategy and a backtest see. Trading signals use this one.

When a stock hasn't traded yet this minute, the two are equal. Before the open, VWAP and the opening range still show yesterday's values; they reset on the day's first candle.

Each finished candle also prints a `closed` line with its open, high, low, close, volume and indicator values.

## What happens on start

1. Indicators are warmed up from the last `--warmup-days` of `bars_1m` in QuestDB, so values like `ema(20)` are ready at the open.
2. Today's finished candles are fetched from Dhan, so a mid-day start still has the right VWAP and opening range.
3. The feed connects and subscribes to every stock.

## What gets stored

| Table | One row per | Joins with |
|---|---|---|
| `ticks` | feed update: price, volume, trade time, 5 levels of buy and sell depth | `indicators_live` |
| `indicators_live` | tick and indicator: the live value at that tick | `ticks` |
| `bars_1m` | finished candle (`source = 'live'`; today's earlier candles fetched on start are `dhan_hist`) | `indicators_1m` |
| `indicators_1m` | finished candle and indicator: the closed value | `bars_1m` |

Each tick's indicator rows carry exactly the tick's `ts`, and each candle's indicator rows carry the candle's `ts`, so a plain join on `ts` and `security_id` lines them up. A tick with no trade this minute gets the closed values as its live values.

All times are UTC: 03:45 is 09:15 IST. Prices are rounded to the paisa. `ticks` and `indicators_live` keep 60 days; candles and closed values are kept for good. A later `dhan-history` download replaces live candles with Dhan's official ones, because `bars_1m` keeps one row per stock and minute.

### Useful queries

Run them in the QuestDB console at http://127.0.0.1:9000.

Price, best bid and ask, and live indicators, tick by tick:

```sql
SELECT t.ts, t.ltp, t.bid_px_1, t.ask_px_1, e9.value AS ema9, vw.value AS vwap
FROM ticks t
JOIN indicators_live e9
  ON t.ts = e9.ts AND t.security_id = e9.security_id AND e9.indicator = 'ema(9)'
JOIN indicators_live vw
  ON t.ts = vw.ts AND t.security_id = vw.security_id AND vw.indicator = 'vwap'
WHERE t.symbol = 'RELIANCE'
ORDER BY t.ts DESC
LIMIT 50;
```

The full order book at each tick:

```sql
SELECT ts, ltp,
       bid_px_1, bid_qty_1, bid_px_2, bid_qty_2, bid_px_3, bid_qty_3,
       ask_px_1, ask_qty_1, ask_px_2, ask_qty_2, ask_px_3, ask_qty_3,
       total_buy_qty, total_sell_qty
FROM ticks
WHERE symbol = 'RELIANCE'
ORDER BY ts DESC
LIMIT 20;
```

Candles with their closed indicator values:

```sql
SELECT b.ts, b.open, b.high, b.low, b.close, b.volume, i.indicator, i.value
FROM bars_1m b
JOIN indicators_1m i ON b.ts = i.ts AND b.security_id = i.security_id
WHERE b.symbol = 'RELIANCE' AND b.ts >= today()
ORDER BY b.ts, i.indicator;
```

Levels 4 and 5 are `bid_px_4` … `ask_orders_5`; each level also has `*_orders`, the number of orders at that price.

## How candles are built

- A quote counts as a trade only when the day's volume went up. Depth changes and snapshots move nothing.
- The candle's minute comes from the exchange trade time, as in Dhan's history.
- A candle closes when a later minute's first trade arrives, or 2 seconds after its minute ends if the stock goes quiet.
- Minutes without a trade have no candle, as in Dhan's history.
- Pre-open volume is left out of the 09:15 candle.
- Dhan sends about one update per stock per second, not every trade, so a price touched and reversed within a second is never seen directly. Each update also carries the exchange's day open, high and low. When the day high rises or the day low falls between two updates, the new extreme goes into the current candle. The 09:15 candle opens at the exchange's day open.

## How live candles compare with Dhan's

Checked against Dhan's official candles on 2026-11-19:

- **From 09:16:** open, close and volume match almost exactly. Highs and lows can differ by a tick when an extreme within a second wasn't a new day high or low.
- **09:15:** the close matches. Our volume is higher, because Dhan's official 09:15 candle starts counting 1–2 seconds after the open and leaves out the first trades. Ours counts from 09:15:00, which is the true exchange volume. A backtest on Dhan history therefore sees a smaller 09:15 volume than live trading does.

## Dhan quirks handled here

- Trade times in the feed are IST clock time written as if it were UTC. The tool subtracts 5h30m.
- Since 3 August 2026, NSE stocks in F&O trade continuously only until 15:15, then a closing auction runs until 15:35. All Nifty 50 stocks are in F&O, so their last candle is 15:14.
