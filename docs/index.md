# Algo Trading

**A monthly momentum rotation for NSE stocks, written in Rust.** Each month it ranks the 500 most-traded stocks, holds the 30 with the strongest and smoothest gains in equal amounts, and replaces the ones that fall too far down the ranking.

!!! warning "Where this stands"
    The strategy passed a 2010–2026 backtest (+26% a year after costs, before tax, against +13% for the market) and is being **paper traded** from 20 November 2026. It places no real orders. Past results don't promise future returns, and this is not investment advice.

<div class="grid cards" markdown>

-   :material-chart-line: **[The strategy](strategies/rotation.md)**

    Rules, tested results, exits, risks, and how to run it each evening.

-   :material-flask: **[Research](research.md)**

    Everything tested so far, including what failed and why.

-   :material-database-check: **[Data quality](data-quality.md)**

    What the data is, and what was cleaned out.

</div>

## At a glance

| Item | Detail |
|---|---|
| **Market** | NSE cash equity, delivery (held for weeks to months) |
| **Data** | Daily candles for 2,601 NSE stocks from 2008, from Dhan, in QuestDB |
| **Strategy** | Momentum + low volatility, top 30, rebalanced on the first trading day of each month |
| **Exit** | Sold at a rebalance when it ranks below 60; no stop-loss |
| **Running** | `scripts/rotation_daily.sh` each evening: update data, advance the paper account, print the report |

## New here?

1. Set up your machine with [Local setup](runbook/local-setup.md), and [Dhan login](runbook/dhan-token.md).
2. Read [the strategy](strategies/rotation.md).
3. Start a paper account and run the evening job:

```bash
cargo run --release -p tools --bin rotation-paper -- init --capital 1000000
scripts/rotation_daily.sh
```

Then see [what's left to build](roadmap.md).
