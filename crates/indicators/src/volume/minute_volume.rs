use trading_core::Bar;

use crate::{BarContext, Indicator, IndicatorError, Spec};

/// Minutes in the longest session this indicator tracks.
const SLOTS: usize = 375;

/// Normal volume for this minute of the day: `vol_tod(days)`, the average
/// volume of the same session minute over the last `days` days that had a
/// bar at that minute. Today's bar is not included.
///
/// Volume follows a strong daily pattern, busiest just after the open, so
/// "above normal" only means something against the same time of day.
/// `None` outside the session and until that minute has `days` past bars.
#[derive(Debug, Clone)]
pub struct MinuteVolume {
    days: usize,
    /// `days` consecutive past volumes per minute, each a ring buffer.
    volumes: Vec<u64>,
    /// Volumes ever recorded per minute; also locates the oldest.
    recorded: Vec<u32>,
    sums: Vec<u64>,
    current: Option<f64>,
}

impl MinuteVolume {
    pub fn new(days: usize) -> Self {
        let days = days.max(1);
        Self {
            days,
            volumes: vec![0; SLOTS * days],
            recorded: vec![0; SLOTS],
            sums: vec![0; SLOTS],
            current: None,
        }
    }

    pub fn from_spec(spec: &Spec) -> Result<Box<dyn Indicator>, IndicatorError> {
        spec.expect_params(1)?;
        Ok(Box::new(Self::new(spec.period(0, 250)?)))
    }

    fn average(&self, slot: usize) -> Option<f64> {
        let recorded = *self.recorded.get(slot)? as usize;
        let sum = *self.sums.get(slot)?;
        (recorded >= self.days).then(|| sum as f64 / self.days as f64)
    }

    fn record(&mut self, slot: usize, volume: u64) {
        let (Some(recorded), Some(sum)) = (self.recorded.get_mut(slot), self.sums.get_mut(slot))
        else {
            return;
        };
        let at = slot * self.days + *recorded as usize % self.days;
        if let Some(entry) = self.volumes.get_mut(at) {
            *sum = sum.saturating_sub(*entry).saturating_add(volume);
            *entry = volume;
            *recorded = recorded.saturating_add(1);
        }
    }
}

impl Indicator for MinuteVolume {
    fn update(&mut self, bar: &Bar, ctx: &BarContext) {
        let Some(slot) = ctx.session_minute.map(usize::from).filter(|s| *s < SLOTS) else {
            self.current = None;
            return;
        };
        self.current = self.average(slot);
        self.record(slot, bar.volume);
    }

    fn value(&self, _: usize) -> Option<f64> {
        self.current
    }
}
