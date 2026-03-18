use std::collections::HashMap;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::protocol::message_types::{ElectionCandidatePayload, ElectionResultPayload, MeshMessage};

/// Default timeout for collecting election candidates.
pub const DEFAULT_ELECTION_TIMEOUT: Duration = Duration::from_secs(3);

/// Default grace period before starting new election after primary loss.
pub const DEFAULT_PRIMARY_LOSS_GRACE: Duration = Duration::from_secs(5);

/// Timing configuration for elections.
#[derive(Debug, Clone)]
pub struct ElectionTimingConfig {
    pub election_timeout: Duration,
    pub primary_loss_grace: Duration,
}

impl Default for ElectionTimingConfig {
    fn default() -> Self {
        Self {
            election_timeout: DEFAULT_ELECTION_TIMEOUT,
            primary_loss_grace: DEFAULT_PRIMARY_LOSS_GRACE,
        }
    }
}

/// Configuration for this device's election participation.
#[derive(Debug, Clone)]
pub struct ElectionConfig {
    pub device_id: String,
    pub started_at: u64,
    pub prefer_primary: bool,
}

/// Election state machine phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElectionPhase {
    Idle,
    Waiting,
    Collecting,
    Decided,
}

/// Events emitted by the PrimaryElection.
#[derive(Debug, Clone)]
pub enum ElectionEvent {
    ElectionStarted,
    PrimaryElected { device_id: String, is_local: bool },
    PrimaryLost { previous_primary_id: String },
    /// Mesh messages to broadcast to all peers.
    Broadcast(MeshMessage),
    /// LOCAL ONLY: Grace period expired, MeshNode should call start_election().
    GracePeriodExpired,
    /// LOCAL ONLY: Election timeout expired, MeshNode should call decide_election().
    DecideNow,
}

/// PrimaryElection - Handles primary device election for STAR topology.
///
/// Election Logic:
/// 1. User-designated primary wins (preferPrimary setting)
/// 2. Longest uptime wins
/// 3. Alphabetically lowest device ID as tiebreaker
pub struct PrimaryElection {
    config: Option<ElectionConfig>,
    phase: ElectionPhase,
    primary_id: Option<String>,
    candidates: HashMap<String, ElectionCandidatePayload>,
    timing: ElectionTimingConfig,
    event_tx: mpsc::Sender<ElectionEvent>,
    /// Handle for the election timeout task (abort to cancel).
    election_timeout_handle: Option<tokio::task::AbortHandle>,
    /// Handle for the grace period task (abort to cancel).
    grace_period_handle: Option<tokio::task::AbortHandle>,
}

impl PrimaryElection {
    pub fn new(
        timing: ElectionTimingConfig,
        event_tx: mpsc::Sender<ElectionEvent>,
    ) -> Self {
        Self {
            config: None,
            phase: ElectionPhase::Idle,
            primary_id: None,
            candidates: HashMap::new(),
            timing,
            event_tx,
            election_timeout_handle: None,
            grace_period_handle: None,
        }
    }

    /// Replace the event channel sender (used when resetting for restart).
    pub fn replace_event_tx(&mut self, tx: mpsc::Sender<ElectionEvent>) {
        self.event_tx = tx;
    }

    // ── Configuration ─────────────────────────────────────────────────────

    pub fn configure(&mut self, config: ElectionConfig) {
        tracing::info!("Election configured for device {}", config.device_id);
        self.config = Some(config);
    }

    // ── State ─────────────────────────────────────────────────────────────

    pub fn is_primary(&self) -> bool {
        match (&self.primary_id, &self.config) {
            (Some(pid), Some(cfg)) => pid == &cfg.device_id,
            _ => false,
        }
    }

    pub fn primary_id(&self) -> Option<&str> {
        self.primary_id.as_deref()
    }

    pub fn phase(&self) -> ElectionPhase {
        self.phase
    }

    /// Set primary directly (e.g., from a device:list message from existing primary).
    pub fn set_primary(&mut self, device_id: &str) {
        self.primary_id = Some(device_id.to_string());
        self.phase = ElectionPhase::Idle;
        tracing::info!("Primary set to {device_id}");
    }

