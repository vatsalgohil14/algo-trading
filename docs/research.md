# Research: testing an idea before building it

A full strategy takes time to write and a backtest mixes many decisions together. Before that, test the idea itself: when the condition happens, does the price then move the way the idea expects, by more than chance and more than costs?

```bash
cargo run --release -p tools --bin event-study -- --idea vwap-reversion
```

Ideas: `vwap-reversion`, `gap-fade`, `relative-strength`, `volume-shock`. Each has its own default thresholds and holding times; override them with `--thresholds` and `--horizons`.

For every time the condition fires in history, the tool enters at the next candle's open (the decision needs the candle's close), holds for 15, 30 and 60 minutes, and never holds past the 15:10 square-off. It reads the same history, with the same session filter and indicator code, as the backtester.

## Reading the table

| Column | Meaning |
|---|---|
| `events` | Times the condition fired (once per stock, side and day) |
| `gross` | Average move in the expected direction, in basis points (1 bp = 0.01%) |
| `vs base` | The move beyond what that stock usually does from that minute of the day, over all days. This is the real effect of the idea |
| `net` | `vs base` minus costs (default 15 bps per round trip: about 10.8 bps of charges on a ₹30,000 trade, measured, plus slippage) |
| `t` | How unlikely the effect is to be chance: beyond ±2 is notable, beyond ±3 strong. With many rows, expect a few beyond ±2 by luck |
| `net@2x` | Net with doubled costs |
| `net by year` | Net per year, to catch an effect that only one year produced |

An idea passes the screen only if `net` is clearly positive, `t` is well above 2, and most years are positive, on both sides or with a reason why one side should differ. Only then is it worth writing as a strategy and backtesting.

## Results

### VWAP reversion: failed (2026-11-19)

**Idea:** between 10:00 and 14:00, a stock that moves far from its VWAP drifts back toward it. Distance is measured in daily ATRs (0.25, 0.5, 0.75 and 1.0). Long when it falls below VWAP by the threshold, short when it rises above.

**Result** (Nifty 50, Jan 2022 to Oct 2026, 65,884 events): no variant clears costs.

- The effect is a few basis points against 15 bps of costs. Most variants show 0–5 bps gross.
- Shorts above VWAP lose slightly and consistently (−1 to −4 bps, t around −4): prices that run above VWAP tend to keep going a little, the opposite of reversion. Real, but far too small to trade.
- The only positive hint is buying deep drops (1 ATR below VWAP): +14.5 bps over 15 minutes, t 2.8, on 431 events. After costs it is still about zero, the years disagree, and one notable row out of 24 is what chance alone would produce.

**Lesson:** on these stocks, the average move 15–60 minutes after a typical intraday signal is a few basis points, while a round trip costs about 15. An intraday idea needs an expected move of 30 bps or more per trade to be worth testing further. That points to rarer, stronger events or longer holds, not frequent small fades.

### Gap fade: failed, one lead (2026-11-19)

**Idea:** a stock that opens far from yesterday's close (0.5, 1, 2 or 3%) fills part of the gap. Short gap-ups, buy gap-downs, entered at 09:16 and held 30, 60, 120 minutes or to 15:10. Gaps beyond 8% are skipped as unadjusted corporate actions.

**Result** (31,193 events):

- Fading gap-ups fails at every size: gap-ups keep drifting up slightly.
- Buying gap-downs improves with the size of the gap: +3 to +6 bps for 0.5% gaps, +12 to +15 for 2%, and +32 bps held to 15:10 for gaps of 3% or more (t 2.0, 331 events). That is the only row in any study so far that clears costs (+17 net), but barely, and 2023 alone (+172) carries it; 2022 and 2024 lose.

**Verdict:** not tradable as is. "Buy large gap-downs and hold the day" is a lead worth one narrower follow-up (for example only when the index itself did not gap down, which would separate stock news from market moves), not a strategy.

### Relative-strength catch-up: failed (2026-11-19)

**Idea:** at 10:14, a stock whose return since the open trails the median Nifty 50 stock by 0.5–2% catches up (buy), and one far ahead gives some back (short). Held 30, 60, 120 minutes or to 15:10.

**Result** (50,633 events): both sides lose before costs (−1 to −10 bps). Laggards keep lagging a little, leaders keep leading a little: mild momentum, not catch-up. Flipped into a momentum trade it would make at most +4 to +9 bps, still far below costs. Every year agrees.

### Volume shock: failed (2026-11-19)

**Idea:** between 09:30 and 14:00, a candle with 3, 5 or 8 times its normal volume for that minute (`vol_tod(20)`) and a move of at least 0.1% continues in its direction. Held 15, 30 or 60 minutes.

