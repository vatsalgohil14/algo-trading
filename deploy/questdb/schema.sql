-- QuestDB schema. Apply with: scripts/questdb.sh init
--
-- Rules for this file:
--   * Every statement must be idempotent (IF NOT EXISTS) and end with ';'.
--   * Never edit a released table definition; add an ALTER TABLE below it.
--   * Market data uses DOUBLE. All timestamps are UTC; a daily candle is
--     stamped at midnight IST (18:30 UTC the day before).
--   * DEDUP keys make re-loading the same data safe (no duplicate rows).
--   * crates/store/src/schema.rs lists the same columns; a unit test keeps
--     the two in sync.

-- 1-minute candles (Nifty 50, Dec 2021 on): kept from the intraday research;
-- nothing writes here now.
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

-- Daily candles for every NSE stock (source dhan_daily) and a few ETFs
-- (dhan_daily_fund), written by dhan-daily. One row per day and stock.
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
