//! Synthetic 1-minute bars for NSE-like stocks.
//!
//! The model is deliberately simple but has the features intraday code must
//! cope with:
//!
//! * **Overnight gaps**, occasionally large.
//! * **U-shaped intraday activity**: volatility and volume are highest just
//!   after the open, quietest at midday, and pick up into the close.
//! * **Trend days and range days**: some days drift steadily one way, others
//!   revert towards the open.
//! * **Tick-size prices**, continuous bars (each open is the previous close)
//!   and consistent OHLC built from sub-minute paths.
//!
//! Output is fully reproducible: the same seed, stocks and dates always give
//! the same bars. Each stock has its own random stream, so adding a stock
//! never changes the others. Days must be generated in order, since each day
//! starts from the previous close.
//!
//! This is test data for the machinery. It contains no real edge, so a
//! strategy's results on it mean nothing.

use chrono::{Datelike, NaiveDate, Weekday};
use trading_core::{Bar, CoreError, InstrumentId, MarketHours, Price, TickRounding};

use crate::rng::{Rng, mix};

/// One synthetic stock.
#[derive(Debug, Clone, PartialEq)]
pub struct StockProfile {
    pub instrument: InstrumentId,
    pub symbol: String,
    pub start_price: Price,
    pub tick_size: Price,
    /// Standard deviation of daily close-to-close log returns, e.g. `0.018`.
    pub daily_vol: f64,
    pub avg_daily_volume: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SyntheticConfig {
    pub seed: u64,
    pub hours: MarketHours,
    /// Fraction of daily volatility that happens overnight (as a gap).
    pub gap_share: f64,
    /// Probability that a day is a trend day rather than a range day.
    pub trend_day_prob: f64,
    /// Simulated price steps per minute, for realistic highs and lows.
    pub substeps: u32,
}

impl Default for SyntheticConfig {
    fn default() -> Self {
        Self {
            seed: 42,
            hours: MarketHours::NSE_EQUITY,
            gap_share: 0.35,
            trend_day_prob: 0.25,
            substeps: 10,
        }
    }
}

/// Approximate NSE tick size for a price: 1 paisa below ₹250, else 5 paise.
pub fn nse_tick_size(price: Price) -> Price {
    if price < Price::from_paise(25_000) {
        Price::from_paise(1)
    } else {
        Price::from_paise(5)
    }
}

/// `count` stocks with varied prices (₹50-₹5,000), volatility (1.2-3% a day)
/// and liquidity, named `SYN001`, `SYN002`, ... and numbered from
/// `InstrumentId(0)`.
pub fn sample_universe(count: u32, seed: u64) -> Vec<StockProfile> {
    let mut rng = Rng::new(mix(seed ^ 0x5eed_0f00));
    (0..count)
        .map(|i| {
            let rupees = (rng.uniform(50f64.ln(), 5000f64.ln())).exp();
            let start = Price::from_rupees_f64(rupees).unwrap_or(Price::from_paise(10_000));
            let tick = nse_tick_size(start);
            StockProfile {
                instrument: InstrumentId(i),
                symbol: format!("SYN{:03}", i + 1),
                start_price: start.round_to_tick(tick, TickRounding::Nearest),
                tick_size: tick,
                daily_vol: rng.uniform(0.012, 0.030),
                avg_daily_volume: rng.uniform(2e5f64.ln(), 2e7f64.ln()).exp() as u64,
            }
        })
        .collect()
}

/// Weekdays from `from` to `to` inclusive. Exchange holidays are not
/// modelled.
pub fn weekdays(from: NaiveDate, to: NaiveDate) -> impl Iterator<Item = NaiveDate> {
    from.iter_days()
        .take_while(move |d| *d <= to)
        .filter(|d| !matches!(d.weekday(), Weekday::Sat | Weekday::Sun))
}

#[derive(Debug, Clone)]
struct StockState {
    profile: StockProfile,
    rng: Rng,
    /// Log of the last close, unrounded, so rounding never accumulates.
    log_close: f64,
}

/// Generates one session at a time for a set of stocks.
#[derive(Debug, Clone)]
pub struct SyntheticMarket {
    config: SyntheticConfig,
    stocks: Vec<StockState>,
    /// Per-minute volatility multiplier, mean square 1.
    vol_shape: Vec<f64>,
    /// Per-minute volume multiplier, mean 1.
    volume_shape: Vec<f64>,
}

impl SyntheticMarket {
    pub fn new(config: SyntheticConfig, profiles: Vec<StockProfile>) -> Result<Self, CoreError> {
        let invalid = |why: String| Err(CoreError::InvalidInstrument(why));
        let minutes = usize::from(config.hours.minutes_per_session());
        if minutes == 0 || config.substeps == 0 {
            return invalid("session length and substeps must be positive".into());
        }
        if !(0.0..1.0).contains(&config.gap_share) || !(0.0..=1.0).contains(&config.trend_day_prob)
        {
            return invalid("gap_share must be in [0, 1) and trend_day_prob in [0, 1]".into());
        }
        for p in &profiles {
            if !p.start_price.is_positive() || !p.tick_size.is_positive() {
                return invalid(format!(
                    "{}: price and tick size must be positive",
                    p.symbol
                ));
            }
            if !(p.daily_vol > 0.0 && p.daily_vol <= 0.2) {
                return invalid(format!(
                    "{}: daily_vol {} must be in (0, 0.2]",
                    p.symbol, p.daily_vol
                ));
            }
        }

        let stocks = profiles
            .into_iter()
            .map(|profile| StockState {
                rng: Rng::new(mix(config.seed ^ mix(u64::from(profile.instrument.0) + 1))),
                log_close: profile.start_price.as_f64().ln(),
                profile,
            })
            .collect();

        let last = (minutes - 1) as f64;
        let vol_raw: Vec<f64> = (0..minutes)
            .map(|i| {
                1.0 + 1.5 * (-(i as f64) / 15.0).exp() + 0.5 * (-(last - i as f64) / 20.0).exp()
            })
            .collect();
        let rms = (vol_raw.iter().map(|v| v * v).sum::<f64>() / minutes as f64).sqrt();
        let vol_shape = vol_raw.iter().map(|v| v / rms).collect();

        let volume_raw: Vec<f64> = (0..minutes)
            .map(|i| {
                1.0 + 2.0 * (-(i as f64) / 20.0).exp() + 1.0 * (-(last - i as f64) / 15.0).exp()
            })
            .collect();
        let mean = volume_raw.iter().sum::<f64>() / minutes as f64;
        let volume_shape = volume_raw.iter().map(|v| v / mean).collect();

        Ok(Self {
            config,
            stocks,
            vol_shape,
            volume_shape,
        })
    }

