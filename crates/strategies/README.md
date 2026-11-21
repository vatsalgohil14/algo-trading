# strategies

Strategies turn bars and indicator values into `Signal`s: enter long or short with a stop and target, move the stop, or exit. They never size or place orders; the risk manager and order manager do that. The same code runs in backtest, paper and live trading.

```rust
let mut builder = BankBuilder::new(Registry::builtin());
let template = Template::new(StrategyId(0), TemplateConfig::default(), &mut builder, universe.len())?;
let mut bank = builder.build(universe.len())?;

let mut set = StrategySet::new(universe.len());
set.add(Box::new(template), &stocks)?;          // which stocks each strategy trades

// per bar:
let session = bank.update(&bar)?;
set.on_bar(&bar, |strategy| Context { /* position of `strategy`, feed health, ... */ }, &mut signals);

// once per minute, after every stock's bar for that minute:
set.on_minute_end(close, &mut signals);
```

| Strategy | Rules | Status |
|---|---|---|
| `template` | `src/template.rs` | Example to copy and the engine's test strategy; not for trading |

No trading strategy yet. Opening Range Breakout was built first, failed its real backtest and was deleted; it is in git history before November 2026. Ideas are screened first with the research tools ([docs/research.md](../../docs/research.md)).

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

Tests: `src/template.rs` checks its rules bar by bar with the bench. `crates/backtest/tests/end_to_end.rs` runs the template, long and short, through the whole system on synthetic data and checks the books balance, everything is flat by day end and runs are deterministic.
