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

// once per minute, after every stock's bar for that minute:
set.on_minute_end(close, &mut signals);
```

| Strategy | Rules | Status |
|---|---|---|
| `orb` | [docs/strategies/orb.md](../../docs/strategies/orb.md) | Retired: failed its first real backtest. Kept as the engine's test example |
| `template` | `src/template.rs` | Example to copy; not for trading |

## Writing a strategy

Start from `src/template.rs`; the full guide is [docs/strategies/how-to-add.md](../../docs/strategies/how-to-add.md).

1. Implement `Strategy` (`id`, `name`, `on_bar`).
2. Ask for indicators by name in the constructor (`BankBuilder::require`). Read them in `on_bar` through `ctx.bank`.
3. Use the kit (`kit`):
   - `Daily<T>` for per-stock state that resets each morning;
   - `entry_allowed(bar, ctx)` before every entry: feed healthy, stock tradable, before the entry cut-off, flat;
   - `Signals` to build entries, exits and stop moves on the tick grid, acted on from the next minute.
4. Work out the strategy's state from `ctx.position`, never from the signals it sent. Orders can be rejected, fill late or not at all.
5. Make settings a `serde` struct with defaults, and reject bad values in `new`. A bad config must stop start-up.
6. Register it with one line in `Registry::builtin`: `r.add("name", MyStrategy::new)`.
7. Test it with `testing::Bench`, which builds it as a config would, feeds bars and validates every signal.

Tests: `src/orb/tests.rs` checks each rule bar by bar. `tests/orb_synthetic.rs` runs six months × 40 synthetic stocks and checks these hold on every bar:
- signals are valid;
- entries only fall in the entry window, at most once per stock per day;
- everything is flat at day end;
- runs are deterministic.
