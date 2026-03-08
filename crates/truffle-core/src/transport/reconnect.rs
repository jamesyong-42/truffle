use std::time::Duration;

/// Maximum reconnect delay (matches TypeScript maxReconnectDelayMs default).
pub const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(30);

/// Initial reconnect delay.
pub const INITIAL_RECONNECT_DELAY: Duration = Duration::from_secs(1);

/// Reconnect strategy with exponential backoff.
#[derive(Debug, Clone)]
pub struct ReconnectStrategy {
    /// Current attempt number (0 = no attempts yet).
    attempt: u32,
    /// Maximum delay between reconnect attempts.
    max_delay: Duration,
    /// Whether auto-reconnect is enabled.
    enabled: bool,
}

impl ReconnectStrategy {
    pub fn new(enabled: bool, max_delay: Duration) -> Self {
        Self {
            attempt: 0,
            max_delay,
            enabled,
        }
    }

    /// Whether reconnection is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Get the current attempt number.
    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    /// Calculate the next reconnect delay and increment the attempt counter.
    ///
    /// Uses exponential backoff: delay = min(1s * 2^(attempt-1), max_delay).
    pub fn next_delay(&mut self) -> Duration {
        self.attempt += 1;
        let delay_secs = INITIAL_RECONNECT_DELAY.as_secs_f64()
            * 2.0f64.powi((self.attempt - 1) as i32);
        let delay = Duration::from_secs_f64(delay_secs);
        delay.min(self.max_delay)
    }

    /// Reset the attempt counter (call after a successful connection).
    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Disable reconnection.
    pub fn disable(&mut self) {
        self.enabled = false;
    }
}

impl Default for ReconnectStrategy {
    fn default() -> Self {
        Self::new(true, MAX_RECONNECT_DELAY)
    }
}

/// Information needed to reconnect to a peer.
#[derive(Debug, Clone)]
pub struct ReconnectTarget {
    /// Device ID of the remote peer.
    pub device_id: String,
    /// Tailscale hostname.
    pub hostname: String,
    /// Full MagicDNS name.
    pub dns_name: Option<String>,
    /// Port to connect to.
    pub port: u16,
    /// Reconnect strategy state.
    pub strategy: ReconnectStrategy,
}

impl ReconnectTarget {
    pub fn new(device_id: String, hostname: String, dns_name: Option<String>, port: u16) -> Self {
        Self {
            device_id,
            hostname,
            dns_name,
            port,
            strategy: ReconnectStrategy::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponential_backoff_sequence() {
        let mut strategy = ReconnectStrategy::default();

        // 1s, 2s, 4s, 8s, 16s, 30s (capped), 30s
        let expected = [1.0, 2.0, 4.0, 8.0, 16.0, 30.0, 30.0];

        for expected_secs in &expected {
            let delay = strategy.next_delay();
            assert!(
                (delay.as_secs_f64() - expected_secs).abs() < 0.01,
                "attempt {}: got {:?}, expected {}s",
                strategy.attempt(),
                delay,
                expected_secs
            );
        }
    }

    #[test]
    fn reset_restarts_backoff() {
        let mut strategy = ReconnectStrategy::default();

        strategy.next_delay(); // 1s
        strategy.next_delay(); // 2s
        strategy.next_delay(); // 4s

        strategy.reset();
        assert_eq!(strategy.attempt(), 0);

        let delay = strategy.next_delay();
        assert!(
            (delay.as_secs_f64() - 1.0).abs() < 0.01,
            "after reset should start at 1s, got {:?}",
            delay
        );
    }

    #[test]
    fn custom_max_delay() {
        let mut strategy = ReconnectStrategy::new(true, Duration::from_secs(5));

        strategy.next_delay(); // 1s
        strategy.next_delay(); // 2s
        strategy.next_delay(); // 4s
        let delay = strategy.next_delay(); // 5s (capped)

        assert!(
            (delay.as_secs_f64() - 5.0).abs() < 0.01,
            "should cap at 5s, got {:?}",
            delay
        );
    }

    #[test]
    fn disabled_strategy() {
        let strategy = ReconnectStrategy::new(false, MAX_RECONNECT_DELAY);
        assert!(!strategy.is_enabled());
    }

    #[test]
    fn reconnect_target_creation() {
        let target = ReconnectTarget::new(
            "device-1".to_string(),
            "my-host".to_string(),
            Some("my-host.tailnet.ts.net".to_string()),
            443,
        );
        assert_eq!(target.device_id, "device-1");
        assert_eq!(target.port, 443);
        assert!(target.strategy.is_enabled());
        assert_eq!(target.strategy.attempt(), 0);
    }
}
