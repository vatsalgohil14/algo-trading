//! Background QWP writer.
//!
//! The QWP sender is synchronous, so it lives on its own OS thread. Callers
//! push [`Record`]s into a bounded queue; the thread batches them into one
//! QWP buffer and flushes it every `flush_interval` or `max_batch_rows`,
//! whichever comes first.
//!
//! Flushing hands the batch to the client's store-and-forward layer, which
//! sends it over the WebSocket, retries across reconnects and (in durable
//! mode) keeps it on disk until QuestDB acknowledges it.
//!
//! **Poisoned batches.** If QuestDB rejects a batch (e.g. a column that
//! doesn't exist), the whole batch is refused and the sender stops accepting
//! data; in durable mode the rejected batch would also block everything
//! queued on disk behind it. The writer detects this, moves the on-disk
//! queue to `<sf_dir>/quarantine/` for inspection, and continues with a
//! fresh sender. [`Store::start`] checks the schema first so this should
//! only ever happen if the database is changed while running.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use questdb::ingress::{AckLevel, Buffer, Sender};

use crate::query::QueryClient;
use crate::{Durability, Record, Result, StoreConfig, StoreError, schema};

enum Msg {
    Row(Box<Record>),
    Flush(SyncSender<Result<()>>),
    Shutdown,
}

#[derive(Debug, Default)]
struct Counters {
    queued: AtomicU64,
    dropped_queue_full: AtomicU64,
    rejected_invalid: AtomicU64,
    flushed_rows: AtomicU64,
    flushes: AtomicU64,
    flush_errors: AtomicU64,
    server_errors: AtomicU64,
    sender_resets: AtomicU64,
}

/// Snapshot of writer counters, for monitoring. Anything other than
/// `queued`, `flushed_rows` and `flushes` being non-zero deserves an alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StoreStats {
    /// Rows accepted into the queue.
    pub queued: u64,
    /// Rows dropped by `try_write` because the queue was full.
    pub dropped_queue_full: u64,
    /// Rows that could not be encoded locally (skipped individually).
    pub rejected_invalid: u64,
    /// Rows handed to the QWP sender.
    pub flushed_rows: u64,
    pub flushes: u64,
    /// Batches that could not be handed to the sender (dropped).
    pub flush_errors: u64,
    /// Errors reported by QuestDB (e.g. schema mismatch).
    pub server_errors: u64,
    /// Times the sender was rebuilt after QuestDB rejected a batch.
    pub sender_resets: u64,
}

/// Handle to the background writer. [`try_write`](Self::try_write) never
/// blocks, so it is safe on the trading path.
#[derive(Debug)]
pub struct Store {
    tx: SyncSender<Msg>,
    counters: Arc<Counters>,
    thread: Option<JoinHandle<()>>,
    ack_timeout: Duration,
}

impl Store {
    /// Checks the database schema (unless `verify_schema` is off), then
    /// starts the writer thread.
    ///
    /// With `verify_schema` off the writer also starts while QuestDB is
    /// down; rows are buffered (in memory or in `sf_dir`) until it is up.
    pub fn start(config: StoreConfig) -> Result<Self> {
        config.validate()?;
        if config.verify_schema {
            let mut q = QueryClient::connect(&config)?;
            schema::verify(&mut q)?;
        }
        let conf = config.sender_conf()?;
        let sender = Sender::from_conf(&conf)?;
        let sf_sender_dir = match (config.durability, &config.sf_dir) {
            (Durability::Periodic, Some(dir)) => Some(dir.join(&config.sender_id)),
            _ => None,
        };
        let (tx, rx) = mpsc::sync_channel(config.queue_capacity);
        let counters = Arc::new(Counters::default());
        let worker = Worker {
            sender: Some(sender),
            conf,
            sf_sender_dir,
            rx,
            counters: Arc::clone(&counters),
            flush_interval: config.flush_interval,
            max_batch_rows: config.max_batch_rows,
            ack_timeout: config.ack_timeout,
        };
        let thread = std::thread::Builder::new()
            .name("store-writer".into())
            .spawn(move || worker.run())
            .map_err(|e| StoreError::Config(format!("cannot spawn writer thread: {e}")))?;
        tracing::info!(addr = %config.addr, durability = ?config.durability, "store writer started");
        Ok(Self {
            tx,
            counters,
            thread: Some(thread),
            ack_timeout: config.ack_timeout,
        })
    }

