# indicators

Incremental technical indicators, asked for by name and computed once per stock.

```rust
let mut builder = BankBuilder::new(Registry::builtin());
let ema = builder.require("ema(20)")?;          // same spec twice = same instance
let orb_high = builder.require("orb(15).high")?;
let mut bank = builder.build(universe.len())?;

bank.update(&bar)?;                             // every bar, O(1) per indicator
if let Some(v) = bank.get(bar.instrument, ema) { /* ... */ }
```

| Spec | Outputs | Notes |
|---|---|---|
| `sma(n)` | value | Exact running sum in paise |
| `ema(n)` | value | Seeded with the SMA of the first `n` closes |
| `rsi(n)` | value | Wilder smoothing, 0-100 |
| `atr(n)` | value | Wilder; includes the overnight gap |
| `vwap` | value | Session VWAP of typical price, resets daily |
| `orb(n)` | high, low, range | First `n` minutes; unavailable all day if any bar of the window is missing |
| `prev_day` | high, low, close | Previous session |

## Adding an indicator

1. Implement `Indicator` (`outputs`, `update`, `value`) in the folder that fits:
   `trend`, `momentum`, `volatility`, `volume`, `levels`.
2. Register it in `Registry::builtin` (one line).

Strategies and config files can then use it by name. Projects can also
register their own with `Registry::register` without touching this crate.

## Guarantees

- The bank rejects bars for unknown stocks and bars that are not later than the
  previous bar of that stock (duplicates, out of order), leaving state untouched.
- Values are `None` until the indicator has enough data; strategies must not
  trade on `None`.
- `tests/reference.rs` checks every indicator on every bar of synthetic data
  against a plain recomputation from the full history.

Speed: `cargo run --release -p indicators --example throughput` (about 20 ns
per bar for 8 indicators on an Apple Silicon laptop).
