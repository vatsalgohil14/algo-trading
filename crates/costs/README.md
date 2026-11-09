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

These are the NSE intraday rates shown on [dhan.co/pricing](https://dhan.co/pricing/) when read in November 2026. The page gives no effective date. **Read it again before trading real money**, because rates change with budgets and exchange circulars. Results haven't yet been compared with a real contract note.

| Charge | Rate | Side |
|---|---|---|
| Brokerage | 0.03% of order value, max ₹20 | both |
| STT | 0.025% | sell |
| Exchange transaction | 0.0030699% | both |
| SEBI fee | ₹10 per crore | both |
| Stamp duty | 0.003% | buy |
| GST | 18% of brokerage + exchange + SEBI | both |

- Amounts are exact integer paise.
- STT and stamp duty are rounded to the nearest rupee, as Dhan's page says it does. Rounding per order is our assumption.
- Dhan's IPFT charge (0.0000001%) is left out: it's under a paisa per order at our sizes.

**Worked example:** 100 shares bought at ₹500 and sold at ₹505 costs ₹54.33 in charges, about 11% of the ₹500 gross profit. Small moves on small orders are expensive.

## Slippage

- Default: 0.02% of the price per fill, at least 1 tick, rounded up to whole ticks.
- Calibrate it in paper trading: compare actual fill prices with signal prices.

Not modelled:
- DP charges (they apply only to delivery).
- Auto square-off fees, which we avoid by squaring off at 15:10.
- Interest on leverage.
