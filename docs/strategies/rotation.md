# Monthly momentum rotation

Status: **research passed; paper trading from 2026-11-20.** Code: `crates/rotation`. Daily paper trading and monitoring: `rotation-paper` (`scripts/rotation_daily.sh`). Orders for real holdings: `rotation-plan`.

**The strategy that is run is momentum + low volatility, with the settings in `RotationConfig::recommended()`:** top 30 of the 500 most-traded stocks, rebalanced on the first trading day of each month, held stocks kept while they rank in the top 60. Momentum 12-1 is the higher-return, much bumpier alternative.

Each month, rank the most-traded NSE stocks, hold the top 30 in equal amounts, and replace the ones that dropped out.

| Strategy | Ranks stocks by | Name in code |
|---|---|---|
| **Momentum + low volatility** | Average of 12-1 and 6-1 momentum, each divided by 1-year volatility, minus 3 × that volatility. Prefers smooth, steady risers | `momentum_low_vol` |
| **Momentum 12-1** | Return over the last 12 months, skipping the most recent month | `momentum` (`{"lookback_months": 12, "skip_months": 1}`) |

Skipping the last month matters, because a stock's most recent month tends to reverse.

## The rotation

| Rule | Default | Why |
|---|---|---|
| Universe | The 500 stocks with the highest median daily traded value over the last 60 days, as of the rebalance day | Point in time, so stocks that later shrank are included; no survivorship bias from today's index list |
| Filters | Price ≥ ₹20; a year of unbroken history | Enough history for the signal; no penny stocks |
| Holdings | Top 30, equal rupees each | Stable from 10 to 50 holdings in testing; 30–40 has the smoothest ride |
| Rebalance | Decided on the close of the **first trading day of each calendar month**; traded at the next open | A fixed calendar day, the same live and in the backtest. Every 21 trading days or every 2 months test about the same |
| **Exit** | A held stock is sold at a rebalance when it ranks below 60 (`exit_rank`), or leaves the universe | Same return as selling below 30, about half the trading, smaller drawdown. No stop-loss: stops tested worse (below) |
| Sizing | Each new stock gets an equal share (equity ÷ 30) at the open. Kept stocks are trimmed or topped up to an equal share only when more than 25% away from it | Close to the backtest's equal weights without many tiny trades |
| Liquidity | Skip a stock if the position would be over 2% of its median daily traded value, and take the next one | So it can be bought and sold within a day |
| Costs | 40 bps per round trip on the replaced part | Delivery STT 0.1% each side, stamp, exchange, DP charges and mid-cap slippage |

## Tested results

All 2,601 NSE stocks, daily, 2010 to November 2026, rebalanced on the first trading day of each month, after costs and **before tax**:

| | Per year | Sharpe | Worst fall | Worst year | Turns a year | Beats the market (t) |
|---|---|---|---|---|---|---|
| Market: top 500, equal weight | +13.2% | 0.67 | −53% | −36% | — | — |
| **Momentum + low volatility, top 30, exit rank 60 (the one run)** | **+26.1%** | 1.23 | **−24%** | −11% | 2.4× | 3.4 |
| Momentum + low volatility, top 30, sold below 30 | +27.2% | 1.26 | −26% | −9% | 4.3× | 3.7 |
| Momentum 12-1, top 30, exit rank 60 | +28.5% | 1.09 | −46% | −25% | 2.1× | 4.2 |

Rebalancing every 21 trading days instead gave 24.9–26.7% for momentum + low volatility, depending on the starting day. That is the normal spread, so treat any one number as roughly ±2 points.

**Paper replay check:** running `rotation-paper` from 2025-11-03 to 2026-11-20 gave +14.3%, while the backtest over the same year gave +16.1% (the Nifty ETF made +1.0%). The paper account trades whole shares, pays for trims and top-ups and includes an unfinished month, so a small gap is expected.

**Robustness:**
- **Both halves of the period agree.**
- **Doubled costs** still give about 21–24% a year.
- **Neighbouring settings all work:**
    - holdings from 10 to 50;
    - volatility penalty from 2 to 4;
    - momentum from 9 to 12 months;
    - rebalancing every 2 weeks to every quarter;
    - starting on four different days of the month.
- **12-2 momentum (skip 2 months)** makes 24.6% a year, a little below 12-1, so 12-1 stays the default.