    // ── Election triggers ─────────────────────────────────────────────────

    /// Handle the primary going offline.
    /// Starts a grace period before triggering a new election.
    pub fn handle_primary_lost(&mut self, previous_primary_id: &str) {
        let _config = match self.config.clone() {
            Some(c) => c,
            None => return,
        };

        tracing::info!("Primary lost: {previous_primary_id}");
        self.primary_id = None;
        self.emit(ElectionEvent::PrimaryLost {
            previous_primary_id: previous_primary_id.to_string(),
        });

        self.cancel_grace_period();
        self.phase = ElectionPhase::Waiting;

        let grace_duration = self.timing.primary_loss_grace;
        let event_tx = self.event_tx.clone();

        let handle = tokio::spawn(async move {
            tokio::time::sleep(grace_duration).await;
            tracing::info!("Grace period expired, requesting election start");
            let _ = event_tx.send(ElectionEvent::GracePeriodExpired).await;
        });

        self.grace_period_handle = Some(handle.abort_handle());
    }

    /// Handle the case where no primary is detected after startup discovery.
    pub fn handle_no_primary_on_startup(&mut self) {
        if self.config.is_none() {
            return;
        }
        tracing::info!("No primary detected on startup");
        self.start_election();
    }

    // ── Election process ──────────────────────────────────────────────────

    /// Start a new election. Broadcasts election:start and our candidacy.
    pub fn start_election(&mut self) {
        let config = match &self.config {
            Some(c) => c.clone(),
            None => return,
        };

        if self.phase == ElectionPhase::Collecting {
            tracing::info!("Election already in progress");
            return;
        }

        tracing::info!("Starting election");
        self.phase = ElectionPhase::Collecting;
        self.candidates.clear();
        self.cancel_election_timeout();

        // Add ourselves as candidate
        let my_candidate = ElectionCandidatePayload {
            device_id: config.device_id.clone(),
            uptime: current_timestamp_ms() - config.started_at,
            user_designated: config.prefer_primary,
        };
        self.candidates.insert(config.device_id.clone(), my_candidate.clone());

        // Broadcast election:start
        self.emit(ElectionEvent::Broadcast(
            MeshMessage::new("election:start", &config.device_id, serde_json::json!({})),
        ));

        // Broadcast our candidacy
        self.emit(ElectionEvent::Broadcast(
            MeshMessage::new(
                "election:candidate",
                &config.device_id,
                serde_json::to_value(&my_candidate).unwrap_or_default(),
            ),
        ));

        self.emit(ElectionEvent::ElectionStarted);

        // Set timeout to decide
        let timeout_duration = self.timing.election_timeout;
        let event_tx = self.event_tx.clone();

        let handle = tokio::spawn(async move {
            tokio::time::sleep(timeout_duration).await;
            let _ = event_tx.send(ElectionEvent::DecideNow).await;
        });

        self.election_timeout_handle = Some(handle.abort_handle());
    }

    /// Decide the election outcome based on collected candidates.
    /// Called from the MeshNode when the election timeout fires.
    pub fn decide_election(&mut self) {
        let config = match &self.config {
            Some(c) => c.clone(),
            None => return,
        };

        tracing::info!("Deciding election with {} candidates", self.candidates.len());

        let winner = self.select_winner();
        match winner {
            None => {
                tracing::info!("No candidates, becoming primary by default");
                self.become_primary();
            }
            Some(winner) if winner.device_id == config.device_id => {
                tracing::info!("We won the election");
                self.become_primary();
            }
            Some(winner) => {
                tracing::info!("Winner: {}", winner.device_id);
                self.primary_id = Some(winner.device_id.clone());
                self.phase = ElectionPhase::Decided;
                self.emit(ElectionEvent::PrimaryElected {
                    device_id: winner.device_id,
                    is_local: false,
                });
            }
        }
    }

