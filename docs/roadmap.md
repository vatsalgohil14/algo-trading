# Roadmap

## Done

- [x] Core types, exact money, Indian market time
- [x] QuestDB storage
- [x] Indicators
- [x] Synthetic market data
- [x] Strategy template
- [x] Cost model from Dhan's published rates
- [x] Risk manager
- [x] Order manager
- [x] Simulated broker with fault injection
- [x] Engine and backtester
- [x] Dhan API client, data downloader and automatic login
- [x] Connect to the real Dhan API, read-only first
- [x] Nifty 50 universe and real 1-minute history from December 2021
- [x] Live feed, live candles, and live and closed indicators ([Live monitor](runbook/live-monitor.md))
- [x] Recording 5-level depth and indicator values in QuestDB, joinable by time
- [x] History checked; mock-trading days removed ([Data quality](data-quality.md))
- [x] First real backtest, Opening Range Breakout: failed before costs at every neighbouring setting, and deleted
- [x] Shared strategy base: daily state, entry gate, signal builder, test bench, registry ([Add a strategy](strategies/how-to-add.md))
- [x] Event-study tool to screen ideas against a baseline and costs ([Research](research.md)); four intraday ideas screened, all failed

## Next

- [ ] Parameter sweeps, walk-forward and random-entry tests
- [ ] A stock list free of survivorship bias
- [ ] Daily tradable list: surveillance, corporate actions, real leverage
- [x] Daily factor study with a walk-forward combined model: monthly contrarian ranking beats costs, not yet significant
- [ ] Nifty 200 daily history with point-in-time index membership, then rerun the factor study
- [ ] More strategies, once an idea passes the screen

## Before real money

- [ ] Dhan order adapter and the paper/live trader
- [ ] One month of paper trading, matching the backtest
- [ ] Check costs against a real contract note
- [ ] AWS deployment with a static IP, required for placing orders
- [ ] Alerts and monitoring
- [ ] Start small

## Known gaps

- Market hours are fixed at 09:15-15:30. Since 3 August 2026, F&O stocks (all of the Nifty 50) trade continuously only until 15:15, then a closing auction runs until 15:35. Hours need to depend on the date and the stock.
- Dhan's own intraday auto square-off time after that change is not yet checked. Check it before trading live.
- The live feed is not yet connected to the engine, so strategies don't run on live data yet.
- The Nifty 50 list is today's, so long backtests on it carry survivorship bias.
- History is not adjusted for bonuses, splits or demergers; see [Data quality](data-quality.md).
