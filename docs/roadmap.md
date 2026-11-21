# Roadmap

## Done

- [x] QuestDB storage, Dhan API client and automatic login
- [x] Daily history for all 2,601 NSE stocks (2008 on), checked and cleaned ([Data quality](data-quality.md)), and a point-in-time top-500 universe
- [x] Intraday ideas tested on 1-minute data (opening range breakout, VWAP reversion, gap fade, volume shock, stat-arb): all failed after costs, and the intraday engine was deleted ([Research](research.md))
- [x] Momentum rotation: `momentum_low_vol` and `momentum` 12-1 pass over 2010–2026 ([Momentum rotation](strategies/rotation.md))
- [x] Exit rules tested: a rank buffer beats stop-losses ([Exits](strategies/rotation.md#exits))
- [x] Candlestick patterns and classic indicator signals tested: no edge ([Research](research.md#candlestick-patterns-and-classic-indicators-2026-11-21))
- [x] One strategy built end to end: momentum + low volatility, top 30, month start, exit rank 60; daily paper trading and monitoring (`rotation-paper`, `scripts/rotation_daily.sh`)

## Next

- [ ] Paper trade the rotation for a few months (started 2026-11-20), and compare it with the backtest
- [ ] Schedule the evening job
- [ ] Surveillance flags (ASM/GSM) in the daily alerts
- [ ] Delisted and merged companies in the history, to remove the last survivorship bias

## Before real money

- [ ] Check costs against a real contract note
- [ ] Place the monthly orders through Dhan (today they are printed only)
- [ ] Start small
