use serde::{Deserialize, Serialize};
use trading_core::MinuteOfDay;

use crate::StrategyError;

/// Settings for [`Orb`](super::Orb). Defaults are the v1 rules in
/// `docs/strategies/orb.md`. Fractions of the range are plain ratios
/// (`0.25` = a quarter of the range); percentages are percent of price
/// (`1.2` = 1.2%).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OrbConfig {
    /// Opening range length in minutes from 09:15.
    pub range_minutes: u16,

    // ---- Daily stock filters ----
    /// Skip the day if the open is further than this from yesterday's
    /// close, in percent.
    pub max_gap_pct: f64,
    /// Price band in rupees (checked on the opening range).
    pub min_price: f64,
    pub max_price: f64,
    /// Minimum average daily turnover in crore rupees, over
    /// `turnover_days` completed days.
    pub min_turnover_cr: f64,
    pub turnover_days: u16,
    /// Opening range width as a multiple of the daily ATR must be within
    /// `min_range_atr..=max_range_atr`.
    pub min_range_atr: f64,
    pub max_range_atr: f64,
    pub atr_days: u16,

    // ---- Entry ----
    pub allow_long: bool,
    pub allow_short: bool,
    /// A bar must close beyond the range by this fraction of the range
    /// width (at least one tick).
    pub breakout_buffer: f64,
    /// ...but not further beyond than this fraction of the width.
    pub max_chase: f64,
    /// No new entries from this IST time (`"HH:MM"`). The bar that ends
    /// at this time is the last that can trigger an entry.
    pub entry_end: String,
    /// Breakout bar volume must be at least this multiple of the normal
    /// volume for that minute of the day, averaged over `volume_days` days.
    pub volume_mult: f64,
    pub volume_days: u16,
    /// Longs only above VWAP, shorts only below.
    pub require_vwap: bool,
    /// Entry limit price this far beyond the breakout close, in percent.
    pub limit_offset_pct: f64,
    /// Give up on an entry order not filled within this many minutes.
    pub entry_timeout_minutes: u16,

    // ---- Risk and exits ----
    /// Skip the trade if the stop (the other side of the range) is further
    /// than this from the entry, in percent.
    pub max_stop_pct: f64,
    /// Target at this multiple of the risk (entry to stop).
    pub target_r: f64,
    /// Skip the trade if the target is closer than this to the entry, in
    /// percent; it would not pay for the charges. Superseded by the cost
    /// model's exact check once that exists.
    pub min_target_pct: f64,
    /// Exit when a bar closes back inside the range by this fraction of
    /// its width (`0` = any close back inside).
    pub failed_breakout_depth: f64,
    /// Move the stop to the entry price once price has gone this many R in
    /// our favour (`0` = never).
    pub breakeven_r: f64,
    /// Exit a trade that has not reached `time_stop_min_r` in its favour
    /// within this many minutes of the entry (`0` = never).
    pub time_stop_minutes: u16,
    pub time_stop_min_r: f64,

    // ---- Limits for this strategy ----
    /// Trades open (or entry orders waiting) at the same time.
    pub max_open: u16,
    pub max_entries_per_day: u16,
    /// Stop opening new trades for the day after this many losing trades.
    pub max_losses_per_day: u16,
}

impl Default for OrbConfig {
    fn default() -> Self {
        Self {
            range_minutes: 15,
            max_gap_pct: 3.0,
            min_price: 100.0,
            max_price: 5_000.0,
            min_turnover_cr: 50.0,
            turnover_days: 20,
            min_range_atr: 0.25,
            max_range_atr: 1.0,
            atr_days: 14,
            allow_long: true,
            allow_short: true,
            breakout_buffer: 0.05,
            max_chase: 0.5,
            entry_end: "11:30".into(),
            volume_mult: 1.5,
            volume_days: 20,
            require_vwap: true,
            limit_offset_pct: 0.1,
            entry_timeout_minutes: 3,
            max_stop_pct: 1.2,
            target_r: 2.0,
            min_target_pct: 0.3,
            failed_breakout_depth: 0.25,
            breakeven_r: 1.0,
            time_stop_minutes: 60,
            time_stop_min_r: 0.5,
            max_open: 5,
            max_entries_per_day: 8,
            max_losses_per_day: 3,
        }
    }
}

/// `"HH:MM"` as a time of day.
pub(crate) fn parse_hhmm(text: &str) -> Option<MinuteOfDay> {
    let (h, m) = text.trim().split_once(':')?;
    if h.len() > 2 || m.len() != 2 {
        return None;
    }
    MinuteOfDay::new(h.parse().ok()?, m.parse().ok()?)
}

