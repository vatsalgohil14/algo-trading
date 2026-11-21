# Adding and testing a strategy

Everything else (indicators, risk, orders, simulated broker, reports) is already there. A new strategy is one file, one line and a config.

## 1. Write the strategy

Copy [`crates/strategies/src/template.rs`](https://github.com/vatsalgohil14/algo-trading/blob/main/crates/strategies/src/template.rs) to a new file. It's a small working example of the shape; the idea itself isn't worth trading. You change four things:

1. **Settings:** a struct with defaults.
2. **Per-stock state:** what to remember during the day, kept in a `Daily<T>` that starts fresh every morning.
3. **`new`:** check settings and ask for indicators by name, such as `"ema(20)"`, `"rsi(14)"` or `"orb(15).high"`.
4. **`on_bar`:** the rules.

The shared kit in `strategies::kit` handles everything that isn't your idea:

| Use | Instead of |
|---|---|
| `days.today(bar)` | Resetting per-stock state on a new day by hand |
| `entry_allowed(bar, ctx)` | Checking feed health, tradability, the 14:45 entry cut-off and that you're flat. Always call it before an entry |
| `signals.enter_long(bar, ctx, stop, target, "my_setup")`, `enter_short`, `exit`, `move_stop` | Building a `Signal` by hand. It acts from the next minute, rounds stops away from the entry and targets to the nearest tick, and returns `None` rather than an invalid signal |

```rust
let Some(today) = self.days.today(bar) else { return };
if today.traded || entry_allowed(bar, ctx).is_err() {
    return;
}
if let Some(s) = self.signals.enter_long(bar, ctx, stop, Some(target), "my_setup") {
    today.traded = true;
    out.push(s);
}
```

Don't size or place orders. Work out what's going on from `ctx.position`, not from signals you sent, because orders can be rejected or not fill.

Give each setup its own `reason`, such as `"breakout_long"`. Reports group by it, and sizing scores attach to it.

Strategies that compare stocks can also use `on_minute_end`. See ORB's ranking.

## 2. Register it

Add `pub mod my_strategy;` in `crates/strategies/src/lib.rs`, and one line in `Registry::builtin` in `crates/strategies/src/registry.rs`:

```rust
r.add("my_strategy", MyStrategy::new);
```

The settings type is read from the config's `[strategy.params]`; fields left out take their defaults.

Need a new indicator? Implement `Indicator` and add one line in the indicators' `Registry::builtin` (see `crates/indicators/README.md`).

## 3. Test it bar by bar

`strategies::testing::Bench` builds your strategy as a config would and feeds it bars:

```rust
use strategies::testing::{Bench, day};

let mut b = Bench::from_registry("my_strategy", json!({"sma_period": 3}), 1).unwrap();
b.flat_day(0, day(2025, 1, 6), 100.0);                 // warm-up day
let out = b.bar(0, day(2025, 1, 7), 1, [98.0, 103.0, 98.0, 103.0], 1000);
assert_eq!(out.len(), 1);                              // minute 1 = 09:16
b.fill(0, 10, 103.0);                                  // as if it filled
b.feed_ok = false;                                     // or a broken feed
```

Every signal is validated as it comes out, so malformed ones fail the test. The template's tests show the pattern.

## 4. Write a config

```toml
# config/backtests/my_strategy.toml
[data]
source = "questdb"
from = "2022-01-01"
to = "2026-10-31"
universe_file = "data/universe.csv"

[[strategy]]
name = "my_strategy"
# stocks = ["RELIANCE", "INFY"]   # every stock if left out

[strategy.params]
sma_period = 30

# Optional: override defaults
# [risk]
# max_exposure = 50000.0
# [risk.setup_scores]
# breakout_long = 0.25            # only after out-of-sample results earn it
```

Several `[[strategy]]` blocks run together. They share capital, risk limits and the one-position-per-stock rule.

## 5. Get data (once)

```bash
scripts/questdb.sh start
```

```bash
scripts/questdb.sh init
```

Set `DHAN_CLIENT_ID`, `DHAN_PIN` and `DHAN_TOTP_SECRET` in your shell, never in a tracked file. Tokens are then issued and saved automatically; see the [token runbook](../runbook/dhan-token.md). Then:

```bash
cargo run --release -p tools --bin dhan-history -- --symbols-file config/universes/nifty50.txt --from 2021-12-01 --to 2026-11-18
```

## 6. Run it

```bash
cargo run --release -p backtester -- --config config/backtests/my_strategy.toml
```

The report covers P&L after costs, the pass mark, results by setup, exit reason and month, which filters blocked trades, and health counters. `data/backtests/latest/` gets `trades.csv` and `daily.csv`. Try it now without any data:

```bash
cargo run --release -p backtester -- --config config/backtests/template_synthetic.toml
```

Then test with doubled slippage:

```bash
cargo run --release -p backtester -- --config config/backtests/my_strategy.toml --slippage-mult 2
```

## 7. Before believing a result

- Synthetic data says nothing about profit.
- One good run proves little. Follow the validation list in [orb.md](orb.md#validation): neighbouring settings, out-of-sample periods, different market regimes, doubled slippage, at least 300 trades.
- `nifty50.txt` and `liquid_starter.txt` are today's large stocks, so results over past years are flattered by survivorship bias.
- The simulated broker works from 1-minute bars. It's deliberately pessimistic, but real fills need checking in paper trading.

## Not built yet

The live and paper trader needs the Dhan adapter, which connects the engine's commands to real orders and the live feed. The read-only side works: the [live monitor](../runbook/live-monitor.md) streams live candles and indicators. Orders come next.
