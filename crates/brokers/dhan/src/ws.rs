//! Shared WebSocket plumbing: connection with timeout and reconnect backoff.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub(crate) type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// How a stream reconnects after losing its connection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReconnectPolicy {
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub multiplier: f64,
    pub connect_timeout: Duration,
    /// Reconnect if nothing (data, ping or pong) arrives for this long.
    pub idle_timeout: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            multiplier: 2.0,
            connect_timeout: Duration::from_secs(10),
            // Dhan pings every 10 s and drops clients silent for 40 s.
            idle_timeout: Duration::from_secs(30),
        }
    }
}

pub(crate) struct Backoff {
    policy: ReconnectPolicy,
    next: Duration,
}

impl Backoff {
    pub(crate) fn new(policy: ReconnectPolicy) -> Self {
        Self {
            policy,
            next: policy.initial_delay,
        }
    }

    pub(crate) fn reset(&mut self) {
        self.next = self.policy.initial_delay;
    }

    /// Next delay with ±20% jitter, so many clients don't reconnect in step.
    pub(crate) fn next_delay(&mut self) -> Duration {
        let base = self.next;
        self.next = base
            .mul_f64(self.policy.multiplier.max(1.0))
            .min(self.policy.max_delay);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let jitter = 0.8 + 0.4 * (f64::from(nanos % 1000) / 1000.0);
        base.mul_f64(jitter)
    }
}

/// Opens a WebSocket. The URL may contain a token, so it is never logged
/// and connection errors are reported without it.
pub(crate) async fn connect(url: &str, timeout: Duration) -> Result<Ws, String> {
    match tokio::time::timeout(timeout, tokio_tungstenite::connect_async(url)).await {
        Ok(Ok((ws, _resp))) => Ok(ws),
        Ok(Err(e)) => Err(format!("connect failed: {e}")),
        Err(_) => Err(format!("connect timed out after {timeout:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        let mut b = Backoff::new(ReconnectPolicy {
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(400),
            ..ReconnectPolicy::default()
        });
        let d1 = b.next_delay();
        let d2 = b.next_delay();
        let _ = b.next_delay();
        let d4 = b.next_delay();
        assert!(d1 >= Duration::from_millis(80) && d1 <= Duration::from_millis(120));
        assert!(d2 >= Duration::from_millis(160) && d2 <= Duration::from_millis(240));
        assert!(d4 <= Duration::from_millis(480));
        b.reset();
        assert!(b.next_delay() <= Duration::from_millis(120));
    }
}
