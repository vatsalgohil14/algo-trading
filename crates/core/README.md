# trading-core

The shared vocabulary of the system. Every other crate speaks in these types,
so one strategy runs unchanged in backtest, paper and live trading.

| Module | What it holds |
|---|---|
| `money` | `Price` and `Money` as integer paise: exact, fast, reproducible. Tick rounding. |
| `ids` | `InstrumentId` / `StrategyId` (dense indexes into `Vec`s), `ClientOrderId` (our order id, sent to the broker as correlation id / tag). |
| `time` | `Timestamp` (UTC microseconds), IST helpers, `MarketHours` (09:15-15:30, last entry 14:45, square-off 15:10). |
| `instrument` | `Instrument` and the `Universe` that numbers them. |
| `market` | `Bar`, `Tick`, `FeedStatus`, `MarketEvent`. |
| `signal` | `Signal`: what a strategy wants (entry, stop, target). Strategies never size or place orders. |
| `order` | `OrderRequest` with validation, `ExecutionReport`, `Fill`, `OrderStatus`. |
| `position` | `Position` with exact average-cost P&L. |
| `clock`, `feed`, `execution` | The three traits that differ between backtest, paper and live: `Clock`, `DataFeed`, `Execution`. |

Safety rules encoded here:

- Every bar, order and signal has a `validate`; inputs from outside are checked
  before the engine acts on them.
- `ExecError::OutcomeUnknown` marks requests whose result is unknown. Only
  `definitely_not_placed()` errors may be retried; anything else is looked up
  by `ClientOrderId` first, so an order is never duplicated.
- `ClientOrderId::compose` includes a per-process boot value, so ids stay
  unique across restarts on the same day.
- Arithmetic saturates instead of wrapping; nothing panics.
