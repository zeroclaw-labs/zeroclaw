//! Jittered exponential backoff for reconnect loops.
//!
//! No external randomness: a small xorshift generator seeded from the clock
//! spreads simultaneous reconnects apart, which is all jitter has to do
//! here.

use std::time::Duration;

/// Exponential backoff with +-25% jitter, capped. Call
/// [`Backoff::next_delay`] before each retry and [`Backoff::reset`] after a
/// success.
#[derive(Debug, Clone)]
pub struct Backoff {
    initial: Duration,
    cap: Duration,
    current: Duration,
    seed: u64,
}

impl Backoff {
    /// Start at `initial` and double up to `cap` on each [`Backoff::next_delay`].
    pub fn new(initial: Duration, cap: Duration) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x9E37_79B9_7F4A_7C15, |d| d.as_nanos() as u64);
        Self {
            initial,
            cap: cap.max(initial),
            current: initial,
            seed: nanos | 1,
        }
    }

    /// The next delay to sleep before retrying, then advance.
    pub fn next_delay(&mut self) -> Duration {
        let base = self.current;
        self.current = (self.current * 2).min(self.cap);
        // xorshift64*: cheap, good enough to de-synchronize retry storms.
        self.seed ^= self.seed >> 12;
        self.seed ^= self.seed << 25;
        self.seed ^= self.seed >> 27;
        let roll = self.seed.wrapping_mul(0x2545_F491_4F6C_DD1D);
        // Map to 75%..=125% of the base delay.
        let percent = 75 + (roll % 51);
        base.mul_f64(percent as f64 / 100.0)
    }

    /// Return to the initial delay after a successful connection.
    pub fn reset(&mut self) {
        self.current = self.initial;
    }

    /// The undelayed delay the next call to [`Backoff::next_delay`] is based on.
    pub fn current(&self) -> Duration {
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_until_the_cap_and_stays_within_jitter() {
        let mut b = Backoff::new(Duration::from_millis(100), Duration::from_millis(1000));
        let mut expected = 100u64;
        for _ in 0..8 {
            let d = b.next_delay().as_millis() as u64;
            assert!(
                d >= expected * 3 / 4 && d <= expected * 5 / 4,
                "{d} vs {expected}"
            );
            expected = (expected * 2).min(1000);
        }
        assert_eq!(b.current(), Duration::from_millis(1000));
        b.reset();
        assert_eq!(b.current(), Duration::from_millis(100));
    }

    #[test]
    fn cap_below_initial_is_raised_to_initial() {
        let b = Backoff::new(Duration::from_secs(2), Duration::from_secs(1));
        assert_eq!(b.current(), Duration::from_secs(2));
    }
}
