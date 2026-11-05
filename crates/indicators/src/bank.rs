//! Holds every indicator for every stock, computing each one once.

use std::collections::HashMap;

use trading_core::{Bar, InstrumentId, MarketHours, Timestamp};

use crate::{BarContext, Indicator, IndicatorError, Registry, Spec};

/// Refers to one output of one indicator in an [`IndicatorBank`]. Cheap to
/// copy; get it once at start-up, then read values with it on every bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Handle {
    slot: u16,
    output: u8,
}

#[derive(Debug)]
struct Slot {
    spec: Spec,
    template: Box<dyn Indicator>,
}

/// Collects the indicators strategies need, before any data flows.
#[derive(Debug)]
pub struct BankBuilder {
    registry: Registry,
    hours: MarketHours,
    slots: Vec<Slot>,
    by_key: HashMap<String, u16>,
}

impl BankBuilder {
    pub fn new(registry: Registry) -> Self {
        Self {
            registry,
            hours: MarketHours::NSE_EQUITY,
            slots: Vec::new(),
            by_key: HashMap::new(),
        }
    }

    /// Session times used for `BarContext::session_minute` (default NSE).
    pub fn with_hours(mut self, hours: MarketHours) -> Self {
        self.hours = hours;
        self
    }

    /// Asks for an indicator output, e.g. `"ema(20)"` or `"orb(15).low"`.
    /// Asking for the same indicator again (in any spelling) returns a handle
    /// to the same instance, so it is only computed once.
    pub fn require(&mut self, text: &str) -> Result<Handle, IndicatorError> {
        let (spec, output) = Spec::parse(text)?;
        let key = spec.to_string();

        let slot = match self.by_key.get(&key) {
            Some(slot) => *slot,
            None => {
                let template = self.registry.create(&spec)?;
                let slot = u16::try_from(self.slots.len()).map_err(|_| IndicatorError::TooMany)?;
                self.slots.push(Slot { spec, template });
                self.by_key.insert(key.clone(), slot);
                slot
            }
        };

        let outputs = self
            .slots
            .get(usize::from(slot))
            .map(|s| s.template.outputs())
            .unwrap_or(&[]);
        let output = match output {
            None => 0,
            Some(name) => outputs.iter().position(|o| *o == name).ok_or_else(|| {
                IndicatorError::UnknownOutput {
                    spec: key,
                    output: name,
                    available: outputs.join(", "),
                }
            })?,
        };
        let output = u8::try_from(output).map_err(|_| IndicatorError::TooMany)?;
        Ok(Handle { slot, output })
    }

    /// Creates the per-stock instances for `instruments` stocks
    /// (`InstrumentId(0)..InstrumentId(instruments - 1)`).
    pub fn build(self, instruments: usize) -> Result<IndicatorBank, IndicatorError> {
        let per_stock = self.slots.len();
        let mut indicators = Vec::with_capacity(per_stock.saturating_mul(instruments));
        for _ in 0..instruments {
            for slot in &self.slots {
                // Fresh state for each stock; the spec was already accepted once.
                indicators.push(self.registry.create(&slot.spec)?);
            }
        }
        let specs = self.slots.into_iter().map(|s| s.spec).collect();
        Ok(IndicatorBank {
            hours: self.hours,
            specs,
            per_stock,
            indicators,
            last: vec![None; instruments],
        })
    }
}

/// Indicator state for all stocks. Not shared between threads: each engine
/// shard owns the bank for its own stocks.
#[derive(Debug)]
pub struct IndicatorBank {
    hours: MarketHours,
    specs: Vec<Spec>,
    per_stock: usize,
    /// `per_stock` consecutive indicators for each stock, stock by stock.
    indicators: Vec<Box<dyn Indicator>>,
    /// Time and IST day of the last bar accepted per stock.
    last: Vec<Option<(Timestamp, i64)>>,
}

impl IndicatorBank {
    /// Feeds a bar to all indicators of its stock. A bar for an unknown
    /// stock, or one not later than the previous bar of that stock (a
    /// duplicate or out of order), is rejected and changes nothing.
    pub fn update(&mut self, bar: &Bar) -> Result<BarContext, IndicatorError> {
        let i = bar.instrument.index();
        let last = self
            .last
            .get_mut(i)
            .ok_or(IndicatorError::UnknownInstrument(bar.instrument))?;
        let day = bar.ts.ist_day();
        let new_session = match *last {
            Some((ts, _)) if bar.ts <= ts => {
                return Err(IndicatorError::OutOfOrder {
                    instrument: bar.instrument,
                    ts: bar.ts,
                    last: ts,
                });
            }
            Some((_, last_day)) => day != last_day,
            None => true,
        };
        *last = Some((bar.ts, day));

        let ctx = BarContext {
            new_session,
            session_minute: self.hours.session_minute(bar.ts),
        };
        let start = i * self.per_stock;
        if let Some(mine) = self.indicators.get_mut(start..start + self.per_stock) {
            for ind in mine {
                ind.update(bar, &ctx);
            }
        }
        Ok(ctx)
    }

