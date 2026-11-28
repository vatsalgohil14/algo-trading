# store

Writes candles into QuestDB, using **QWP** (QuestDB Wire
Protocol, new in QuestDB 10): binary, columnar, compressed, over WebSocket on
the normal HTTP port 9000. It replaces ILP for ingestion and the Postgres wire
protocol for queries. Client: `questdb-rs` 7.2.

## Writing

```rust
use store::{Record, Store, StoreConfig};
use store::rows::Bar;

// Durable: unsent rows are kept on disk until QuestDB has them.
let store = Store::start(StoreConfig::durable("127.0.0.1:9000", "data/sf", "loader"))?;

// Waits for queue space; `try_write` never blocks and drops (and counts)
// the row if the queue is full.
store.write(Record::BarDaily(Bar { /* ... */ }))?;

store.flush()?;      // optional: wait until QuestDB has everything so far
store.shutdown()?;   // flush, wait for acks, stop
```

- **`write`** waits for queue space (downloads); **`try_write`** never blocks.
- A background thread batches rows and sends them every 100 ms or 10,000 rows.
- **Store-and-forward**: with `StoreConfig::durable`, rows are written to
  `sf_dir` first, so they survive a QuestDB outage *and* a crash of this
  process, and are replayed on restart. `StoreConfig::default()` keeps them in
  memory only (fine for tests and re-runnable backfills).
- **`stats()`** exposes counters; anything other than `queued`, `flushed_rows`
  and `flushes` being non-zero should raise an alert.

## Rows

`store::rows::Bar` is one candle; `Record::BarDaily` writes it to `bars_daily` and `Record::Bar1m` to `bars_1m`, the two tables in `deploy/questdb/schema.sql`. `dhan-daily` is the only writer.

## Schema safety

QuestDB is configured (`deploy/questdb/server.conf`) to **reject** writes to
unknown tables or columns instead of silently creating them. Two consequences
were verified against QuestDB 10.1 (and the live tests pass on 10.2) and are handled here:

1. A rejected row fails its **whole batch**, and the sender stops accepting
   data. In durable mode the rejected batch would also block everything queued
   behind it on disk, forever.
2. So `Store::start` first checks every table and column it writes against the
   live database and refuses to start on any mismatch (`verify_schema`, on by
   default). A unit test keeps the Rust column list identical to `schema.sql`.
3. If the schema still changes under a running writer, the writer moves the
   poisoned on-disk queue to `<sf_dir>/quarantine/` (kept for inspection),
   rebuilds the sender and carries on (`stats().sender_resets`).

## Tests

```bash
cargo test -p store                                        # unit tests, no server
scripts/questdb.sh start && scripts/questdb.sh init
cargo test -p store -- --ignored --test-threads=1          # live tests
```

The live tests cover: round trip and dedup of 375 bars, a daily bar accepted by `bars_daily`, store-and-forward across an outage, and recovery from a rejected batch. They write only to 1990 (security IDs above 990,000,000) and drop those partitions afterwards, so real data is never touched.