    /// Select the winner from current candidates.
    ///
    /// Priority:
    /// 1. User-designated primary wins
    /// 2. Longest uptime wins
    /// 3. Lexicographically lowest device ID as tiebreaker
    fn select_winner(&self) -> Option<ElectionCandidatePayload> {
        let mut candidates: Vec<_> = self.candidates.values().cloned().collect();
        if candidates.is_empty() {
            return None;
        }

        candidates.sort_by(|a, b| {
            // User-designated first
            match (a.user_designated, b.user_designated) {
                (true, false) => return std::cmp::Ordering::Less,
                (false, true) => return std::cmp::Ordering::Greater,
                _ => {}
            }
            // Longest uptime wins (descending)
            match b.uptime.cmp(&a.uptime) {
                std::cmp::Ordering::Equal => {}
                other => return other,
            }
            // Lexicographic tiebreaker (ascending)
            a.device_id.cmp(&b.device_id)
        });

        Some(candidates[0].clone())
    }

    fn become_primary(&mut self) {
        let config = match &self.config {
            Some(c) => c.clone(),
            None => return,
        };

        tracing::info!("Becoming primary");
        self.primary_id = Some(config.device_id.clone());
        self.phase = ElectionPhase::Decided;

        let result = ElectionResultPayload {
            new_primary_id: config.device_id.clone(),
            previous_primary_id: None,
            reason: "election".to_string(),
        };
        self.emit(ElectionEvent::Broadcast(
            MeshMessage::new(
                "election:result",
                &config.device_id,
                serde_json::to_value(&result).unwrap_or_default(),
            ),
        ));
        self.emit(ElectionEvent::PrimaryElected {
            device_id: config.device_id,
            is_local: true,
        });
    }

    // ── Message handling ──────────────────────────────────────────────────

    /// Handle election:start from a remote peer.
    pub fn handle_election_start(&mut self, from: &str) {
        let config = match &self.config {
            Some(c) => c.clone(),
            None => return,
        };

        tracing::info!("Received election:start from {from}");

        if self.phase != ElectionPhase::Collecting {
            self.phase = ElectionPhase::Collecting;
            self.candidates.clear();
            self.cancel_election_timeout();

            // Add ourselves
            let my_candidate = ElectionCandidatePayload {
                device_id: config.device_id.clone(),
                uptime: current_timestamp_ms() - config.started_at,
                user_designated: config.prefer_primary,
            };
            self.candidates.insert(config.device_id.clone(), my_candidate.clone());

            // Broadcast candidacy
            self.emit(ElectionEvent::Broadcast(
                MeshMessage::new(
                    "election:candidate",
                    &config.device_id,
                    serde_json::to_value(&my_candidate).unwrap_or_default(),
                ),
            ));

            // Set timeout
            let timeout_duration = self.timing.election_timeout;
            let event_tx = self.event_tx.clone();

            let handle = tokio::spawn(async move {
                tokio::time::sleep(timeout_duration).await;
                let _ = event_tx.send(ElectionEvent::DecideNow).await;
            });

            self.election_timeout_handle = Some(handle.abort_handle());
        }
    }

    /// Handle election:candidate from a remote peer.
    pub fn handle_election_candidate(&mut self, _from: &str, payload: &ElectionCandidatePayload) {
        if self.config.is_none() {
            return;
        }
        tracing::info!(
            "Received candidacy from {}: uptime={}, designated={}",
            payload.device_id, payload.uptime, payload.user_designated
        );
        self.candidates.insert(payload.device_id.clone(), payload.clone());
    }

    /// Handle election:result from a remote peer.
    pub fn handle_election_result(&mut self, _from: &str, payload: &ElectionResultPayload) {
        let device_id = match &self.config {
            Some(c) => c.device_id.clone(),
            None => return,
        };

        tracing::info!(
            "Received election result: winner={}",
            payload.new_primary_id
        );

        self.cancel_election_timeout();
        self.cancel_grace_period();

        self.primary_id = Some(payload.new_primary_id.clone());
        self.phase = ElectionPhase::Decided;
        self.candidates.clear();

        let is_local = payload.new_primary_id == device_id;
        self.emit(ElectionEvent::PrimaryElected {
            device_id: payload.new_primary_id.clone(),
            is_local,
        });
    }

