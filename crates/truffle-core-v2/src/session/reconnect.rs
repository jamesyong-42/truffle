//! Exponential backoff tracker for reconnection attempts.

use std::time::{Duration, Instant};

/// Exponential backoff tracker for reconnection attempts.
/// Starts at 100ms, doubles each attempt, caps at 30s.
#[derive(Debug)]
pub struct ReconnectBackoff {
    attempt: u32,
    base: Duration,
    max: Duration,
    last_attempt: Option<Instant>,
}

impl ReconnectBackoff {
    /// Create a new backoff tracker with default parameters.
    /// Base delay: 100ms, max delay: 30s.
    pub fn new() -> Self {
        Self {
            attempt: 0,
            base: Duration::from_millis(100),
            max: Duration::from_secs(30),
            last_attempt: None,
        }
    }

    /// Returns how long to wait before the next attempt.
    /// Returns `None` if the backoff period hasn't elapsed yet
    /// (i.e., the caller must wait before retrying).
    /// Returns `Some(Duration::ZERO)` if the caller may retry immediately.
    pub fn should_retry(&self) -> Option<Duration> {
        if self.attempt == 0 {
            return Some(Duration::ZERO);
        }

        let delay = self.current_delay();

        match self.last_attempt {
            Some(last) => {
                let elapsed = last.elapsed();
                if elapsed >= delay {
                    Some(Duration::ZERO)
                } else {
                    // Backoff period not yet elapsed — return remaining wait
                    None
                }
            }
            // No previous attempt recorded, allow retry
            None => Some(Duration::ZERO),
        }
    }

    /// The retry-after duration: how long the caller must wait from now.
    /// Returns `Duration::ZERO` if retrying is allowed immediately.
    pub fn retry_after(&self) -> Duration {
        if self.attempt == 0 {
            return Duration::ZERO;
        }

        let delay = self.current_delay();

        match self.last_attempt {
            Some(last) => {
                let elapsed = last.elapsed();
                if elapsed >= delay {
                    Duration::ZERO
                } else {
                    delay - elapsed
                }
            }
            None => Duration::ZERO,
        }
    }

    /// Record a successful connection -- resets the backoff.
    pub fn success(&mut self) {
        self.attempt = 0;
        self.last_attempt = None;
    }

    /// Record a failed attempt -- increases the backoff.
    pub fn failure(&mut self) {
        self.last_attempt = Some(Instant::now());
        self.attempt = self.attempt.saturating_add(1);
    }

    /// Current delay for the current attempt number.
    fn current_delay(&self) -> Duration {
        // 100ms * 2^(attempt-1), capped at 30s
        let shift = (self.attempt.saturating_sub(1)).min(31);
        let delay_ms = self.base.as_millis() as u64 * (1u64 << shift);
        let delay = Duration::from_millis(delay_ms);
        delay.min(self.max)
    }
}

impl Default for ReconnectBackoff {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state_allows_retry() {
        let backoff = ReconnectBackoff::new();
        assert!(backoff.should_retry().is_some());
        assert_eq!(backoff.retry_after(), Duration::ZERO);
    }

    #[test]
    fn test_failure_increases_delay() {
        let mut backoff = ReconnectBackoff::new();

        backoff.failure(); // attempt 1 → 100ms
        assert_eq!(backoff.current_delay(), Duration::from_millis(100));

        backoff.last_attempt = Some(Instant::now() - Duration::from_secs(60));
        backoff.failure(); // attempt 2 → 200ms
        assert_eq!(backoff.current_delay(), Duration::from_millis(200));

        backoff.last_attempt = Some(Instant::now() - Duration::from_secs(60));
        backoff.failure(); // attempt 3 → 400ms
        assert_eq!(backoff.current_delay(), Duration::from_millis(400));
    }

    #[test]
    fn test_delay_caps_at_max() {
        let mut backoff = ReconnectBackoff::new();
        for _ in 0..30 {
            backoff.last_attempt = Some(Instant::now() - Duration::from_secs(60));
            backoff.failure();
        }
        assert!(backoff.current_delay() <= Duration::from_secs(30));
    }

    #[test]
    fn test_success_resets() {
        let mut backoff = ReconnectBackoff::new();
        backoff.failure();
        backoff.failure();
        backoff.failure();

        backoff.success();
        assert_eq!(backoff.attempt, 0);
        assert!(backoff.last_attempt.is_none());
        assert!(backoff.should_retry().is_some());
    }

    #[test]
    fn test_should_retry_blocks_during_backoff() {
        let mut backoff = ReconnectBackoff::new();
        backoff.failure(); // attempt 1, last_attempt = now

        // Immediately after failure, should_retry returns None (must wait)
        assert!(backoff.should_retry().is_none());
    }
}
