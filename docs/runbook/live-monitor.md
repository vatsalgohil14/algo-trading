# Live monitor

Streams live quotes from Dhan for every stock in the universe, builds 1-minute candles, and shows each indicator two ways. It saves everything to QuestDB. It places no orders.

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

| Table | What | `source` |
|---|---|---|
| `ticks` | Every quote, with receive time and trade time | |
| `bars_1m` | Each finished live candle | `live` |
| `bars_1m` | Today's earlier candles fetched on start | `dhan_hist` |

A later `dhan-history` download replaces live candles with Dhan's official ones, because `bars_1m` keeps one row per stock and minute.

## How candles are built

- A quote counts as a trade only when the day's volume went up. Depth changes and snapshots move nothing.
- The candle's minute comes from the exchange trade time, as in Dhan's history.
- A candle closes when a later minute's first trade arrives, or 2 seconds after its minute ends if the stock goes quiet.
- Minutes without a trade have no candle, as in Dhan's history.
- Pre-open volume is left out of the 09:15 candle.

## Dhan quirks handled here

- Trade times in the feed are IST clock time written as if it were UTC. The tool subtracts 5h30m.
- Since 3 August 2026, NSE stocks in F&O trade continuously only until 15:15, then a closing auction runs until 15:35. All Nifty 50 stocks are in F&O, so their last candle is 15:14.
