//! A minimal strategy to copy when writing a new one: long when the close
//! crosses above its moving average (and, with `allow_short`, short when it
//! crosses below), with a fixed percentage stop and target.
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
    /// Also sell short on a cross below the average.
    pub allow_short: bool,
}

impl Default for TemplateConfig {
    fn default() -> Self {
        Self {
            sma_period: 20,
            stop_pct: 0.5,
            target_pct: 1.5,
            allow_short: false,
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
        let crossed_down = today.was_above == Some(true) && !above && self.config.allow_short;
        today.was_above = Some(above);
        // The shared gate checks feed, tradability, entry time and position.
        if !(crossed_up || crossed_down) || today.traded || entry_allowed(bar, ctx).is_err() {
            return;
        }
        let (stop_pct, target_pct) = if crossed_up {
            (-self.config.stop_pct, self.config.target_pct)
        } else {
            (self.config.stop_pct, -self.config.target_pct)
        };
        let (Some(stop), Some(target)) = (
            bar.close.offset_pct(stop_pct),
            bar.close.offset_pct(target_pct),
        ) else {
            return;
        };
        // Signals rounds to the tick and acts from the next minute;
        // `reason` names the setup in reports.
        let signal = if crossed_up {
            self.signals
                .enter_long(bar, ctx, stop, Some(target), "template_long")
        } else {
            self.signals
                .enter_short(bar, ctx, stop, Some(target), "template_short")
        };
        if let Some(signal) = signal {
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
    fn shorts_only_when_allowed() {
        let d = day(2025, 1, 7);
        // Default: no shorts.
        let mut b = bench();
        b.bar(0, d, 0, [101.0, 101.0, 101.0, 101.0], 1000); // above
        assert!(b.bar(0, d, 1, [101.0, 101.0, 97.0, 97.0], 1000).is_empty());

        let mut b =
            Bench::from_registry("template", json!({"sma_period": 3, "allow_short": true}), 1)
                .unwrap();
        b.flat_day(0, day(2025, 1, 6), 100.0);
        b.bar(0, d, 0, [101.0, 101.0, 101.0, 101.0], 1000);
        let out = b.bar(0, d, 1, [101.0, 101.0, 97.0, 97.0], 1000);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].intent, Intent::EnterShort);
        assert_eq!(out[0].reason, "template_short");
        // Stop 0.5% above 97 (97.485) rounds up to 97.50.
        assert_eq!(out[0].stop, Some(Price::from_paise(9_750)));
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