    /// Current value, or `None` while warming up or for an unknown stock.
    #[inline]
    pub fn get(&self, instrument: InstrumentId, handle: Handle) -> Option<f64> {
        let idx = instrument.index() * self.per_stock + usize::from(handle.slot);
        self.indicators.get(idx)?.value(usize::from(handle.output))
    }

    pub fn instruments(&self) -> usize {
        self.last.len()
    }

    /// Canonical specs of the distinct indicators computed per stock.
    pub fn specs(&self) -> impl Iterator<Item = String> + '_ {
        self.specs.iter().map(Spec::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trading_core::Price;

    fn bar(instrument: u32, ts: Timestamp, close: i64) -> Bar {
        let p = Price::from_paise(close);
        Bar {
            instrument: InstrumentId(instrument),
            ts,
            open: p,
            high: p,
            low: p,
            close: p,
            volume: 100,
        }
    }

    fn open() -> Timestamp {
        let date = chrono::NaiveDate::from_ymd_opt(2025, 1, 6).unwrap();
        MarketHours::NSE_EQUITY.open_at(date).unwrap()
    }

    #[test]
    fn same_indicator_is_shared() {
        let mut b = BankBuilder::new(Registry::builtin());
        let a = b.require("ema(3)").unwrap();
        let again = b.require(" EMA(3) ").unwrap();
        let high = b.require("orb(5).high").unwrap();
        let low = b.require("orb(5).low").unwrap();
        assert_eq!(a, again);
        assert_eq!(high.slot, low.slot);
        assert_ne!(high.output, low.output);
        let bank = b.build(10).unwrap();
        assert_eq!(bank.specs().collect::<Vec<_>>(), ["ema(3)", "orb(5)"]);
        assert_eq!(bank.indicators.len(), 20);
    }

    #[test]
    fn bad_requests() {
        let mut b = BankBuilder::new(Registry::builtin());
        assert!(matches!(
            b.require("nope(3)"),
            Err(IndicatorError::Unknown(_))
        ));
        assert!(matches!(
            b.require("ema(0)"),
            Err(IndicatorError::BadParams { .. })
        ));
        assert!(matches!(
            b.require("ema(3).high"),
            Err(IndicatorError::UnknownOutput { .. })
        ));
        assert!(matches!(
            b.require("ema"),
            Err(IndicatorError::BadParams { .. })
        ));
    }

    #[test]
    fn stocks_are_independent_and_order_is_enforced() {
        let mut b = BankBuilder::new(Registry::builtin());
        let sma = b.require("sma(2)").unwrap();
        let mut bank = b.build(2).unwrap();
        let t = open();

        assert!(bank.update(&bar(0, t, 100)).unwrap().new_session);
        bank.update(&bar(1, t, 500)).unwrap();
        bank.update(&bar(0, t.plus_minutes(1), 200)).unwrap();
        assert_eq!(bank.get(InstrumentId(0), sma), Some(1.5));
        assert_eq!(bank.get(InstrumentId(1), sma), None);

        // Duplicate and older bars are rejected without touching state.
        assert!(matches!(
            bank.update(&bar(0, t.plus_minutes(1), 900)),
            Err(IndicatorError::OutOfOrder { .. })
        ));
        assert!(bank.update(&bar(0, t, 900)).is_err());
        assert_eq!(bank.get(InstrumentId(0), sma), Some(1.5));

        assert!(matches!(
            bank.update(&bar(2, t, 100)),
            Err(IndicatorError::UnknownInstrument(_))
        ));
        assert_eq!(bank.get(InstrumentId(7), sma), None);
    }

    #[test]
    fn session_context() {
        let mut bank = BankBuilder::new(Registry::builtin()).build(1).unwrap();
        let t = open();
        let first = bank.update(&bar(0, t, 100)).unwrap();
        assert_eq!(
            first,
            BarContext {
                new_session: true,
                session_minute: Some(0)
            }
        );
        let next = bank.update(&bar(0, t.plus_minutes(374), 100)).unwrap();
        assert_eq!(
            next,
            BarContext {
                new_session: false,
                session_minute: Some(374)
            }
        );
        let tomorrow = bank.update(&bar(0, t.plus_minutes(24 * 60), 100)).unwrap();
        assert!(tomorrow.new_session);
    }
}