    pub fn profiles(&self) -> impl Iterator<Item = &StockProfile> {
        self.stocks.iter().map(|s| &s.profile)
    }

    /// Bars for every stock for one session, ordered by time and then by
    /// instrument, as a live feed would deliver them.
    pub fn session(&mut self, date: NaiveDate) -> Result<Vec<Bar>, CoreError> {
        let open = self
            .config
            .hours
            .open_at(date)
            .ok_or_else(|| CoreError::InvalidBar(format!("no session open time for {date}")))?;
        let per_stock: Vec<Vec<Bar>> = self
            .stocks
            .iter_mut()
            .map(|s| simulate_day(&self.config, &self.vol_shape, &self.volume_shape, s, open))
            .collect();

        let minutes = self.vol_shape.len();
        let mut out = Vec::with_capacity(minutes * per_stock.len());
        for i in 0..minutes {
            out.extend(per_stock.iter().filter_map(|bars| bars.get(i).copied()));
        }
        Ok(out)
    }
}

fn to_price(log_price: f64, tick: Price) -> Price {
    let p = Price::from_rupees_f64(log_price.exp()).unwrap_or(tick);
    p.round_to_tick(tick, TickRounding::Nearest).max(tick)
}

fn simulate_day(
    config: &SyntheticConfig,
    vol_shape: &[f64],
    volume_shape: &[f64],
    stock: &mut StockState,
    open: trading_core::Timestamp,
) -> Vec<Bar> {
    let p = &stock.profile;
    let rng = &mut stock.rng;
    let minutes = vol_shape.len() as f64;
    let tick = p.tick_size;

    // Split daily variance between the overnight gap and the session.
    let gap_vol = p.daily_vol * config.gap_share;
    let session_vol = p.daily_vol * (1.0 - config.gap_share * config.gap_share).sqrt();
    let minute_vol = session_vol / minutes.sqrt();

    let mut gap = gap_vol * rng.normal();
    if rng.chance(0.04) {
        gap *= 3.0; // news gap
    }
    let day_open = stock.log_close + gap;

    // Trend days drift steadily; range days are pulled back to the open.
    let (drift, pull) = if rng.chance(config.trend_day_prob) {
        let direction = if rng.chance(0.5) { 1.0 } else { -1.0 };
        (
            direction * rng.uniform(0.5, 1.5) * session_vol / minutes,
            0.0,
        )
    } else {
        (0.0, 0.03)
    };

    let steps = f64::from(config.substeps);
    let mut x = day_open;
    let mut prev_close = to_price(day_open, tick);
    let mut bars = Vec::with_capacity(vol_shape.len());

    for (i, (vol_mult, volume_mult)) in vol_shape.iter().zip(volume_shape).enumerate() {
        let step_vol = minute_vol * vol_mult / steps.sqrt();
        let (mut hi, mut lo) = (x, x);
        for _ in 0..config.substeps {
            x += (drift - pull * (x - day_open)) / steps + step_vol * rng.normal();
            hi = hi.max(x);
            lo = lo.min(x);
        }

        let open_px = prev_close;
        let close_px = to_price(x, tick);
        let high = to_price(hi, tick).max(open_px).max(close_px);
        let low = to_price(lo, tick).min(open_px).min(close_px);

        // Log-normal noise with mean 1 around the U-shaped profile.
        let noise = (0.5 * rng.normal() - 0.125).exp();
        let volume = (p.avg_daily_volume as f64 / minutes * volume_mult * noise)
            .round()
            .max(1.0) as u64;

        bars.push(Bar {
            instrument: p.instrument,
            ts: open.plus_minutes(i as i64),
            open: open_px,
            high,
            low,
            close: close_px,
            volume,
        });
        prev_close = close_px;
    }

    stock.log_close = x;
    bars
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn market(stocks: u32, seed: u64) -> SyntheticMarket {
        let config = SyntheticConfig {
            seed,
            ..SyntheticConfig::default()
        };
        SyntheticMarket::new(config, sample_universe(stocks, seed)).unwrap()
    }

    #[test]
    fn sessions_are_complete_ordered_and_valid() {
        let mut m = market(5, 1);
        let hours = MarketHours::NSE_EQUITY;
        for date in weekdays(d(2025, 1, 1), d(2025, 1, 31)) {
            let bars = m.session(date).unwrap();
            assert_eq!(bars.len(), 5 * 375);
            for pair in bars.windows(2) {
                assert!((pair[0].ts, pair[0].instrument) < (pair[1].ts, pair[1].instrument));
            }
            for b in &bars {
                b.validate().unwrap();
                assert!(hours.session_minute(b.ts).is_some());
                let tick = m.profiles().nth(b.instrument.index()).unwrap().tick_size;
                for p in [b.open, b.high, b.low, b.close] {
                    assert!(p.is_on_tick(tick), "{p:?} not on {tick:?}");
                }
                assert!(b.volume > 0);
            }
            // Each bar opens at the previous bar's close.
            for id in 0..5 {
                let mine: Vec<&Bar> = bars
                    .iter()
                    .filter(|b| b.instrument == InstrumentId(id))
                    .collect();
                for pair in mine.windows(2) {
                    assert_eq!(pair[1].open, pair[0].close);
                }
            }
        }
    }

    #[test]
    fn reproducible_and_independent_per_stock() {
        let date = d(2025, 3, 3);
        let a = market(3, 9).session(date).unwrap();
        let b = market(3, 9).session(date).unwrap();
        assert_eq!(a, b);
        let c = market(3, 10).session(date).unwrap();
        assert_ne!(a, c);

        // Adding stocks leaves the existing ones unchanged.
        let more = market(6, 9).session(date).unwrap();
        let first: Vec<Bar> = more.into_iter().filter(|b| b.instrument.0 < 3).collect();
        assert_eq!(a, first);
    }

    #[test]
    fn statistics_look_like_a_market() {
        let profile = StockProfile {
            instrument: InstrumentId(0),
            symbol: "TEST".into(),
            start_price: Price::from_paise(100_000),
            tick_size: Price::from_paise(5),
            daily_vol: 0.02,
            avg_daily_volume: 1_000_000,
        };
        let mut m = SyntheticMarket::new(SyntheticConfig::default(), vec![profile]).unwrap();
        let days: Vec<Vec<Bar>> = weekdays(d(2024, 1, 1), d(2025, 12, 31))
            .map(|date| m.session(date).unwrap())
            .collect();

        // Close-to-close volatility is in the right range. Range days revert
        // to the open, so it comes out somewhat below the nominal 2%.
        let closes: Vec<f64> = days
            .iter()
            .map(|b| b.last().unwrap().close.as_f64())
            .collect();
        let rets: Vec<f64> = closes.windows(2).map(|w| (w[1] / w[0]).ln()).collect();
        let mean = rets.iter().sum::<f64>() / rets.len() as f64;
        let sd = (rets.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / rets.len() as f64).sqrt();
        assert!((0.010..0.025).contains(&sd), "daily vol {sd}");

        // U-shape: the first 15 minutes are busier than midday.
        let avg = |range: std::ops::Range<usize>, f: &dyn Fn(&Bar) -> f64| {
            let n = (days.len() * range.len()) as f64;
            days.iter()
                .flat_map(|b| &b[range.clone()])
                .map(f)
                .sum::<f64>()
                / n
        };
        let range_pct = |b: &Bar| b.range().as_f64() / b.close.as_f64();
        let volume = |b: &Bar| b.volume as f64;
        assert!(avg(0..15, &range_pct) > 1.5 * avg(180..195, &range_pct));
        assert!(avg(0..15, &volume) > 1.5 * avg(180..195, &volume));

        // Daily volume averages out near the profile.
        let daily: f64 = days
            .iter()
            .map(|b| b.iter().map(|x| x.volume as f64).sum::<f64>())
            .sum::<f64>()
            / days.len() as f64;
        assert!(
            (daily / 1_000_000.0 - 1.0).abs() < 0.1,
            "avg daily volume {daily}"
        );
    }

    #[test]
    fn rejects_bad_profiles() {
        let mut p = sample_universe(1, 0);
        p[0].daily_vol = 0.0;
        assert!(SyntheticMarket::new(SyntheticConfig::default(), p).is_err());
        let bad = SyntheticConfig {
            substeps: 0,
            ..SyntheticConfig::default()
        };
        assert!(SyntheticMarket::new(bad, sample_universe(1, 0)).is_err());
    }

    #[test]
    fn weekdays_skip_weekends() {
        // 2026-11-02 is a Monday.
        let days: Vec<NaiveDate> = weekdays(d(2026, 11, 2), d(2026, 11, 15)).collect();
        assert_eq!(days.len(), 10);
        assert!(days.iter().all(|d| d.weekday().number_from_monday() <= 5));
    }

    #[test]
    fn tick_sizes() {
        assert_eq!(
            nse_tick_size(Price::from_paise(24_999)),
            Price::from_paise(1)
        );
        assert_eq!(
            nse_tick_size(Price::from_paise(25_000)),
            Price::from_paise(5)
        );
    }
}
