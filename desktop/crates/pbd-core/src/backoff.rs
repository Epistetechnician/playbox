//! Restart delays: start small, double each time, stop growing at a cap, and forget the past once a run was long enough to count as stable.

use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Backoff {
    first: Duration,
    cap: Duration,
    attempt: u32,
}

impl Backoff {
    pub fn new(first: Duration, cap: Duration) -> Self {
        Self {
            first,
            cap: cap.max(first),
            attempt: 0,
        }
    }

    /// The delay before the next restart; each call moves one step along.
    pub fn next_delay(&mut self) -> Duration {
        let shift = self.attempt.min(30);
        self.attempt = self.attempt.saturating_add(1);
        self.first.saturating_mul(1u32 << shift).min(self.cap)
    }

    /// How many restarts have been scheduled since the last reset.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new(Duration::from_secs(1), Duration::from_secs(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_then_caps() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(10));
        let d: Vec<u64> = (0..6).map(|_| b.next_delay().as_secs()).collect();
        assert_eq!(d, [1, 2, 4, 8, 10, 10]);
        assert_eq!(b.attempt(), 6);
    }

    #[test]
    fn reset_starts_over() {
        let mut b = Backoff::default();
        b.next_delay();
        b.next_delay();
        b.reset();
        assert_eq!(b.next_delay(), Duration::from_secs(1));
    }

    #[test]
    fn never_overflows_however_long_it_runs() {
        let mut b = Backoff::new(Duration::from_secs(1), Duration::from_secs(60));
        for _ in 0..1000 {
            assert!(b.next_delay() <= Duration::from_secs(60));
        }
    }

    #[test]
    fn a_cap_below_the_first_delay_is_raised_to_it() {
        let mut b = Backoff::new(Duration::from_secs(5), Duration::from_secs(1));
        assert_eq!(b.next_delay(), Duration::from_secs(5));
    }
}
