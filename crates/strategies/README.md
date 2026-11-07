# strategies

Strategies turn bars and indicator values into `Signal`s: enter long or short with a stop and target, move the stop, or exit. They never size or place orders; the risk manager and order manager do that. The same code runs in backtest, paper and live trading.

```rust
let mut builder = BankBuilder::new(Registry::builtin());
let orb = Orb::new(StrategyId(0), OrbConfig::default(), &mut builder, universe.len())?;
let mut bank = builder.build(universe.len())?;

let mut set = StrategySet::new(universe.len());
set.add(Box::new(orb), &stocks_for_orb)?;      // which stocks each strategy trades

// per bar:
let session = bank.update(&bar)?;
set.on_bar(&bar, |strategy| Context { /* position of `strategy`, feed health, ... */ }, &mut signals);
```

| Strategy | Rules |
|---|---|
| `orb` | [docs/strategies/orb.md](../../docs/strategies/orb.md) |

## Writing a strategy

1. Implement `Strategy` (`id`, `name`, `on_bar`).
   - Keep per-stock state in a `Vec` indexed by `InstrumentId`.
2. Ask for indicators by name in the constructor (`BankBuilder::require`). Read them in `on_bar` through `ctx.bank`.
3. Work out the strategy's state from `ctx.position`, never from the signals it sent. Orders can be rejected, fill late or not at all.
4. Don't open trades when `ctx.feed_ok` or `ctx.tradable` is false.
5. Make settings a `serde` struct with defaults and a `validate()`. A bad config must stop start-up.

Tests: `src/orb/tests.rs` checks each rule bar by bar. `tests/orb_synthetic.rs` runs six months × 40 synthetic stocks and checks these hold on every bar:
- signals are valid;
- entries only fall in the entry window, at most once per stock per day;
- everything is flat at day end;
- runs are deterministic.
