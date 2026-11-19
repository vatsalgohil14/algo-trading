//! A minimal strategy to copy when writing a new one: long when the close
//! crosses above its moving average, with a fixed percentage stop and target.
//!
//! It is an example of the shape of a strategy, not an idea worth trading.
//! See `docs/strategies/how-to-add.md`.

use indicators::{BankBuilder, Handle};
use serde::{Deserialize, Serialize};
use trading_core::{Bar, Signal, StrategyId};

use crate::kit::{Daily, Signals, entry_allowed};
use crate::{Context, Strategy, StrategyError};

/// Step 1: settings, with defaults. Unknown fields in a config file are errors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TemplateConfig {
    pub sma_period: u16,
    pub stop_pct: f64,
    pub target_pct: f64,
}

impl Default for TemplateConfig {
    fn default() -> Self {
        Self {
            sma_period: 20,
            stop_pct: 0.5,
            target_pct: 1.5,
        }
    }
}

/// Step 2: what is remembered per stock. Starts fresh every morning.
#[derive(Debug, Clone, Copy, Default)]
struct Today {
    traded: bool,
    was_above: Option<bool>,
}

#[derive(Debug)]
pub struct Template {
    id: StrategyId,
    config: TemplateConfig,
    sma: Handle,
    days: Daily<Today>,
    signals: Signals,
}

impl Template {
    /// Step 3: check the settings and ask for indicators by name.
    pub fn new(
        id: StrategyId,
        config: TemplateConfig,
        bank: &mut BankBuilder,
        instruments: usize,
    ) -> Result<Self, StrategyError> {
        let positive = |v: f64| v.is_finite() && v > 0.0;
        if !positive(config.stop_pct) || !positive(config.target_pct) {
            return Err(StrategyError::Config {
                strategy: "template",
                why: "stop_pct and target_pct must be above 0".into(),
            });
        }
        Ok(Self {
            sma: bank.require(&format!("sma({})", config.sma_period))?,
            id,
            config,
            days: Daily::new(instruments),
            signals: Signals::new(id),
        })
    }
}

impl Strategy for Template {
    fn id(&self) -> StrategyId {
        self.id
    }

    fn name(&self) -> &'static str {
        "template"
    }

    /// Step 4: the rules. Runs on each completed bar. Push signals; never size
    /// or place orders. The stop and target sit at the broker, and the
    /// engine squares off at the end of the day.
    fn on_bar(&mut self, bar: &Bar, ctx: &Context<'_>, out: &mut Vec<Signal>) {
        let Some(today) = self.days.today(bar) else {
            return;
        };
        let Some(sma) = ctx.bank.get(bar.instrument, self.sma) else {
            return; // still warming up
        };
        let above = bar.close.as_f64() > sma;
        let crossed_up = today.was_above == Some(false) && above;
        today.was_above = Some(above);
        // The shared gate checks feed, tradability, entry time and position.
        if !crossed_up || today.traded || entry_allowed(bar, ctx).is_err() {
            return;
        }
        let (Some(stop), Some(target)) = (
            bar.close.offset_pct(-self.config.stop_pct),
            bar.close.offset_pct(self.config.target_pct),
        ) else {
            return;
        };
        // Signals rounds to the tick and acts from the next minute;
        // `reason` names the setup in reports.
        if let Some(signal) = self
            .signals
            .enter_long(bar, ctx, stop, Some(target), "template_long")
        {
            today.traded = true;
            out.push(signal);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trading_core::{Intent, Price};

    use crate::testing::{Bench, day};

    fn bench() -> Bench {
        let mut b = Bench::from_registry("template", json!({"sma_period": 3}), 1).unwrap();
        b.flat_day(0, day(2025, 1, 6), 100.0);
        b
    }

    #[test]
    fn enters_once_on_a_cross_above_the_average() {
        let mut b = bench();
        let d = day(2025, 1, 7);
        assert!(b.bar(0, d, 0, [99.0, 99.0, 98.0, 98.0], 1000).is_empty()); // below
        let out = b.bar(0, d, 1, [98.0, 103.0, 98.0, 103.0], 1000); // crosses up
        assert_eq!(out.len(), 1);
        let s = out[0];
        assert_eq!(s.intent, Intent::EnterLong);
        // 0.5% stop below 103 is 102.485, rounded down to 102.45.
        assert_eq!(s.stop, Some(Price::from_paise(10_245)));
        assert_eq!(s.reason, "template_long");
        // Down and up again the same day: no second trade.
        b.bar(0, d, 2, [103.0, 103.0, 90.0, 90.0], 1000);
        assert!(b.bar(0, d, 3, [90.0, 120.0, 90.0, 120.0], 1000).is_empty());
    }

    #[test]
    fn respects_the_shared_gate() {
        let mut b = bench();
        let d = day(2025, 1, 7);
        b.feed_ok = false;
        b.bar(0, d, 0, [99.0, 99.0, 98.0, 98.0], 1000);
        assert!(b.bar(0, d, 1, [98.0, 103.0, 98.0, 103.0], 1000).is_empty());
    }

    #[test]
    fn no_entries_after_the_cut_off() {
        let mut b = bench();
        let d = day(2025, 1, 7);
        b.bar(0, d, 340, [99.0, 99.0, 98.0, 98.0], 1000);
        // 14:56: after the 14:45 last entry.
        assert!(
            b.bar(0, d, 341, [98.0, 103.0, 98.0, 103.0], 1000)
                .is_empty()
        );
    }
}
