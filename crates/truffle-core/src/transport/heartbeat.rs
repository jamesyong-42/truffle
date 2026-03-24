use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::protocol::frame::ControlMessage;

/// Default ping interval.
/// Relaxed from the TypeScript default (2s) because the Go bridge adds
/// latency: Rust → bridge TCP → Go → tsnet/WireGuard → remote Go → bridge → Rust.
/// A 2s/5s ping/timeout caused constant heartbeat_timeout disconnects.
pub const DEFAULT_PING_INTERVAL: Duration = Duration::from_secs(10);

/// Default heartbeat timeout.
/// Must be generous enough for a full round-trip through the bridge + tsnet.
pub const DEFAULT_HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(30);

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

// ---------------------------------------------------------------------------
// v3 heartbeat helpers
// ---------------------------------------------------------------------------

/// Get the current timestamp in milliseconds since epoch.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Create a v3 `ControlMessage::Ping`.
pub fn create_v3_ping() -> ControlMessage {
    ControlMessage::Ping {
        timestamp: now_ms(),
    }
}

/// Create a v3 `ControlMessage::Pong` responding to a ping timestamp.
pub fn create_v3_pong(ping_timestamp: u64) -> ControlMessage {
    ControlMessage::Pong {
        timestamp: now_ms(),
        echo_timestamp: ping_timestamp,
    }
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
/// Sends v3 control frame pings via `write_tx` and signals timeout via return.
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

                // Send v3 control frame ping (RFC 009 Phase 2)
                let ping = create_v3_ping();
                match super::websocket::encode_control_frame(&ping, false) {
                    Ok(encoded) => {
                        if write_tx.send(encoded).await.is_err() {
                            // Write channel closed — connection is gone
                            return Ok(());
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to encode v3 ping: {e}");
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

    // ═══════════════════════════════════════════════════════════════════════
    // Adversarial edge case tests for heartbeat
    // ═══════════════════════════════════════════════════════════════════════

    #[tokio::test]
    #[should_panic(expected = "must be non-zero")]
    async fn heartbeat_loop_zero_interval_panics() {
        // Edge case: 0ms ping interval triggers a panic in
        // tokio::time::interval(Duration::ZERO). This test documents
        // that callers MUST use a non-zero interval.
        let config = HeartbeatConfig {
            ping_interval: Duration::from_millis(0),
            timeout: Duration::from_millis(50),
        };

        let (write_tx, _write_rx) = mpsc::channel(256);
        let (_activity_tx, activity_rx) = mpsc::channel(16);

        // This will panic: "period must be non-zero"
        let _ = heartbeat_loop(config, write_tx, activity_rx).await;
    }

    #[tokio::test]
    async fn heartbeat_loop_very_short_interval_no_spin() {
        // Edge case #12 (unit test version): 1ms ping interval should not
        // cause an infinite busy-loop or CPU spin. Verify it times out
        // quickly and sends pings without hanging.
        let config = HeartbeatConfig {
            ping_interval: Duration::from_millis(1),
            timeout: Duration::from_millis(50),
        };

        let (write_tx, mut write_rx) = mpsc::channel(256);
        let (_activity_tx, activity_rx) = mpsc::channel(16);

        // Should timeout fairly quickly (50ms)
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            heartbeat_loop(config, write_tx, activity_rx),
        )
        .await
        .expect("heartbeat_loop with 1ms interval should not hang");

        assert!(
            result.is_err(),
            "should have detected heartbeat timeout"
        );

        // Verify that at least some pings were sent (proves the loop ran)
        let mut ping_count = 0;
        while write_rx.try_recv().is_ok() {
            ping_count += 1;
        }
        assert!(
            ping_count > 0,
            "with 1ms interval, should have sent at least some pings"
        );
    }

    #[tokio::test]
    async fn heartbeat_loop_write_channel_closed_exits_cleanly() {
        // Edge case: The write channel is closed (connection torn down)
        // while the heartbeat loop is trying to send a ping. Should exit
        // with Ok(()) rather than hanging.
        let config = HeartbeatConfig {
            ping_interval: Duration::from_millis(10),
            timeout: Duration::from_secs(60),
        };

        let (write_tx, write_rx) = mpsc::channel(16);
        let (_activity_tx, activity_rx) = mpsc::channel(16);

        // Drop the write receiver to simulate the write pump being gone
        drop(write_rx);

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            heartbeat_loop(config, write_tx, activity_rx),
        )
        .await
        .expect("should not timeout — write channel closure should cause exit");

        assert!(
            result.is_ok(),
            "heartbeat_loop should exit Ok when write channel is closed"
        );
    }

    #[tokio::test]
    async fn heartbeat_loop_activity_prevents_timeout() {
        // Verify that continuous activity prevents timeout even with
        // a short timeout window.
        let config = HeartbeatConfig {
            ping_interval: Duration::from_millis(50),
            timeout: Duration::from_millis(200),
        };

        let (write_tx, _write_rx) = mpsc::channel(256);
        let (activity_tx, activity_rx) = mpsc::channel(64);

        // Spawn the heartbeat loop
        let hb_handle = tokio::spawn(heartbeat_loop(config, write_tx, activity_rx));

        // Send activity signals faster than the timeout
        for _ in 0..10 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if activity_tx.send(()).await.is_err() {
                break;
            }
        }

        // Drop activity sender to let the loop exit cleanly
        drop(activity_tx);

        let result = tokio::time::timeout(Duration::from_secs(2), hb_handle)
            .await
            .expect("should not hang")
            .expect("task should not panic");

        // Should exit Ok (activity channel closed) rather than Err (timeout)
        assert!(
            result.is_ok(),
            "heartbeat should not have timed out with continuous activity"
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // v3 heartbeat tests
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn v3_ping_has_timestamp() {
        let ping = create_v3_ping();
        match ping {
            ControlMessage::Ping { timestamp } => {
                assert!(timestamp > 0, "timestamp should be non-zero");
            }
            other => panic!("expected Ping, got {other:?}"),
        }
    }

    #[test]
    fn v3_pong_echoes_timestamp() {
        let ping = create_v3_ping();
        let ping_ts = match ping {
            ControlMessage::Ping { timestamp } => timestamp,
            _ => panic!("not a ping"),
        };

        let pong = create_v3_pong(ping_ts);
        match pong {
            ControlMessage::Pong {
                timestamp,
                echo_timestamp,
            } => {
                assert_eq!(echo_timestamp, ping_ts);
                assert!(timestamp >= ping_ts, "pong timestamp should be >= ping timestamp");
            }
            other => panic!("expected Pong, got {other:?}"),
        }
    }

    #[test]
    fn v3_ping_encode_decode_roundtrip() {
        use super::super::websocket::{decode_frame, encode_control_frame, DecodedFrame};

        let ping = create_v3_ping();
        let encoded = encode_control_frame(&ping, false).unwrap();

        // Verify v3 frame header
        assert_eq!(encoded[0], 0x01); // FrameType::Control
        assert_eq!(encoded[1], 0x00); // msgpack flags

        let decoded = decode_frame(&encoded).unwrap();
        match decoded {
            DecodedFrame::V3Control(ctrl) => assert_eq!(ctrl, ping),
            other => panic!("expected V3Control, got {other:?}"),
        }
    }

    #[test]
    fn v3_pong_encode_decode_roundtrip() {
        use super::super::websocket::{decode_frame, encode_control_frame, DecodedFrame};

        let pong = create_v3_pong(1234567890);
        let encoded = encode_control_frame(&pong, true).unwrap();

        assert_eq!(encoded[0], 0x01); // FrameType::Control
        assert_eq!(encoded[1], 0x01); // JSON flags

        let decoded = decode_frame(&encoded).unwrap();
        match decoded {
            DecodedFrame::V3Control(ctrl) => assert_eq!(ctrl, pong),
            other => panic!("expected V3Control, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn heartbeat_loop_sends_v3_control_frames() {
        // Verify that the heartbeat loop sends v3 control frames (not v2).
        use super::super::websocket::{decode_frame, DecodedFrame};

        let config = HeartbeatConfig {
            ping_interval: Duration::from_millis(10),
            timeout: Duration::from_millis(200),
        };

        let (write_tx, mut write_rx) = mpsc::channel(256);
        let (activity_tx, activity_rx) = mpsc::channel(64);

        // Spawn the heartbeat loop
        let hb_handle = tokio::spawn(heartbeat_loop(config, write_tx, activity_rx));

        // Wait a bit for some pings to be sent
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Drop to stop the loop
        drop(activity_tx);
        let _ = tokio::time::timeout(Duration::from_secs(1), hb_handle).await;

        // Verify at least one ping was sent as a v3 control frame
        let mut v3_ping_count = 0;
        while let Ok(data) = write_rx.try_recv() {
            match decode_frame(&data) {
                Ok(DecodedFrame::V3Control(ControlMessage::Ping { timestamp })) => {
                    assert!(timestamp > 0);
                    v3_ping_count += 1;
                }
                other => panic!("expected v3 control ping, got {other:?}"),
            }
        }
        assert!(
            v3_ping_count > 0,
            "heartbeat loop should have sent at least one v3 ping"
        );
    }
}
