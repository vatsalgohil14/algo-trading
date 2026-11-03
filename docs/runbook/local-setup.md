# Local development setup (macOS, Apple Silicon)

Everything lives in your home folder; nothing needs admin rights.

| Component | Version | Location |
|---|---|---|
| Rust toolchain | pinned in `rust-toolchain.toml` | `~/.cargo`, `~/.rustup` |
| Java runtime (for QuestDB) | Eclipse Temurin 25 JRE | `~/.local/opt/temurin-25-jre` |
| QuestDB | 10.1.0 | `~/.local/opt/questdb` → `questdb-10.1.0` |
| QuestDB data | — | `~/.questdb` |
| cargo-audit, cargo-deny | latest | `~/.cargo/bin` |

## Daily use

```bash
scripts/questdb.sh start     # start the database (idempotent)
scripts/questdb.sh status
scripts/questdb.sh stop
scripts/questdb.sh logs      # follow the server log
scripts/questdb.sh init      # create/upgrade tables (safe to run any time)
scripts/questdb.sh sql "SELECT count() FROM bars_1m"
```

- Web console: <http://127.0.0.1:9000>
- **QWP (what our code uses)**: `ws://127.0.0.1:9000`, for both ingestion and
  queries. See `crates/store/README.md`.
- Legacy, kept for tools: Postgres wire `127.0.0.1:8812` (user `admin`,
  password `quest`), ILP over HTTP `127.0.0.1:9000/write` or TCP `127.0.0.1:9009`.

QuestDB listens on localhost only (`deploy/questdb/server.conf`), so the default
password is not exposed to the network. Telemetry is off. Ingestion may not
create tables or columns (`line.auto.create.new.tables/columns=false`; applies
to QWP too): the schema is owned by `schema.sql`, and a misspelled name is
rejected instead of silently creating junk.

After changing `deploy/questdb/server.conf`, copy it into place and restart
(the start script only installs it when none exists yet):

```bash
cp deploy/questdb/server.conf ~/.questdb/conf/server.conf && scripts/questdb.sh restart
```

QuestDB does not start automatically after a reboot; run `scripts/questdb.sh start`.

## Schema

`deploy/questdb/schema.sql` defines all tables. Rules:

- Statements must be idempotent and end with `;`.
- Never edit a table that already holds data; append an `ALTER TABLE` instead.
- Market data uses `DOUBLE`; account money (orders, fills, P&L) uses `DECIMAL`.
- `mode` (backtest / paper / live) lets all three share tables.
- DEDUP keys make re-loading the same data safe.

| Table | Purpose | Partition | Retention |
|---|---|---|---|
| `instruments` | daily snapshot of tradable instruments | month | forever |
| `bars_1m` | 1-minute candles (history + live) | day | forever |
| `bars_daily` | daily candles | year | forever |
| `ticks` | raw live ticks for replay/debugging | day | 60 days |
| `runs` | each backtest/paper/live run and its parameters | year | forever |
| `signals` | every signal, incl. risk rejections and reasons | day | forever |
| `orders` | order event log (one row per status change) | day | forever |
| `fills` | executions, expected vs actual price, charges | day | forever |
| `positions` | position snapshots and reconciliation | day | forever |
| `pnl_daily` | daily P&L per strategy, net of costs | year | forever |
| `system_events` | connects, halts, kill switch, errors | day | 180 days |

## Reinstalling from scratch

The install was done by hand (no Homebrew):

1. Download the Temurin 25 JRE for `mac/aarch64` from Adoptium and the
   `questdb-<version>-no-jre-bin.tar.gz` asset from QuestDB's GitHub releases.
2. Verify both SHA-256 checksums against the published values.
3. Extract to `~/.local/opt/temurin-25-jre` and `~/.local/opt/questdb-<version>`,
   then point the `~/.local/opt/questdb` symlink at the new version.
4. `scripts/questdb.sh start && scripts/questdb.sh init`.

To upgrade QuestDB later: stop it, extract the new version next to the old
one, move the symlink, start again. Data in `~/.questdb` is kept. Check the
release notes for the Java version it needs.

## Resetting the database

Stops the server and deletes **all** local data:

```bash
scripts/questdb.sh stop && rm -rf ~/.questdb && scripts/questdb.sh start && scripts/questdb.sh init
```
