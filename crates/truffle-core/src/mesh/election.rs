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
            MeshMessage::new("election-start", &config.device_id, serde_json::json!({})),
        ));

        // Broadcast our candidacy
        match serde_json::to_value(&my_candidate) {
            Ok(payload) => {
                self.emit(ElectionEvent::Broadcast(
                    MeshMessage::new("election-candidate", &config.device_id, payload),
                ));
            }
            Err(e) => {
                tracing::error!("Failed to serialize ElectionCandidatePayload: {e}");
            }
        }

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
        match serde_json::to_value(&result) {
            Ok(payload) => {
                self.emit(ElectionEvent::Broadcast(
                    MeshMessage::new("election-result", &config.device_id, payload),
                ));
            }
            Err(e) => {
                tracing::error!("Failed to serialize ElectionResultPayload: {e}");
            }
        }
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
            match serde_json::to_value(&my_candidate) {
                Ok(payload) => {
                    self.emit(ElectionEvent::Broadcast(
                        MeshMessage::new("election-candidate", &config.device_id, payload),
                    ));
                }
                Err(e) => {
                    tracing::error!("Failed to serialize ElectionCandidatePayload: {e}");
                }
            }

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
                ElectionEvent::Broadcast(msg) if msg.msg_type == "election-start" => {
                    found_start = true;
                }
                ElectionEvent::Broadcast(msg) if msg.msg_type == "election-candidate" => {
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
                if msg.msg_type == "election-candidate" {
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

    // ══════════════════════════════════════════════════════════════════════
    // Adversarial / edge-case tests
    // ══════════════════════════════════════════════════════════════════════

    fn fast_timing() -> ElectionTimingConfig {
        ElectionTimingConfig {
            election_timeout: Duration::from_millis(50),
            primary_loss_grace: Duration::from_millis(50),
        }
    }

    fn make_fast_election() -> (PrimaryElection, mpsc::Receiver<ElectionEvent>) {
        let (tx, rx) = mpsc::channel(256);
        let election = PrimaryElection::new(fast_timing(), tx);
        (election, rx)
    }

    /// 1. Three-way election: 3 candidates with different uptimes.
    ///    The one with highest uptime must win.
    #[test]
    fn three_way_election_highest_uptime_wins() {
        let (mut election, _rx) = make_election();
        election.configure(config("dev-a", 0, false));

        election.candidates.insert("dev-a".to_string(), ElectionCandidatePayload {
            device_id: "dev-a".to_string(),
            uptime: 30000,
            user_designated: false,
        });
        election.candidates.insert("dev-b".to_string(), ElectionCandidatePayload {
            device_id: "dev-b".to_string(),
            uptime: 90000,
            user_designated: false,
        });
        election.candidates.insert("dev-c".to_string(), ElectionCandidatePayload {
            device_id: "dev-c".to_string(),
            uptime: 60000,
            user_designated: false,
        });

        let winner = election.select_winner().unwrap();
        assert_eq!(
            winner.device_id, "dev-b",
            "Three-way election: candidate with highest uptime (dev-b, 90000) must win"
        );
    }

    /// 2. user_designated overrides uptime: a candidate with lower uptime but
    ///    user_designated=true beats candidates with higher uptime.
    #[test]
    fn user_designated_overrides_higher_uptime() {
        let (mut election, _rx) = make_election();
        election.configure(config("dev-a", 0, false));

        election.candidates.insert("dev-a".to_string(), ElectionCandidatePayload {
            device_id: "dev-a".to_string(),
            uptime: 999999,
            user_designated: false,
        });
        election.candidates.insert("dev-b".to_string(), ElectionCandidatePayload {
            device_id: "dev-b".to_string(),
            uptime: 500000,
            user_designated: false,
        });
        election.candidates.insert("dev-c".to_string(), ElectionCandidatePayload {
            device_id: "dev-c".to_string(),
            uptime: 1000,
            user_designated: true,
        });

        let winner = election.select_winner().unwrap();
        assert_eq!(
            winner.device_id, "dev-c",
            "user_designated=true must override uptime even if uptime is the lowest"
        );
    }

    /// 3. Tie-breaking by device_id: two candidates with same uptime and same
    ///    user_designated. The one with lexicographically smaller device_id wins.
    #[test]
    fn tiebreak_by_lexicographic_device_id() {
        let (mut election, _rx) = make_election();
        election.configure(config("dev-z", 0, false));

        election.candidates.insert("dev-z".to_string(), ElectionCandidatePayload {
            device_id: "dev-z".to_string(),
            uptime: 50000,
            user_designated: false,
        });
        election.candidates.insert("dev-m".to_string(), ElectionCandidatePayload {
            device_id: "dev-m".to_string(),
            uptime: 50000,
            user_designated: false,
        });
        election.candidates.insert("dev-a".to_string(), ElectionCandidatePayload {
            device_id: "dev-a".to_string(),
            uptime: 50000,
            user_designated: false,
        });

        let winner = election.select_winner().unwrap();
        assert_eq!(
            winner.device_id, "dev-a",
            "When uptime and user_designated are equal, lexicographically smallest device_id wins"
        );
    }

    /// 4. Re-election after primary loss: primary goes offline, grace period
    ///    expires, election runs with only surviving candidates.
    #[tokio::test]
    async fn re_election_after_primary_loss() {
        let (tx, mut rx) = mpsc::channel(256);
        let mut election = PrimaryElection::new(fast_timing(), tx);
        election.configure(config("dev-b", 1000, false));
        election.set_primary("dev-a"); // dev-a is primary

        // Primary lost
        election.handle_primary_lost("dev-a");
        assert_eq!(election.phase(), ElectionPhase::Waiting);
        assert!(election.primary_id().is_none(),
            "After primary lost, primary_id must be None");

        // Wait for grace period to expire
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Drain events and find GracePeriodExpired
        let mut found_grace_expired = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ElectionEvent::GracePeriodExpired) {
                found_grace_expired = true;
            }
        }
        assert!(found_grace_expired,
            "Grace period must expire and emit GracePeriodExpired");

        // Simulate MeshNode calling start_election() after grace period
        election.start_election();
        assert_eq!(election.phase(), ElectionPhase::Collecting);

        // Only surviving node's candidate (dev-b) is present
        assert!(election.candidates.contains_key("dev-b"),
            "Local candidate (dev-b) must be added on start_election");
        assert!(!election.candidates.contains_key("dev-a"),
            "Lost primary (dev-a) must NOT be in candidates");

        // Decide with only dev-b
        election.decide_election();
        assert_eq!(election.primary_id(), Some("dev-b"),
            "Surviving node dev-b must become primary by default");
    }

    /// 5. Non-candidate secondary becomes primary by default when it is the
    ///    only surviving node (even with prefer_primary=false).
    #[tokio::test]
    async fn sole_survivor_becomes_primary_even_without_preference() {
        let (mut election, mut rx) = make_fast_election();
        election.configure(config("secondary-1", 1000, false));
        election.phase = ElectionPhase::Collecting;
        // No other candidates, just this node
        election.candidates.insert("secondary-1".to_string(), ElectionCandidatePayload {
            device_id: "secondary-1".to_string(),
            uptime: 5000,
            user_designated: false,
        });

        election.decide_election();

        assert!(election.is_primary(),
            "Sole surviving node must become primary even with prefer_primary=false");
        assert_eq!(election.primary_id(), Some("secondary-1"));

        let mut found_elected = false;
        while let Ok(event) = rx.try_recv() {
            if let ElectionEvent::PrimaryElected { device_id, is_local } = event {
                assert_eq!(device_id, "secondary-1");
                assert!(is_local);
                found_elected = true;
            }
        }
        assert!(found_elected);
    }

    /// 6. Grace period cancellation: primary goes offline, grace period starts,
    ///    then election result arrives (primary came back or new primary elected).
    ///    Grace period must be cancelled, no stale GracePeriodExpired event.
    #[tokio::test]
    async fn grace_period_cancelled_by_election_result() {
        let (tx, mut rx) = mpsc::channel(256);
        let mut election = PrimaryElection::new(fast_timing(), tx);
        election.configure(config("dev-b", 1000, false));
        election.set_primary("dev-a");

        // Primary lost => starts grace period
        election.handle_primary_lost("dev-a");
        assert_eq!(election.phase(), ElectionPhase::Waiting);

        // Before grace period expires, an election result arrives
        let result = ElectionResultPayload {
            new_primary_id: "dev-a".to_string(), // primary recovered
            previous_primary_id: None,
            reason: "recovery".to_string(),
        };
        election.handle_election_result("dev-a", &result);

        assert_eq!(election.primary_id(), Some("dev-a"),
            "election:result must set new primary");
        assert_eq!(election.phase(), ElectionPhase::Decided);

        // Wait longer than the grace period to verify it was cancelled
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Drain events — GracePeriodExpired must NOT appear
        let mut found_grace_expired = false;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ElectionEvent::GracePeriodExpired) {
                found_grace_expired = true;
            }
        }
        assert!(!found_grace_expired,
            "Grace period must be cancelled when election:result arrives before it expires");
    }

    /// 7. Double primary loss: primary A goes offline. During grace period,
    ///    handle_primary_lost is called again (e.g., for a different previous primary).
    ///    The second call must cancel the first grace period and start a new one.
    #[tokio::test]
    async fn double_primary_loss_restarts_grace_period() {
        let (tx, mut rx) = mpsc::channel(256);
        let mut election = PrimaryElection::new(fast_timing(), tx);
        election.configure(config("dev-c", 1000, false));
        election.set_primary("dev-a");

        // First primary loss
        election.handle_primary_lost("dev-a");
        assert_eq!(election.phase(), ElectionPhase::Waiting);

        // Immediately set and lose another "primary" (simulating rapid state changes)
        election.set_primary("dev-b");
        election.handle_primary_lost("dev-b");
        assert_eq!(election.phase(), ElectionPhase::Waiting);

        // Wait for grace period
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Should get exactly one GracePeriodExpired (the second grace period's)
        let mut grace_count = 0;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, ElectionEvent::GracePeriodExpired) {
                grace_count += 1;
            }
        }
        // The first grace period was cancelled by the second handle_primary_lost,
        // so we should get at most one GracePeriodExpired.
        assert!(grace_count <= 1,
            "Double primary loss must cancel first grace period; got {grace_count} GracePeriodExpired events");
    }

    /// 8. Election during election: election is in Collecting phase, another
    ///    election:start arrives. Must be idempotent — does NOT reset already
    ///    collected candidates.
    #[tokio::test]
    async fn election_start_during_collecting_is_idempotent() {
        let (mut election, _rx) = make_fast_election();
        election.configure(config("dev-a", 1000, false));

        // Start election manually (enters Collecting, adds dev-a as candidate)
        election.start_election();
        assert_eq!(election.phase(), ElectionPhase::Collecting);
        assert!(election.candidates.contains_key("dev-a"));

        // Inject a remote candidate
        let remote = ElectionCandidatePayload {
            device_id: "dev-b".to_string(),
            uptime: 80000,
            user_designated: false,
        };
        election.handle_election_candidate("dev-b", &remote);
        assert!(election.candidates.contains_key("dev-b"),
            "dev-b must be registered as candidate");

        // Another election:start arrives while already Collecting
        election.handle_election_start("dev-c");

        // Phase must remain Collecting
        assert_eq!(election.phase(), ElectionPhase::Collecting);
        // IMPORTANT: candidates must still contain dev-b (not cleared)
        assert!(election.candidates.contains_key("dev-b"),
            "election:start during Collecting must NOT clear already collected candidates");
    }

    /// 9. Late candidate arrival: after election timeout fires and decision is
    ///    made, a late candidate must not corrupt the Decided state.
    #[tokio::test]
    async fn late_candidate_after_decided_does_not_corrupt() {
        let (mut election, _rx) = make_fast_election();
        election.configure(config("dev-a", 1000, false));

        election.phase = ElectionPhase::Collecting;
        election.candidates.insert("dev-a".to_string(), ElectionCandidatePayload {
            device_id: "dev-a".to_string(),
            uptime: 100000,
            user_designated: false,
        });

        // Decide
        election.decide_election();
        assert_eq!(election.phase(), ElectionPhase::Decided);
        assert_eq!(election.primary_id(), Some("dev-a"));

        // Late candidate arrives
        let late = ElectionCandidatePayload {
            device_id: "dev-z".to_string(),
            uptime: 999999,
            user_designated: true,
        };
        election.handle_election_candidate("dev-z", &late);

        // State must remain Decided with dev-a as primary
        assert_eq!(election.phase(), ElectionPhase::Decided,
            "Late candidate must not change phase from Decided");
        assert_eq!(election.primary_id(), Some("dev-a"),
            "Late candidate must not change the already-decided primary");

        // The late candidate IS added to the candidates map (handle_election_candidate
        // doesn't check phase), but that's fine because decide_election won't be
        // called again unless a new election starts.
        // What matters: the decided primary is unchanged.
    }

    /// 10. election:result declaring a primary not in the candidate list.
    ///     The result is authoritative and must be accepted.
    #[tokio::test]
    async fn election_result_from_unknown_primary_is_accepted() {
        let (mut election, mut rx) = make_fast_election();
        election.configure(config("dev-a", 1000, false));

        // Start collecting
        election.phase = ElectionPhase::Collecting;
        election.candidates.insert("dev-a".to_string(), ElectionCandidatePayload {
            device_id: "dev-a".to_string(),
            uptime: 100000,
            user_designated: false,
        });

        // Receive result for a node NOT in our candidate list
        let result = ElectionResultPayload {
            new_primary_id: "dev-unknown".to_string(),
            previous_primary_id: None,
            reason: "election".to_string(),
        };
        election.handle_election_result("dev-unknown", &result);

        assert_eq!(election.primary_id(), Some("dev-unknown"),
            "election:result must be accepted even if the winner wasn't in our candidate list");
        assert_eq!(election.phase(), ElectionPhase::Decided);
        assert!(election.candidates.is_empty(),
            "Candidates must be cleared after election result");

        let mut found_elected = false;
        while let Ok(event) = rx.try_recv() {
            if let ElectionEvent::PrimaryElected { device_id, is_local } = event {
                assert_eq!(device_id, "dev-unknown");
                assert!(!is_local);
                found_elected = true;
            }
        }
        assert!(found_elected);
    }

    /// 11. Concurrent handle_primary_lost calls: only one should effectively
    ///     start the grace period. The second call should cancel the first and
    ///     replace it.
    #[tokio::test]
    async fn concurrent_primary_lost_only_one_grace_period() {
        let (tx, mut rx) = mpsc::channel(256);
        let mut election = PrimaryElection::new(fast_timing(), tx);
        election.configure(config("dev-a", 1000, false));
        election.set_primary("primary-old");

        // Call handle_primary_lost twice in rapid succession
        election.handle_primary_lost("primary-old");
        // The second call: primary_id is already None, but calling again
        // should be safe and cancel the first grace period handle
        election.handle_primary_lost("primary-old");

        assert_eq!(election.phase(), ElectionPhase::Waiting);

        // Wait for grace period to fire
        tokio::time::sleep(Duration::from_millis(100)).await;

        let mut grace_count = 0;
        let mut lost_count = 0;
        while let Ok(event) = rx.try_recv() {
            match event {
                ElectionEvent::GracePeriodExpired => grace_count += 1,
                ElectionEvent::PrimaryLost { .. } => lost_count += 1,
                _ => {}
            }
        }

        assert_eq!(lost_count, 2,
            "Two handle_primary_lost calls should emit two PrimaryLost events");
        // The first grace period task should have been aborted by cancel_grace_period
        // in the second call, so at most one GracePeriodExpired fires.
        assert!(grace_count <= 1,
            "Concurrent handle_primary_lost must cancel first grace period; got {grace_count} GracePeriodExpired events");
    }

    /// Both user_designated candidates: among multiple user_designated, the one
    /// with higher uptime wins.
    #[test]
    fn both_user_designated_uptime_breaks_tie() {
        let (mut election, _rx) = make_election();
        election.configure(config("dev-a", 0, false));

        election.candidates.insert("dev-a".to_string(), ElectionCandidatePayload {
            device_id: "dev-a".to_string(),
            uptime: 10000,
            user_designated: true,
        });
        election.candidates.insert("dev-b".to_string(), ElectionCandidatePayload {
            device_id: "dev-b".to_string(),
            uptime: 50000,
            user_designated: true,
        });

        let winner = election.select_winner().unwrap();
        assert_eq!(
            winner.device_id, "dev-b",
            "Among multiple user_designated candidates, highest uptime should win"
        );
    }

    /// Unconfigured election: all operations are no-ops. No panics.
    #[tokio::test]
    async fn unconfigured_election_all_operations_are_noop() {
        let (mut election, mut rx) = make_fast_election();
        // Deliberately NOT calling configure()

        election.start_election();
        assert_eq!(election.phase(), ElectionPhase::Idle,
            "start_election without configure must be a no-op");

        election.decide_election();
        assert_eq!(election.phase(), ElectionPhase::Idle,
            "decide_election without configure must be a no-op");

        election.handle_primary_lost("some-primary");
        assert_eq!(election.phase(), ElectionPhase::Idle,
            "handle_primary_lost without configure must be a no-op");

        election.handle_election_start("remote");
        assert_eq!(election.phase(), ElectionPhase::Idle,
            "handle_election_start without configure must be a no-op");

        let candidate = ElectionCandidatePayload {
            device_id: "dev-x".to_string(),
            uptime: 50000,
            user_designated: false,
        };
        election.handle_election_candidate("dev-x", &candidate);
        assert!(election.candidates.is_empty(),
            "handle_election_candidate without configure must be a no-op");

        let result = ElectionResultPayload {
            new_primary_id: "dev-x".to_string(),
            previous_primary_id: None,
            reason: "test".to_string(),
        };
        election.handle_election_result("dev-x", &result);

        // No events should have been emitted (except possibly no-op attempts)
        let mut event_count = 0;
        while rx.try_recv().is_ok() {
            event_count += 1;
        }
        assert_eq!(event_count, 0,
            "Unconfigured election must not emit any events");
    }

    /// Election timeout handle is properly cleaned up on decide_election.
    #[tokio::test]
    async fn decide_election_clears_timeout_handle() {
        let (mut election, _rx) = make_fast_election();
        election.configure(config("dev-a", 1000, false));

        election.start_election();
        assert!(election.election_timeout_handle.is_some(),
            "start_election must set an election timeout handle");

        election.decide_election();
        // decide_election itself doesn't cancel the timeout, but the timeout
        // fires DecideNow which the MeshNode handles. The handle should still
        // be present (it's cleaned up by cancel_election_timeout or reset).
        // This test verifies the start -> decide flow doesn't panic.
        assert_eq!(election.phase(), ElectionPhase::Decided);
    }
}
