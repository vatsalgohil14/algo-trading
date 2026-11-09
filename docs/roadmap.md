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

## Next

- [ ] Connect to the real Dhan API, read-only first
- [ ] Download real history
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

- Nothing has run against real Dhan yet. Expect some fixes.
- ORB takes no new trades after a mid-day restart.
- Whether ORB has any edge is still unknown. If it fails the tests, the right answer is not to trade it.
