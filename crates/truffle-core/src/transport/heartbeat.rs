use std::time::{Duration, Instant};

use tokio::sync::mpsc;

/// Default ping interval (2 seconds, matching TypeScript DEFAULT_HEARTBEAT_PING_INTERVAL_MS).
pub const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(2);

/// Default heartbeat timeout (5 seconds, matching TypeScript DEFAULT_HEARTBEAT_TIMEOUT_MS).
pub const DEFAULT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(5);

/// Heartbeat configuration.
#[derive(Debug, Clone)]
pub struct HeartbeatConfig {
    /// How often to send pings.
    pub ping_interval: Duration,
    /// How long to wait without any activity before declaring timeout.
    pub timeout: Duration,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            ping_interval: DEFAULT_PING_INTERVAL,
            timeout: DEFAULT_HEARTBEAT_TIMEOUT,
        }
    }
}

/// A ping message to send over the WebSocket.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PingMessage {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub timestamp: u64,
}

/// A pong message sent in response to a ping.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PongMessage {
    #[serde(rename = "type")]
    pub msg_type: String,
    pub timestamp: u64,
    #[serde(rename = "echoTimestamp")]
    pub echo_timestamp: u64,
}

/// Check if a JSON value is a heartbeat message (ping or pong).
/// Returns true if the message was handled as a heartbeat.
pub fn is_heartbeat_message(value: &serde_json::Value) -> bool {
    value
        .get("type")
        .and_then(|t| t.as_str())
        .map(|t| t == "ping" || t == "pong")
        .unwrap_or(false)
}

/// Create a ping message as a JSON value.
pub fn create_ping() -> serde_json::Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    serde_json::json!({
        "type": "ping",
        "timestamp": now
    })
}

/// Create a pong message responding to a ping.
pub fn create_pong(ping_timestamp: u64) -> serde_json::Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    serde_json::json!({
        "type": "pong",
        "timestamp": now,
        "echoTimestamp": ping_timestamp
    })
}

/// Tracks heartbeat state for a connection.
pub struct HeartbeatTracker {
    config: HeartbeatConfig,
    last_activity: Instant,
}

impl HeartbeatTracker {
    pub fn new(config: HeartbeatConfig) -> Self {
        Self {
            config,
            last_activity: Instant::now(),
        }
    }

    /// Record activity (message received).
    pub fn record_activity(&mut self) {
        self.last_activity = Instant::now();
    }

    /// Check if the connection has timed out.
    pub fn is_timed_out(&self) -> bool {
        self.last_activity.elapsed() > self.config.timeout
    }

    /// Duration since last activity.
    pub fn time_since_activity(&self) -> Duration {
        self.last_activity.elapsed()
    }

    /// Get the ping interval.
    pub fn ping_interval(&self) -> Duration {
        self.config.ping_interval
    }
}

