//! Tradable instruments and the universe they are loaded into.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::{CoreError, InstrumentId, Price};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Exchange {
    Nse,
    Bse,
}

impl Exchange {
    pub const fn as_str(self) -> &'static str {
        match self {
            Exchange::Nse => "NSE",
            Exchange::Bse => "BSE",
        }
    }
}

impl fmt::Display for Exchange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One equity the system may trade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instrument {
    pub id: InstrumentId,
    pub exchange: Exchange,
    /// Exchange trading symbol, e.g. `RELIANCE`.
    pub symbol: Arc<str>,
    /// The exchange's numeric token. Dhan uses it as `security_id`; Kite
    /// reports it as `exchange_token`. This is what we persist.
    pub exchange_token: u32,
    pub tick_size: Price,
    /// Shares per order unit; 1 for cash equities.
    pub lot_size: u32,
}

/// All instruments of a run, numbered densely from 0.
///
/// Built once at start-up and then only read, so it can be shared freely
/// (e.g. in an `Arc`) between shards.
#[derive(Debug, Clone, Default)]
pub struct Universe {
    instruments: Vec<Instrument>,
    by_symbol: HashMap<(Exchange, Arc<str>), InstrumentId>,
    by_token: HashMap<(Exchange, u32), InstrumentId>,
}

impl Universe {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds an instrument and returns its id. Rejects blank symbols,
    /// non-positive tick or lot sizes, and duplicates by symbol or token.
    pub fn add(
        &mut self,
        exchange: Exchange,
        symbol: &str,
        exchange_token: u32,
        tick_size: Price,
        lot_size: u32,
    ) -> Result<InstrumentId, CoreError> {
        let symbol = symbol.trim();
        if symbol.is_empty() {
            return Err(CoreError::InvalidInstrument(format!(
                "{exchange} token {exchange_token}: empty symbol"
            )));
        }
        if !tick_size.is_positive() || lot_size == 0 {
            return Err(CoreError::InvalidInstrument(format!(
                "{exchange}:{symbol}: tick size {tick_size} and lot size {lot_size} must be positive"
            )));
        }
        let symbol: Arc<str> = Arc::from(symbol);
        if self.by_symbol.contains_key(&(exchange, symbol.clone())) {
            return Err(CoreError::DuplicateInstrument(format!(
                "{exchange}:{symbol}"
            )));
        }
        if self.by_token.contains_key(&(exchange, exchange_token)) {
            return Err(CoreError::DuplicateInstrument(format!(
                "{exchange} token {exchange_token} ({symbol})"
            )));
        }
        let index = u32::try_from(self.instruments.len())
            .map_err(|_| CoreError::InvalidInstrument("universe is full".into()))?;
        let id = InstrumentId(index);
        self.by_symbol.insert((exchange, symbol.clone()), id);
        self.by_token.insert((exchange, exchange_token), id);
        self.instruments.push(Instrument {
            id,
            exchange,
            symbol,
            exchange_token,
            tick_size,
            lot_size,
        });
        Ok(id)
    }

    pub fn get(&self, id: InstrumentId) -> Option<&Instrument> {
        self.instruments.get(id.index())
    }

    pub fn by_symbol(&self, exchange: Exchange, symbol: &str) -> Option<&Instrument> {
        let id = self.by_symbol.get(&(exchange, Arc::from(symbol)))?;
        self.get(*id)
    }

    pub fn by_token(&self, exchange: Exchange, exchange_token: u32) -> Option<&Instrument> {
        let id = self.by_token.get(&(exchange, exchange_token))?;
        self.get(*id)
    }

    pub fn len(&self) -> usize {
        self.instruments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.instruments.is_empty()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &Instrument> {
        self.instruments.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_ids_and_lookups() {
        let mut u = Universe::new();
        let tick = Price::from_paise(10);
        let a = u.add(Exchange::Nse, "RELIANCE", 2885, tick, 1).unwrap();
        let b = u.add(Exchange::Nse, " TCS ", 11536, tick, 1).unwrap();
        assert_eq!((a, b), (InstrumentId(0), InstrumentId(1)));
        assert_eq!(u.len(), 2);
        assert_eq!(u.by_symbol(Exchange::Nse, "TCS").unwrap().id, b);
        assert_eq!(
            u.by_token(Exchange::Nse, 2885).unwrap().symbol.as_ref(),
            "RELIANCE"
        );
        assert!(u.by_symbol(Exchange::Bse, "TCS").is_none());
        assert!(u.get(InstrumentId(2)).is_none());
    }

    #[test]
    fn rejects_bad_and_duplicate_entries() {
        let mut u = Universe::new();
        let tick = Price::from_paise(5);
        u.add(Exchange::Nse, "INFY", 1594, tick, 1).unwrap();
        assert!(matches!(
            u.add(Exchange::Nse, "INFY", 1, tick, 1),
            Err(CoreError::DuplicateInstrument(_))
        ));
        assert!(matches!(
            u.add(Exchange::Nse, "X", 1594, tick, 1),
            Err(CoreError::DuplicateInstrument(_))
        ));
        assert!(u.add(Exchange::Nse, " ", 2, tick, 1).is_err());
        assert!(u.add(Exchange::Nse, "Y", 3, Price::ZERO, 1).is_err());
        assert!(u.add(Exchange::Nse, "Z", 4, tick, 0).is_err());
        // Same symbol on another exchange is a different instrument.
        assert!(u.add(Exchange::Bse, "INFY", 500209, tick, 1).is_ok());
        assert_eq!(u.len(), 2);
    }
}