The full numbers are on the [Research](../research.md#monthly-rotation-on-every-nse-stock-2026-11-21) page.

## Exits

There is no stop-loss and no profit target. A stock is held as long as it keeps ranking near the top, which can be one month or many, and is sold at the next open after a rebalance where it ranks too low.

Tested over 2010–2026, top 30, first trading day of each month, after costs:

| Sold when | Momentum + low vol | Worst fall | Turns a year | Momentum 12-1 | Worst fall |
|---|---|---|---|---|---|
| It leaves the top 30 | +27.2% | −26% | 4.3× | +29.2% | −51% |
| It leaves the top 45 | +27.5% | −24% | 3.0× | +30.5% | −45% |
| **It leaves the top 60 (used)** | **+26.1%** | **−24%** | **2.4×** | +28.5% | −46% |
| It leaves the top 90 | +24.6% | −23% | 1.8× | +27.7% | −51% |
| A close 15% below entry (fixed stop) | +22.6% | −28% | 5.1× | +23.5% | −47% |
| A close 25% below entry | +25.9% | −30% | 4.5× | +27.4% | −50% |
| A close 20% below its peak (trailing stop) | +24.2% | −30% | 4.8× | +25.1% | −47% |

With the older every-21-trading-days schedule the picture was the same: stops lost 2–5 points a year, and a 60 buffer kept the return with half the trading. Buffers of 45 and 60 are within noise of each other. 60 is used because fewer sales mean fewer short-term tax events.

The stop-loss rows were tested with code that has since been removed, because stops lost in every test. `rotation-study --sections exits` now compares only the rank buffers.

**Why stops hurt here:**
- Momentum stocks are volatile, so stops fire on ordinary dips. The money then waits in cash while the stock often recovers. Stops sold near the lows, which made the worst fall *deeper*, not shallower.
- The ranking is already an exit: a stock that keeps falling loses its momentum score and is sold at the next rebalance.

**Why the buffer helps:** a stock moving between rank 28 and 35 isn't worse, so a buffer stops selling it and buying it back. That halves turnover, which halves costs and short-term tax events, for the same return.

## Risks

- **Bumpy years:** 5 of 17 years lost money, and a few huge years (2014, 2017, 2021) carry the average. It can trail the market for a year or two, as in 2024–2025.
- **Drops:** −29% at worst for momentum + low volatility, and −53% for plain momentum.
- **Capacity:** results hold up to about ₹50 lakh–₹2 crore. Above that, too many of the best picks are too thin to trade, and the edge fades by ₹10 crore.
- **Mid and small caps:** much of the edge comes from stocks ranked 200–500 by liquidity.
- **Tax:** about 4 portfolio turns a year means mostly short-term gains (20% STCG), which takes several points off. Rebalancing every 2 months cuts turnover by a third for the same return.
- **Market timing hurts it:** going to cash or gold when Nifty is weak cut the return from 25.5% to 14–18% a year. The strategy already moves away from falling stocks by itself.
- **Not covered:** delisted and merged companies (missing from Dhan's list), and live fills. A paper-trading period comes before any real money.

## Running it

**Once:** start a paper account.

```bash
scripts/questdb.sh start
cargo run --release -p tools --bin rotation-paper -- init --capital 1000000
```

The first rebalance is decided on the latest close in `bars_daily` and filled at the next open. `--start 2025-11-03` replays history from that day instead, which is how the replay check above was made (`--name replay` keeps it apart).

**Every evening** after the close (any time after about 18:00 IST):

```bash
scripts/rotation_daily.sh
```

This downloads the last 10 days into `bars_daily` from Dhan (about 12 minutes for 2,600 stocks, because of Dhan's rate limit), then runs `rotation-paper run`. Stocks with no trades in the window (Dhan error DH-907) count as "no data", not failures. The run:

1. **Fills** pending orders at the open of each new day: sells first, then trims, buys and top-ups, with 20 bps cost per side.
2. **Values** holdings at the close and tracks the peak and drawdown.
3. **Rebalances** on the first trading day of a month: decides the new targets on that close, to be filled at the next open.
4. **Reports** equity vs the Nifty ETF (`NIFTYBEES`) and every holding's P&L, day move and rank today. It also prints **the orders for the next open** (the ones you would place for real) and the next rebalance date.

**What the daily check does and doesn't do:** it watches every day but trades only on rebalance days, because daily selling (stops) tested worse. It **alerts** on:

| Alert | What to do |
|---|---|
| A holding has not traded today | Check for a suspension, delisting or merger. A sale that cannot fill is retried for 5 days |
| A holding fell 10% or more in a day | Read the news. It is not sold automatically |
| A holding ranks below 60, or left the universe | Nothing: it is sold at the next rebalance unless it recovers |
| Data more than 4 days old, or far fewer stocks than the day before | The download failed or is incomplete. Re-run the script |
| A split or bonus changed the price history | Handled automatically: shares are rescaled |

A missed evening loses nothing, because the next run processes every day since the last one. Running twice changes nothing.

Files in `data/paper/<name>/`:

| File | Holds |
|---|---|
| `state.json` | The account: cash, holdings, pending orders |
| `trades.csv` | Every fill |
| `equity.csv` | One row a day, with the Nifty ETF's close |
| `events.log` | Fills, rebalances and adjustments |
| `report.txt` | The latest report |

**Real holdings:** `rotation-plan` prints the same targets and the orders from a `symbol,shares` file of what you actually hold. Run it on the rebalance day's close:

```bash
cargo run --release -p tools --bin rotation-plan -- --capital 1000000 --holdings my_holdings.csv --out data/plan.csv
```

Neither tool places orders.

**Capital:** at ₹10 lakh each stock is about ₹33,000. A few very expensive shares come out as one or two shares (PTCIL at ₹24,000 a share), so their weight is a little off. Below about ₹3 lakh that gets coarse; ₹5 lakh or more works well.
