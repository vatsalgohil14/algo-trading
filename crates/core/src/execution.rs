//! Orders out. Implemented by the broker adapters and by the simulator.

use std::future::Future;
use std::sync::Arc;

use crate::{BrokerOrderId, ClientOrderId, InstrumentId, OrderRequest, OrderStatus, Price};

/// Why an order request failed, and — the part that matters for safety —
/// whether the order might exist anyway.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecError {
    /// The broker or exchange refused it. Nothing was placed.
    #[error("rejected: {0}")]
    Rejected(Arc<str>),
    /// Held back by a rate limit before sending. Nothing was placed.
    #[error("rate limited")]
    RateLimited,
    /// Could not reach the broker. Nothing was placed.
    #[error("broker unavailable: {0}")]
    Unavailable(Arc<str>),
    /// The request was sent but no clear answer came back (timeout, dropped
    /// connection, server error). The order may or may not exist. Never
    /// resend: look it up with [`Execution::order_status`] first.
    #[error("outcome unknown: {0}")]
    OutcomeUnknown(Arc<str>),
    /// No order with this client id is known to the broker.
    #[error("unknown order {0}")]
    UnknownOrder(ClientOrderId),
}

impl ExecError {
    /// `true` only when the order certainly does not exist, so a new attempt
    /// cannot create a duplicate.
    pub const fn definitely_not_placed(&self) -> bool {
        matches!(
            self,
            ExecError::Rejected(_) | ExecError::RateLimited | ExecError::Unavailable(_)
        )
    }
}

/// Fields to change on a working order; `None` keeps the current value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OrderChange {
    pub qty: Option<u32>,
    pub price: Option<Price>,
    pub trigger: Option<Price>,
}

/// The broker's view of one order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderSnapshot {
    pub client_id: ClientOrderId,
    pub broker_order_id: BrokerOrderId,
    pub instrument: InstrumentId,
    pub status: OrderStatus,
    pub filled_qty: u32,
    pub avg_fill_price: Option<Price>,
}

/// The broker's view of a net position, used to reconcile ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PositionSnapshot {
    pub instrument: InstrumentId,
    /// Shares; positive long, negative short.
    pub qty: i64,
    pub avg_price: Option<Price>,
}

/// Places and manages orders.
///
/// Requests return as soon as the broker has answered. What happens to the
/// order afterwards (fills, cancels, rejections at the exchange) arrives
/// separately as [`ExecutionReport`](crate::ExecutionReport)s on a channel
/// handed out when the implementation is created, because the broker pushes
/// those at any time.
///
/// The broker is the source of truth: after any [`ExecError::OutcomeUnknown`],
/// a reconnect or a restart, the order manager rebuilds its view from
/// `order_status`, `open_orders` and `positions`.
pub trait Execution: Send + Sync {
    /// Places an order and returns the broker's id for it.
    fn submit(
        &self,
        order: &OrderRequest,
    ) -> impl Future<Output = Result<BrokerOrderId, ExecError>> + Send;

    fn modify(
        &self,
        id: ClientOrderId,
        change: OrderChange,
    ) -> impl Future<Output = Result<(), ExecError>> + Send;

    fn cancel(&self, id: ClientOrderId) -> impl Future<Output = Result<(), ExecError>> + Send;

    /// Looks an order up by our id. `Ok(None)` means the broker has no such
    /// order, so it was never placed.
    fn order_status(
        &self,
        id: ClientOrderId,
    ) -> impl Future<Output = Result<Option<OrderSnapshot>, ExecError>> + Send;

    /// All of today's orders that are not in a terminal state.
    fn open_orders(&self) -> impl Future<Output = Result<Vec<OrderSnapshot>, ExecError>> + Send;

    /// Net intraday positions.
    fn positions(&self) -> impl Future<Output = Result<Vec<PositionSnapshot>, ExecError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_clear_failures_allow_a_retry() {
        assert!(ExecError::Rejected("margin".into()).definitely_not_placed());
        assert!(ExecError::RateLimited.definitely_not_placed());
        assert!(!ExecError::OutcomeUnknown("timeout".into()).definitely_not_placed());
        let id = ClientOrderId::new("a1").unwrap();
        assert!(!ExecError::UnknownOrder(id).definitely_not_placed());
    }
}
