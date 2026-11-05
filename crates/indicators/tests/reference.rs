//! Each indicator is checked on every bar of realistic synthetic data
//! against a plain recomputation from the full history, plus small
//! hand-worked examples.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use chrono::NaiveDate;
use indicators::{BankBuilder, Handle, IndicatorBank, Registry};
use sim::synthetic::{SyntheticConfig, SyntheticMarket, sample_universe, weekdays};
use trading_core::{Bar, InstrumentId, MarketHours, Price};

const STOCKS: u32 = 3;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn synthetic_bars() -> Vec<Bar> {
    let mut market =
        SyntheticMarket::new(SyntheticConfig::default(), sample_universe(STOCKS, 7)).unwrap();
    weekdays(d(2025, 3, 3), d(2025, 3, 14))
        .flat_map(|date| market.session(date).unwrap())
        .collect()
}

fn close_enough(a: Option<f64>, b: Option<f64>, what: &str, at: usize) {
    match (a, b) {
        (None, None) => {}
        (Some(x), Some(y)) => {
            let tol = 1e-9 * y.abs().max(1.0);
            assert!(
                (x - y).abs() <= tol,
                "{what} at bar {at}: got {x}, expected {y}"
            );
        }
        _ => panic!("{what} at bar {at}: got {a:?}, expected {b:?}"),
    }
}

// ---- plain reference implementations over a stock's history ----

fn closes(h: &[Bar]) -> Vec<f64> {
    h.iter().map(|b| b.close.as_f64()).collect()
}

fn ref_sma(h: &[Bar], n: usize) -> Option<f64> {
    let c = closes(h);
    (c.len() >= n).then(|| c[c.len() - n..].iter().sum::<f64>() / n as f64)
}

fn ref_ema(h: &[Bar], n: usize) -> Option<f64> {
    let c = closes(h);
    if c.len() < n {
        return None;
    }
    let alpha = 2.0 / (n as f64 + 1.0);
    let mut v = c[..n].iter().sum::<f64>() / n as f64;
    for x in &c[n..] {
        v = alpha * x + (1.0 - alpha) * v;
    }
    Some(v)
}

fn ref_atr(h: &[Bar], n: usize) -> Option<f64> {
    if h.len() < n {
        return None;
    }
    let tr: Vec<f64> = h
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let (hi, lo) = (b.high.as_f64(), b.low.as_f64());
            if i == 0 {
                hi - lo
            } else {
                let pc = h[i - 1].close.as_f64();
                (hi - lo).max((hi - pc).abs()).max((lo - pc).abs())
            }
        })
        .collect();
    let mut v = tr[..n].iter().sum::<f64>() / n as f64;
    for x in &tr[n..] {
        v = (v * (n as f64 - 1.0) + x) / n as f64;
    }
    Some(v)
}

fn ref_rsi(h: &[Bar], n: usize) -> Option<f64> {
    let c = closes(h);
    if c.len() < n + 1 {
        return None;
    }
    let ch: Vec<f64> = c.windows(2).map(|w| w[1] - w[0]).collect();
    let (mut g, mut l) = (0.0, 0.0);
    for x in &ch[..n] {
        g += x.max(0.0) / n as f64;
        l += (-x).max(0.0) / n as f64;
    }
    for x in &ch[n..] {
        g = (g * (n as f64 - 1.0) + x.max(0.0)) / n as f64;
        l = (l * (n as f64 - 1.0) + (-x).max(0.0)) / n as f64;
    }
    Some(if l > 0.0 {
        100.0 - 100.0 / (1.0 + g / l)
    } else if g > 0.0 {
        100.0
    } else {
        50.0
    })
}

fn today(h: &[Bar]) -> &[Bar] {
    let day = h.last().unwrap().ts.ist_day();
    let start = h.iter().position(|b| b.ts.ist_day() == day).unwrap();
    &h[start..]
}

fn ref_vwap(h: &[Bar]) -> Option<f64> {
    let t = today(h);
    let v: f64 = t.iter().map(|b| b.volume as f64).sum();
    (v > 0.0).then(|| {
        t.iter()
            .map(|b| b.typical_price() * b.volume as f64)
            .sum::<f64>()
            / v
    })
}