    /// Queues a row without ever blocking. If the queue is full the row is
    /// dropped, counted, and `QueueFull` returned. Use on the trading path.
    pub fn try_write(&self, record: impl Into<Record>) -> Result<()> {
        match self.tx.try_send(Msg::Row(Box::new(record.into()))) {
            Ok(()) => {
                self.counters.queued.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(TrySendError::Full(_)) => {
                let n = self
                    .counters
                    .dropped_queue_full
                    .fetch_add(1, Ordering::Relaxed);
                if (n + 1).is_power_of_two() {
                    tracing::warn!(dropped_total = n + 1, "store queue full; dropping rows");
                }
                Err(StoreError::QueueFull)
            }
            Err(TrySendError::Disconnected(_)) => Err(StoreError::Closed),
        }
    }

    /// Queues a row, waiting for space if the queue is full. Use for
    /// backfills and tools, where backpressure is what you want.
    pub fn write(&self, record: impl Into<Record>) -> Result<()> {
        self.tx
            .send(Msg::Row(Box::new(record.into())))
            .map_err(|_| StoreError::Closed)?;
        self.counters.queued.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Sends everything queued so far and waits until QuestDB has accepted
    /// it. Blocking; don't call on the trading hot path.
    pub fn flush(&self) -> Result<()> {
        let (ack_tx, ack_rx) = mpsc::sync_channel(1);
        self.tx
            .send(Msg::Flush(ack_tx))
            .map_err(|_| StoreError::Closed)?;
        // The worker bounds its own wait by `ack_timeout`; allow a margin
        // for rows still queued ahead of the flush request.
        ack_rx
            .recv_timeout(self.ack_timeout + Duration::from_secs(5))
            .map_err(|_| StoreError::Closed)?
    }

    pub fn stats(&self) -> StoreStats {
        let c = &self.counters;
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);
        StoreStats {
            queued: get(&c.queued),
            dropped_queue_full: get(&c.dropped_queue_full),
            rejected_invalid: get(&c.rejected_invalid),
            flushed_rows: get(&c.flushed_rows),
            flushes: get(&c.flushes),
            flush_errors: get(&c.flush_errors),
            server_errors: get(&c.server_errors),
            sender_resets: get(&c.sender_resets),
        }
    }

    /// Flushes, waits for acknowledgement (up to `ack_timeout`) and stops the
    /// writer thread. In durable mode anything not yet acknowledged stays in
    /// `sf_dir` and is sent by the next writer with the same `sender_id`.
    pub fn shutdown(mut self) -> Result<()> {
        self.stop()
    }

    fn stop(&mut self) -> Result<()> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        let _ = self.tx.send(Msg::Shutdown);
        thread
            .join()
            .map_err(|_| StoreError::Config("store writer thread panicked".into()))
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        if let Err(e) = self.stop() {
            tracing::error!(error = %e, "store writer did not stop cleanly");
        }
    }
}

struct Worker {
    /// `None` only briefly while being rebuilt, or if rebuilding failed.
    sender: Option<Sender>,
    conf: String,
    /// This writer's store-and-forward folder (durable mode only).
    sf_sender_dir: Option<PathBuf>,
    rx: Receiver<Msg>,
    counters: Arc<Counters>,
    flush_interval: Duration,
    max_batch_rows: usize,
    ack_timeout: Duration,
}

