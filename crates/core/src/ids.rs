//! Identifiers. All are small `Copy` values so they cost nothing to pass
//! around on the hot path.

use std::fmt;

use crate::CoreError;

/// Dense internal index of an instrument (`0..n`), assigned when the
/// [`Universe`](crate::Universe) is built. Use it to index `Vec`s of
/// per-instrument state. It is only meaningful within one run; persist the
/// exchange token or symbol instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstrumentId(pub u32);

impl InstrumentId {
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

impl fmt::Display for InstrumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Dense index of a configured strategy instance (one strategy type with one
/// parameter set). Indexes `Vec`s like [`InstrumentId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StrategyId(pub u16);

impl StrategyId {
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Per-run order counter, incremented by the order manager for every order
/// it creates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct OrderSeq(pub u32);

impl OrderSeq {
    /// Returns the current value and advances. `None` once exhausted
    /// (4 billion orders), which the caller must treat as fatal.
    pub fn advance(&mut self) -> Option<OrderSeq> {
        let current = *self;
        self.0 = self.0.checked_add(1)?;
        Some(current)
    }
}

/// Our own id for an order, sent to the broker as the correlation id. It is
/// how we find an order again when a request times out and we do not know
/// whether it was placed, so it must be unique for the trading day,
/// including across restarts.
///
/// Limited to what Dhan accepts: 1-20 ASCII letters and digits.
/// Stored inline, so it is `Copy`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClientOrderId {
    len: u8,
    bytes: [u8; ClientOrderId::MAX_LEN],
}

const BASE36: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

/// Writes `value` in base 36, left-padded with zeros to `width`. Digits
/// beyond `width` are dropped from the left (the value wraps).
fn push_base36(out: &mut Vec<u8>, mut value: u64, width: usize) {
    let start = out.len();
    for _ in 0..width {
        let digit = BASE36.get((value % 36) as usize).copied().unwrap_or(b'0');
        out.push(digit);
        value /= 36;
    }
    if let Some(slice) = out.get_mut(start..) {
        slice.reverse();
    }
}

impl ClientOrderId {
    pub const MAX_LEN: usize = 20;

    pub fn new(id: &str) -> Result<Self, CoreError> {
        let raw = id.as_bytes();
        let valid = !raw.is_empty()
            && raw.len() <= Self::MAX_LEN
            && raw.iter().all(u8::is_ascii_alphanumeric);
        if !valid {
            return Err(CoreError::InvalidClientOrderId(id.to_owned()));
        }
        let mut bytes = [0u8; Self::MAX_LEN];
        for (dst, src) in bytes.iter_mut().zip(raw) {
            *dst = *src;
        }
        Ok(Self {
            len: raw.len() as u8,
            bytes,
        })
    }

    /// Builds a 16-character id from a per-process `boot` value (e.g. the
    /// start time in seconds), the strategy and the order sequence, e.g.
    /// `"0kq3z1x" + "03" + "000001f"`. Distinct boots keep ids unique across
    /// restarts on the same day. Each part wraps at its width (boot every
    /// ~2.5 years of seconds, strategy at 1296), far beyond one trading day.
    pub fn compose(boot: u64, strategy: StrategyId, seq: OrderSeq) -> Self {
        let mut buf = Vec::with_capacity(16);
        push_base36(&mut buf, boot, 7);
        push_base36(&mut buf, u64::from(strategy.0), 2);
        push_base36(&mut buf, u64::from(seq.0), 7);
        let mut bytes = [0u8; Self::MAX_LEN];
        for (dst, src) in bytes.iter_mut().zip(&buf) {
            *dst = *src;
        }
        Self {
            len: buf.len() as u8,
            bytes,
        }
    }

    pub fn as_str(&self) -> &str {
        // Only ASCII is ever stored, so this cannot fail.
        self.bytes
            .get(..usize::from(self.len))
            .and_then(|b| std::str::from_utf8(b).ok())
            .unwrap_or("")
    }
}

impl fmt::Display for ClientOrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl fmt::Debug for ClientOrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClientOrderId({})", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates() {
        assert_eq!(
            ClientOrderId::new("abc123XYZ").unwrap().as_str(),
            "abc123XYZ"
        );
        assert!(ClientOrderId::new("").is_err());
        assert!(ClientOrderId::new("has-dash").is_err());
        assert!(ClientOrderId::new("has space").is_err());
        assert!(ClientOrderId::new("ünïcode").is_err());
        assert!(ClientOrderId::new(&"a".repeat(20)).is_ok());
        assert!(ClientOrderId::new(&"a".repeat(21)).is_err());
    }

    #[test]
    fn compose_is_fixed_width_and_valid() {
        let id = ClientOrderId::compose(35, StrategyId(3), OrderSeq(51));
        assert_eq!(id.as_str(), "000000z03000001f");
        assert_eq!(ClientOrderId::new(id.as_str()).unwrap(), id);
        // Different boots never collide for the same strategy and sequence.
        let other = ClientOrderId::compose(36, StrategyId(3), OrderSeq(51));
        assert_ne!(id, other);
        // Extremes still fit.
        let max = ClientOrderId::compose(u64::MAX, StrategyId(u16::MAX), OrderSeq(u32::MAX));
        assert_eq!(max.as_str().len(), 16);
    }

    #[test]
    fn order_seq_counts_and_stops() {
        let mut seq = OrderSeq(0);
        assert_eq!(seq.advance(), Some(OrderSeq(0)));
        assert_eq!(seq.advance(), Some(OrderSeq(1)));
        let mut last = OrderSeq(u32::MAX);
        assert_eq!(last.advance(), None);
    }
}