fn ref_orb(h: &[Bar], n: usize) -> Option<(f64, f64)> {
    let t = today(h);
    if t.len() < n {
        return None;
    }
    let w = &t[..n];
    Some((
        w.iter().map(|b| b.high).max()?.as_f64(),
        w.iter().map(|b| b.low).min()?.as_f64(),
    ))
}

fn ref_prev_day(h: &[Bar]) -> Option<(f64, f64, f64)> {
    let day = h.last().unwrap().ts.ist_day();
    let prev: Vec<&Bar> = h.iter().filter(|b| b.ts.ist_day() < day).collect();
    let last_day = prev.last()?.ts.ist_day();
    let p: Vec<&&Bar> = prev.iter().filter(|b| b.ts.ist_day() == last_day).collect();
    Some((
        p.iter().map(|b| b.high).max()?.as_f64(),
        p.iter().map(|b| b.low).min()?.as_f64(),
        p.last()?.close.as_f64(),
    ))
}

#[test]
fn every_indicator_matches_its_reference_on_every_bar() {
    let mut b = BankBuilder::new(Registry::builtin());
    let sma = b.require("sma(20)").unwrap();
    let ema = b.require("ema(20)").unwrap();
    let atr = b.require("atr(14)").unwrap();
    let rsi = b.require("rsi(14)").unwrap();
    let vwap = b.require("vwap").unwrap();
    let orb_h = b.require("orb(15).high").unwrap();
    let orb_l = b.require("orb(15).low").unwrap();
    let orb_r = b.require("orb(15).range").unwrap();
    let pd_h = b.require("prev_day.high").unwrap();
    let pd_l = b.require("prev_day.low").unwrap();
    let pd_c = b.require("prev_day.close").unwrap();
    let mut bank = b.build(STOCKS as usize).unwrap();

    let bars = synthetic_bars();
    let mut history: Vec<Vec<Bar>> = vec![Vec::new(); STOCKS as usize];
    for (i, bar) in bars.iter().enumerate() {
        bank.update(bar).unwrap();
        let h = &mut history[bar.instrument.index()];
        h.push(*bar);
        let id = bar.instrument;
        let get = |handle: Handle| bank.get(id, handle);

        close_enough(get(sma), ref_sma(h, 20), "sma", i);
        close_enough(get(ema), ref_ema(h, 20), "ema", i);
        close_enough(get(atr), ref_atr(h, 14), "atr", i);
        close_enough(get(rsi), ref_rsi(h, 14), "rsi", i);
        close_enough(get(vwap), ref_vwap(h), "vwap", i);
        let orb = ref_orb(h, 15);
        close_enough(get(orb_h), orb.map(|o| o.0), "orb high", i);
        close_enough(get(orb_l), orb.map(|o| o.1), "orb low", i);
        close_enough(get(orb_r), orb.map(|o| o.0 - o.1), "orb range", i);
        let pd = ref_prev_day(h);
        close_enough(get(pd_h), pd.map(|p| p.0), "prev high", i);
        close_enough(get(pd_l), pd.map(|p| p.1), "prev low", i);
        close_enough(get(pd_c), pd.map(|p| p.2), "prev close", i);
    }
    assert_eq!(bars.len(), 10 * 375 * STOCKS as usize);
}

// ---- hand-worked examples ----

fn flat_bar(minute: i64, close: i64, volume: u64) -> Bar {
    let open = MarketHours::NSE_EQUITY.open_at(d(2025, 1, 6)).unwrap();
    let p = Price::from_paise(close);
    Bar {
        instrument: InstrumentId(0),
        ts: open.plus_minutes(minute),
        open: p,
        high: p,
        low: p,
        close: p,
        volume,
    }
}

fn bank_with(spec: &str) -> (IndicatorBank, Handle) {
    let mut b = BankBuilder::new(Registry::builtin());
    let h = b.require(spec).unwrap();
    (b.build(1).unwrap(), h)
}

fn run(spec: &str, closes_in_rupees: &[i64]) -> Vec<Option<f64>> {
    let (mut bank, h) = bank_with(spec);
    closes_in_rupees
        .iter()
        .enumerate()
        .map(|(i, c)| {
            bank.update(&flat_bar(i as i64, c * 100, 10)).unwrap();
            bank.get(InstrumentId(0), h)
        })
        .collect()
}

