use std::path::PathBuf;
use std::time::Duration;

use crate::{Result, StoreError};

/// How hard store-and-forward tries to keep unsent rows across failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Durability {
    /// Rows buffered in memory only: lost if this process dies before they
    /// reach QuestDB. Fine for tests and backfills that can be re-run.
    Memory,
    /// Rows written to `sf_dir` and synced to disk periodically: survive a
    /// crash of this process and are replayed on restart.
    Periodic,
}

#[derive(Debug, Clone)]
pub struct StoreConfig {
    /// QuestDB HTTP/QWP address, `host:port`.
    pub addr: String,
    /// Store-and-forward directory. Required for [`Durability::Periodic`].
    pub sf_dir: Option<PathBuf>,
    /// Identifies this writer's store-and-forward files; must be unique per
    /// running process that shares `sf_dir`. `[A-Za-z0-9_-]` only.
    pub sender_id: String,
    pub durability: Durability,
    /// Send buffered rows at least this often.
    pub flush_interval: Duration,
    /// ... or as soon as this many rows are buffered.
    pub max_batch_rows: usize,
    /// Rows waiting for the writer thread. When full, `try_write` drops
    /// rows (and counts them) rather than blocking trading.
    pub queue_capacity: usize,
    /// How long `flush()` and shutdown wait for QuestDB to acknowledge.
    pub ack_timeout: Duration,
    /// Check on start that every table/column this crate writes exists with
    /// the right type, and refuse to start otherwise. Turning this off lets
    /// the writer start while QuestDB is down.
    pub verify_schema: bool,
    /// Extra raw `key=value;` pairs appended to the QWP connect string.
    pub extra_conf: String,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:9000".into(),
            sf_dir: None,
            sender_id: "store".into(),
            durability: Durability::Memory,
            flush_interval: Duration::from_millis(100),
            max_batch_rows: 10_000,
            queue_capacity: 100_000,
            ack_timeout: Duration::from_secs(30),
            verify_schema: true,
            extra_conf: String::new(),
        }
    }
}

impl StoreConfig {
    /// Durable configuration for live/paper trading: unsent rows are kept on
    /// disk in `sf_dir` until QuestDB has them.
    pub fn durable(
        addr: impl Into<String>,
        sf_dir: impl Into<PathBuf>,
        sender_id: impl Into<String>,
    ) -> Self {
        Self {
            addr: addr.into(),
            sf_dir: Some(sf_dir.into()),
            sender_id: sender_id.into(),
            durability: Durability::Periodic,
            ..Self::default()
        }
    }

    /// Reads `QDB_ADDR` (default `127.0.0.1:9000`), `QDB_SF_DIR` (enables
    /// durable mode when set) and `QDB_SENDER_ID`.
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let mut cfg = Self::default();
        if let Some(addr) = var("QDB_ADDR") {
            cfg.addr = addr;
        }
        if let Some(id) = var("QDB_SENDER_ID") {
            cfg.sender_id = id;
        }
        if let Some(dir) = var("QDB_SF_DIR") {
            cfg.sf_dir = Some(dir.into());
            cfg.durability = Durability::Periodic;
        }
        cfg
    }

    pub(crate) fn validate(&self) -> Result<()> {
        let bad = |m: String| Err(StoreError::Config(m));
        if self.addr.trim().is_empty() || self.addr.contains(';') {
            return bad(format!("invalid addr {:?}", self.addr));
        }
        if self.sender_id.is_empty()
            || !self
                .sender_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return bad(format!(
                "sender_id {:?} must be [A-Za-z0-9_-]+",
                self.sender_id
            ));
        }
        if self.durability == Durability::Periodic && self.sf_dir.is_none() {
            return bad("Durability::Periodic needs sf_dir".into());
        }
        if self.max_batch_rows == 0 || self.queue_capacity == 0 {
            return bad("max_batch_rows and queue_capacity must be > 0".into());
        }
        if self.flush_interval.is_zero() {
            return bad("flush_interval must be > 0".into());
        }
        if !self.extra_conf.is_empty() && !self.extra_conf.ends_with(';') {
            return bad("extra_conf must be `key=value;` pairs ending in ';'".into());
        }
        Ok(())
    }

    /// The QWP sender connect string.
    pub(crate) fn sender_conf(&self) -> Result<String> {
        let mut conf = format!(
            // `async`: start even if QuestDB is down; rows queue up in
            // store-and-forward and are sent once it is reachable.
            "ws::addr={};sender_id={};initial_connect_retry=async;",
            self.addr, self.sender_id
        );
        match (self.durability, &self.sf_dir) {
            (Durability::Periodic, Some(dir)) => {
                std::fs::create_dir_all(dir).map_err(|e| {
                    StoreError::Config(format!("cannot create sf_dir {}: {e}", dir.display()))
                })?;
                let dir = dir
                    .to_str()
                    .ok_or_else(|| StoreError::Config("sf_dir must be UTF-8".into()))?;
                if dir.contains(';') {
                    return Err(StoreError::Config("sf_dir must not contain ';'".into()));
                }
                conf.push_str(&format!("sf_dir={dir};sf_durability=periodic;"));
            }
            (Durability::Memory, _) => conf.push_str("sf_durability=memory;"),
            (Durability::Periodic, None) => {
                return Err(StoreError::Config(
                    "Durability::Periodic needs sf_dir".into(),
                ));
            }
        }
        conf.push_str(&self.extra_conf);
        Ok(conf)
    }

    /// The QWP reader connect string.
    pub(crate) fn reader_conf(&self) -> String {
        format!("ws::addr={};", self.addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_conf() {
        let c = StoreConfig::default();
        c.validate().unwrap();
        assert_eq!(
            c.sender_conf().unwrap(),
            "ws::addr=127.0.0.1:9000;sender_id=store;initial_connect_retry=async;sf_durability=memory;"
        );
    }

    #[test]
    fn rejects_bad_configs() {
        let bad_id = StoreConfig {
            sender_id: "bad id".into(),
            ..StoreConfig::default()
        };
        assert!(bad_id.validate().is_err());

        let durable_without_dir = StoreConfig {
            durability: Durability::Periodic,
            ..StoreConfig::default()
        };
        assert!(durable_without_dir.validate().is_err());

        let injected = StoreConfig {
            addr: "x;evil=1".into(),
            ..StoreConfig::default()
        };
        assert!(injected.validate().is_err());
    }
}
