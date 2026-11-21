# Research

!!! note "Tools removed"
    The intraday engine and the one-off research tools (`event-study`, `factor-study`, `statarb-study`, `daily-study`, `pattern-study`) were deleted on 2026-11-21 once their results were recorded here. Commands for them below are kept as a record only. `rotation-study` remains.: testing an idea before building it

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

## Intraday stat-arb: random-matrix factors + Ornstein-Uhlenbeck reversion (2026-11-21)

**Idea** (Avellaneda and Lee, 2010, moved to intraday bars): strip out the common factors, then trade each stock's leftover move back toward its equilibrium.

1. Each morning, form the correlation matrix of the last 10 sessions of 5-minute returns for the 50 stocks.
2. Keep only the eigenvectors above the Marchenko-Pastur noise edge: the real common factors, such as the market and sector baskets.
3. Every bar, a stock's residual is its return minus what those factors explain.
4. Fit an Ornstein-Uhlenbeck process (an AR(1)) to the last 60 bars of cumulative residual. That gives a half-life, an equilibrium level and its spread.
5. Trade the s-score (distance from equilibrium in spreads): short above +1.25 or +2, long below the mirror level. Close at −0.5 (long) or +0.75 (short), or at 15:10.
6. Only stocks whose half-life is 15–90 minutes. An optional cost gate requires the expected reversion (σ_eq·|s|) to be at least 1× or 3× the 15 bps round trip.

```bash
cargo run --release -p tools --bin statarb-study                 # 5-minute bars
cargo run --release -p tools --bin statarb-study -- --bar-minutes 15 --ou-bars 40
```

**The model behaves as the theory says:**
- about 6 factors above the noise edge each day, the first (the market) explaining about 21% of the variance;
- median residual half-life of 38 minutes, so the reversion does fit inside a session.