#[test]
fn hand_worked_examples() {
    // EMA(3): seed (1+2+3)/3 = 2, then 2 + 0.5 * (4 - 2) = 3.
    assert_eq!(
        run("ema(3)", &[1, 2, 3, 4]),
        [None, None, Some(2.0), Some(3.0)]
    );
    assert_eq!(run("sma(2)", &[1, 2, 4]), [None, Some(1.5), Some(3.0)]);
    // RSI(2): +1, +1 -> all gains = 100; then -1 -> gain 0.5, loss 0.5 = 50.
    assert_eq!(
        run("rsi(2)", &[1, 2, 3, 2]),
        [None, None, Some(100.0), Some(50.0)]
    );
    assert_eq!(run("rsi(2)", &[5, 5, 5]), [None, None, Some(50.0)]);
    // ATR(2) of flat bars is the close-to-close jump: (0 + 1) / 2, then (0.5 + 3) / 2.
    assert_eq!(run("atr(2)", &[10, 11, 14]), [None, Some(0.5), Some(1.75)]);
}

#[test]
fn vwap_resets_each_session() {
    let (mut bank, vwap) = bank_with("vwap");
    bank.update(&flat_bar(0, 10_000, 100)).unwrap();
    bank.update(&flat_bar(1, 20_000, 300)).unwrap();
    assert_eq!(bank.get(InstrumentId(0), vwap), Some(175.0));
    bank.update(&flat_bar(24 * 60, 30_000, 50)).unwrap(); // next day
    assert_eq!(bank.get(InstrumentId(0), vwap), Some(300.0));
}

#[test]
fn vwap_without_volume_is_unavailable() {
    let (mut bank, vwap) = bank_with("vwap");
    bank.update(&flat_bar(0, 10_000, 0)).unwrap();
    assert_eq!(bank.get(InstrumentId(0), vwap), None);
}

#[test]
fn opening_range_needs_every_bar_of_the_window() {
    let (mut bank, high) = bank_with("orb(3).high");
    let id = InstrumentId(0);
    bank.update(&flat_bar(0, 100, 1)).unwrap();
    bank.update(&flat_bar(1, 300, 1)).unwrap();
    assert_eq!(bank.get(id, high), None); // window not complete yet
    bank.update(&flat_bar(2, 200, 1)).unwrap();
    assert_eq!(bank.get(id, high), Some(3.0));
    bank.update(&flat_bar(3, 900, 1)).unwrap();
    assert_eq!(bank.get(id, high), Some(3.0)); // later bars do not move it

    // Next day the 09:16 bar is missing: no range all day.
    let day = 24 * 60;
    bank.update(&flat_bar(day, 100, 1)).unwrap();
    bank.update(&flat_bar(day + 2, 100, 1)).unwrap();
    bank.update(&flat_bar(day + 3, 100, 1)).unwrap();
    assert_eq!(bank.get(id, high), None);

    // The day after, the feed starts late (first bar after the window).
    bank.update(&flat_bar(2 * day + 10, 100, 1)).unwrap();
    assert_eq!(bank.get(id, high), None);

    // And recovers fully on a normal day.
    for m in 0..4 {
        bank.update(&flat_bar(3 * day + m, 500, 1)).unwrap();
    }
    assert_eq!(bank.get(id, high), Some(5.0));
}

#[test]
fn custom_indicators_plug_in_by_name() {
    use indicators::{BarContext, Indicator, IndicatorError, Spec};

    /// Counts bars: the smallest possible indicator.
    #[derive(Debug, Default)]
    struct Count(u64);
    impl Indicator for Count {
        fn update(&mut self, _: &Bar, _: &BarContext) {
            self.0 += 1;
        }
        fn value(&self, _: usize) -> Option<f64> {
            Some(self.0 as f64)
        }
    }
    fn make(spec: &Spec) -> Result<Box<dyn Indicator>, IndicatorError> {
        spec.expect_params(0)?;
        Ok(Box::new(Count::default()))
    }

    let mut registry = Registry::builtin();
    registry.register("count", make);
    let mut b = BankBuilder::new(registry);
    let count = b.require("count").unwrap();
    let mut bank = b.build(1).unwrap();
    for m in 0..5 {
        bank.update(&flat_bar(m, 100, 1)).unwrap();
    }
    assert_eq!(bank.get(InstrumentId(0), count), Some(5.0));
}
