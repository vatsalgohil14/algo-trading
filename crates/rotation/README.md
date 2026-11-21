# rotation

Monthly stock rotation on daily bars. The backtest, the paper account and the real monthly plan use the same code.

```rust
let panel = Panel::from_csv(&csv)?;                       // date,symbol,open,high,close,value
let strategy = build("momentum_low_vol", Value::Null)?;  // or "momentum" (12-1 by default)
let config = RotationConfig::recommended();              // top 500, hold 30, month start, exit rank 60, 40 bps

let bt = backtest(&panel, Some(strategy.as_ref()), &config, from, to, None)?;
let bench = backtest(&panel, None, &config, from, to, None)?;   // equal-weight universe
println!("{:?} excess {:?}", bt.summary(), excess(&bt, &bench));

let targets = plan(&panel, strategy.as_ref(), &config, last_day, 1_000_000.0, &held)?;
let orders = trades(&targets, &current_holdings);          // sells first

let mut account = paper::Account::new("momentum_low_vol", Value::Null, config, 1_000_000.0, start)?;
let progress = account.advance(&panel, strategy.as_ref())?; // every day since the last call
```

| Piece | What it does |
|---|---|
| `Panel` | Daily bars of many stocks on one calendar. A one-day jump beyond ±50%, or more than 10 missing days, restarts a stock's history, so no signal or return spans it |
| `universe` | The `size` stocks with the highest median traded value over the last 60 days, on that day |
| `Ranker` | Scores a stock on a day: `Momentum`, `MomentumLowVol`, or any function via `FnRanker` |
| `select` | Top scores, skipping stocks too thin for the capital (2% of daily traded value); with `exit_rank`, held stocks are kept while they rank within it |
| `Schedule` | `MonthStart` (first trading day of each month, the default) or `EveryDays(n)` |
| `backtest` | Rebalance on the schedule, buy at the next open, costs on the replaced part; optional daily `stop_loss` / `trailing_stop` (tested worse, off by default) |
| `plan` / `trades` | Target holdings in whole shares, and the orders from current holdings |
| `paper::Account` | Day-by-day paper trading: fills at the open with costs, equal weights with a 25% band, split/bonus rescaling, retries for stocks not trading. Plain data, saved as JSON between runs |

Rules, results and risks: [docs/strategies/rotation.md](../../docs/strategies/rotation.md). Research: `rotation-study` in `apps/tools`.
