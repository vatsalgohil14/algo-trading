//! Market data in. Implemented by the live broker feeds and by the replay of
//! stored bars, so the engine cannot tell them apart.

use std::future::Future;

use crate::{InstrumentId, MarketEvent};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FeedError {
    #[error("unknown instrument {0}")]
    UnknownInstrument(InstrumentId),
    #[error("feed unavailable: {0}")]
    Unavailable(String),
}

/// A stream of market events.
///
/// Contract for implementations:
/// * Events for one instrument arrive in time order.
/// * Every bar passed on has passed [`Bar::validate`](crate::Bar::validate).
/// * Connection problems are reported in-band as
///   [`MarketEvent::Status`](crate::MarketEvent::Status), not by ending the
///   stream; a live feed reconnects by itself.
/// * `next_event` returns `None` only when the feed is finished for good (end
///   of a replay, or shutdown).
///
/// Generic (not `dyn`) on purpose: the engine is compiled for one feed type,
/// so calls are static and can be inlined.
pub trait DataFeed: Send {
    /// Starts delivering events for these instruments, in addition to any
    /// already subscribed.
    fn subscribe(
        &mut self,
        instruments: &[InstrumentId],
    ) -> impl Future<Output = Result<(), FeedError>> + Send;

    fn next_event(&mut self) -> impl Future<Output = Option<MarketEvent>> + Send;
}
