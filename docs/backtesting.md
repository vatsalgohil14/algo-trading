# Backtesting

A backtest runs the real engine over history, with a simulated broker in place of Dhan.

```bash
cargo run --release -p backtester -- --config config/backtests/orb_synthetic.toml
```

You get P&L after costs, results by setup, exit and month, how often each filter blocked a trade, and a health check. `trades.csv` and `daily.csv` are saved for further analysis.

## The simulated broker is a pessimist

A backtest that fills too kindly is worse than none.

| Situation | Assumption |
|---|---|
| A decision | Can never trade on its own bar |
| Limit order | Must trade *through* the price. Only touching it isn't a fill |
| Runaway breakout | The entry is simply missed |
| Large order | Takes at most 10% of a bar's volume |
| Stop | Fills at the worse of stop and open, plus slippage |
| Bar hits both stop and target | The stop came first |
| Bar of entry | The stop is live, the target isn't |

## The pass mark

| Metric | Needed |
|---|---|
| Trades | At least 300 |
| Profit factor after costs | At least 1.3 |
| Average per trade | At least 0.15R |
| Maximum drawdown | At most 10% of capital |
| Profitable months | At least 60% |

R is the amount risked on the trade.

## One good run proves very little

Before believing a result:

1. **Neighbouring settings work too.** If only one exact combination wins, it's luck.
2. **It works out-of-sample.** Tune on two years, test on the next six months, roll forward.
3. **It survives different markets**, including 2020 and 2022.
4. **It survives doubled slippage:** add `--slippage-mult 2`.
5. **It beats random entries** at the same times.
6. **One month of paper trading** matches the backtest.

!!! danger "Synthetic data says nothing about profit"
    The random data tests the machinery only. Any profit or loss it shows is meaningless. Also, the starter stock list is chosen from today's large companies, which flatters results over past years.