/// Run a heartbeat loop for a connection.
///
/// Periodically checks if we need to send a ping, and detects timeouts.
/// Sends encoded ping messages via `write_tx` and signals timeout via return.
///
/// Returns `Ok(())` if the activity channel is closed (connection cleaned up).
/// Returns `Err(())` if heartbeat timeout is detected.
pub async fn heartbeat_loop(
    config: HeartbeatConfig,
    write_tx: mpsc::Sender<Vec<u8>>,
    mut activity_rx: mpsc::Receiver<()>,
) -> Result<(), HeartbeatTimeout> {
    let mut tracker = HeartbeatTracker::new(config);
    let mut interval = tokio::time::interval(tracker.ping_interval());

    loop {
        tokio::select! {
            _ = interval.tick() => {
                // Check for timeout
                if tracker.is_timed_out() {
                    tracing::info!(
                        "heartbeat timeout (no activity for {:?})",
                        tracker.time_since_activity()
                    );
                    return Err(HeartbeatTimeout);
                }

                // Send ping
                let ping = create_ping();
                match super::websocket::encode_message(&ping, false) {
                    Ok(encoded) => {
                        if write_tx.send(encoded).await.is_err() {
                            // Write channel closed — connection is gone
                            return Ok(());
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to encode ping: {e}");
                    }
                }
            }
            activity = activity_rx.recv() => {
                match activity {
                    Some(()) => {
                        tracker.record_activity();
                    }
                    None => {
                        // Activity channel closed — connection is gone
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// Sentinel error type for heartbeat timeout.
#[derive(Debug)]
pub struct HeartbeatTimeout;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_pong_format_matches_typescript() {
        let ping = create_ping();
        assert_eq!(ping.get("type").unwrap().as_str().unwrap(), "ping");
        assert!(ping.get("timestamp").unwrap().as_u64().is_some());

        let ts = ping.get("timestamp").unwrap().as_u64().unwrap();
        let pong = create_pong(ts);
        assert_eq!(pong.get("type").unwrap().as_str().unwrap(), "pong");
        assert_eq!(pong.get("echoTimestamp").unwrap().as_u64().unwrap(), ts);
        assert!(pong.get("timestamp").unwrap().as_u64().is_some());
    }

    #[test]
    fn is_heartbeat_message_detection() {
        assert!(is_heartbeat_message(&serde_json::json!({"type": "ping", "timestamp": 123})));
        assert!(is_heartbeat_message(&serde_json::json!({"type": "pong", "timestamp": 123})));
        assert!(!is_heartbeat_message(&serde_json::json!({"type": "announce"})));
        assert!(!is_heartbeat_message(&serde_json::json!({"foo": "bar"})));
        assert!(!is_heartbeat_message(&serde_json::json!(null)));
    }

    #[test]
    fn heartbeat_tracker_timeout() {
        let config = HeartbeatConfig {
            ping_interval: Duration::from_millis(100),
            timeout: Duration::from_millis(1), // very short for test
        };
        let tracker = HeartbeatTracker::new(config);

        // Sleep a tiny bit to exceed 1ms timeout
        std::thread::sleep(Duration::from_millis(5));
        assert!(tracker.is_timed_out());
    }

    #[test]
    fn heartbeat_tracker_not_timed_out() {
        let config = HeartbeatConfig {
            ping_interval: Duration::from_secs(1),
            timeout: Duration::from_secs(60),
        };
        let tracker = HeartbeatTracker::new(config);
        assert!(!tracker.is_timed_out());
    }

    #[test]
    fn heartbeat_tracker_activity_resets() {
        let config = HeartbeatConfig {
            ping_interval: Duration::from_millis(100),
            timeout: Duration::from_millis(50),
        };
        let mut tracker = HeartbeatTracker::new(config);

        std::thread::sleep(Duration::from_millis(30));
        tracker.record_activity();
        assert!(!tracker.is_timed_out());
    }

    #[tokio::test]
    async fn heartbeat_loop_timeout_detected() {
        let config = HeartbeatConfig {
            ping_interval: Duration::from_millis(10),
            timeout: Duration::from_millis(1), // will timeout immediately
        };

        let (write_tx, _write_rx) = mpsc::channel(16);
        let (_activity_tx, activity_rx) = mpsc::channel(16);

        // Should return Err(HeartbeatTimeout) quickly
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            heartbeat_loop(config, write_tx, activity_rx),
        )
        .await
        .expect("should not timeout the outer timeout");

        assert!(result.is_err(), "should have timed out");
    }

    #[tokio::test]
    async fn heartbeat_loop_stops_on_channel_close() {
        let config = HeartbeatConfig {
            ping_interval: Duration::from_millis(10),
            timeout: Duration::from_secs(60),
        };

        let (write_tx, _write_rx) = mpsc::channel(16);
        let (activity_tx, activity_rx) = mpsc::channel(16);

        // Drop activity sender to close the channel
        drop(activity_tx);

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            heartbeat_loop(config, write_tx, activity_rx),
        )
        .await
        .expect("should not timeout");

        assert!(result.is_ok(), "should exit cleanly when channel closes");
    }
}
