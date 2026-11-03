-- QuestDB schema for the trading system. Apply with: scripts/questdb.sh init
--
-- Rules for this file:
--   * Every statement must be idempotent (IF NOT EXISTS) and end with ';'.
--   * Never edit a released table definition; add an ALTER TABLE below it.
--   * Market data uses DOUBLE (it arrives as floats and feeds analytics).
--     Money on the account side (orders, fills, P&L, charges) uses DECIMAL.
--   * All timestamps are UTC. `mode` is backtest | paper | live, so all three
--     share tables and can be compared directly.
--   * DEDUP keys make re-loading the same data safe (no duplicate rows).

-- ---------------------------------------------------------------------------
-- Reference data
-- ---------------------------------------------------------------------------

-- Daily snapshot of the instruments we trade (from Dhan's instrument master).
CREATE TABLE IF NOT EXISTS instruments (
  ts TIMESTAMP,
  segment SYMBOL,
  security_id INT,
  symbol SYMBOL CAPACITY 8192,
  series SYMBOL,
  isin VARCHAR,
  name VARCHAR,
  tick_size DOUBLE,
  lot_size INT,
  upper_circuit DOUBLE,
  lower_circuit DOUBLE,
  asm_gsm BOOLEAN,
  mtf_leverage DOUBLE
) TIMESTAMP(ts) PARTITION BY MONTH WAL
DEDUP UPSERT KEYS(ts, segment, security_id);

-- ---------------------------------------------------------------------------
-- Market data
-- ---------------------------------------------------------------------------

-- 1-minute candles: historical downloads and bars built from live ticks.
CREATE TABLE IF NOT EXISTS bars_1m (
  ts TIMESTAMP,
  segment SYMBOL,
  security_id INT,
  symbol SYMBOL CAPACITY 8192,
  open DOUBLE,
  high DOUBLE,
  low DOUBLE,
  close DOUBLE,
  volume LONG,
  oi LONG,
  source SYMBOL
) TIMESTAMP(ts) PARTITION BY DAY WAL
DEDUP UPSERT KEYS(ts, segment, security_id);

CREATE TABLE IF NOT EXISTS bars_daily (
  ts TIMESTAMP,
  segment SYMBOL,
  security_id INT,
  symbol SYMBOL CAPACITY 8192,
  open DOUBLE,
  high DOUBLE,
  low DOUBLE,
  close DOUBLE,
  volume LONG,
  oi LONG,
  source SYMBOL
) TIMESTAMP(ts) PARTITION BY YEAR WAL
DEDUP UPSERT KEYS(ts, segment, security_id);

-- Raw live ticks, kept for replay and debugging. Old partitions expire.
CREATE TABLE IF NOT EXISTS ticks (
  ts TIMESTAMP,
  segment SYMBOL,
  security_id INT,
  symbol SYMBOL CAPACITY 8192,
  ltp DOUBLE,
  ltq INT,
  ltt TIMESTAMP,
  volume LONG,
  atp DOUBLE,
  bid DOUBLE,
  ask DOUBLE,
  bid_qty INT,
  ask_qty INT,
  total_buy_qty LONG,
  total_sell_qty LONG
) TIMESTAMP(ts) PARTITION BY DAY TTL 60 DAYS WAL;

-- ---------------------------------------------------------------------------
-- Trading activity (audit trail)
-- ---------------------------------------------------------------------------

-- One row per backtest / paper / live run, with the exact parameters used.
CREATE TABLE IF NOT EXISTS runs (
  ts TIMESTAMP,
  run_id SYMBOL CAPACITY 4096,
  mode SYMBOL,
  strategy SYMBOL,
  broker SYMBOL,
  git_sha VARCHAR,
  params VARCHAR,
  notes VARCHAR
) TIMESTAMP(ts) PARTITION BY YEAR WAL
DEDUP UPSERT KEYS(ts, run_id);

-- Every signal a strategy emits, including the ones risk rejected and why.
CREATE TABLE IF NOT EXISTS signals (
  ts TIMESTAMP,
  run_id SYMBOL CAPACITY 4096,
  mode SYMBOL,
  strategy SYMBOL,
  segment SYMBOL,
  security_id INT,
  symbol SYMBOL CAPACITY 8192,
  side SYMBOL,
  entry DECIMAL(12, 2),
  stop DECIMAL(12, 2),
  target DECIMAL(12, 2),
  qty INT,
  decision SYMBOL,
  reason VARCHAR
) TIMESTAMP(ts) PARTITION BY DAY WAL;

-- Order event log: one row per status change (placed, open, part-filled,
-- filled, rejected, cancelled). The latest row per correlation_id is the
-- current state.
CREATE TABLE IF NOT EXISTS orders (
  ts TIMESTAMP,
  run_id SYMBOL CAPACITY 4096,
  mode SYMBOL,
  strategy SYMBOL,
  correlation_id VARCHAR,
  order_id VARCHAR,
  segment SYMBOL,
  security_id INT,
  symbol SYMBOL CAPACITY 8192,
  side SYMBOL,
  product SYMBOL,
  order_type SYMBOL,
  qty INT,
  price DECIMAL(12, 2),
  trigger_price DECIMAL(12, 2),
  status SYMBOL,
  filled_qty INT,
  avg_price DECIMAL(12, 2),
  reason VARCHAR
) TIMESTAMP(ts) PARTITION BY DAY WAL
DEDUP UPSERT KEYS(ts, correlation_id, status);

-- Executions with the actual price paid and charges, to measure slippage.
CREATE TABLE IF NOT EXISTS fills (
  ts TIMESTAMP,
  run_id SYMBOL CAPACITY 4096,
  mode SYMBOL,
  strategy SYMBOL,
  correlation_id VARCHAR,
  order_id VARCHAR,
  trade_id VARCHAR,
  segment SYMBOL,
  security_id INT,
  symbol SYMBOL CAPACITY 8192,
  side SYMBOL,
  qty INT,
  price DECIMAL(12, 2),
  expected_price DECIMAL(12, 2),
  charges DECIMAL(12, 2)
) TIMESTAMP(ts) PARTITION BY DAY WAL
DEDUP UPSERT KEYS(ts, trade_id);

-- Periodic position snapshots (and the end-of-day reconciliation result).
CREATE TABLE IF NOT EXISTS positions (
  ts TIMESTAMP,
  run_id SYMBOL CAPACITY 4096,
  mode SYMBOL,
  strategy SYMBOL,
  segment SYMBOL,
  security_id INT,
  symbol SYMBOL CAPACITY 8192,
  net_qty INT,
  avg_price DECIMAL(12, 2),
  ltp DOUBLE,
  realized_pnl DECIMAL(14, 2),
  unrealized_pnl DECIMAL(14, 2)
) TIMESTAMP(ts) PARTITION BY DAY WAL;

-- Daily results per strategy, net of all costs.
CREATE TABLE IF NOT EXISTS pnl_daily (
  ts TIMESTAMP,
  run_id SYMBOL CAPACITY 4096,
  mode SYMBOL,
  strategy SYMBOL,
  trades INT,
  wins INT,
  losses INT,
  gross_pnl DECIMAL(14, 2),
  charges DECIMAL(14, 2),
  net_pnl DECIMAL(14, 2),
  max_drawdown DECIMAL(14, 2),
  turnover DECIMAL(16, 2)
) TIMESTAMP(ts) PARTITION BY YEAR WAL
DEDUP UPSERT KEYS(ts, run_id, strategy);

-- Operational events: connects/disconnects, risk halts, kill switch, errors.
CREATE TABLE IF NOT EXISTS system_events (
  ts TIMESTAMP,
  run_id SYMBOL CAPACITY 4096,
  mode SYMBOL,
  level SYMBOL,
  component SYMBOL,
  event SYMBOL,
  message VARCHAR
) TIMESTAMP(ts) PARTITION BY DAY TTL 180 DAYS WAL;
