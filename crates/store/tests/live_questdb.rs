//! End-to-end tests against a running QuestDB with the schema applied.
//!
//!     scripts/questdb.sh start && scripts/questdb.sh init
//!     cargo test -p store -- --ignored --test-threads=1
//!
//! Test rows use security IDs above 990_000_000 and timestamps on
//! 1990-01-02, a date no real market data has. Each test removes the
//! 1990-01-02 partition of the tables it touched, so real data is never
//! modified.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use store::query::{BarTable, QueryClient};
use store::rows::{
    Bar, FillRow, InstrumentRow, Level, Mode, OrderEvent, PnlDailyRow, PositionRow, Record, RunRow,
    SignalRow, SystemEvent, Tick,
};
use store::{Durability, Store, StoreConfig};

const TEST_DAY: &str = "1990-01-02";

fn addr() -> String {
    std::env::var("QDB_ADDR").unwrap_or_else(|_| "127.0.0.1:9000".into())
}

fn config(sender_id: &str) -> StoreConfig {
    StoreConfig {
        addr: addr(),
        sender_id: sender_id.into(),
        flush_interval: Duration::from_millis(20),
        ack_timeout: Duration::from_secs(10),
        ..StoreConfig::default()
    }
}

fn day_start() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(1990, 1, 2, 3, 45, 0).unwrap()
}

fn bar(security_id: i32, minute: i64, close: f64) -> Bar {
    Bar {
        ts: day_start() + chrono::TimeDelta::minutes(minute),
        segment: "NSE_EQ".into(),
        security_id,
        symbol: "ZZTEST".into(),
        open: close - 1.0,
        high: close + 2.0,
        low: close - 2.0,
        close,
        volume: 1000 + minute,
        oi: None,
        source: "test".into(),
    }
}

fn cleanup(q: &mut QueryClient, tables: &[&str]) {
    for t in tables {
        // Fails harmlessly when the partition doesn't exist.
        let _ = q.execute(&format!("ALTER TABLE {t} DROP PARTITION LIST '{TEST_DAY}'"));
    }
}

