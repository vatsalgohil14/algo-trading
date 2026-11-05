//! Where "now" comes from. Trading logic must never read the system clock
//! directly, or backtests would depend on when they run.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use crate::{CoreError, Timestamp};

pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

/// Wall-clock time, for paper and live trading.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_datetime(&chrono::Utc::now())
    }
}

/// Time set by whoever drives a replay, usually to each event's timestamp.
/// Clones share the same time. It only moves forward.
#[derive(Debug, Clone, Default)]
pub struct SimClock(Arc<AtomicI64>);

impl SimClock {
    pub fn starting_at(ts: Timestamp) -> Self {
        Self(Arc::new(AtomicI64::new(ts.micros())))
    }

    /// Moves the clock to `ts`. Setting the current time again is fine;
    /// going backwards is an error, since it means events arrived out of
    /// order.
    pub fn set(&self, ts: Timestamp) -> Result<(), CoreError> {
        let requested = ts.micros();
        self.0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |now| {
                (requested >= now).then_some(requested)
            })
            .map(|_| ())
            .map_err(|now| CoreError::ClockBackwards { now, requested })
    }
}

impl Clock for SimClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_micros(self.0.load(Ordering::Acquire))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sim_clock_only_moves_forward() {
        let clock = SimClock::starting_at(Timestamp::from_secs(100));
        let shared = clock.clone();
        clock.set(Timestamp::from_secs(160)).unwrap();
        assert_eq!(shared.now(), Timestamp::from_secs(160));
        clock.set(Timestamp::from_secs(160)).unwrap();
        assert!(matches!(
            clock.set(Timestamp::from_secs(159)),
            Err(CoreError::ClockBackwards { .. })
        ));
        assert_eq!(clock.now(), Timestamp::from_secs(160));
    }

    #[test]
    fn system_clock_is_recent() {
        // After 2025-01-01.
        assert!(SystemClock.now() > Timestamp::from_secs(1_735_689_600));
    }
}