**Result** (182,908 events): the follow-through is 0–1.6 bps. Statistically there (t up to 3 on longs) because the sample is huge, but a hundredth of what costs need. Shocks are also far more common than expected: 3× normal volume happens on about 40,000 candles a year.

### Where this leaves us

Four ideas, about 330,000 events: on Nifty 50 stocks, intraday signals built from one stock's own price and volume over 15–60 minutes move a few basis points, and a round trip costs 15. The only move large enough is buying big gap-downs and holding the day. Next directions with a real chance of 30+ bps per trade:

- **Longer holds:** overnight or multi-day swing trades in delivery (no intraday square-off; STT on delivery is higher, so this needs its own cost check).
- **Rarer events with outside information:** results days, index rebalances, block deals; these need data we don't store yet.
- **Cheaper instruments:** index futures, where costs per round trip are a fraction of cash equity.

## Daily factor study (2026-11-21)

Intraday signals can't pay for costs, so this study holds for days instead. Each evening it ranks the 50 stocks on 14 signals built from daily bars (made from the 1-minute history), buys the top 10 at the next open and holds 1, 5, 10 or 20 days.

```bash
cargo run --release -p tools --bin factor-study      # first run caches daily bars to data/daily_bars.csv
```

- **Rank IC:** the correlation between a signal's ranking today and the ranking of the coming returns. Above 0.02 with t above 3 is usable.
- **Combined model:** weights every signal by its past IC, using only returns that had already finished, so its trading results (from Dec 2022) are out of sample.
- **Costs:** 28 bps per round trip for cash delivery (STT 0.1% each side, plus stamp, exchange, DP and slippage). 10 bps per leg for stock futures (STT 0.05% on the sell side since April 2026, plus slippage).
- **Data breaks:** the BSE 2022 and TMPV 2025 jumps (unadjusted corporate actions) split those stocks' history, so no return spans them.

### What it found

| Signal | What it means | Result |
|---|---|---|
| `rev_1d`, `intraday_rev` | Today's losers beat today's winners **tomorrow** | Real: IC +0.025, t 5, positive every year. But trading it daily turns over 68% of the book, and costs eat it (out-of-sample CAGR −24%) |
| `near_high`, `low_vol`, `mom_3m` (negative) | Over the next **month**, stocks far below their 6-month high, volatile stocks and recent laggards beat the rest | IC −0.04 to −0.06 at 20 days, t about 2.5, same sign in most years |
| Momentum (`mom_1m`/`3m`/`6m`) | Classic "winners keep winning" | Absent in this universe and period; slightly reversed at 1–3 months |
| Volume, VWAP, gap, overnight/intraday split | Assorted | Nothing at t above 3 |

**Best results traded out of sample (Dec 2022 to Nov 2026, after costs):**

| Strategy | CAGR | Sharpe | Max drawdown | Excess per trade (t) |
|---|---|---|---|---|
| Equal-weight all 50 (no costs) | +15.0% | 1.04 | −12.5% | — |
| Combined model, monthly, long top 10 | +19.7% | 1.03 | −19.6% | +53 bps (1.3) |
| Furthest below 6-month high, monthly, long top 10 | +21.4% | 1.17 | −14.8% | +63 bps (1.6) |
| Same, long 10 / short 10 with futures | +12.5% | 0.95 | −8.0% | |

### Verdict: promising direction, not proven

- Monthly holds are the first setting where the moves (50–60 bps per trade beyond the market) are bigger than the costs.
- But none of it is statistically solid: t is 1.3–1.6 on about 47 monthly rebalances. Four years of 50 stocks is too little data to tell an edge of this size from luck.
- **Survivorship bias works in this result's favour, so it is probably overstated.** The universe is today's Nifty 50. A stock that fell and never recovered dropped out of the index and isn't in our data, while one that fell and recovered is. "Buy the stocks furthest below their high" is exactly the strategy this bias flatters. The equal-weight +15% a year is inflated the same way.

**To make it trustworthy:**

1. **A bigger universe.** Nifty 200 or 500 instead of 50: ranking needs breadth, and the noise falls with the number of stocks. Daily bars are enough for this, and Dhan's daily history goes back many years, so it's a quick download.
2. **Point-in-time membership.** Use who was actually in the index on each date (NSE publishes index changes), including stocks that later left.
3. **Longer history.** 10+ years of daily data covers more than one market regime.

Then rerun this study. If the monthly contrarian and low-high ideas still hold at t above 3, build them as a strategy.
