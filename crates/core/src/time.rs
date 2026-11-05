//! Time. Hot-path timestamps are plain microseconds since the Unix epoch
//! (UTC); session logic works in IST minutes of the day. India has no
//! daylight saving, so IST is always UTC+05:30.

use std::fmt;

use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime, Offset, TimeZone, Utc};

pub const IST_OFFSET_SECS: i32 = 5 * 3600 + 30 * 60;
const MICROS_PER_SEC: i64 = 1_000_000;
const MICROS_PER_MINUTE: i64 = 60 * MICROS_PER_SEC;
const MINUTES_PER_DAY: i64 = 24 * 60;

fn ist() -> FixedOffset {
    // IST_OFFSET_SECS is in range, so this never falls back.
    FixedOffset::east_opt(IST_OFFSET_SECS).unwrap_or_else(|| Utc.fix())
}

/// Microseconds since 1970-01-01T00:00:00Z.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Timestamp(i64);

impl Timestamp {
    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    pub const fn micros(self) -> i64 {
        self.0
    }

    pub const fn from_secs(secs: i64) -> Self {
        Self(secs.saturating_mul(MICROS_PER_SEC))
    }

    pub fn from_datetime<Tz: TimeZone>(dt: &DateTime<Tz>) -> Self {
        Self(dt.timestamp_micros())
    }

    /// The instant at `time` (IST) on `date`.
    pub fn from_ist(date: NaiveDate, time: NaiveTime) -> Option<Self> {
        let local = date.and_time(time);
        ist()
            .from_local_datetime(&local)
            .single()
            .map(|dt| Self::from_datetime(&dt))
    }

    pub fn to_utc(self) -> Option<DateTime<Utc>> {
        DateTime::from_timestamp_micros(self.0)
    }

    pub fn to_ist(self) -> Option<DateTime<FixedOffset>> {
        self.to_utc().map(|dt| dt.with_timezone(&ist()))
    }

    /// The IST calendar date.
    pub fn ist_date(self) -> Option<NaiveDate> {
        self.to_ist().map(|dt| dt.date_naive())
    }

    /// IST minute of the day (`0..1440`). Pure arithmetic, no allocation.
    pub fn ist_minute_of_day(self) -> MinuteOfDay {
        let local_minutes =
            (self.0.div_euclid(MICROS_PER_MINUTE)) + i64::from(IST_OFFSET_SECS / 60);
        MinuteOfDay(local_minutes.rem_euclid(MINUTES_PER_DAY) as u16)
    }

    pub const fn plus_secs(self, secs: i64) -> Self {
        Self(self.0.saturating_add(secs.saturating_mul(MICROS_PER_SEC)))
    }

    pub const fn plus_minutes(self, minutes: i64) -> Self {
        Self(
            self.0
                .saturating_add(minutes.saturating_mul(MICROS_PER_MINUTE)),
        )
    }

    /// Start of the minute containing this instant.
    pub const fn floor_minute(self) -> Self {
        Self(self.0.div_euclid(MICROS_PER_MINUTE) * MICROS_PER_MINUTE)
    }

    /// `self - earlier` in whole microseconds (negative if `earlier` is later).
    pub const fn micros_since(self, earlier: Timestamp) -> i64 {
        self.0.saturating_sub(earlier.0)
    }
}

impl fmt::Debug for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.to_ist() {
            Some(dt) => write!(f, "Timestamp({})", dt.format("%Y-%m-%d %H:%M:%S%.6f IST")),
            None => write!(f, "Timestamp({}us)", self.0),
        }
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.to_ist() {
            Some(dt) => write!(f, "{}", dt.format("%Y-%m-%d %H:%M:%S%.3f IST")),
            None => write!(f, "{}us", self.0),
        }
    }
}

/// Minutes since local midnight (IST), `0..1440`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MinuteOfDay(u16);

impl MinuteOfDay {
    pub const fn new(hour: u16, minute: u16) -> Option<Self> {
        if hour < 24 && minute < 60 {
            Some(Self(hour * 60 + minute))
        } else {
            None
        }
    }

    pub const fn get(self) -> u16 {
        self.0
    }

    pub const fn hour(self) -> u16 {
        self.0 / 60
    }

    pub const fn minute(self) -> u16 {
        self.0 % 60
    }

    pub fn to_naive_time(self) -> Option<NaiveTime> {
        NaiveTime::from_hms_opt(u32::from(self.hour()), u32::from(self.minute()), 0)
    }
}

impl fmt::Debug for MinuteOfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}:{:02}", self.hour(), self.minute())
    }
}

impl fmt::Display for MinuteOfDay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

