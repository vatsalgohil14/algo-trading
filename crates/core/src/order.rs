//! Orders, broker reports and fills, independent of any broker's API.

use std::sync::Arc;

use crate::{ClientOrderId, CoreError, Instrument, InstrumentId, Price, StrategyId, Timestamp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    /// `+1` for buys, `-1` for sells.
    pub const fn sign(self) -> i64 {
        match self {
            Side::Buy => 1,
            Side::Sell => -1,
        }
    }

    pub const fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Side::Buy => "BUY",
            Side::Sell => "SELL",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Product {
    /// Intraday with leverage (MIS); must be closed the same day.
    Intraday,
    /// Delivery (CNC); no leverage.
    Delivery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderKind {
    Market,
    Limit {
        price: Price,
    },
    /// Becomes a market order when the trigger trades.
    StopMarket {
        trigger: Price,
    },
    /// Becomes a limit order at `price` when the trigger trades.
    StopLimit {
        trigger: Price,
        price: Price,
    },
}

/// Protective legs placed at the broker together with the entry (a Dhan
/// super order), so the stop exists even if this process dies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bracket {
    pub stop: Price,
    pub target: Option<Price>,
}

/// Everything needed to place one order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderRequest {
    pub client_id: ClientOrderId,
    pub strategy: StrategyId,
    pub instrument: InstrumentId,
    pub side: Side,
    pub qty: u32,
    pub kind: OrderKind,
    pub product: Product,
    pub bracket: Option<Bracket>,
    pub created: Timestamp,
}

impl OrderRequest {
    /// Checks the order against its instrument before it leaves the process:
    /// positive quantity in whole lots, positive prices on the tick grid, a
    /// stop-limit price on the right side of its trigger, and bracket legs on
    /// the right side of the entry.
    ///
    /// `reference` is the expected entry price for market orders (usually the
    /// last traded price); it is ignored for priced orders.
    pub fn validate(&self, instrument: &Instrument, reference: Price) -> Result<(), CoreError> {
        let fail = |why: String| {
            Err(CoreError::InvalidOrder(format!(
                "{}: {why}",
                self.client_id
            )))
        };

        if instrument.id != self.instrument {
            return fail(format!(
                "instrument {} does not match {}",
                instrument.id, self.instrument
            ));
        }
        if self.qty == 0 || !self.qty.is_multiple_of(instrument.lot_size) {
            return fail(format!(
                "quantity {} is not a positive multiple of lot {}",
                self.qty, instrument.lot_size
            ));
        }

        let tick = instrument.tick_size;
        let check_price = |name: &str, p: Price| {
            if !p.is_positive() || !p.is_on_tick(tick) {
                Err(CoreError::InvalidOrder(format!(
                    "{}: {name} {p} must be positive and a multiple of tick {tick}",
                    self.client_id
                )))
            } else {
                Ok(())
            }
        };

        let entry = match self.kind {
            OrderKind::Market => reference,
            OrderKind::Limit { price } => {
                check_price("limit price", price)?;
                price
            }
            OrderKind::StopMarket { trigger } => {
                check_price("trigger", trigger)?;
                trigger
            }
            OrderKind::StopLimit { trigger, price } => {
                check_price("trigger", trigger)?;
                check_price("limit price", price)?;
                // A buy stop-limit must allow paying at least the trigger,
                // a sell at most; otherwise it can never fill once triggered.
                let ok = match self.side {
                    Side::Buy => price >= trigger,
                    Side::Sell => price <= trigger,
                };
                if !ok {
                    return fail(format!(
                        "{} stop-limit price {price} is on the wrong side of trigger {trigger}",
                        self.side.as_str()
                    ));
                }
                trigger
            }
        };

        if let Some(Bracket { stop, target }) = self.bracket {
            check_price("stop", stop)?;
            if !entry.is_positive() {
                return fail("bracket needs a positive reference price".into());
            }
            let stop_ok = match self.side {
                Side::Buy => stop < entry,
                Side::Sell => stop > entry,
            };
            if !stop_ok {
                return fail(format!(
                    "{} stop {stop} must be on the losing side of entry {entry}",
                    self.side.as_str()
                ));
            }
            if let Some(target) = target {
                check_price("target", target)?;
                let target_ok = match self.side {
                    Side::Buy => target > entry,
                    Side::Sell => target < entry,
                };
                if !target_ok {
                    return fail(format!(
                        "{} target {target} must be on the winning side of entry {entry}",
                        self.side.as_str()
                    ));
                }
            }
        }
        Ok(())
    }
}

