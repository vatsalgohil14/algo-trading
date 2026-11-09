# risk

Decides whether a signal becomes a trade, and how big. Used the same way in backtest, paper and live trading.

```rust
let mut risk = RiskManager::new(&RiskConfig::default(), CostModel::default(), universe.len())?;
risk.set_leverage(id, 5.0)?;                    // from the broker's margin list

match risk.evaluate(&signal, instrument, last_close) {
    Decision::Enter(a) => { /* place a.qty shares */ }
    Decision::Allow => { /* exit or stop change */ }
    Decision::Reject(why) => { /* counted in risk.rejections() */ }
}

risk.release(id);                               // entry order finished
if let Some(event) = risk.on_fill(&fill) { /* flatten everything */ }
if let Some(event) = risk.on_mark(id, close, ts) { /* flatten everything */ }
```

## Rules (defaults for ₹5L of capital)

| Rule | Default |
|---|---|
| Exposure per trade | ₹30k + score × ₹20k, in whole lots |
| Score | Per setup (`setup_scores`), 0 unless listed. Add one only after out-of-sample results earn it |
| Maximum loss per trade | ₹500 at the stop. Size shrinks to fit; skipped if that takes it below ₹30k |
| Reward vs cost | Expected reward ≥ 4× expected round-trip cost |
| Positions per stock | One across all strategies, because the broker nets per stock |
| Total exposure | ₹15L |
| Net long or short exposure | ₹7.5L |
| Margin | Exposure ÷ the stock's leverage; the total stays within capital less today's loss |
| Daily loss | ₹10k including charges and open positions: halts new trades and reports `DailyLossHit` |

- Exits are never blocked, even while halted.
- A stop can only be tightened.
- `halt()` is a manual kill switch that stays on until `resume()`.
- `restore()` rebuilds positions from the broker after a restart, with today's realized P&L, so a restart can't reset the daily loss limit.

**Worked example:** with score 1, entry ₹500 and stop ₹494 risk ₹6 a share. ₹500 ÷ ₹6 = 83 shares, ₹41,500 of exposure instead of ₹50,000.

## What the caller must do

- Pass each fill once; the order manager removes duplicates by trade id.
- Call `release` when an entry order finishes, otherwise the stock stays reserved.
- Close every position when `DailyLossHit` is reported.
- Load each stock's real leverage. The default is 5×, and 5× is not available on every stock.