impl OrbConfig {
    /// Checks every setting is usable. Run before trading; a bad config
    /// must stop start-up, not misbehave at 09:30.
    pub fn validate(&self) -> Result<(), StrategyError> {
        let fail = |why: String| {
            Err(StrategyError::Config {
                strategy: "orb",
                why,
            })
        };
        let finite = [
            ("max_gap_pct", self.max_gap_pct),
            ("min_price", self.min_price),
            ("max_price", self.max_price),
            ("min_turnover_cr", self.min_turnover_cr),
            ("min_range_atr", self.min_range_atr),
            ("max_range_atr", self.max_range_atr),
            ("breakout_buffer", self.breakout_buffer),
            ("max_chase", self.max_chase),
            ("volume_mult", self.volume_mult),
            ("limit_offset_pct", self.limit_offset_pct),
            ("max_stop_pct", self.max_stop_pct),
            ("target_r", self.target_r),
            ("min_target_pct", self.min_target_pct),
            ("failed_breakout_depth", self.failed_breakout_depth),
            ("breakeven_r", self.breakeven_r),
            ("time_stop_min_r", self.time_stop_min_r),
        ];
        for (name, v) in finite {
            if !v.is_finite() || v < 0.0 {
                return fail(format!("{name} must be a number >= 0, got {v}"));
            }
        }

        if !(1..=120).contains(&self.range_minutes) {
            return fail(format!(
                "range_minutes must be 1-120, got {}",
                self.range_minutes
            ));
        }
        for (name, v) in [
            ("turnover_days", self.turnover_days),
            ("atr_days", self.atr_days),
            ("volume_days", self.volume_days),
            ("entry_timeout_minutes", self.entry_timeout_minutes),
            ("max_open", self.max_open),
            ("max_entries_per_day", self.max_entries_per_day),
            ("max_losses_per_day", self.max_losses_per_day),
        ] {
            if v == 0 {
                return fail(format!("{name} must be at least 1"));
            }
        }
        if self.min_price <= 0.0 || self.min_price >= self.max_price {
            return fail("need 0 < min_price < max_price".into());
        }
        if self.min_range_atr >= self.max_range_atr {
            return fail("need min_range_atr < max_range_atr".into());
        }
        if self.max_chase <= self.breakout_buffer {
            return fail("max_chase must be larger than breakout_buffer".into());
        }
        if self.failed_breakout_depth > 1.0 {
            return fail("failed_breakout_depth must be at most 1 (the whole range)".into());
        }
        if self.target_r < 0.5 {
            return fail("target_r must be at least 0.5".into());
        }
        if self.max_stop_pct <= 0.0 || self.max_stop_pct > 10.0 {
            return fail("max_stop_pct must be in (0, 10]".into());
        }
        if self.limit_offset_pct > 1.0 {
            return fail("limit_offset_pct above 1% would allow very bad fills".into());
        }
        if !self.allow_long && !self.allow_short {
            return fail("at least one of allow_long and allow_short must be true".into());
        }
        let Some(_) = parse_hhmm(&self.entry_end) else {
            return fail(format!(
                "entry_end must be \"HH:MM\", got {:?}",
                self.entry_end
            ));
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid_and_round_trip() {
        let c = OrbConfig::default();
        c.validate().unwrap();
        let json = serde_json::to_string(&c).unwrap();
        let back: OrbConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, c);
        // Unspecified fields take the defaults; unknown ones are an error.
        let partial: OrbConfig = serde_json::from_str(r#"{"target_r": 3.0}"#).unwrap();
        assert_eq!(partial.target_r, 3.0);
        assert_eq!(partial.range_minutes, 15);
        assert!(serde_json::from_str::<OrbConfig>(r#"{"target": 3.0}"#).is_err());
    }

    #[test]
    fn rejects_bad_settings() {
        let bad = |f: fn(&mut OrbConfig)| {
            let mut c = OrbConfig::default();
            f(&mut c);
            c.validate().is_err()
        };
        assert!(bad(|c| c.range_minutes = 0));
        assert!(bad(|c| c.target_r = f64::NAN));
        assert!(bad(|c| c.max_stop_pct = -1.0));
        assert!(bad(|c| c.min_range_atr = 2.0));
        assert!(bad(|c| c.entry_end = "1130".into()));
        assert!(bad(|c| c.entry_end = "25:00".into()));
        assert!(bad(|c| {
            c.allow_long = false;
            c.allow_short = false;
        }));
        assert!(bad(|c| c.max_open = 0));
        assert!(bad(|c| c.max_chase = 0.01));
    }

    #[test]
    fn parses_times() {
        assert_eq!(parse_hhmm("11:30"), MinuteOfDay::new(11, 30));
        assert_eq!(parse_hhmm(" 9:45 "), MinuteOfDay::new(9, 45));
        assert_eq!(parse_hhmm("9:5"), None);
        assert_eq!(parse_hhmm("x:30"), None);
    }
}