**Result: failed** (Jan 2022 to Nov 2026, entry at the next bar's open):

| Bars | Entry | Trades | Per trade, before costs | t | Net after 15 bps |
|---|---|---|---|---|---|
| 5 min | \|s\| > 1.25 | 80,427 | +0.6 bps | 3.5 | −14.4 |
| 5 min | \|s\| > 1.25, cost gate 3× | 33,568 | +0.5 bps | 1.6 | −14.5 |
| 5 min | \|s\| > 2 | 17,544 | +0.3 bps | 0.7 | −14.8 |
| 15 min | \|s\| > 1.25 | 42,770 | −0.5 bps | −1.6 | −15.5 |
| 1 min | \|s\| > 1.25 | 112,806 | +0.3 bps | 3.6 | −14.7 |

- **The reversion is real but tiny:** t 3.5 on 80,000 trades, worth about half a basis point. The factor-hedged residual earns the same, so hedging doesn't rescue it.
- **The cost gate doesn't help:** the OU fit "expects" 45+ bps of reversion on 33,000 trades, but they deliver 0.5. On 60 bars the fitted spread is mostly noise in a near-random walk, so the expected reversion is badly overstated.
- **It's the same in every year,** and at 1, 5 and 15 minutes.

**Why it fails here and works on desks:** desks run this with costs of 1–3 bps, on thousands of names, entering at the signal price rather than the next bar's open. With 15 bps of retail cost per round trip on cash equity, the edge would need to be about 30 times larger.

**Not testable with our data:** order-flow models such as Hawkes processes need every trade with its aggressor side; Dhan's feed sends about one snapshot per second. A cash-futures Kalman spread needs futures history, which we don't store.

## Monthly rotation on every NSE stock (2026-11-21)

**Data:** daily candles for all 2,601 NSE company shares in Dhan's list (EQ and BE series, Indian ISINs), January 2008 to November 2026, in `bars_daily` (6.3 million rows, from `dhan-daily`). Prices are adjusted for splits and bonuses, and the close is NSE's official closing price.

**Universe, point in time:** on each rebalance, the 500 stocks with the highest median daily traded value over the previous 60 days, priced at least ₹20, with a year of unbroken history. 1,285 different stocks passed through it over 2016–2026. This avoids using today's index members. Companies that were delisted or merged away are still missing, because Dhan's list only has current listings.

**Method:** every 21 trading days, rank the universe, buy the top 20 or 30 in equal amounts at the next open, and hold to the next rebalance. Costs are 40 bps per round trip on the replaced fraction (delivery STT, stamp, exchange, DP and mid-cap slippage). The benchmark is all 500, equally weighted. The trend filter goes to cash while that benchmark is below its 200-day average.

```bash
cargo run --release -p tools --bin rotation-study                          # 2016-2026
cargo run --release -p tools --bin rotation-study -- --from 2010-01-01 --cache data/daily_nse_2008.csv
```

### Results, 2010–2026 (17 years), top 30, after costs

| Strategy | CAGR | Sharpe | Max drawdown | Worst year | Excess per month (t) |
|---|---|---|---|---|---|
| Benchmark: all 500 equal weight | +13.3% | 0.68 | −54% | −33% | — |
| **Momentum + low volatility** | **+25.4%** | **1.18** | **−29%** | **−15%** | **+0.85% (3.4)** |
| Momentum 12-1 | +28.7% | 1.06 | −53% | −34% | +1.20% (4.2) |
| Volatility-adjusted momentum | +24.5% | 1.01 | −46% | −27% | +0.86% (3.1) |
| Low volatility | +10.7% | 0.89 | −19% | −18% | −0.35% (−1.2) |
| Momentum 6-1 | similar to 12-1, weaker | | | | |
| Near 52-week high | about the benchmark | | | | |
| Far below the 6-month high (the 50-stock lead) | −0.6% (2016–26) | 0.14 | −79% | | −0.98% (−2.7) |
| 1-month reversal | +4.2% (2016–26) | 0.29 | −68% | | −0.68% (−2.6) |

**Checks on the momentum family:**
- **Before the main period:** 2010–2015, which the 2016–2026 work never looked at, is even stronger. Momentum 12-1 top 20 made +43% a year (t 5.0); momentum + low volatility top 30 made +29% (t 2.9).
- **Main period alone,** 2016–2026: momentum 12-1 top 30 made +26.3% (t 2.5); momentum + low volatility top 30 made +22.9% (t 1.9), against a benchmark of +14.4%.
- **Doubled costs** (80 bps): momentum 12-1 top 30 still made +24.4%, and momentum + low volatility +20.7% (2016–2026).
- **Top 200 instead of 500:** the edge shrinks to about +3–5% a year over the benchmark. Much of it comes from stocks ranked 200–500 by liquidity, the mid and small caps.
- **Trend filter:** it cuts drawdowns but costs 4–6% a year of return. Not worth it here.

### Verdict: momentum passes, the contrarian lead was an artefact

- **Momentum in Indian stocks is real in this data.** It shows up in both halves, holds with doubled costs, and reaches t 3–4 over 17 years.
- **Momentum + low volatility is the best balance:** about 25% a year with half the drawdown of plain momentum. It's the candidate to build.
- **The 50-stock contrarian result reversed on the unbiased universe** (−0.6% a year). That's the survivorship bias we suspected, now measured.

**Before building it:**
1. **Parameter stability:** the volatility penalty (3×), rebalance day offsets, and 15/20/30/40 holdings.
2. **Liquidity:** check the traded value of what it buys against the capital size, since the edge sits in mid and small caps.
3. **Taxes are not included.** About 4 portfolio turns a year means most gains are short-term (20% STCG), which takes several points off the net return.
4. **Survivorship:** delisted companies are missing; momentum rarely holds them, but this remains unmeasured.

### More rotation research (2026-11-21)

These all run on the `rotation` crate, the same code `rotation-plan` uses. The crate reproduces the numbers above.

**New ranking signals, 2010–2026, top 30:**

| Signal | CAGR | Max drawdown | t |
|---|---|---|---|
| **Momentum consistency** (months up out of the last 11, ties broken by 12-1) | +26.7% | −29% | 4.0 |
| Residual momentum (12-1 of returns after removing market beta) | +25.1% | −41% | 3.4 |
| Momentum 6-1 | +24.7% | −53% | 3.4 |
| 1-month reversal | −2.6% | −77% | −4.3 |

Momentum consistency is as good as momentum + low volatility. It's the next candidate for the crate.

**Stability of the two chosen strategies** (full tables in `data/rotation_research_2010.txt` and `_2016.txt`):

| What was varied | Result |
|---|---|
| Holdings, 10–50 | Works throughout; 30–50 is smoothest (momentum + low volatility top 40–50: −25% drawdown) |
| Volatility penalty, 0–6 | 2–4 is best; 3 balances return and drawdown |
| Momentum windows | 12-1 best (+28.8%), then 9-1 (+26.3%), 12-2 (+24.6%) and 12-3 (+23.3%) |
| Start day of the month | 2016–2026 spread: 23–28% for momentum + low volatility, 24–26% for 12-1. Part of any single result is luck |
| Rebalance frequency | 2 weeks to quarterly all work; every 2 months keeps the return with a third less turnover |
| Capital (2% of daily value cap) | ₹10–50 lakh unchanged; ₹2 crore −3%/yr; ₹10 crore −9%/yr; ₹50 crore gone |
| Half low-volatility sleeve | Smoother but gives up 6–8%/yr. Not worth it |

**Other daily strategies** (`daily-study`, 2010–2026):

| Strategy | CAGR | Max drawdown | Verdict |
|---|---|---|---|
| Swing pullback: close above 200-day average, RSI(2) < 10, sell above 5-day average or after 10 days, 10 slots | +13.0% | −51% | **Fails:** about the market, 570 trades a year, and −10% a year at 80 bps |
| Buy and hold Nifty (NIFTYBEES) | +10.4% | −30% | Reference |
| **Monthly asset rotation:** best 12-month performer of Nifty, Nifty Next 50 and gold, if it beats 6.5% cash | **+16.7%** | **−19%** | **Passes:** a low-risk core, about 2 switches a year |
| Momentum + low volatility, but cash when Nifty's 12-month return < cash | +14.3% | −33% | Hurts: misses the rebounds |
| Same, gold instead of cash when gold is strong | +18.0% | −29% | Hurts, less |

The asset rotation assumes cash earns 6.5% a year; LIQUIDBEES pays its interest as units, so its price can't be used. Gold ETFs are taxed differently from shares.

### Exits for the rotation (2026-11-21)

Should a momentum stock be sold on a stop-loss? Tested over 2010–2026 with the `rotation` crate (`stop_loss`, `trailing_stop`, `exit_rank`). Full table on the [Momentum rotation](strategies/rotation.md#exits) page.

- **Fixed and trailing stops (15–25%) all lower the return** by 2–7% a year and make the worst fall deeper. Momentum stocks are volatile: stops sell ordinary dips and miss the rebounds.
- **A rank buffer** (keep a held stock until it falls below rank 45–90) gives the same return with half the turnover. For momentum + low volatility it also cuts the worst fall from −29% to −23%.
- Recommended: `--exit-rank 60`, no stop.

### Calendar rebalancing and a paper replay (2026-11-21)

**Calendar rebalancing:** the rotation now rebalances on the close of the first trading day of each month, a day you can know in advance, instead of every 21 trading days. Over 2010–2026, momentum + low volatility with exit rank 60 made +26.1% a year with a −24% worst fall. On the old schedule it made 24.8%, and 24.9–26.7% depending on the starting day. So the calendar changes nothing real; it makes live runs and the backtest pick the same days.

**Paper replay:** the day-by-day paper account (`rotation-paper`) made +14.3% from 2025-11-03 to 2026-11-20, while the backtest made +16.1% over the same year. The account trades whole shares, keeps equal weights within a 25% band and includes the unfinished November. The replay also caught a sizing bug before it went live: without trimming, a stock that had risen 3.5× took up so much of the account that later buys ran out of cash.

## Candlestick patterns and classic indicators (2026-11-21)

**Question:** do the usual chart signals predict returns on NSE stocks?

**Method** (`pattern-study`):
- 11 candlestick patterns (textbook definitions, with the prior trend they need) and 13 indicator signals: RSI, MACD, Bollinger Bands, Supertrend(10,3), golden and death crosses, 20-day volume breakouts, 20-day average crosses.
- Daily bars, 2010–2026, only stocks in that month's top 500.
- Enter at the next open; measure the return beyond the equal-weight top 500 over 1, 5, 10 and 20 days.
- t treats each signal day as one observation. Costs for delivery are about 40 bps per round trip.

```bash
cargo run --release -p tools --bin pattern-study
```

**Results, return beyond the market, 20-day hold, in bps:**

| Signal | Expected | Events a year | 20 days (t) | Verdict |
|---|---|---|---|---|
| Hammer after a fall | up | 624 | +14 (1.0) | Noise |
| Bullish engulfing after a fall | up | 2,513 | +3 (0.3) | Noise |
| Piercing line | up | 311 | +16 (0.8) | Noise |
| Morning star | up | 152 | −35 (−1.5) | Wrong way, not significant |
| Three white soldiers | up | 225 | −39 (−1.5) | Wrong way |
| Doji | — | 14,163 | +4 (1.8) | Nothing |
| Shooting star after a rise | down | 1,108 | +21 (1.8) | Wrong way |
| Bearish engulfing after a rise | down | 4,469 | +1 (0.1) | Noise (−10 bps next day, t −6.7, too small to trade) |
| Dark cloud cover | down | 490 | −26 (−1.9) | Right way, weak |
| Evening star / three black crows | down | 180 / 770 | −8 / −24 | Weak |
| **Supertrend turns up** | up | 1,395 | **−81 (−7.0)** | **Strongly wrong way** |
| **20-day high breakout on 2× volume** | up | 3,259 | **−42 (−4.6)** | **Wrong way** |
| Close above the upper Bollinger band | up | 4,075 | −35 (−4.9) | Wrong way |
| Close below the lower Bollinger band | up | 3,275 | −50 (−5.3) | Wrong way: weak stocks stay weak |
| RSI(14) crosses up through 30 | up | 1,544 | −65 (−5.3) | Wrong way |
| RSI(14) crosses up through 70 | up | 2,339 | −6 (−0.6) | Noise |
| MACD crosses above its signal | up | 4,802 | −13 (−2.2) | Wrong way |
| Golden cross | up | 312 | +15 (0.9) | Noise |
| Death cross | down | 362 | −39 (−2.5) | Right way, weak |

**Verdict:**
- **Candlestick patterns carry no tradable information here.** Effects are within ±40 bps, rarely significant, flip sign across years, and are well below the 40 bps round trip.
- **Popular indicator buy signals do worse than the market.** A signal that fires after a sharp move up (Supertrend flip, volume breakout, Bollinger breakout, MACD cross) buys the next open, after the jump, and the stock then lags by 0.3–0.8% over a month. Oversold signals (RSI up through 30, lower Bollinger band) lag too: falling stocks keep falling, the same fact that made reversal lose and momentum win.
- What works is slow and cross-sectional: ranking stocks on 6–12 months of steady gains. One-day chart signals are too common, too crowded and too short.
- Bearish signals (death cross, dark cloud cover) point the right way, weakly. They could be tested later as sell filters for held stocks; the stop-loss results suggest little gain.
