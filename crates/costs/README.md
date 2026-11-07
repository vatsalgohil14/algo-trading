# costs

What a trade really costs on NSE cash intraday (MIS): charges plus slippage.
Every backtest and every risk check uses this one model.

```rust
let model = CostModel::new(&CostConfig::default())?;
let trade = model.round_trip(Side::Buy, 100, entry_fill, exit_fill);   // gross, charges, net
let cost = model.expected_round_trip_cost(price, qty, tick);           // before taking a trade
let fill = model.slippage().apply(Side::Buy, price, tick);             // backtest fills
```

## Default charges (per executed order)

These are the published rates for Dhan and Zerodha as of 2025. **Check them against the broker's charges page before trading real money.** Rates change with budgets and exchange circulars.

| Charge | Rate | Side |
|---|---|---|
| Brokerage | 0.03% of order value, max ₹20 | both |
| STT | 0.025% | sell |
| Exchange transaction | 0.00297% | both |
| SEBI fee | ₹10 per crore | both |
| Stamp duty | 0.003% | buy |
| GST | 18% of brokerage + exchange + SEBI | both |

- Amounts are exact integer paise.
- STT and stamp duty are rounded to whole rupees, as contract notes do.

**Worked example:** 100 shares bought at ₹500 and sold at ₹505 costs ₹54.23 in charges, about 11% of the ₹500 gross profit. Small moves on small orders are expensive.

## Slippage

- Default: 0.02% of the price per fill, at least 1 tick, rounded up to whole ticks.
- Calibrate it in paper trading: compare actual fill prices with signal prices.

Not modelled:
- DP charges (they apply only to delivery).
- Auto square-off fees, which we avoid by squaring off at 15:10.
- Interest on leverage.
