# Adding and testing a strategy

Everything else (indicators, risk, orders, simulated broker, reports) is already there. A new strategy is one file, one line and a config.

## 1. Write the strategy

Copy [`crates/strategies/src/template.rs`](https://github.com/vatsalgohil14/algo-trading/blob/main/crates/strategies/src/template.rs) to a new file. It's a small working example of the shape; the idea itself isn't worth trading. You change four things:

1. **Settings:** a struct with defaults.
2. **Per-stock state:** what to remember between bars.
3. **`new`:** check settings and ask for indicators by name, such as `"ema(20)"`, `"rsi(14)"` or `"orb(15).high"`.
4. **`on_bar`:** the rules. Push a `Signal`:
   - `EnterLong` or `EnterShort`, with a stop and a target (both required).
   - `AdjustStop`, which can only tighten.
   - `Exit`.

Don't size or place orders. Work out what's going on from `ctx.position`, not from signals you sent, because orders can be rejected or not fill.

Give each setup its own `reason`, such as `"breakout_long"`. Reports group by it, and sizing scores attach to it.

Strategies that compare stocks can also use `on_minute_end`. See ORB's ranking.

## 2. Register it

Add `pub mod my_strategy;` in `crates/strategies/src/lib.rs`, and one match arm in `crates/strategies/src/registry.rs`.

Need a new indicator? Implement `Indicator` and add one line in `Registry::builtin` (see `crates/indicators/README.md`).

## 3. Write a config

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

## 4. Get data (once)

```bash
scripts/questdb.sh start
```

```bash
scripts/questdb.sh init
```

Set `DHAN_CLIENT_ID`, `DHAN_PIN` and `DHAN_TOTP_SECRET` in your shell, never in a tracked file. Tokens are then issued and saved automatically; see the [token runbook](../runbook/dhan-token.md). Then:

```bash
cargo run --release -p tools --bin dhan-history -- --symbols-file config/universes/liquid_starter.txt --from 2022-01-01 --to 2026-10-31
```

## 5. Run it

```bash
cargo run --release -p backtester -- --config config/backtests/my_strategy.toml
```

The report covers P&L after costs, the pass mark, results by setup, exit reason and month, which filters blocked trades, and health counters. `data/backtests/latest/` gets `trades.csv` and `daily.csv`. Try it now without any data:

```bash
cargo run --release -p backtester -- --config config/backtests/orb_synthetic.toml
```

Then test with doubled slippage:

```bash
cargo run --release -p backtester -- --config config/backtests/my_strategy.toml --slippage-mult 2
```

## 6. Before believing a result

- Synthetic data says nothing about profit.
- One good run proves little. Follow the validation list in [orb.md](orb.md#validation): neighbouring settings, out-of-sample periods, different market regimes, doubled slippage, at least 300 trades.
- `liquid_starter.txt` is chosen from today's large stocks, so results over past years are flattered by survivorship bias.
- The simulated broker works from 1-minute bars. It's deliberately pessimistic, but real fills need checking in paper trading.

## Not built yet

The live and paper trader needs the Dhan adapter, which connects the engine's commands to real orders and the live feed. That comes after the API has been tested with read-only calls.
