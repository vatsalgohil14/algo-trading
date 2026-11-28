//! End-to-end tests against a running QuestDB with the schema applied.
//!
//!     scripts/questdb.sh start && scripts/questdb.sh init
//!     cargo test -p store -- --ignored --test-threads=1
//!
//! Test rows use security IDs above 990_000_000 and timestamps on
//! 1990-01-02, a date no real market data has. Each test removes the 1990
//! partitions it wrote (`bars_1m` by day, `bars_daily` by year), so real
//! data is never modified.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use store::query::QueryClient;
use store::rows::{Bar, Record};
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

/// Drops the test partitions. Fails harmlessly when they don't exist.
fn cleanup(q: &mut QueryClient) {
    let _ = q.execute(&format!(
        "ALTER TABLE bars_1m DROP PARTITION LIST '{TEST_DAY}'"
    ));
    let _ = q.execute("ALTER TABLE bars_daily DROP PARTITION LIST '1990'");
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
    cleanup(&mut q);

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

    let volume: i64 = bars.iter().map(|b| b.volume).sum();
    let sum_sql = format!("SELECT sum(volume) FROM bars_1m WHERE security_id = {sid}");
    assert_eq!(q.scalar_i64(&sum_sql).unwrap(), volume);

    cleanup(&mut q);
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
    cleanup(&mut q);

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

    cleanup(&mut q);
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

/// A daily bar goes to `bars_daily`, so drift between the Rust row and
/// that table shows up as a server error here.
#[test]
#[ignore = "needs a running QuestDB with the schema applied"]
fn daily_bars_are_accepted() {
    let cfg = config("test_daily");
    let mut q = QueryClient::connect(&cfg).unwrap();
    cleanup(&mut q);
    let sid = 990_000_004;
    let store = Store::start(cfg).unwrap();
    store.write(Record::BarDaily(bar(sid, 0, 100.0))).unwrap();
    store.flush().unwrap();
    let stats = store.stats();
    store.shutdown().unwrap();
    assert_eq!(stats.server_errors, 0);
    assert_eq!(stats.rejected_invalid, 0);
    let sql = format!("SELECT count() FROM bars_daily WHERE security_id = {sid}");
    assert_eq!(wait_for_count(&mut q, &sql, 1), 1);
    cleanup(&mut q);
}

/// Renames `bars_1m.source` for the duration of a test and always renames
/// it back, even if the test fails.
struct RenamedColumn<'a> {
    q: &'a mut QueryClient,
}

impl<'a> RenamedColumn<'a> {
    fn new(q: &'a mut QueryClient) -> Self {
        q.execute("ALTER TABLE bars_1m RENAME COLUMN source TO zz_source_tmp")
            .unwrap();
        Self { q }
    }
}

impl Drop for RenamedColumn<'_> {
    fn drop(&mut self) {
        self.q
            .execute("ALTER TABLE bars_1m RENAME COLUMN zz_source_tmp TO source")
            .expect("restore bars_1m.source");
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
    cleanup(&mut q);
    let (rejected, accepted) = (990_000_005, 990_000_006);

    let store = Store::start(cfg).unwrap();
    {
        let _renamed = RenamedColumn::new(&mut q);
        store.write(Record::Bar1m(bar(rejected, 0, 10.0))).unwrap();
        assert!(store.flush().is_err(), "the batch must be rejected");
    }
    // Schema restored: a fresh sender must take over.
    store.write(Record::Bar1m(bar(accepted, 0, 10.0))).unwrap();
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
    let ok = format!("SELECT count() FROM bars_1m WHERE security_id = {accepted}");
    assert_eq!(wait_for_count(&mut q, &ok, 1), 1);
    let bad = format!("SELECT count() FROM bars_1m WHERE security_id = {rejected}");
    assert_eq!(q.scalar_i64(&bad).unwrap(), 0);

    cleanup(&mut q);
    let _ = std::fs::remove_dir_all(&sf_dir);
}