/// The broker's id for an order. Only known once the broker accepts it.
pub type BrokerOrderId = Arc<str>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderStatus {
    /// Sent, not yet acknowledged.
    PendingNew,
    /// Working at the exchange (includes triggered-pending stop orders).
    Open,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
    /// Expired at end of day without filling.
    Expired,
}

impl OrderStatus {
    /// No further fills or changes can happen.
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            OrderStatus::Filled
                | OrderStatus::Cancelled
                | OrderStatus::Rejected
                | OrderStatus::Expired
        )
    }
}

/// One execution. Partial fills produce several.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fill {
    pub client_id: ClientOrderId,
    pub instrument: InstrumentId,
    pub side: Side,
    pub qty: u32,
    pub price: Price,
    pub ts: Timestamp,
    /// Exchange trade id; the key for de-duplicating fills reported twice.
    pub trade_id: Arc<str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrderEvent {
    Accepted,
    Rejected { reason: Arc<str> },
    Fill(Fill),
    Modified,
    Cancelled,
    Expired,
}

/// A change to one of our orders, as reported by the broker (or simulator).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionReport {
    pub client_id: ClientOrderId,
    pub broker_order_id: Option<BrokerOrderId>,
    pub ts: Timestamp,
    pub event: OrderEvent,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Exchange, Universe};

    fn setup() -> (Instrument, OrderRequest) {
        let mut u = Universe::new();
        let id = u
            .add(Exchange::Nse, "SBIN", 3045, Price::from_paise(5), 1)
            .unwrap();
        let instrument = u.get(id).unwrap().clone();
        let req = OrderRequest {
            client_id: ClientOrderId::new("t1").unwrap(),
            strategy: StrategyId(0),
            instrument: id,
            side: Side::Buy,
            qty: 10,
            kind: OrderKind::Limit {
                price: Price::from_paise(80_000),
            },
            product: Product::Intraday,
            bracket: Some(Bracket {
                stop: Price::from_paise(79_000),
                target: Some(Price::from_paise(82_000)),
            }),
            created: Timestamp::from_secs(0),
        };
        (instrument, req)
    }

    #[test]
    fn valid_bracket_order() {
        let (inst, req) = setup();
        req.validate(&inst, Price::ZERO).unwrap();
        let sell = OrderRequest {
            side: Side::Sell,
            bracket: Some(Bracket {
                stop: Price::from_paise(81_000),
                target: Some(Price::from_paise(78_000)),
            }),
            ..req
        };
        sell.validate(&inst, Price::ZERO).unwrap();
    }

    #[test]
    fn rejects_bad_orders() {
        let (inst, req) = setup();
        let bad = |r: OrderRequest| {
            r.validate(&inst, Price::from_paise(80_000))
                .unwrap_err()
                .to_string()
        };

        assert!(
            bad(OrderRequest {
                qty: 0,
                ..req.clone()
            })
            .contains("quantity")
        );
        assert!(
            bad(OrderRequest {
                kind: OrderKind::Limit {
                    price: Price::from_paise(80_002)
                },
                ..req.clone()
            })
            .contains("tick")
        );
        assert!(
            bad(OrderRequest {
                side: Side::Sell,
                ..req.clone()
            })
            .contains("stop")
        );
        assert!(
            bad(OrderRequest {
                bracket: Some(Bracket {
                    stop: Price::from_paise(79_000),
                    target: Some(Price::from_paise(79_500))
                }),
                ..req.clone()
            })
            .contains("target")
        );
        assert!(
            bad(OrderRequest {
                kind: OrderKind::StopLimit {
                    trigger: Price::from_paise(80_000),
                    price: Price::from_paise(79_900)
                },
                ..req.clone()
            })
            .contains("wrong side")
        );
        assert!(
            bad(OrderRequest {
                instrument: InstrumentId(9),
                ..req.clone()
            })
            .contains("does not match")
        );
        // A market order with a bracket needs a reference price.
        let market = OrderRequest {
            kind: OrderKind::Market,
            ..req
        };
        assert!(market.validate(&inst, Price::ZERO).is_err());
        market.validate(&inst, Price::from_paise(80_000)).unwrap();
    }

    #[test]
    fn side_and_status_helpers() {
        assert_eq!(Side::Buy.sign() + Side::Sell.sign(), 0);
        assert_eq!(Side::Buy.opposite(), Side::Sell);
        assert!(OrderStatus::Rejected.is_terminal());
        assert!(!OrderStatus::PartiallyFilled.is_terminal());
    }
}
