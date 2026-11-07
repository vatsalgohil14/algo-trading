//! Which strategies run, and on which stocks.

use trading_core::{Bar, InstrumentId, Signal, StrategyId};

use crate::{Context, Strategy, StrategyError, in_range};

#[derive(Debug)]
struct Entry {
    strategy: Box<dyn Strategy>,
    /// `enabled[i]`: the strategy trades `InstrumentId(i)`.
    enabled: Vec<bool>,
}

/// The configured strategies, each with the stocks it trades.
///
/// Strategy ids are dense: the first one added must be `StrategyId(0)`,
/// the next `StrategyId(1)`, and so on, so per-strategy state elsewhere can
/// live in plain `Vec`s.
#[derive(Debug)]
pub struct StrategySet {
    instruments: usize,
    entries: Vec<Entry>,
}

impl StrategySet {
    /// An empty set for a universe of `instruments` stocks.
    pub fn new(instruments: usize) -> Self {
        Self {
            instruments,
            entries: Vec::new(),
        }
    }

    /// Adds a strategy that trades `stocks`.
    pub fn add(
        &mut self,
        strategy: Box<dyn Strategy>,
        stocks: &[InstrumentId],
    ) -> Result<StrategyId, StrategyError> {
        let expected =
            StrategyId(
                u16::try_from(self.entries.len()).map_err(|_| StrategyError::Config {
                    strategy: strategy.name(),
                    why: "too many strategies".into(),
                })?,
            );
        if strategy.id() != expected {
            return Err(StrategyError::StrategyId {
                expected,
                got: strategy.id(),
            });
        }
        let mut enabled = vec![false; self.instruments];
        for &id in stocks {
            let slot = enabled
                .get_mut(id.index())
                .ok_or(StrategyError::UnknownInstrument(id))?;
            *slot = true;
        }
        self.entries.push(Entry { strategy, enabled });
        Ok(expected)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether `strategy` trades `instrument`.
    pub fn trades(&self, strategy: StrategyId, instrument: InstrumentId) -> bool {
        self.entries
            .get(strategy.index())
            .and_then(|e| e.enabled.get(instrument.index()))
            .copied()
            .unwrap_or(false)
    }

    /// Strategies that trade `instrument`, in id order.
    pub fn strategies_for(
        &self,
        instrument: InstrumentId,
    ) -> impl Iterator<Item = StrategyId> + '_ {
        self.entries
            .iter()
            .filter(move |e| e.enabled.get(instrument.index()).copied().unwrap_or(false))
            .map(|e| e.strategy.id())
    }

    /// Gives `bar` to every strategy that trades its stock. `context` builds
    /// each strategy's view (its own position differs per strategy).
    /// Returns the number of signals added to `out`.
    pub fn on_bar<'a>(
        &mut self,
        bar: &Bar,
        mut context: impl FnMut(StrategyId) -> Context<'a>,
        out: &mut Vec<Signal>,
    ) -> usize {
        let before = out.len();
        if !in_range(bar.instrument, self.instruments) {
            return 0;
        }
        for entry in &mut self.entries {
            if entry
                .enabled
                .get(bar.instrument.index())
                .copied()
                .unwrap_or(false)
            {
                let ctx = context(entry.strategy.id());
                entry.strategy.on_bar(bar, &ctx, out);
            }
        }
        out.len() - before
    }
}

#[cfg(test)]
mod tests {
    use indicators::{BankBuilder, Registry};
    use trading_core::{Exchange, Intent, MarketHours, Price, Timestamp, Universe};

    use super::*;
    use crate::PositionView;

    /// Emits one exit signal per bar, to see who gets called.
    #[derive(Debug)]
    struct Echo(StrategyId);

    impl Strategy for Echo {
        fn id(&self) -> StrategyId {
            self.0
        }
        fn name(&self) -> &'static str {
            "echo"
        }
        fn on_bar(&mut self, bar: &Bar, _: &Context<'_>, out: &mut Vec<Signal>) {
            out.push(Signal {
                strategy: self.0,
                instrument: bar.instrument,
                ts: bar.ts,
                intent: Intent::Exit,
                entry: None,
                stop: None,
                target: None,
                reason: "echo",
            });
        }
    }

    #[test]
    fn routes_bars_to_assigned_strategies() {
        let mut set = StrategySet::new(3);
        set.add(
            Box::new(Echo(StrategyId(0))),
            &[InstrumentId(0), InstrumentId(2)],
        )
        .unwrap();
        set.add(Box::new(Echo(StrategyId(1))), &[InstrumentId(2)])
            .unwrap();
        assert!(matches!(
            set.add(Box::new(Echo(StrategyId(5))), &[]),
            Err(StrategyError::StrategyId { .. })
        ));
        assert!(matches!(
            set.add(Box::new(Echo(StrategyId(2))), &[InstrumentId(3)]),
            Err(StrategyError::UnknownInstrument(_))
        ));
        assert_eq!(set.len(), 2);
        assert!(set.trades(StrategyId(0), InstrumentId(0)));
        assert!(!set.trades(StrategyId(1), InstrumentId(0)));
        assert_eq!(
            set.strategies_for(InstrumentId(2)).collect::<Vec<_>>(),
            [StrategyId(0), StrategyId(1)]
        );

        let mut universe = Universe::new();
        for i in 0..3 {
            universe
                .add(Exchange::Nse, &format!("S{i}"), i, Price::from_paise(5), 1)
                .unwrap();
        }
        let bank = BankBuilder::new(Registry::builtin()).build(3).unwrap();
        let mut out = Vec::new();
        let run = |set: &mut StrategySet, instrument: u32, out: &mut Vec<Signal>| {
            let id = InstrumentId(instrument);
            let p = Price::from_paise(10_000);
            let bar = Bar {
                instrument: id,
                ts: Timestamp::from_secs(0),
                open: p,
                high: p,
                low: p,
                close: p,
                volume: 1,
            };
            let inst = universe
                .get(id)
                .unwrap_or_else(|| universe.get(InstrumentId(0)).unwrap());
            set.on_bar(
                &bar,
                |_| Context {
                    instrument: inst,
                    bank: &bank,
                    session: indicators::BarContext {
                        new_session: true,
                        session_minute: None,
                    },
                    hours: MarketHours::NSE_EQUITY,
                    feed_ok: true,
                    tradable: true,
                    position: PositionView::FLAT,
                },
                out,
            )
        };
        assert_eq!(run(&mut set, 0, &mut out), 1);
        assert_eq!(run(&mut set, 1, &mut out), 0);
        assert_eq!(run(&mut set, 2, &mut out), 2);
        assert_eq!(run(&mut set, 9, &mut out), 0); // unknown stock
        let who: Vec<_> = out.iter().map(|s| (s.strategy.0, s.instrument.0)).collect();
        assert_eq!(who, [(0, 0), (0, 2), (1, 2)]);
    }
}
