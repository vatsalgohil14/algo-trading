# Algo Trading

**An intraday trading system for NSE stocks, written in Rust.** It looks for setups minute by minute, decides how much to risk, places orders with the stop already at the broker, and closes everything before the market does.

!!! warning "Where this stands"
    The system is built and tested on **simulated data only**. It has not yet run on real market data or placed a real order, so nothing here shows that it makes money. This is an engineering project, not investment advice.

<div class="grid cards" markdown>

-   :material-transit-connection-variant: **[Life of a trade](how-it-works.md)**

    From a one-minute candle to a closed position, in six steps.

-   :material-shield-check: **[When things go wrong](safety.md)**

    Timeouts, duplicate fills, a dead feed, a restart. What happens in each.

-   :material-scale-balance: **[Risk and costs](risk-and-costs.md)**

    How big each trade is, and what it really costs.

-   :material-flask: **[Backtesting](backtesting.md)**

    How a strategy is tested, and what it must pass.

</div>

## At a glance

| Item | Detail |
|---|---|
| **Market** | NSE cash equity, intraday only |
| **Broker** | Dhan (Kite later) |
| **Capital** | ₹5 lakh, at up to 5× intraday leverage |
| **Trade size** | ₹30,000 to ₹50,000 of exposure |
| **Risk limits** | ₹500 per trade, ₹10,000 per day |
| **Data** | 1-minute bars in QuestDB |
| **First strategy** | [Opening Range Breakout](strategies/orb.md) |

## Four ideas behind the design

**One code path.** A backtest and live trading run the same engine. Only the source of bars and the destination of orders change.

**The broker is the truth.** Strategies are told their real position on every bar. They never assume an order worked.

**Protection first.** The stop goes to the broker with the entry, so it exists even if this program dies.

**Exact money.** Prices are whole paise, never floats, so results are exact and repeatable.

## New here?

1. Read [Life of a trade](how-it-works.md).
2. Set up your machine with [Local setup](runbook/local-setup.md).
3. Run a backtest on synthetic data:

```bash
cargo run --release -p backtester -- --config config/backtests/orb_synthetic.toml
```

Then see [what's left to build](roadmap.md).
