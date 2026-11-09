//! A minimal strategy to copy when writing a new one: long when the close
//! crosses above its moving average, with a fixed percentage stop and target.
//!
//! It is an example of the shape of a strategy, not an idea worth trading.
//! See `docs/strategies/how-to-add.md`.

use indicators::{BankBuilder, Handle};
use serde::{Deserialize, Serialize};
use trading_core::{Bar, Intent, Signal, StrategyId, TickRounding};

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

/// Step 2: what is remembered per stock.
#[derive(Debug, Clone, Copy, Default)]
struct StockState {
    day: i64,
    traded_today: bool,
    was_above: Option<bool>,
}

#[derive(Debug)]
pub struct Template {
    id: StrategyId,
    config: TemplateConfig,
    sma: Handle,
    stocks: Vec<StockState>,
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
            stocks: vec![StockState::default(); instruments],
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
        let Some(state) = self.stocks.get_mut(bar.instrument.index()) else {
            return;
        };
        let today = bar.ts.ist_day();
        if state.day != today {
            *state = StockState {
                day: today,
                ..StockState::default()
            };
        }
        let Some(sma) = ctx.bank.get(bar.instrument, self.sma) else {
            return; // still warming up
        };
        let above = bar.close.as_f64() > sma;
        let crossed_up = state.was_above == Some(false) && above;
        state.was_above = Some(above);

        if !crossed_up || state.traded_today || !ctx.position.is_flat() {
            return;
        }
        if !ctx.feed_ok || !ctx.tradable || !ctx.hours.entries_allowed(bar.ts) {
            return;
        }
        let tick = ctx.instrument.tick_size;
        let (Some(stop), Some(target)) = (
            bar.close.offset_pct(-self.config.stop_pct),
            bar.close.offset_pct(self.config.target_pct),
        ) else {
            return;
        };
        state.traded_today = true;
        out.push(Signal {
            strategy: self.id,
            instrument: bar.instrument,
            ts: bar.ts.plus_minutes(1),
            intent: Intent::EnterLong,
            entry: None, // at market
            stop: Some(stop.round_to_tick(tick, TickRounding::Down)),
            target: Some(target.round_to_tick(tick, TickRounding::Nearest)),
            reason: "template_long", // names the setup in reports and scores
        });
    }
}