    // ── Cleanup ───────────────────────────────────────────────────────────

    fn cancel_election_timeout(&mut self) {
        if let Some(handle) = self.election_timeout_handle.take() {
            handle.abort();
        }
    }

    fn cancel_grace_period(&mut self) {
        if let Some(handle) = self.grace_period_handle.take() {
            handle.abort();
        }
    }

    pub fn reset(&mut self) {
        self.cancel_election_timeout();
        self.cancel_grace_period();
        self.phase = ElectionPhase::Idle;
        self.primary_id = None;
        self.candidates.clear();
    }

    // ── Internal ──────────────────────────────────────────────────────────

    fn emit(&self, event: ElectionEvent) {
        if let Err(mpsc::error::TrySendError::Full(event)) = self.event_tx.try_send(event) {
            tracing::warn!(
                "ElectionEvent channel full, dropping {:?}",
                std::mem::discriminant(&event)
            );
        }
    }
}

fn current_timestamp_ms() -> u64 {
    crate::util::current_timestamp_ms()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_election() -> (PrimaryElection, mpsc::Receiver<ElectionEvent>) {
        let (tx, rx) = mpsc::channel(256);
        let election = PrimaryElection::new(ElectionTimingConfig::default(), tx);
        (election, rx)
    }

    fn config(id: &str, started_at: u64, prefer: bool) -> ElectionConfig {
        ElectionConfig {
            device_id: id.to_string(),
            started_at,
            prefer_primary: prefer,
        }
    }

    #[test]
    fn initial_state() {
        let (election, _rx) = make_election();
        assert!(!election.is_primary());
        assert!(election.primary_id().is_none());
        assert_eq!(election.phase(), ElectionPhase::Idle);
    }

    #[test]
    fn set_primary() {
        let (mut election, _rx) = make_election();
        election.configure(config("dev-1", 0, false));
        election.set_primary("dev-1");
        assert!(election.is_primary());
        assert_eq!(election.primary_id(), Some("dev-1"));
    }

    #[test]
    fn select_winner_user_designated_wins() {
        let (mut election, _rx) = make_election();
        election.configure(config("dev-1", 0, false));

        election.candidates.insert("dev-1".to_string(), ElectionCandidatePayload {
            device_id: "dev-1".to_string(),
            uptime: 100000,
            user_designated: false,
        });
        election.candidates.insert("dev-2".to_string(), ElectionCandidatePayload {
            device_id: "dev-2".to_string(),
            uptime: 50000,
            user_designated: true,
        });

        let winner = election.select_winner().unwrap();
        assert_eq!(winner.device_id, "dev-2");
    }

    #[test]
    fn select_winner_longest_uptime_wins() {
        let (mut election, _rx) = make_election();
        election.configure(config("dev-1", 0, false));

        election.candidates.insert("dev-1".to_string(), ElectionCandidatePayload {
            device_id: "dev-1".to_string(),
            uptime: 50000,
            user_designated: false,
        });
        election.candidates.insert("dev-2".to_string(), ElectionCandidatePayload {
            device_id: "dev-2".to_string(),
            uptime: 100000,
            user_designated: false,
        });

        let winner = election.select_winner().unwrap();
        assert_eq!(winner.device_id, "dev-2");
    }

    #[test]
    fn select_winner_lexicographic_tiebreaker() {
        let (mut election, _rx) = make_election();
        election.configure(config("dev-1", 0, false));

        election.candidates.insert("dev-b".to_string(), ElectionCandidatePayload {
            device_id: "dev-b".to_string(),
            uptime: 50000,
            user_designated: false,
        });
        election.candidates.insert("dev-a".to_string(), ElectionCandidatePayload {
            device_id: "dev-a".to_string(),
            uptime: 50000,
            user_designated: false,
        });

        let winner = election.select_winner().unwrap();
        assert_eq!(winner.device_id, "dev-a");
    }

    #[test]
    fn select_winner_no_candidates() {
        let (election, _rx) = make_election();
        assert!(election.select_winner().is_none());
    }

    #[tokio::test]
    async fn decide_election_become_primary() {
        let (mut election, mut rx) = make_election();
        election.configure(config("dev-1", 0, false));

        election.phase = ElectionPhase::Collecting;
        election.candidates.insert("dev-1".to_string(), ElectionCandidatePayload {
            device_id: "dev-1".to_string(),
            uptime: 100000,
            user_designated: false,
        });

        election.decide_election();

        assert!(election.is_primary());
        assert_eq!(election.phase(), ElectionPhase::Decided);

        // Should have emitted broadcast + elected events
        let mut found_elected = false;
        while let Ok(event) = rx.try_recv() {
            if let ElectionEvent::PrimaryElected { device_id, is_local } = event {
                assert_eq!(device_id, "dev-1");
                assert!(is_local);
                found_elected = true;
            }
        }
        assert!(found_elected);
    }

    #[tokio::test]
    async fn decide_election_remote_wins() {
        let (mut election, mut rx) = make_election();
        election.configure(config("dev-1", 1000, false));

        election.phase = ElectionPhase::Collecting;
        election.candidates.insert("dev-1".to_string(), ElectionCandidatePayload {
            device_id: "dev-1".to_string(),
            uptime: 50000,
            user_designated: false,
        });
        election.candidates.insert("dev-2".to_string(), ElectionCandidatePayload {
            device_id: "dev-2".to_string(),
            uptime: 100000,
            user_designated: false,
        });

        election.decide_election();

        assert!(!election.is_primary());
        assert_eq!(election.primary_id(), Some("dev-2"));

        let mut found_elected = false;
        while let Ok(event) = rx.try_recv() {
            if let ElectionEvent::PrimaryElected { device_id, is_local } = event {
                assert_eq!(device_id, "dev-2");
                assert!(!is_local);
                found_elected = true;
            }
        }
        assert!(found_elected);
    }

    #[test]
    fn handle_election_candidate() {
        let (mut election, _rx) = make_election();
        election.configure(config("dev-1", 0, false));
        election.phase = ElectionPhase::Collecting;

        let payload = ElectionCandidatePayload {
            device_id: "dev-2".to_string(),
            uptime: 75000,
            user_designated: false,
        };

        election.handle_election_candidate("dev-2", &payload);
        assert!(election.candidates.contains_key("dev-2"));
    }

    #[tokio::test]
    async fn handle_election_result() {
        let (mut election, mut rx) = make_election();
        election.configure(config("dev-1", 0, false));

        let result = ElectionResultPayload {
            new_primary_id: "dev-2".to_string(),
            previous_primary_id: None,
            reason: "election".to_string(),
        };

        election.handle_election_result("dev-2", &result);
        assert_eq!(election.primary_id(), Some("dev-2"));
        assert_eq!(election.phase(), ElectionPhase::Decided);
        assert!(!election.is_primary());

        let mut found = false;
        while let Ok(event) = rx.try_recv() {
            if let ElectionEvent::PrimaryElected { device_id, is_local } = event {
                assert_eq!(device_id, "dev-2");
                assert!(!is_local);
                found = true;
            }
        }
        assert!(found);
    }

    #[test]
    fn reset_clears_state() {
        let (mut election, _rx) = make_election();
        election.configure(config("dev-1", 0, false));
        election.set_primary("dev-1");
        election.phase = ElectionPhase::Decided;

        election.reset();
        assert!(election.primary_id().is_none());
        assert_eq!(election.phase(), ElectionPhase::Idle);
        assert!(election.candidates.is_empty());
    }

    // ── BUG-3/BUG-4 regression tests ──────────────────────────────────────

    /// handle_primary_lost() must emit PrimaryLost and set phase to Waiting.
    #[tokio::test]
    async fn handle_primary_lost_emits_primary_lost_event() {
        let (mut election, mut rx) = make_election();
        election.configure(config("dev-1", 1000, false));
        election.set_primary("dev-2");

        election.handle_primary_lost("dev-2");

        assert_eq!(election.phase(), ElectionPhase::Waiting);
        assert!(election.primary_id().is_none());

        let event = rx.try_recv().unwrap();
        match event {
            ElectionEvent::PrimaryLost { previous_primary_id } => {
                assert_eq!(previous_primary_id, "dev-2");
            }
            other => panic!("Expected PrimaryLost, got: {other:?}"),
        }
    }

    /// BUG-3: NO event with msg_type "election:timeout" should be broadcast.
    /// After the fix, the timeout should use a local-only event variant.
    #[tokio::test]
    async fn no_election_timeout_broadcast_on_wire() {
        let timing = ElectionTimingConfig {
            election_timeout: Duration::from_millis(50),
            primary_loss_grace: Duration::from_millis(50),
        };
        let (tx, mut rx) = mpsc::channel(256);
        let mut election = PrimaryElection::new(timing, tx);
        election.configure(config("dev-1", 1000, false));
        election.set_primary("old-primary");

        election.handle_primary_lost("old-primary");

        // Wait for grace period + election timeout + margin
        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut events = vec![];
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        for event in &events {
            if let ElectionEvent::Broadcast(msg) = event {
                assert_ne!(
                    msg.msg_type, "election:timeout",
                    "BUG-3: 'election:timeout' must NOT be broadcast to peers"
                );
            }
        }
    }

    /// start_election() sets phase to Collecting and adds self as candidate.
    #[tokio::test]
    async fn start_election_transitions_to_collecting() {
        let (mut election, mut rx) = make_election();
        election.configure(config("dev-1", 1000, false));
        election.phase = ElectionPhase::Waiting;

        election.start_election();

        assert_eq!(election.phase(), ElectionPhase::Collecting);
        assert!(election.candidates.contains_key("dev-1"),
            "start_election() must add self as candidate");

        let mut found_start = false;
        let mut found_candidate = false;
        let mut found_started = false;

        while let Ok(event) = rx.try_recv() {
            match &event {
                ElectionEvent::Broadcast(msg) if msg.msg_type == "election:start" => {
                    found_start = true;
                }
                ElectionEvent::Broadcast(msg) if msg.msg_type == "election:candidate" => {
                    found_candidate = true;
                }
                ElectionEvent::ElectionStarted => {
                    found_started = true;
                }
                _ => {}
            }
        }

        assert!(found_start, "Must broadcast election:start");
        assert!(found_candidate, "Must broadcast election:candidate");
        assert!(found_started, "Must emit ElectionStarted");
    }

    /// decide_election() picks winner with highest uptime from candidates.
    #[tokio::test]
    async fn decide_election_picks_correct_winner() {
        let (mut election, mut rx) = make_election();
        election.configure(config("dev-1", 1000, false));
        election.phase = ElectionPhase::Collecting;

        election.candidates.insert("dev-1".to_string(), ElectionCandidatePayload {
            device_id: "dev-1".to_string(),
            uptime: 60000,
            user_designated: false,
        });
        election.candidates.insert("dev-2".to_string(), ElectionCandidatePayload {
            device_id: "dev-2".to_string(),
            uptime: 120000,
            user_designated: false,
        });
        election.candidates.insert("dev-3".to_string(), ElectionCandidatePayload {
            device_id: "dev-3".to_string(),
            uptime: 90000,
            user_designated: false,
        });

        election.decide_election();

        assert_eq!(election.primary_id(), Some("dev-2"));
        assert_eq!(election.phase(), ElectionPhase::Decided);

        let mut found = false;
        while let Ok(event) = rx.try_recv() {
            if let ElectionEvent::PrimaryElected { device_id, is_local } = event {
                assert_eq!(device_id, "dev-2");
                assert!(!is_local);
                found = true;
            }
        }
        assert!(found);
    }

    /// handle_election_start from remote sets phase to Collecting.
    #[tokio::test]
    async fn handle_election_start_sets_collecting() {
        let (mut election, mut rx) = make_election();
        election.configure(config("dev-1", 1000, false));

        election.handle_election_start("dev-2");

        assert_eq!(election.phase(), ElectionPhase::Collecting);
        assert!(election.candidates.contains_key("dev-1"));

        let mut found_candidate = false;
        while let Ok(event) = rx.try_recv() {
            if let ElectionEvent::Broadcast(msg) = &event {
                if msg.msg_type == "election:candidate" {
                    found_candidate = true;
                }
            }
        }
        assert!(found_candidate);
    }

    /// start_election() is idempotent when already Collecting.
    #[tokio::test]
    async fn start_election_idempotent_when_collecting() {
        let (mut election, _rx) = make_election();
        election.configure(config("dev-1", 1000, false));
        election.phase = ElectionPhase::Collecting;

        election.start_election();
        assert_eq!(election.phase(), ElectionPhase::Collecting);
    }

    /// decide_election with no candidates becomes primary by default.
    #[tokio::test]
    async fn decide_election_no_candidates_becomes_primary() {
        let (mut election, mut rx) = make_election();
        election.configure(config("dev-1", 1000, false));
        election.phase = ElectionPhase::Collecting;

        election.decide_election();

        assert!(election.is_primary());
        assert_eq!(election.phase(), ElectionPhase::Decided);

        let mut found = false;
        while let Ok(event) = rx.try_recv() {
            if let ElectionEvent::PrimaryElected { device_id, is_local } = event {
                assert_eq!(device_id, "dev-1");
                assert!(is_local);
                found = true;
            }
        }
        assert!(found);
    }

    /// handle_election_result cancels pending election timeout.
    #[tokio::test]
    async fn handle_election_result_cancels_timeout() {
        let (mut election, _rx) = make_election();
        election.configure(config("dev-1", 1000, false));

        election.start_election();
        assert!(election.election_timeout_handle.is_some());

        let result = ElectionResultPayload {
            new_primary_id: "dev-2".to_string(),
            previous_primary_id: None,
            reason: "election".to_string(),
        };
        election.handle_election_result("dev-2", &result);

        assert!(election.election_timeout_handle.is_none());
        assert_eq!(election.phase(), ElectionPhase::Decided);
        assert!(election.candidates.is_empty());
    }

    /// ARCH-6: emit() on full channel does not panic.
    #[tokio::test]
    async fn emit_on_full_channel_does_not_panic() {
        let (tx, _rx) = mpsc::channel(1);
        let mut election = PrimaryElection::new(ElectionTimingConfig::default(), tx);
        election.configure(config("dev-1", 0, false));

        election.emit(ElectionEvent::ElectionStarted);
        // Should not panic when channel full
        election.emit(ElectionEvent::ElectionStarted);
    }

    /// replace_event_tx works for stop/start cycle.
    #[tokio::test]
    async fn replace_event_tx_works() {
        let (mut election, _rx1) = make_election();
        election.configure(config("dev-1", 0, false));

        let (tx2, mut rx2) = mpsc::channel(256);
        election.replace_event_tx(tx2);

        election.emit(ElectionEvent::ElectionStarted);

        let event = rx2.try_recv().unwrap();
        assert!(matches!(event, ElectionEvent::ElectionStarted));
    }

    /// Full failover: primary lost -> grace -> events emitted.
    #[tokio::test]
    async fn full_failover_emits_events() {
        let timing = ElectionTimingConfig {
            election_timeout: Duration::from_millis(50),
            primary_loss_grace: Duration::from_millis(50),
        };
        let (tx, mut rx) = mpsc::channel(256);
        let mut election = PrimaryElection::new(timing, tx);
        election.configure(config("dev-1", 1000, false));
        election.set_primary("old-primary");

        election.handle_primary_lost("old-primary");
        assert_eq!(election.phase(), ElectionPhase::Waiting);

        tokio::time::sleep(Duration::from_millis(200)).await;

        let mut events = vec![];
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }

        assert!(
            events.iter().any(|e| matches!(e, ElectionEvent::PrimaryLost { .. })),
            "Must emit PrimaryLost"
        );
    }
}
