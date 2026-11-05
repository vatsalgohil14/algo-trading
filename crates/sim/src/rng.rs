//! A tiny deterministic random number generator (SplitMix64).
//!
//! Written here rather than taken from a crate so that a given seed produces
//! the same data forever, whatever happens to dependency versions.

#[derive(Debug, Clone)]
pub(crate) struct Rng {
    state: u64,
    spare_normal: Option<f64>,
}

pub(crate) fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

impl Rng {
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            state: seed,
            spare_normal: None,
        }
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        mix(self.state)
    }

    /// Uniform in `[0, 1)`.
    pub(crate) fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    pub(crate) fn uniform(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.next_f64()
    }

    pub(crate) fn chance(&mut self, p: f64) -> bool {
        self.next_f64() < p
    }

    /// Standard normal (Box-Muller).
    pub(crate) fn normal(&mut self) -> f64 {
        if let Some(z) = self.spare_normal.take() {
            return z;
        }
        let u1 = 1.0 - self.next_f64(); // (0, 1], so ln is finite
        let u2 = self.next_f64();
        let r = (-2.0 * u1.ln()).sqrt();
        let theta = std::f64::consts::TAU * u2;
        self.spare_normal = Some(r * theta.sin());
        r * theta.cos()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_well_behaved() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        assert_eq!(a.next_u64(), b.next_u64());

        let mut r = Rng::new(42);
        let n = 200_000;
        let (mut sum, mut sum_sq) = (0.0, 0.0);
        for _ in 0..n {
            let z = r.normal();
            sum += z;
            sum_sq += z * z;
        }
        let mean = sum / n as f64;
        let var = sum_sq / n as f64 - mean * mean;
        assert!(mean.abs() < 0.01, "mean {mean}");
        assert!((var - 1.0).abs() < 0.02, "var {var}");
        assert!(
            (0..1000)
                .map(|_| r.next_f64())
                .all(|u| (0.0..1.0).contains(&u))
        );
    }

    #[test]
    fn known_first_value() {
        // Pins the sequence: changing it would silently change all synthetic data.
        assert_eq!(Rng::new(0).next_u64(), 0xe220_a839_7b1d_cdaf);
    }
}