/// The trading day's timetable, in IST.
///
/// `open..close` is the continuous session. Our own rules are tighter: no new
/// entries from `last_entry`, and everything is closed from `square_off`, well
/// before the broker's own auto square-off (around 15:15-15:20).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarketHours {
    pub open: MinuteOfDay,
    pub close: MinuteOfDay,
    pub last_entry: MinuteOfDay,
    pub square_off: MinuteOfDay,
}

impl MarketHours {
    /// NSE cash market: 09:15-15:30, no entries from 14:45, flatten at 15:10.
    pub const NSE_EQUITY: MarketHours = MarketHours {
        open: MinuteOfDay(9 * 60 + 15),
        close: MinuteOfDay(15 * 60 + 30),
        last_entry: MinuteOfDay(14 * 60 + 45),
        square_off: MinuteOfDay(15 * 60 + 10),
    };

    /// Number of 1-minute bars in a full session (375 for NSE).
    pub const fn minutes_per_session(&self) -> u16 {
        self.close.0.saturating_sub(self.open.0)
    }

    /// Whether the continuous session is running at `ts`.
    pub fn is_open(&self, ts: Timestamp) -> bool {
        let m = ts.ist_minute_of_day();
        m >= self.open && m < self.close
    }

    /// Minutes since the open (`0` for the 09:15 bar), or `None` outside the
    /// session.
    pub fn session_minute(&self, ts: Timestamp) -> Option<u16> {
        let m = ts.ist_minute_of_day();
        (m >= self.open && m < self.close).then(|| m.0 - self.open.0)
    }

    pub fn entries_allowed(&self, ts: Timestamp) -> bool {
        let m = ts.ist_minute_of_day();
        m >= self.open && m < self.last_entry
    }

    pub fn must_square_off(&self, ts: Timestamp) -> bool {
        ts.ist_minute_of_day() >= self.square_off
    }

    /// The instant the session opens on `date`.
    pub fn open_at(&self, date: NaiveDate) -> Option<Timestamp> {
        Timestamp::from_ist(date, self.open.to_naive_time()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    #[test]
    fn ist_conversions() {
        let open = MarketHours::NSE_EQUITY.open_at(d(2026, 11, 6)).unwrap();
        // 09:15 IST is 03:45 UTC.
        assert_eq!(
            open.to_utc().unwrap().to_rfc3339(),
            "2026-11-06T03:45:00+00:00"
        );
        assert_eq!(open.ist_minute_of_day(), MinuteOfDay::new(9, 15).unwrap());
        assert_eq!(open.ist_date(), Some(d(2026, 11, 6)));
        // 23:59 IST is still the same IST date; one minute later it is not.
        let late = open.plus_minutes(14 * 60 + 44);
        assert_eq!(late.ist_minute_of_day(), MinuteOfDay::new(23, 59).unwrap());
        assert_eq!(late.ist_date(), Some(d(2026, 11, 6)));
        assert_eq!(late.plus_minutes(1).ist_date(), Some(d(2026, 11, 7)));
        assert_eq!(
            format!("{open:?}"),
            "Timestamp(2026-11-06 09:15:00.000000 IST)"
        );
    }

    #[test]
    fn before_epoch_is_handled() {
        let ts = Timestamp::from_micros(-1);
        assert_eq!(
            ts.floor_minute(),
            Timestamp::from_micros(-MICROS_PER_MINUTE)
        );
        // 1969-12-31 23:59:59.999999 UTC is 05:29 IST.
        assert_eq!(ts.ist_minute_of_day(), MinuteOfDay::new(5, 29).unwrap());
    }

    #[test]
    fn session_rules() {
        let h = MarketHours::NSE_EQUITY;
        assert_eq!(h.minutes_per_session(), 375);
        let open = h.open_at(d(2026, 11, 6)).unwrap();
        assert_eq!(h.session_minute(open), Some(0));
        assert_eq!(h.session_minute(open.plus_minutes(374)), Some(374));
        assert_eq!(h.session_minute(open.plus_minutes(375)), None);
        assert_eq!(h.session_minute(open.plus_secs(-1)), None);
        assert!(h.entries_allowed(open));
        assert!(!h.entries_allowed(open.plus_minutes(330))); // 14:45
        assert!(!h.must_square_off(open.plus_minutes(354))); // 15:09
        assert!(h.must_square_off(open.plus_minutes(355))); // 15:10
    }

    #[test]
    fn minute_of_day() {
        assert!(MinuteOfDay::new(24, 0).is_none());
        assert!(MinuteOfDay::new(9, 60).is_none());
        assert_eq!(MinuteOfDay::new(15, 10).unwrap().to_string(), "15:10");
    }
}
