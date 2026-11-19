# Roadmap

## Done

- [x] Core types, exact money, Indian market time
- [x] QuestDB storage
- [x] Indicators
- [x] Synthetic market data
- [x] ORB strategy and strategy template
- [x] Cost model from Dhan's published rates
- [x] Risk manager
- [x] Order manager
- [x] Simulated broker with fault injection
- [x] Engine and backtester
- [x] Dhan API client, data downloader and automatic login
- [x] Connect to the real Dhan API, read-only first
- [x] Nifty 50 universe and real 1-minute history from December 2021
- [x] Live feed, live candles, and live and closed indicators ([Live monitor](runbook/live-monitor.md))

## Next

- [ ] First real ORB backtest
- [ ] Parameter sweeps, walk-forward and random-entry tests
- [ ] A stock list free of survivorship bias
- [ ] Daily tradable list: surveillance, corporate actions, real leverage
- [ ] More strategies

## Before real money

- [ ] Dhan order adapter and the paper/live trader
- [ ] One month of paper trading, matching the backtest
- [ ] Check costs against a real contract note
- [ ] AWS deployment with a static IP, required for placing orders
- [ ] Alerts and monitoring
- [ ] Start small

## Known gaps

- Market hours are fixed at 09:15-15:30. Since 3 August 2026, F&O stocks (all of the Nifty 50) trade continuously only until 15:15, then a closing auction runs until 15:35. Hours need to depend on the date and the stock. ORB is unaffected because it closes everything at 15:10.
- Dhan's own intraday auto square-off time after that change is not yet checked. Check it before trading live.
- The live feed is not yet connected to the engine, so strategies don't run on live data yet.
- The Nifty 50 list is today's, so long backtests on it carry survivorship bias.
- ORB takes no new trades after a mid-day restart.
- Whether ORB has any edge is still unknown. If it fails the tests, the right answer is not to trade it.