/// WAL tables apply asynchronously; poll until the count settles.
fn wait_for_count(q: &mut QueryClient, sql: &str, expected: i64) -> i64 {
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last = -1;
    while Instant::now() < deadline {
        last = q.scalar_i64(sql).unwrap();
        if last == expected {
            // Give any extra (duplicate) rows a moment to show up too.
            std::thread::sleep(Duration::from_millis(300));
            return q.scalar_i64(sql).unwrap();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    last
}

#[test]
#[ignore = "needs a running QuestDB with the schema applied"]
fn bars_round_trip_and_dedup() {
    let cfg = config("test_bars");
    let mut q = QueryClient::connect(&cfg).unwrap();
    cleanup(&mut q, &["bars_1m"]);

    let sid = 990_000_001;
    let bars: Vec<Bar> = (0..375)
        .map(|m| bar(sid, m, 1000.0 + m as f64 * 0.05))
        .collect();

    let store = Store::start(cfg).unwrap();
    for b in &bars {
        store.write(Record::Bar1m(b.clone())).unwrap();
    }
    store.flush().unwrap();
    // Same rows again: DEDUP keys must keep the table at 375 rows.
    for b in &bars {
        store.write(Record::Bar1m(b.clone())).unwrap();
    }
    store.flush().unwrap();
    let stats = store.stats();
    store.shutdown().unwrap();

    assert_eq!(stats.flushed_rows, 750);
    assert_eq!(stats.rejected_invalid, 0);
    assert_eq!(stats.server_errors, 0);

    let count_sql = format!("SELECT count() FROM bars_1m WHERE security_id = {sid}");
    assert_eq!(wait_for_count(&mut q, &count_sql, 375), 375);

    let from = day_start();
    let to = from + chrono::TimeDelta::days(1);
    let back = q.bars(BarTable::OneMinute, sid, from, to).unwrap();
    assert_eq!(back.len(), 375);
    assert_eq!(back, bars, "bars read back must equal bars written");

    cleanup(&mut q, &["bars_1m"]);
}

#[test]
#[ignore = "needs a running QuestDB with the schema applied"]
fn decimals_are_exact() {
    let cfg = config("test_fills");
    let mut q = QueryClient::connect(&cfg).unwrap();
    cleanup(&mut q, &["fills"]);

    let store = Store::start(cfg).unwrap();
    store
        .write(FillRow {
            ts: day_start(),
            run_id: "test".into(),
            mode: Mode::Backtest,
            strategy: "test".into(),
            correlation_id: "zz-1".into(),
            order_id: "1".into(),
            trade_id: "zz-trade-1".into(),
            segment: "NSE_EQ".into(),
            security_id: 990_000_002,
            symbol: "ZZTEST".into(),
            side: "BUY".into(),
            qty: 10,
            price: Decimal::new(142_805, 2),             // 1428.05
            expected_price: Some(Decimal::new(1428, 0)), // 1428 → 1428.00
            charges: Some(Decimal::new(17_995, 3)),      // 17.995 → 18.00
        })
        .unwrap();
    store.shutdown().unwrap();

    let exact = "SELECT count() FROM fills WHERE trade_id = 'zz-trade-1' \
                 AND price = 1428.05m AND expected_price = 1428.00m AND charges = 18.00m";
    assert_eq!(wait_for_count(&mut q, exact, 1), 1);

    cleanup(&mut q, &["fills"]);
}

/// Store-and-forward: rows written while QuestDB is unreachable are kept on
/// disk and delivered by the next writer with the same sender_id.
#[test]
#[ignore = "needs a running QuestDB with the schema applied"]
fn rows_survive_an_outage_on_disk() {
    let sf_dir = std::env::temp_dir().join(format!("store-sf-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&sf_dir);
    let sid = 990_000_003;

    let mut q = QueryClient::connect(&config("unused")).unwrap();
    cleanup(&mut q, &["bars_1m"]);

    // 1. QuestDB "down": point at a port where nothing listens.
    let offline = StoreConfig {
        addr: "127.0.0.1:9".into(),
        sf_dir: Some(sf_dir.clone()),
        durability: Durability::Periodic,
        ack_timeout: Duration::from_secs(2),
        // QuestDB is unreachable here, so the schema can't be checked.
        verify_schema: false,
        ..config("test_sf")
    };
    let store = Store::start(offline).expect("writer must start even when QuestDB is down");
    for m in 0..50 {
        store.try_write(Record::Bar1m(bar(sid, m, 500.0))).unwrap();
    }
    store.shutdown().unwrap();
    let on_disk: u64 = std::fs::read_dir(&sf_dir)
        .unwrap()
        .flatten()
        .map(|e| dir_size(&e.path()))
        .sum();
    assert!(on_disk > 0, "unsent rows must be kept in sf_dir");

    // 2. QuestDB "back": same sf_dir and sender_id replays the backlog.
    let online = StoreConfig {
        sf_dir: Some(sf_dir.clone()),
        durability: Durability::Periodic,
        ..config("test_sf")
    };
    let store = Store::start(online).unwrap();
    store.flush().unwrap();
    store.shutdown().unwrap();

    let count_sql = format!("SELECT count() FROM bars_1m WHERE security_id = {sid}");
    assert_eq!(wait_for_count(&mut q, &count_sql, 50), 50);

    cleanup(&mut q, &["bars_1m"]);
    let _ = std::fs::remove_dir_all(&sf_dir);
}

fn dir_size(p: &std::path::Path) -> u64 {
    if p.is_dir() {
        std::fs::read_dir(p)
            .map(|rd| rd.flatten().map(|e| dir_size(&e.path())).sum())
            .unwrap_or(0)
    } else {
        std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
    }
}

const ALL_TABLES: &[&str] = &[
    "instruments",
    "bars_1m",
    "bars_daily",
    "ticks",
    "runs",
    "signals",
    "orders",
    "fills",
    "positions",
    "pnl_daily",
    "system_events",
];

/// One row of every type, so any drift between the Rust rows and the real
/// tables shows up as a server error here instead of in production.
#[test]
#[ignore = "needs a running QuestDB with the schema applied"]
fn every_record_type_is_accepted() {
    let cfg = config("test_all");
    let mut q = QueryClient::connect(&cfg).unwrap();
    cleanup(&mut q, ALL_TABLES);

    let ts = day_start();
    let t = |s: &str| -> store::rows::Text { s.into() };
    let d = |v: i64| Decimal::new(v, 2);
    let sid = 990_000_004;
    let records: Vec<Record> = vec![
        InstrumentRow {
            ts,
            segment: t("NSE_EQ"),
            security_id: sid,
            symbol: t("ZZTEST"),
            series: Some(t("EQ")),
            isin: Some(t("INE000000000")),
            name: Some(t("Test Ltd")),
            tick_size: 0.05,
            lot_size: 1,
            upper_circuit: Some(110.0),
            lower_circuit: Some(90.0),
            asm_gsm: false,
            mtf_leverage: Some(4.5),
        }
        .into(),
        Record::Bar1m(bar(sid, 0, 100.0)),
        Record::BarDaily(bar(sid, 0, 100.0)),
        Tick {
            ts,
            segment: t("NSE_EQ"),
            security_id: sid,
            symbol: t("ZZTEST"),
            ltp: 100.05,
            ltq: Some(5),
            ltt: Some(ts),
            volume: Some(1000),
            atp: Some(100.01),
            bid: Some(100.0),
            ask: Some(100.1),
            bid_qty: Some(10),
            ask_qty: Some(20),
            total_buy_qty: Some(500),
            total_sell_qty: None,
        }
        .into(),
        RunRow {
            ts,
            run_id: t("zz-run"),
            mode: Mode::Backtest,
            strategy: t("orb"),
            broker: t("sim"),
            git_sha: None,
            params: Some(t(r#"{"or_minutes":15}"#)),
            notes: None,
        }
        .into(),
        SignalRow {
            ts,
            run_id: t("zz-run"),
            mode: Mode::Backtest,
            strategy: t("orb"),
            segment: t("NSE_EQ"),
            security_id: sid,
            symbol: t("ZZTEST"),
            side: t("BUY"),
            entry: Some(d(10_005)),
            stop: Some(d(9_900)),
            target: Some(d(10_300)),
            qty: 10,
            decision: t("ACCEPTED"),
            reason: None,
        }
        .into(),
        OrderEvent {
            ts,
            run_id: t("zz-run"),
            mode: Mode::Backtest,
            strategy: t("orb"),
            correlation_id: t("zz-c1"),
            order_id: Some(t("1")),
            segment: t("NSE_EQ"),
            security_id: sid,
            symbol: t("ZZTEST"),
            side: t("BUY"),
            product: t("INTRADAY"),
            order_type: t("LIMIT"),
            qty: 10,
            price: Some(d(10_005)),
            trigger_price: None,
            status: t("TRADED"),
            filled_qty: 10,
            avg_price: Some(d(10_005)),
            reason: None,
        }
        .into(),
        FillRow {
            ts,
            run_id: t("zz-run"),
            mode: Mode::Backtest,
            strategy: t("orb"),
            correlation_id: t("zz-c1"),
            order_id: t("1"),
            trade_id: t("zz-t1"),
            segment: t("NSE_EQ"),
            security_id: sid,
            symbol: t("ZZTEST"),
            side: t("BUY"),
            qty: 10,
            price: d(10_005),
            expected_price: Some(d(10_000)),
            charges: Some(d(1_850)),
        }
        .into(),
        PositionRow {
            ts,
            run_id: t("zz-run"),
            mode: Mode::Backtest,
            strategy: t("orb"),
            segment: t("NSE_EQ"),
            security_id: sid,
            symbol: t("ZZTEST"),
            net_qty: 10,
            avg_price: d(10_005),
            ltp: Some(100.2),
            realized_pnl: d(0),
            unrealized_pnl: d(150),
        }
        .into(),
        PnlDailyRow {
            ts,
            run_id: t("zz-run"),
            mode: Mode::Backtest,
            strategy: t("orb"),
            trades: 1,
            wins: 1,
            losses: 0,
            gross_pnl: d(15_000),
            charges: d(1_850),
            net_pnl: d(13_150),
            max_drawdown: d(0),
            turnover: d(200_100),
        }
        .into(),
        SystemEvent {
            ts,
            run_id: t("zz-run"),
            mode: Mode::Backtest,
            level: Level::Info,
            component: t("test"),
            event: t("started"),
            message: t("hello"),
        }
        .into(),
    ];
    assert_eq!(records.len(), ALL_TABLES.len());

    let store = Store::start(cfg).unwrap();
    for r in records {
        store.write(r).unwrap();
    }
    store.flush().unwrap();
    let stats = store.stats();
    store.shutdown().unwrap();
    assert_eq!(stats.server_errors, 0);
    assert_eq!(stats.sender_resets, 0);
    assert_eq!(stats.rejected_invalid, 0);

    for table in ALL_TABLES {
        let sql = format!("SELECT count() FROM {table} WHERE ts = '1990-01-02T03:45:00.000000Z'");
        assert_eq!(wait_for_count(&mut q, &sql, 1), 1, "row missing in {table}");
    }
    cleanup(&mut q, ALL_TABLES);
}

/// Renames `system_events.message` for the duration of a test and always
/// renames it back, even if the test fails.
struct RenamedColumn<'a> {
    q: &'a mut QueryClient,
}

impl<'a> RenamedColumn<'a> {
    fn new(q: &'a mut QueryClient) -> Self {
        q.execute("ALTER TABLE system_events RENAME COLUMN message TO zz_message_tmp")
            .unwrap();
        Self { q }
    }
}

impl Drop for RenamedColumn<'_> {
    fn drop(&mut self) {
        self.q
            .execute("ALTER TABLE system_events RENAME COLUMN zz_message_tmp TO message")
            .expect("restore system_events.message");
    }
}

/// If the schema changes under a running writer, QuestDB rejects the batch
/// and the sender dies. The writer must quarantine the poisoned on-disk
/// queue and keep going with a fresh sender.
#[test]
#[ignore = "needs a running QuestDB with the schema applied"]
fn recovers_from_a_rejected_batch() {
    let sf_dir = std::env::temp_dir().join(format!("store-poison-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&sf_dir);
    let cfg = StoreConfig {
        sf_dir: Some(sf_dir.clone()),
        durability: Durability::Periodic,
        ack_timeout: Duration::from_secs(5),
        ..config("test_poison")
    };
    let mut q = QueryClient::connect(&cfg).unwrap();
    cleanup(&mut q, &["system_events"]);
    let event = |msg: &str| SystemEvent {
        ts: day_start(),
        run_id: "zz-poison".into(),
        mode: Mode::Backtest,
        level: Level::Warn,
        component: "test".into(),
        event: msg.into(),
        message: msg.into(),
    };

    let store = Store::start(cfg).unwrap();
    {
        let _renamed = RenamedColumn::new(&mut q);
        store.write(event("rejected")).unwrap();
        assert!(store.flush().is_err(), "the batch must be rejected");
    }
    // Schema restored: a fresh sender must take over.
    store.write(event("accepted")).unwrap();
    store.flush().unwrap();
    let stats = store.stats();
    store.shutdown().unwrap();

    assert!(
        stats.sender_resets >= 1,
        "sender must have been rebuilt: {stats:?}"
    );
    let quarantine = sf_dir.join("quarantine");
    assert!(
        std::fs::read_dir(&quarantine)
            .map(|d| d.count())
            .unwrap_or(0)
            >= 1,
        "rejected queue must be kept in {}",
        quarantine.display()
    );
    let ok = "SELECT count() FROM system_events WHERE run_id = 'zz-poison' AND event = 'accepted'";
    assert_eq!(wait_for_count(&mut q, ok, 1), 1);
    let bad = "SELECT count() FROM system_events WHERE run_id = 'zz-poison' AND event = 'rejected'";
    assert_eq!(q.scalar_i64(bad).unwrap(), 0);

    cleanup(&mut q, &["system_events"]);
    let _ = std::fs::remove_dir_all(&sf_dir);
}