impl Worker {
    fn run(mut self) {
        let mut buffer = Buffer::new_qwp_ws();
        let mut last_flush = Instant::now();
        loop {
            let wait = self.flush_interval.saturating_sub(last_flush.elapsed());
            match self.rx.recv_timeout(wait) {
                Ok(Msg::Row(record)) => {
                    self.append(&mut buffer, &record);
                    // Take whatever else is already queued before flushing.
                    while buffer.row_count() < self.max_batch_rows {
                        match self.rx.try_recv() {
                            Ok(Msg::Row(r)) => self.append(&mut buffer, &r),
                            Ok(Msg::Flush(ack)) => {
                                let _ = ack.send(self.flush_and_wait(&mut buffer));
                                last_flush = Instant::now();
                            }
                            Ok(Msg::Shutdown) | Err(TryRecvError::Disconnected) => {
                                self.finish(&mut buffer);
                                return;
                            }
                            Err(TryRecvError::Empty) => break,
                        }
                    }
                    if buffer.row_count() >= self.max_batch_rows {
                        self.flush(&mut buffer);
                        last_flush = Instant::now();
                    }
                }
                Ok(Msg::Flush(ack)) => {
                    let _ = ack.send(self.flush_and_wait(&mut buffer));
                    last_flush = Instant::now();
                }
                Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                    self.finish(&mut buffer);
                    return;
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
            if last_flush.elapsed() >= self.flush_interval {
                if buffer.row_count() > 0 {
                    self.flush(&mut buffer);
                }
                self.check_health();
                last_flush = Instant::now();
            }
        }
    }

    /// Appends one row; a row that fails to encode is rolled back so it
    /// cannot corrupt the rest of the batch.
    fn append(&self, buffer: &mut Buffer, record: &Record) {
        let res = buffer.set_marker().and_then(|()| record.append_to(buffer));
        if let Err(e) = res {
            let _ = buffer.rewind_to_marker();
            self.counters
                .rejected_invalid
                .fetch_add(1, Ordering::Relaxed);
            tracing::error!(table = record.table(), error = %e, "skipping row that could not be encoded");
        }
    }

    fn flush(&mut self, buffer: &mut Buffer) {
        let rows = buffer.row_count() as u64;
        if self.sender.is_none() {
            self.rebuild_sender();
        }
        let result = match self.sender.as_mut() {
            Some(sender) => sender.flush(buffer),
            None => {
                self.counters.flush_errors.fetch_add(1, Ordering::Relaxed);
                tracing::error!(rows, "no QWP sender available; batch dropped");
                buffer.clear();
                return;
            }
        };
        match result {
            Ok(()) => {
                self.counters.flushes.fetch_add(1, Ordering::Relaxed);
                self.counters
                    .flushed_rows
                    .fetch_add(rows, Ordering::Relaxed);
            }
            Err(e) => {
                self.counters.flush_errors.fetch_add(1, Ordering::Relaxed);
                tracing::error!(rows, error = %e, "QWP flush failed; batch dropped");
                // Never let an unsendable batch grow without bound.
                buffer.clear();
                self.check_health();
            }
        }
    }

    fn flush_and_wait(&mut self, buffer: &mut Buffer) -> Result<()> {
        if buffer.row_count() > 0 {
            self.flush(buffer);
        }
        let res = match self.sender.as_mut() {
            Some(sender) => sender
                .wait(AckLevel::Ok, self.ack_timeout)
                .map_err(StoreError::from),
            None => Err(StoreError::Closed),
        };
        self.check_health();
        res
    }

    /// Logs errors QuestDB reported and replaces a sender that has stopped
    /// accepting data.
    fn check_health(&mut self) {
        let Some(sender) = self.sender.as_mut() else {
            self.rebuild_sender();
            return;
        };
        loop {
            match sender.poll_qwp_ws_error() {
                Ok(Some(err)) => {
                    self.counters.server_errors.fetch_add(1, Ordering::Relaxed);
                    tracing::error!(error = ?err, "QuestDB rejected data");
                }
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!(error = %e, "could not poll QWP errors");
                    break;
                }
            }
        }
        if sender.must_close() {
            self.counters.sender_resets.fetch_add(1, Ordering::Relaxed);
            tracing::error!(
                "QWP sender stopped after a rejected batch; quarantining its queue and reconnecting"
            );
            // Drop the dead sender first: it holds a lock on its sf folder.
            self.sender = None;
            self.quarantine_sf_dir();
            self.rebuild_sender();
        }
    }

    /// Moves this writer's store-and-forward folder aside so the rejected
    /// batch is not replayed forever. The data is kept for inspection.
    fn quarantine_sf_dir(&self) {
        let Some(dir) = &self.sf_sender_dir else {
            return;
        };
        if !dir.exists() {
            return;
        }
        let (Some(root), Some(name)) = (dir.parent(), dir.file_name()) else {
            return;
        };
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or_default();
        let target = root
            .join("quarantine")
            .join(format!("{}-{stamp}", name.to_string_lossy()));
        let moved = std::fs::create_dir_all(root.join("quarantine"))
            .and_then(|()| std::fs::rename(dir, &target));
        match moved {
            Ok(()) => {
                tracing::error!(path = %target.display(), "store-and-forward queue quarantined")
            }
            Err(e) => {
                tracing::error!(error = %e, path = %dir.display(), "could not quarantine store-and-forward queue")
            }
        }
    }

    fn rebuild_sender(&mut self) {
        match Sender::from_conf(&self.conf) {
            Ok(s) => {
                self.sender = Some(s);
                tracing::info!("QWP sender rebuilt");
            }
            Err(e) => tracing::error!(error = %e, "could not rebuild QWP sender; will retry"),
        }
    }

    fn finish(&mut self, buffer: &mut Buffer) {
        if let Err(e) = self.flush_and_wait(buffer) {
            tracing::warn!(error = %e, "not all rows were acknowledged before shutdown");
        }
        if let Some(mut sender) = self.sender.take()
            && let Err(e) = sender.close_drain()
        {
            tracing::warn!(error = %e, "QWP sender did not close cleanly");
        }
        tracing::info!("store writer stopped");
    }
}
