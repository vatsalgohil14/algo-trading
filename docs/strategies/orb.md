# ORB v1: Opening Range Breakout

Status: implemented in `crates/strategies/src/orb` (rules below); not yet
backtested on real data. Treat it as a hypothesis until it passes the
checks under [Validation](#validation).

Settings named in `code` are fields of `OrbConfig`; defaults are shown.

## 1. Stock filters (once per stock and day)

| Rule | Default | Why |
|---|---|---|
| Average daily turnover over `turnover_days` days | ≥ ₹50 crore (`min_turnover_cr`), 20 days | Tight spreads, fills near our price |
| Price (opening range) | ₹100 – ₹5,000 (`min_price`, `max_price`) | No penny-stock noise or huge share prices |
| Opening gap vs yesterday's close | ≤ 3% (`max_gap_pct`) | News days behave differently |
| Tradable today: MIS allowed, not ASM/GSM, no corporate action, not near a circuit | `Context::tradable`, set before the open | Erratic moves, may be impossible to exit |
| History | Enough days for the daily ATR and turnover | No guessing on new listings |

## 2. Opening range

- First **15 minutes** (`range_minutes`): the 09:15 to 09:29 bars.
- **All bars must be present.** If any is missing (feed down at the open), no trade in that stock today.
- **Width filter:** range width between **0.25× and 1.0×** the daily ATR(14) (`min_range_atr`, `max_range_atr`, `atr_days`).
  - Too narrow is noise, which means many false breakouts.
  - Too wide means a far stop, and the day's move is used up.

## 3. Entry (long; short is the mirror image)

Every condition must hold on a completed 1-minute bar:

1. It **closes** at or above range high + buffer. The buffer is 5% of the range width, at least one tick (`breakout_buffer`). A spike that closes back inside doesn't count.
2. The bar starts before **11:30** (`entry_end`) and before the session's last-entry time (14:45).
3. Close is **above VWAP** (`require_vwap`).
4. Bar volume is ≥ **1.5×** the average of the previous 20 bars (`volume_mult`, `volume_bars`).
5. Close is no more than **0.5× the range width** beyond the level (`max_chase`): don't chase.
6. The feed is healthy, and the stock hasn't been tried today: one attempt per stock per day.
7. Strategy limits: fewer than 5 trades open (`max_open`), fewer than 8 entries today (`max_entries_per_day`), fewer than 3 losers today (`max_losses_per_day`).

**Order:** limit at close + 0.1% (`limit_offset_pct`), rounded away from the close to the tick. The stop and target go to the broker with the entry (a super order).

- If it isn't filled within **3 minutes** (`entry_timeout_minutes`), the entry is cancelled.
- If a fill arrives after that, the position is closed immediately.

## 4. Stop and target

- **Stop:** the other side of the range.
  - If it's more than **1.2%** from the entry (`max_stop_pct`), skip the trade. The stop is never pulled closer to make the trade fit.
- **Target:** **2R** (`target_r`), where R = entry − stop.
  - Skip the trade if the target is less than 0.3% away (`min_target_pct`): too small to pay the charges.
  - The cost model will replace this with an exact check.

## 5. Managing an open trade (checked on each bar close, in this order)

| Exit | Rule | Signal reason |
|---|---|---|
| Square-off | At 15:10, close everything; also cancel an unfilled entry | `square_off` |
| Backstop | A close at or beyond the current stop while still in the position means the broker stop didn't work: exit | `stop_backstop` |
| Failed breakout | A close back inside the range by more than 25% of its width (`failed_breakout_depth`) | `failed_breakout` |
| Time stop | After 60 minutes (`time_stop_minutes`), if price never got +0.5R (`time_stop_min_r`) | `time_stop` |
| Breakeven | Once price has reached +1R (`breakeven_r`), move the stop to the entry, once | `AdjustStop`, `breakeven` |

**Other cases:**
- **Stop or target hit at the broker:** the position turns flat and the trade is over.
- **Exit doesn't go through:** the exit is repeated each bar until the position is flat. The order manager ignores repeats while an exit is in flight.
- **Unexpected positions:** these are closed at once (`unexpected_position`, `stale_position`). Examples: the wrong side, a fill after the entry was given up, or a position left from a previous day.

## 6. Sizing and account limits (risk manager, not the strategy)

| Rule | Value |
|---|---|
| Risk per trade | 0.5% of capital (₹2,500 on ₹5L): qty = ₹2,500 ÷ (entry − stop) |
| Max exposure per trade | ₹1.5L |
| Daily loss limit | 2% of capital stops all trading |
| Gross exposure cap | about 3–3.5× capital |
| Exact cost check | Expected reward ≥ 4× round-trip charges |

## Design notes

- **State comes from the position, not from orders.** On every bar, the strategy is told its position, which mirrors the broker. A rejected entry, a stop hit at the broker and a restart are all handled the same way.
- **No lookahead.**
  - A signal is decided at a bar's close and acted on from the next bar.
  - Daily ATR and turnover use completed days only.
  - The volume average excludes the current bar.
- **Every rejection is counted** (`Orb::rejections`) as stock-days per filter. A backtest report can then show which filters do the work.

## Validation

Before any real money, all of these must hold:

1. Few settings with round values, nothing tuned per stock.
2. **Stable around the defaults:** good results across neighbouring settings (range 10/15/20/30 min; target 1.5/2/2.5/3R; and so on), not at one exact combination.
3. **Walk-forward:** pick settings on 2 years of data, test on the next 6 months, roll forward. Only out-of-sample results count.
4. **Across regimes:** the 2020 crash, the sideways 2022, the 2023–24 trend years.
5. **Still profitable with doubled slippage.**
6. **At least 300 trades.**
7. **Clearly better than random entries** at the same times with the same exits.
8. **One month of paper trading**, with fills matching the backtest.

**Pass mark:**
- Profit factor ≥ 1.3 after costs.
- Average ≥ 0.15R per trade.
- Maximum drawdown ≤ 10% of capital.
- Profitable in ≥ 60% of months.

## Later (v2, only after v1 is proven)

- Market filter: longs only when NIFTY is above its VWAP, shorts only below.
- Partial profit at 1R plus a trailing stop.
- One reversal trade after a failed breakout.
