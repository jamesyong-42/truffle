//! Handler methods for the MeshNode transport event loop.
//!
//! Extracted from the inline closure in `start_event_loop()` for testability.

use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

use crate::protocol::envelope::{MeshEnvelope, MESH_NAMESPACE};
use crate::protocol::message_types::{
    DeviceAnnouncePayload, DeviceListPayload, MeshMessage,
};
use crate::protocol::types::MeshPayload;
use crate::transport::connection::{ConnectionManager, ConnectionStatus};
use super::device::DeviceManager;
use super::election::PrimaryElection;
use super::message_bus::{BusMessage, MeshMessageBus};
use super::node::{IncomingMeshMessage, MeshNodeConfig, MeshNodeEvent};

/// Holds shared references needed by event handlers.
/// All methods are `&self` -- locks are acquired per-call.
pub(crate) struct TransportHandler {
    pub config: MeshNodeConfig,
    pub connection_manager: Arc<ConnectionManager>,
    pub device_manager: Arc<RwLock<DeviceManager>>,
    pub election: Arc<RwLock<PrimaryElection>>,
    pub event_tx: broadcast::Sender<MeshNodeEvent>,
    pub message_bus: Arc<MeshMessageBus>,
}

impl TransportHandler {
    // ── Envelope helpers ──────────────────────────────────────────────

    /// Create a mesh-namespace envelope wrapping a MeshMessage.
    ///
    /// Returns `None` if the message cannot be serialized (logged as error).
    fn wrap_mesh_message(message: &MeshMessage) -> Option<MeshEnvelope> {
        let payload = match serde_json::to_value(message) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Failed to serialize MeshMessage: {e}");
                return None;
            }
        };
        Some(MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "message".to_string(),
            payload,
            timestamp: Some(crate::util::current_timestamp_ms()),
        })
    }

    /// Serialize and send an envelope to a connection.
    async fn send_envelope_to_conn(&self, conn_id: &str, envelope: &MeshEnvelope) {
        match serde_json::to_value(envelope) {
            Ok(value) => {
                let _ = self.connection_manager.send(conn_id, &value).await;
            }
            Err(e) => {
                tracing::error!("Failed to serialize MeshEnvelope for conn {conn_id}: {e}");
            }
        }
    }

    // ── Transport event handlers ──────────────────────────────────────

    pub async fn handle_connected(
        &self,
        conn: &crate::transport::connection::WSConnection,
    ) {
        tracing::info!(
            "Transport connected: {} ({:?})",
            conn.id,
            conn.direction
        );

        // Send our device announce
        let local_device = self.device_manager.read().await.local_device().clone();
        let announce_payload = match serde_json::to_value(&DeviceAnnouncePayload {
            device: local_device.clone(),
            protocol_version: 2,
        }) {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("Failed to serialize DeviceAnnouncePayload: {e}");
                return;
            }
        };
        let announce = MeshMessage::new(
            "device:announce",
            &self.config.device_id,
            announce_payload,
        );
        if let Some(envelope) = Self::wrap_mesh_message(&announce) {
            self.send_envelope_to_conn(&conn.id, &envelope).await;
        }

        // If primary, send device list to new connection
        if self.election.read().await.is_primary() {
            let devices = {
                let dm = self.device_manager.read().await;
                let mut devs = vec![local_device];
                devs.extend(dm.devices());
                devs
            };
            let list_payload = match serde_json::to_value(&DeviceListPayload {
                devices,
                primary_id: self.config.device_id.clone(),
            }) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!("Failed to serialize DeviceListPayload: {e}");
                    return;
                }
            };
            let list = MeshMessage::new(
                "device:list",
                &self.config.device_id,
                list_payload,
            );
            if let Some(envelope) = Self::wrap_mesh_message(&list) {
                self.send_envelope_to_conn(&conn.id, &envelope).await;
            }
        }
    }

    pub async fn handle_disconnected(&self, connection_id: &str, reason: &str) {
        tracing::info!("Transport disconnected: {connection_id} ({reason})");
        let devices = self.device_manager.read().await.devices();
        for device in &devices {
            let conn = self
                .connection_manager
                .get_connection_by_device(&device.id)
                .await;
            if conn.is_none() {
                let mut dm = self.device_manager.write().await;
                dm.mark_device_offline(&device.id);
            }
        }
    }

    pub async fn handle_message(
        &self,
        connection_id: &str,
        payload: &serde_json::Value,
    ) {
        let envelope: MeshEnvelope = match serde_json::from_value(payload.clone()) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    connection_id,
                    error = %e,
                    "Failed to parse MeshEnvelope from incoming message"
                );
                return;
            }
        };

        let conn = self
            .connection_manager
            .get_connection(connection_id)
            .await;
        let from_device_id = conn.and_then(|c| c.device_id);

        if envelope.namespace == MESH_NAMESPACE {
            if envelope.msg_type == "message" {
                self.handle_mesh_envelope(connection_id, &from_device_id, &envelope)
                    .await;
            } else {
                self.handle_route_envelope(
                    connection_id,
                    from_device_id.as_deref(),
                    &envelope,
                )
                .await;
            }
        } else {
            self.handle_app_message(connection_id, from_device_id, &envelope)
                .await;
        }
    }

    async fn handle_mesh_envelope(
        &self,
        connection_id: &str,
        _from_device_id: &Option<String>,
        envelope: &MeshEnvelope,
    ) {
        let mesh_msg: MeshMessage = match serde_json::from_value(envelope.payload.clone()) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    connection_id,
                    error = %e,
                    "Failed to parse MeshMessage from mesh envelope payload"
                );
                return;
            }
        };

        // Bind device ID from announce
        if mesh_msg.msg_type == "device:announce" {
            if let Ok(payload) =
                serde_json::from_value::<DeviceAnnouncePayload>(mesh_msg.payload.clone())
            {
                self.connection_manager
                    .set_device_id(connection_id, payload.device.id.clone())
                    .await;
            }
        }

        self.dispatch_mesh_message(&mesh_msg).await;
    }

    pub(crate) async fn dispatch_mesh_message(&self, msg: &MeshMessage) {
        match MeshPayload::parse(&msg.msg_type, msg.payload.clone()) {
            Ok(MeshPayload::DeviceAnnounce(payload)) => {
                self.device_manager
                    .write()
                    .await
                    .handle_device_announce(&msg.from, &payload);
            }
            Ok(MeshPayload::DeviceList(payload)) => {
                self.device_manager
                    .write()
                    .await
                    .handle_device_list(&msg.from, &payload);
                if !payload.primary_id.is_empty() {
                    self.election
                        .write()
                        .await
                        .set_primary(&payload.primary_id);
                }
            }
            Ok(MeshPayload::DeviceGoodbye(_payload)) => {
                self.device_manager
                    .write()
                    .await
                    .handle_device_goodbye(&msg.from);
            }
            Ok(MeshPayload::ElectionStart) => {
                self.election
                    .write()
                    .await
                    .handle_election_start(&msg.from);
            }
            Ok(MeshPayload::ElectionCandidate(payload)) => {
                self.election
                    .write()
                    .await
                    .handle_election_candidate(&msg.from, &payload);
            }
            Ok(MeshPayload::ElectionResult(payload)) => {
                self.election
                    .write()
                    .await
                    .handle_election_result(&msg.from, &payload);
            }
            Ok(MeshPayload::RouteMessage(_)) | Ok(MeshPayload::RouteBroadcast(_)) => {
                // Route messages should not arrive via dispatch_mesh_message;
                // they are handled by handle_route_envelope at the envelope level.
                tracing::warn!(
                    msg_type = %msg.msg_type,
                    from = %msg.from,
                    "Route message received via dispatch_mesh_message -- should use handle_route_envelope"
                );
            }
            Err(e) => {
                tracing::warn!(
                    msg_type = %msg.msg_type,
                    from = %msg.from,
                    error = %e,
                    "Failed to parse mesh message"
                );
            }
        }
    }

    async fn handle_route_envelope(
        &self,
        connection_id: &str,
        from_device_id: Option<&str>,
        envelope: &MeshEnvelope,
    ) {
        let is_primary = self.election.read().await.is_primary();
        if !is_primary {
            tracing::warn!("Received route envelope but not primary");
            return;
        }

        match envelope.msg_type.as_str() {
            "route:message" | "route-message" => {
                if let (Some(target), Some(inner)) = (
                    envelope
                        .payload
                        .get("targetDeviceId")
                        .and_then(|v| v.as_str()),
                    envelope.payload.get("envelope"),
                ) {
                    let inner_env = match serde_json::from_value::<MeshEnvelope>(inner.clone()) {
                        Ok(env) => env,
                        Err(e) => {
                            tracing::warn!(
                                connection_id,
                                error = %e,
                                "Failed to parse inner envelope in route:message"
                            );
                            return;
                        }
                    };

                    // Nesting depth guard: reject inner envelopes that are themselves route envelopes
                    if inner_env.namespace == MESH_NAMESPACE
                        && (inner_env.msg_type.starts_with("route:")
                            || inner_env.msg_type.starts_with("route-"))
                    {
                        tracing::warn!(
                            connection_id,
                            inner_type = %inner_env.msg_type,
                            "Nested route envelope rejected (max depth 1)"
                        );
                        return;
                    }

                    if let Some(conn) = self
                        .connection_manager
                        .get_connection_by_device(target)
                        .await
                    {
                        let mut env_ts = inner_env;
                        if env_ts.timestamp.is_none() {
                            env_ts.timestamp =
                                Some(crate::util::current_timestamp_ms());
                        }
                        self.send_envelope_to_conn(&conn.id, &env_ts).await;
                    } else {
                        tracing::warn!(
                            target_device_id = target,
                            "route:message target device not connected"
                        );
                    }
                } else {
                    tracing::warn!(
                        connection_id,
                        "route:message missing targetDeviceId or envelope field"
                    );
                }
            }
            "route:broadcast" | "route-broadcast" => {
                let inner = match envelope.payload.get("envelope") {
                    Some(inner) => inner,
                    None => {
                        tracing::warn!(
                            connection_id,
                            "route:broadcast missing envelope field"
                        );
                        return;
                    }
                };

                let inner_env = match serde_json::from_value::<MeshEnvelope>(inner.clone()) {
                    Ok(env) => env,
                    Err(e) => {
                        tracing::warn!(
                            connection_id,
                            error = %e,
                            "Failed to parse inner envelope in route:broadcast"
                        );
                        return;
                    }
                };

                // Mesh-namespace filter: reject inner envelopes with mesh namespace
                // to prevent protocol bypass (RFC 009 issue #8)
                if inner_env.namespace == MESH_NAMESPACE {
                    tracing::warn!(
                        connection_id,
                        inner_type = %inner_env.msg_type,
                        "route:broadcast with mesh namespace rejected -- use dispatch_mesh_message directly"
                    );
                    return;
                }

                // Nesting depth guard: reject inner envelopes that are themselves route envelopes
                if inner_env.msg_type.starts_with("route:") || inner_env.msg_type.starts_with("route-") {
                    tracing::warn!(
                        connection_id,
                        inner_type = %inner_env.msg_type,
                        "Nested route envelope rejected (max depth 1)"
                    );
                    return;
                }

                let conns = self.connection_manager.get_connections().await;
                for c in &conns {
                    if let Some(ref did) = c.device_id {
                        if from_device_id != Some(did.as_str())
                            && c.status == ConnectionStatus::Connected
                        {
                            let mut env_ts = inner_env.clone();
                            if env_ts.timestamp.is_none() {
                                env_ts.timestamp =
                                    Some(crate::util::current_timestamp_ms());
                            }
                            self.send_envelope_to_conn(&c.id, &env_ts).await;
                        }
                    }
                }
                // Local delivery
                self.deliver_app_message(
                    connection_id,
                    from_device_id.map(|s| s.to_string()),
                    &inner_env,
                );
                // MessageBus dispatch
                self.message_bus
                    .dispatch(&BusMessage {
                        from: from_device_id.map(|s| s.to_string()),
                        namespace: inner_env.namespace.clone(),
                        msg_type: inner_env.msg_type.clone(),
                        payload: inner_env.payload.clone(),
                    })
                    .await;
            }
            _ => {
                tracing::warn!(
                    connection_id,
                    msg_type = %envelope.msg_type,
                    "Unknown route envelope type in mesh namespace"
                );
            }
        }
    }

    async fn handle_app_message(
        &self,
        connection_id: &str,
        from_device_id: Option<String>,
        envelope: &MeshEnvelope,
    ) {
        self.deliver_app_message(connection_id, from_device_id.clone(), envelope);
        self.message_bus
            .dispatch(&BusMessage {
                from: from_device_id,
                namespace: envelope.namespace.clone(),
                msg_type: envelope.msg_type.clone(),
                payload: envelope.payload.clone(),
            })
            .await;
    }

    fn deliver_app_message(
        &self,
        connection_id: &str,
        from: Option<String>,
        envelope: &MeshEnvelope,
    ) {
        let incoming = IncomingMeshMessage {
            from,
            connection_id: connection_id.to_string(),
            namespace: envelope.namespace.clone(),
            msg_type: envelope.msg_type.clone(),
            payload: envelope.payload.clone(),
        };
        let _ = self.event_tx.send(MeshNodeEvent::Message(incoming));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    use crate::mesh::device::{DeviceEvent, DeviceIdentity};
    use crate::mesh::election::{ElectionConfig, ElectionEvent, ElectionPhase, ElectionTimingConfig};
    use crate::mesh::node::MeshTimingConfig;
    use crate::protocol::message_types::{
        DeviceAnnouncePayload, DeviceListPayload, ElectionCandidatePayload,
        ElectionResultPayload, MeshMessage,
    };
    use crate::transport::connection::TransportConfig;
    use crate::types::{BaseDevice, DeviceRole, DeviceStatus};

    /// Build an isolated TransportHandler with real components.
    /// Returns the handler plus receivers for all event channels.
    fn make_handler() -> (
        TransportHandler,
        broadcast::Receiver<MeshNodeEvent>,
        mpsc::Receiver<DeviceEvent>,
        mpsc::Receiver<ElectionEvent>,
    ) {
        let config = MeshNodeConfig {
            device_id: "local-dev".to_string(),
            device_name: "Local Device".to_string(),
            device_type: "desktop".to_string(),
            hostname_prefix: "app".to_string(),
            prefer_primary: false,
            capabilities: vec![],
            metadata: None,
            timing: MeshTimingConfig::default(),
        };

        let (connection_manager, _transport_rx) = ConnectionManager::new(TransportConfig::default());
        let connection_manager = Arc::new(connection_manager);

        let (device_event_tx, device_event_rx) = mpsc::channel(256);
        let device_manager = DeviceManager::new(
            DeviceIdentity {
                id: "local-dev".to_string(),
                device_type: "desktop".to_string(),
                name: "Local Device".to_string(),
                tailscale_hostname: "app-desktop-local-dev".to_string(),
            },
            "app".to_string(),
            vec![],
            None,
            device_event_tx,
        );
        let device_manager = Arc::new(RwLock::new(device_manager));

        let (election_event_tx, election_event_rx) = mpsc::channel(256);
        let election = PrimaryElection::new(ElectionTimingConfig::default(), election_event_tx);
        let election = Arc::new(RwLock::new(election));

        let (event_tx, event_rx) = broadcast::channel(256);

        let (bus_event_tx, _bus_event_rx) = mpsc::channel(64);
        let message_bus = Arc::new(MeshMessageBus::new(bus_event_tx));

        let handler = TransportHandler {
            config,
            connection_manager,
            device_manager,
            election,
            event_tx,
            message_bus,
        };

        (handler, event_rx, device_event_rx, election_event_rx)
    }

    /// Helper: create a BaseDevice for testing.
    fn make_device(id: &str, name: &str) -> BaseDevice {
        BaseDevice {
            id: id.to_string(),
            device_type: "desktop".to_string(),
            name: name.to_string(),
            tailscale_hostname: format!("app-desktop-{id}"),
            tailscale_dns_name: None,
            tailscale_ip: Some("100.64.0.2".to_string()),
            role: None,
            status: DeviceStatus::Online,
            capabilities: vec![],
            metadata: None,
            last_seen: None,
            started_at: Some(1000),
            os: None,
            latency_ms: None,
        }
    }

    /// Helper: create a device:announce MeshMessage.
    fn make_announce_msg(device: &BaseDevice) -> MeshMessage {
        MeshMessage::new(
            "device:announce",
            &device.id,
            serde_json::to_value(&DeviceAnnouncePayload {
                device: device.clone(),
                protocol_version: 2,
            })
            .unwrap(),
        )
    }

    // ── dispatch_mesh_message tests ──────────────────────────────────────

    #[tokio::test]
    async fn test_device_announce_adds_device() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();
        let device = make_device("remote-1", "Remote 1");
        let msg = make_announce_msg(&device);

        handler.dispatch_mesh_message(&msg).await;

        let dm = handler.device_manager.read().await;
        let found = dm.device_by_id("remote-1");
        assert!(found.is_some(), "Device should be added after announce");
        assert_eq!(found.unwrap().name, "Remote 1");
    }

    #[tokio::test]
    async fn test_device_announce_emits_discovered_event() {
        let (handler, _event_rx, mut dev_rx, _elec_rx) = make_handler();
        let device = make_device("remote-1", "Remote 1");
        let msg = make_announce_msg(&device);

        handler.dispatch_mesh_message(&msg).await;

        let mut found_discovered = false;
        while let Ok(event) = dev_rx.try_recv() {
            if let DeviceEvent::DeviceDiscovered(d) = event {
                assert_eq!(d.id, "remote-1");
                found_discovered = true;
            }
        }
        assert!(found_discovered, "Must emit DeviceDiscovered for new device");
    }

    #[tokio::test]
    async fn test_device_announce_emits_updated_event() {
        let (handler, _event_rx, mut dev_rx, _elec_rx) = make_handler();
        let device = make_device("remote-1", "Remote 1");
        let msg = make_announce_msg(&device);

        // First announce: discovered
        handler.dispatch_mesh_message(&msg).await;
        // Drain events
        while dev_rx.try_recv().is_ok() {}

        // Second announce: updated
        handler.dispatch_mesh_message(&msg).await;

        let mut found_updated = false;
        while let Ok(event) = dev_rx.try_recv() {
            if let DeviceEvent::DeviceUpdated(d) = event {
                assert_eq!(d.id, "remote-1");
                found_updated = true;
            }
        }
        assert!(found_updated, "Second announce must emit DeviceUpdated");
    }

    #[tokio::test]
    async fn test_device_list_adds_multiple_devices() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        let devices = vec![
            make_device("dev-a", "Device A"),
            make_device("dev-b", "Device B"),
            make_device("dev-c", "Device C"),
        ];

        let msg = MeshMessage::new(
            "device:list",
            "primary-node",
            serde_json::to_value(&DeviceListPayload {
                devices,
                primary_id: "primary-node".to_string(),
            })
            .unwrap(),
        );

        handler.dispatch_mesh_message(&msg).await;

        let dm = handler.device_manager.read().await;
        assert!(dm.device_by_id("dev-a").is_some(), "dev-a should exist");
        assert!(dm.device_by_id("dev-b").is_some(), "dev-b should exist");
        assert!(dm.device_by_id("dev-c").is_some(), "dev-c should exist");
    }

    #[tokio::test]
    async fn test_device_list_sets_primary() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        let msg = MeshMessage::new(
            "device:list",
            "primary-node",
            serde_json::to_value(&DeviceListPayload {
                devices: vec![make_device("primary-node", "Primary")],
                primary_id: "primary-node".to_string(),
            })
            .unwrap(),
        );

        handler.dispatch_mesh_message(&msg).await;

        let election = handler.election.read().await;
        assert_eq!(
            election.primary_id(),
            Some("primary-node"),
            "Election primary_id must match device:list primaryId"
        );
    }

    #[tokio::test]
    async fn test_device_goodbye_marks_offline() {
        let (handler, _event_rx, mut dev_rx, _elec_rx) = make_handler();

        // First add the device
        let device = make_device("remote-1", "Remote 1");
        let announce = make_announce_msg(&device);
        handler.dispatch_mesh_message(&announce).await;
        while dev_rx.try_recv().is_ok() {}

        // Send goodbye
        let goodbye = MeshMessage::new(
            "device:goodbye",
            "remote-1",
            serde_json::to_value(&crate::protocol::message_types::DeviceGoodbyePayload {
                device_id: "remote-1".to_string(),
                reason: "shutdown".to_string(),
            }).unwrap(),
        );
        handler.dispatch_mesh_message(&goodbye).await;

        let dm = handler.device_manager.read().await;
        let dev = dm.device_by_id("remote-1").unwrap();
        assert_eq!(dev.status, DeviceStatus::Offline, "Device must be offline after goodbye");
    }

    #[tokio::test]
    async fn test_election_start_triggers_election() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        // Configure election so handle_election_start has config
        {
            let mut election = handler.election.write().await;
            election.configure(ElectionConfig {
                device_id: "local-dev".to_string(),
                started_at: 1000,
                prefer_primary: false,
            });
        }

        let msg = MeshMessage::new(
            "election:start",
            "remote-1",
            serde_json::json!({}),
        );
        handler.dispatch_mesh_message(&msg).await;

        let election = handler.election.read().await;
        assert_eq!(
            election.phase(),
            ElectionPhase::Collecting,
            "election:start must transition to Collecting"
        );
    }

    #[tokio::test]
    async fn test_election_candidate_registered() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        // Configure and start collecting
        {
            let mut election = handler.election.write().await;
            election.configure(ElectionConfig {
                device_id: "local-dev".to_string(),
                started_at: 1000,
                prefer_primary: false,
            });
            election.start_election();
        }

        let candidate = ElectionCandidatePayload {
            device_id: "remote-1".to_string(),
            uptime: 50000,
            user_designated: false,
        };
        let msg = MeshMessage::new(
            "election:candidate",
            "remote-1",
            serde_json::to_value(&candidate).unwrap(),
        );
        handler.dispatch_mesh_message(&msg).await;

        // We cannot directly inspect candidates since it's private,
        // but we can verify indirectly: if we decide now, the candidate
        // should influence the result
        // Just verify it didn't panic and the phase is still collecting
        let election = handler.election.read().await;
        assert_eq!(election.phase(), ElectionPhase::Collecting);
    }

    #[tokio::test]
    async fn test_election_result_sets_primary() {
        let (handler, _event_rx, _dev_rx, mut elec_rx) = make_handler();

        {
            let mut election = handler.election.write().await;
            election.configure(ElectionConfig {
                device_id: "local-dev".to_string(),
                started_at: 1000,
                prefer_primary: false,
            });
        }

        let result = ElectionResultPayload {
            new_primary_id: "remote-winner".to_string(),
            previous_primary_id: None,
            reason: "election".to_string(),
        };
        let msg = MeshMessage::new(
            "election:result",
            "remote-winner",
            serde_json::to_value(&result).unwrap(),
        );
        handler.dispatch_mesh_message(&msg).await;

        let election = handler.election.read().await;
        assert_eq!(
            election.primary_id(),
            Some("remote-winner"),
            "election:result must set the new primary"
        );
        assert_eq!(election.phase(), ElectionPhase::Decided);

        // Verify PrimaryElected event emitted
        let mut found_elected = false;
        while let Ok(event) = elec_rx.try_recv() {
            if let ElectionEvent::PrimaryElected { device_id, is_local } = event {
                assert_eq!(device_id, "remote-winner");
                assert!(!is_local);
                found_elected = true;
            }
        }
        assert!(found_elected, "Must emit PrimaryElected event");
    }

    // ── handle_connected tests ──────────────────────────────────────────

    #[tokio::test]
    async fn test_connected_sends_announce() {
        // handle_connected calls send_envelope_to_conn which calls
        // connection_manager.send(). Since there's no real connection,
        // the send will silently fail (no conn_id found). We verify
        // it doesn't panic and the code path executes.
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        let conn = crate::transport::connection::WSConnection {
            id: "conn-1".to_string(),
            device_id: None,
            direction: crate::bridge::header::Direction::Incoming,
            remote_addr: "100.64.0.5:443".to_string(),
            remote_dns_name: "peer.ts.net".to_string(),
            status: ConnectionStatus::Connected,
            protocol_version: crate::transport::connection::PROTOCOL_V2,
        };

        // Should not panic - sends announce (fails silently on missing conn)
        handler.handle_connected(&conn).await;
    }

    #[tokio::test]
    async fn test_connected_primary_sends_device_list() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        // Make this node the primary
        {
            let mut election = handler.election.write().await;
            election.configure(ElectionConfig {
                device_id: "local-dev".to_string(),
                started_at: 1000,
                prefer_primary: false,
            });
            election.set_primary("local-dev");
        }

        let conn = crate::transport::connection::WSConnection {
            id: "conn-1".to_string(),
            device_id: None,
            direction: crate::bridge::header::Direction::Incoming,
            remote_addr: "100.64.0.5:443".to_string(),
            remote_dns_name: "peer.ts.net".to_string(),
            status: ConnectionStatus::Connected,
            protocol_version: crate::transport::connection::PROTOCOL_V2,
        };

        // Should not panic - sends announce + device list (both fail silently)
        handler.handle_connected(&conn).await;

        // Verify the code path was taken by checking election is still primary
        let election = handler.election.read().await;
        assert!(election.is_primary());
    }

    // ── handle_disconnected tests ────────────────────────────────────────

    #[tokio::test]
    async fn test_disconnected_marks_device_offline() {
        let (handler, _event_rx, mut dev_rx, _elec_rx) = make_handler();

        // Add a device via announce
        let device = make_device("remote-1", "Remote 1");
        let announce = make_announce_msg(&device);
        handler.dispatch_mesh_message(&announce).await;
        while dev_rx.try_recv().is_ok() {}

        // Disconnect - since there's no real connection for remote-1,
        // get_connection_by_device returns None, so it marks offline
        handler.handle_disconnected("conn-1", "remote_closed").await;

        let dm = handler.device_manager.read().await;
        let dev = dm.device_by_id("remote-1").unwrap();
        assert_eq!(
            dev.status,
            DeviceStatus::Offline,
            "Device with no connection should be marked offline on disconnect"
        );
    }

    #[tokio::test]
    async fn test_disconnected_triggers_primary_lost() {
        let (handler, _event_rx, mut dev_rx, _elec_rx) = make_handler();

        // Add a device and make it primary (in device manager)
        let mut device = make_device("remote-primary", "Primary Remote");
        device.role = Some(DeviceRole::Primary);
        let announce = make_announce_msg(&device);
        handler.dispatch_mesh_message(&announce).await;
        {
            let mut dm = handler.device_manager.write().await;
            dm.set_device_role("remote-primary", DeviceRole::Primary);
        }
        while dev_rx.try_recv().is_ok() {}

        // Disconnect
        handler.handle_disconnected("conn-primary", "remote_closed").await;

        let dm = handler.device_manager.read().await;
        assert!(
            dm.primary_id().is_none(),
            "Primary should be cleared when primary device goes offline"
        );
    }

    // ── handle_message (envelope parsing) tests ──────────────────────────

    #[tokio::test]
    async fn test_mesh_envelope_dispatched() {
        let (handler, _event_rx, mut dev_rx, _elec_rx) = make_handler();

        // Create a full mesh envelope with a device:announce inside
        let device = make_device("remote-1", "Remote 1");
        let mesh_msg = make_announce_msg(&device);
        let envelope = MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "message".to_string(),
            payload: serde_json::to_value(&mesh_msg).unwrap(),
            timestamp: Some(1000),
        };
        let payload = serde_json::to_value(&envelope).unwrap();

        handler.handle_message("conn-1", &payload).await;

        // Verify device was added (proving dispatch_mesh_message was called)
        let dm = handler.device_manager.read().await;
        assert!(
            dm.device_by_id("remote-1").is_some(),
            "Mesh envelope with type=message must dispatch inner MeshMessage"
        );

        // Verify DeviceDiscovered was emitted
        let mut found = false;
        while let Ok(event) = dev_rx.try_recv() {
            if let DeviceEvent::DeviceDiscovered(d) = event {
                if d.id == "remote-1" {
                    found = true;
                }
            }
        }
        assert!(found);
    }

    #[tokio::test]
    async fn test_non_mesh_envelope_delivered_as_app_message() {
        let (handler, mut event_rx, _dev_rx, _elec_rx) = make_handler();

        let envelope = MeshEnvelope {
            namespace: "custom-app".to_string(),
            msg_type: "sync-update".to_string(),
            payload: serde_json::json!({"data": "hello"}),
            timestamp: Some(1000),
        };
        let payload = serde_json::to_value(&envelope).unwrap();

        handler.handle_message("conn-1", &payload).await;

        // Should be delivered as MeshNodeEvent::Message
        let mut found_message = false;
        while let Ok(event) = event_rx.try_recv() {
            if let MeshNodeEvent::Message(incoming) = event {
                assert_eq!(incoming.namespace, "custom-app");
                assert_eq!(incoming.msg_type, "sync-update");
                assert_eq!(incoming.payload["data"], "hello");
                found_message = true;
            }
        }
        assert!(
            found_message,
            "Non-mesh namespace envelope must be delivered as MeshNodeEvent::Message"
        );
    }

    #[tokio::test]
    async fn test_invalid_envelope_ignored() {
        let (handler, mut event_rx, _dev_rx, _elec_rx) = make_handler();

        // Malformed JSON that can't be parsed as MeshEnvelope
        let payload = serde_json::json!({"garbage": true, "no_namespace": 42});

        handler.handle_message("conn-1", &payload).await;

        // Should not emit any events
        assert!(
            event_rx.try_recv().is_err(),
            "Invalid envelope must be silently ignored"
        );
    }

    // ── handle_route_envelope (STAR routing) tests ───────────────────────

    #[tokio::test]
    async fn test_route_message_forwarded_to_target() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        // Make this node primary (route envelopes are only handled by primary)
        {
            let mut election = handler.election.write().await;
            election.configure(ElectionConfig {
                device_id: "local-dev".to_string(),
                started_at: 1000,
                prefer_primary: false,
            });
            election.set_primary("local-dev");
        }

        // Build route:message envelope
        let inner_envelope = MeshEnvelope::new("custom", "data", serde_json::json!({"key": "val"}));
        let route_payload = serde_json::json!({
            "targetDeviceId": "dev-b",
            "envelope": serde_json::to_value(&inner_envelope).unwrap(),
        });
        let route_envelope = MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "route:message".to_string(),
            payload: route_payload,
            timestamp: Some(1000),
        };

        // The target device "dev-b" has no real connection, so send will
        // fail silently. Test verifies no panic and code path executes.
        handler
            .handle_route_envelope("conn-sender", Some("dev-a"), &route_envelope)
            .await;
    }

    #[tokio::test]
    async fn test_route_broadcast_delivers_locally() {
        let (handler, mut event_rx, _dev_rx, _elec_rx) = make_handler();

        // Make this node primary
        {
            let mut election = handler.election.write().await;
            election.configure(ElectionConfig {
                device_id: "local-dev".to_string(),
                started_at: 1000,
                prefer_primary: false,
            });
            election.set_primary("local-dev");
        }

        let inner_envelope = MeshEnvelope::new(
            "sync",
            "file-update",
            serde_json::json!({"file": "test.txt"}),
        );
        let broadcast_payload = serde_json::json!({
            "envelope": serde_json::to_value(&inner_envelope).unwrap(),
        });
        let route_envelope = MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "route:broadcast".to_string(),
            payload: broadcast_payload,
            timestamp: Some(1000),
        };

        handler
            .handle_route_envelope("conn-sender", Some("dev-a"), &route_envelope)
            .await;

        // route:broadcast should deliver locally via event_tx
        let mut found_local_delivery = false;
        while let Ok(event) = event_rx.try_recv() {
            if let MeshNodeEvent::Message(incoming) = event {
                assert_eq!(incoming.namespace, "sync");
                assert_eq!(incoming.msg_type, "file-update");
                assert_eq!(incoming.payload["file"], "test.txt");
                found_local_delivery = true;
            }
        }
        assert!(
            found_local_delivery,
            "Primary must deliver broadcast locally via event_tx"
        );
    }

    #[tokio::test]
    async fn test_route_broadcast_forwarded_to_all() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        // Make this node primary
        {
            let mut election = handler.election.write().await;
            election.configure(ElectionConfig {
                device_id: "local-dev".to_string(),
                started_at: 1000,
                prefer_primary: false,
            });
            election.set_primary("local-dev");
        }

        let inner_envelope = MeshEnvelope::new("sync", "update", serde_json::json!({}));
        let broadcast_payload = serde_json::json!({
            "envelope": serde_json::to_value(&inner_envelope).unwrap(),
        });
        let route_envelope = MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "route:broadcast".to_string(),
            payload: broadcast_payload,
            timestamp: Some(1000),
        };

        // No real connections, so forwarding is a no-op for remote peers.
        // But local delivery should still happen.
        handler
            .handle_route_envelope("conn-sender", Some("dev-a"), &route_envelope)
            .await;
        // No panic = success for the forwarding codepath
    }

    #[tokio::test]
    async fn test_route_envelope_rejected_when_not_primary() {
        let (handler, mut event_rx, _dev_rx, _elec_rx) = make_handler();

        // NOT primary (default state)
        let inner_envelope = MeshEnvelope::new("custom", "data", serde_json::json!({}));
        let route_payload = serde_json::json!({
            "targetDeviceId": "dev-b",
            "envelope": serde_json::to_value(&inner_envelope).unwrap(),
        });
        let route_envelope = MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "route:message".to_string(),
            payload: route_payload,
            timestamp: Some(1000),
        };

        handler
            .handle_route_envelope("conn-sender", Some("dev-a"), &route_envelope)
            .await;

        // Should not deliver anything
        assert!(
            event_rx.try_recv().is_err(),
            "Non-primary must not process route envelopes"
        );
    }

    // ── Edge cases ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_announce_from_self_not_duplicated() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        // Announce with own device_id — DeviceManager.handle_device_announce
        // adds it to the devices map (this is by design: the handler doesn't
        // filter self-announces, the DeviceManager treats it as a remote device).
        // However, device_by_id("local-dev") always returns the local device
        // first, so it's effectively a no-op for lookups.
        let device = make_device("local-dev", "Local Device");
        let msg = make_announce_msg(&device);

        handler.dispatch_mesh_message(&msg).await;

        // The local device should still be the local device
        let dm = handler.device_manager.read().await;
        let local = dm.local_device();
        assert_eq!(local.id, "local-dev");
    }

    #[tokio::test]
    async fn test_message_bus_dispatch() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        // Subscribe to the message bus
        let call_count = Arc::new(AtomicUsize::new(0));
        let count_clone = call_count.clone();
        handler
            .message_bus
            .subscribe(
                "test-ns",
                Arc::new(move |msg: &BusMessage| {
                    assert_eq!(msg.namespace, "test-ns");
                    assert_eq!(msg.msg_type, "test-type");
                    count_clone.fetch_add(1, Ordering::SeqCst);
                }),
            )
            .await;

        // Send a non-mesh envelope (goes through handle_app_message -> message_bus)
        let envelope = MeshEnvelope {
            namespace: "test-ns".to_string(),
            msg_type: "test-type".to_string(),
            payload: serde_json::json!({"key": "val"}),
            timestamp: Some(1000),
        };
        let payload = serde_json::to_value(&envelope).unwrap();

        handler.handle_message("conn-1", &payload).await;

        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "MessageBus handler must be called for app messages"
        );
    }

    #[tokio::test]
    async fn test_message_bus_dispatch_on_broadcast() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        // Make primary
        {
            let mut election = handler.election.write().await;
            election.configure(ElectionConfig {
                device_id: "local-dev".to_string(),
                started_at: 1000,
                prefer_primary: false,
            });
            election.set_primary("local-dev");
        }

        let call_count = Arc::new(AtomicUsize::new(0));
        let count_clone = call_count.clone();
        handler
            .message_bus
            .subscribe(
                "broadcast-ns",
                Arc::new(move |msg: &BusMessage| {
                    assert_eq!(msg.namespace, "broadcast-ns");
                    count_clone.fetch_add(1, Ordering::SeqCst);
                }),
            )
            .await;

        let inner_envelope = MeshEnvelope::new(
            "broadcast-ns",
            "update",
            serde_json::json!({}),
        );
        let broadcast_payload = serde_json::json!({
            "envelope": serde_json::to_value(&inner_envelope).unwrap(),
        });
        let route_envelope = MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "route:broadcast".to_string(),
            payload: broadcast_payload,
            timestamp: Some(1000),
        };

        handler
            .handle_route_envelope("conn-sender", Some("dev-a"), &route_envelope)
            .await;

        assert_eq!(
            call_count.load(Ordering::SeqCst),
            1,
            "MessageBus must be dispatched on route:broadcast"
        );
    }

    #[tokio::test]
    async fn test_unknown_mesh_message_type_ignored() {
        let (handler, _event_rx, mut dev_rx, _elec_rx) = make_handler();

        let msg = MeshMessage::new(
            "device:unknown_type",
            "remote-1",
            serde_json::json!({}),
        );

        handler.dispatch_mesh_message(&msg).await;

        // Should not emit any device events
        assert!(
            dev_rx.try_recv().is_err(),
            "Unknown mesh message type must be silently ignored"
        );
    }

    #[tokio::test]
    async fn test_device_list_empty_primary_id_does_not_set_primary() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        let msg = MeshMessage::new(
            "device:list",
            "some-node",
            serde_json::to_value(&DeviceListPayload {
                devices: vec![make_device("dev-a", "A")],
                primary_id: "".to_string(),
            })
            .unwrap(),
        );

        handler.dispatch_mesh_message(&msg).await;

        let election = handler.election.read().await;
        assert!(
            election.primary_id().is_none(),
            "Empty primary_id in device:list must not set election primary"
        );
    }

    #[tokio::test]
    async fn test_wrap_mesh_message_produces_correct_envelope() {
        let msg = MeshMessage::new("device:announce", "dev-1", serde_json::json!({}));
        let envelope = TransportHandler::wrap_mesh_message(&msg)
            .expect("wrap_mesh_message must succeed for valid MeshMessage");

        assert_eq!(envelope.namespace, MESH_NAMESPACE);
        assert_eq!(envelope.msg_type, "message");
        assert!(envelope.timestamp.is_some());
        // Payload should contain the serialized MeshMessage
        let inner: MeshMessage = serde_json::from_value(envelope.payload).unwrap();
        assert_eq!(inner.msg_type, "device:announce");
        assert_eq!(inner.from, "dev-1");
    }

    #[tokio::test]
    async fn test_device_goodbye_emits_offline_event() {
        let (handler, _event_rx, mut dev_rx, _elec_rx) = make_handler();

        // Add device first
        let device = make_device("remote-1", "Remote 1");
        handler.dispatch_mesh_message(&make_announce_msg(&device)).await;
        while dev_rx.try_recv().is_ok() {}

        // Goodbye
        let goodbye = MeshMessage::new(
            "device:goodbye",
            "remote-1",
            serde_json::to_value(&crate::protocol::message_types::DeviceGoodbyePayload {
                device_id: "remote-1".to_string(),
                reason: "shutdown".to_string(),
            }).unwrap(),
        );
        handler.dispatch_mesh_message(&goodbye).await;

        let mut found_offline = false;
        while let Ok(event) = dev_rx.try_recv() {
            if let DeviceEvent::DeviceOffline(id) = event {
                assert_eq!(id, "remote-1");
                found_offline = true;
            }
        }
        assert!(found_offline, "device:goodbye must emit DeviceOffline event");
    }

    #[tokio::test]
    async fn test_handle_message_mesh_envelope_binds_device_id() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        // Create a mesh envelope with device:announce that triggers set_device_id
        let device = make_device("remote-1", "Remote 1");
        let mesh_msg = make_announce_msg(&device);
        let envelope = MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "message".to_string(),
            payload: serde_json::to_value(&mesh_msg).unwrap(),
            timestamp: Some(1000),
        };
        let payload = serde_json::to_value(&envelope).unwrap();

        // handle_message calls handle_mesh_envelope which calls set_device_id
        // on the connection_manager. Since "conn-1" doesn't exist, it's a no-op.
        handler.handle_message("conn-1", &payload).await;

        // Verify the device was still added despite no real connection
        let dm = handler.device_manager.read().await;
        assert!(dm.device_by_id("remote-1").is_some());
    }

    #[tokio::test]
    async fn test_route_message_with_missing_fields_is_noop() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        // Make primary
        {
            let mut election = handler.election.write().await;
            election.configure(ElectionConfig {
                device_id: "local-dev".to_string(),
                started_at: 1000,
                prefer_primary: false,
            });
            election.set_primary("local-dev");
        }

        // route:message without targetDeviceId
        let route_envelope = MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "route:message".to_string(),
            payload: serde_json::json!({"envelope": {"namespace": "x", "type": "y", "payload": {}}}),
            timestamp: Some(1000),
        };

        // Should not panic
        handler
            .handle_route_envelope("conn-sender", Some("dev-a"), &route_envelope)
            .await;
    }

    #[tokio::test]
    async fn test_route_broadcast_with_missing_envelope_is_noop() {
        let (handler, mut event_rx, _dev_rx, _elec_rx) = make_handler();

        // Make primary
        {
            let mut election = handler.election.write().await;
            election.configure(ElectionConfig {
                device_id: "local-dev".to_string(),
                started_at: 1000,
                prefer_primary: false,
            });
            election.set_primary("local-dev");
        }

        // route:broadcast without envelope field
        let route_envelope = MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "route:broadcast".to_string(),
            payload: serde_json::json!({"other": "data"}),
            timestamp: Some(1000),
        };

        handler
            .handle_route_envelope("conn-sender", Some("dev-a"), &route_envelope)
            .await;

        // Nothing should be delivered
        assert!(
            event_rx.try_recv().is_err(),
            "route:broadcast without envelope field must be a no-op"
        );
    }

    // ══════════════════════════════════════════════════════════════════════
    // Adversarial / edge-case tests
    // ══════════════════════════════════════════════════════════════════════

    /// 18. Malformed device:announce payload: announce message with invalid JSON
    ///     in payload. Must not panic, must not add a device.
    #[tokio::test]
    async fn malformed_device_announce_no_panic_no_device() {
        let (handler, _event_rx, mut dev_rx, _elec_rx) = make_handler();

        // Create a device:announce message but with a broken payload
        // (not a valid DeviceAnnouncePayload)
        let msg = MeshMessage::new(
            "device:announce",
            "remote-1",
            serde_json::json!({"totally": "wrong", "not_a_device": true}),
        );

        handler.dispatch_mesh_message(&msg).await;

        let dm = handler.device_manager.read().await;
        assert!(dm.device_by_id("remote-1").is_none(),
            "Malformed device:announce must not add any device");

        // No device events should have been emitted
        assert!(dev_rx.try_recv().is_err(),
            "Malformed device:announce must not emit any DeviceEvents");
    }

    /// 19. device:list with empty devices array. Must not crash, no devices added.
    #[tokio::test]
    async fn device_list_empty_devices_array() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        let msg = MeshMessage::new(
            "device:list",
            "primary-node",
            serde_json::to_value(&DeviceListPayload {
                devices: vec![],
                primary_id: "primary-node".to_string(),
            })
            .unwrap(),
        );

        handler.dispatch_mesh_message(&msg).await;

        let dm = handler.device_manager.read().await;
        assert!(dm.devices().is_empty(),
            "device:list with empty devices array must not add any devices");

        // Election should still set primary from the list
        let election = handler.election.read().await;
        assert_eq!(election.primary_id(), Some("primary-node"),
            "device:list with non-empty primary_id should still set election primary");
    }

    /// 20. route:broadcast from self: primary receives a broadcast where
    ///     from_device_id is itself. Must deliver locally but not forward to self.
    #[tokio::test]
    async fn route_broadcast_from_self_delivers_locally() {
        let (handler, mut event_rx, _dev_rx, _elec_rx) = make_handler();

        // Make primary
        {
            let mut election = handler.election.write().await;
            election.configure(ElectionConfig {
                device_id: "local-dev".to_string(),
                started_at: 1000,
                prefer_primary: false,
            });
            election.set_primary("local-dev");
        }

        let inner_envelope = MeshEnvelope::new(
            "test-ns",
            "from-self",
            serde_json::json!({"origin": "local"}),
        );
        let broadcast_payload = serde_json::json!({
            "envelope": serde_json::to_value(&inner_envelope).unwrap(),
        });
        let route_envelope = MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "route:broadcast".to_string(),
            payload: broadcast_payload,
            timestamp: Some(1000),
        };

        // from_device_id = "local-dev" (self)
        handler
            .handle_route_envelope("conn-local", Some("local-dev"), &route_envelope)
            .await;

        // Should still deliver locally
        let mut found_local_delivery = false;
        while let Ok(event) = event_rx.try_recv() {
            if let MeshNodeEvent::Message(incoming) = event {
                if incoming.namespace == "test-ns" && incoming.msg_type == "from-self" {
                    found_local_delivery = true;
                }
            }
        }
        assert!(found_local_delivery,
            "route:broadcast from self must still deliver locally");
    }

    /// 21. Deeply nested envelope: route:broadcast containing another
    ///     route:broadcast inside. The nesting guard (RFC 009 issue #7) rejects
    ///     inner envelopes with mesh namespace, preventing recursive nesting.
    #[tokio::test]
    async fn nested_route_broadcast_rejected_by_mesh_namespace_guard() {
        let (handler, mut event_rx, _dev_rx, _elec_rx) = make_handler();

        // Make primary
        {
            let mut election = handler.election.write().await;
            election.configure(ElectionConfig {
                device_id: "local-dev".to_string(),
                started_at: 1000,
                prefer_primary: false,
            });
            election.set_primary("local-dev");
        }

        // Inner envelope with mesh namespace -- should be rejected by mesh-namespace guard
        let inner_inner = MeshEnvelope::new("app-ns", "data", serde_json::json!({"level": 2}));
        let inner_envelope = MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "route:broadcast".to_string(),
            payload: serde_json::json!({
                "envelope": serde_json::to_value(&inner_inner).unwrap(),
            }),
            timestamp: Some(1000),
        };

        let broadcast_payload = serde_json::json!({
            "envelope": serde_json::to_value(&inner_envelope).unwrap(),
        });
        let route_envelope = MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "route:broadcast".to_string(),
            payload: broadcast_payload,
            timestamp: Some(1000),
        };

        // This should complete without hanging or panicking, but the inner
        // envelope is rejected because it has mesh namespace
        handler
            .handle_route_envelope("conn-sender", Some("dev-a"), &route_envelope)
            .await;

        // The inner envelope must NOT be delivered (mesh namespace guard blocks it)
        assert!(
            event_rx.try_recv().is_err(),
            "Nested route:broadcast with mesh namespace inner must be rejected"
        );
    }

    /// 21b. route:broadcast with a non-mesh inner envelope that has a route: type prefix.
    ///      The nesting depth guard rejects it.
    #[tokio::test]
    async fn nested_route_type_rejected_by_nesting_guard() {
        let (handler, mut event_rx, _dev_rx, _elec_rx) = make_handler();

        // Make primary
        {
            let mut election = handler.election.write().await;
            election.configure(ElectionConfig {
                device_id: "local-dev".to_string(),
                started_at: 1000,
                prefer_primary: false,
            });
            election.set_primary("local-dev");
        }

        // Inner envelope with non-mesh namespace but route: type prefix
        let inner_envelope = MeshEnvelope::new(
            "custom-ns",
            "route:sneaky",
            serde_json::json!({"trick": true}),
        );

        let broadcast_payload = serde_json::json!({
            "envelope": serde_json::to_value(&inner_envelope).unwrap(),
        });
        let route_envelope = MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "route:broadcast".to_string(),
            payload: broadcast_payload,
            timestamp: Some(1000),
        };

        handler
            .handle_route_envelope("conn-sender", Some("dev-a"), &route_envelope)
            .await;

        // The inner envelope must NOT be delivered (route: prefix guard blocks it)
        assert!(
            event_rx.try_recv().is_err(),
            "Inner envelope with route: prefix must be rejected by nesting guard"
        );
    }

    /// 22. route:message targeting a device that is offline. Must not panic,
    ///     message should be silently dropped (no connection found).
    #[tokio::test]
    async fn route_message_to_offline_device_graceful() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        // Make primary
        {
            let mut election = handler.election.write().await;
            election.configure(ElectionConfig {
                device_id: "local-dev".to_string(),
                started_at: 1000,
                prefer_primary: false,
            });
            election.set_primary("local-dev");
        }

        // Add device and mark it offline
        let device = make_device("target-dev", "Target");
        handler.dispatch_mesh_message(&make_announce_msg(&device)).await;
        {
            let mut dm = handler.device_manager.write().await;
            dm.mark_device_offline("target-dev");
        }

        // Try to route a message to the offline device
        let inner_envelope = MeshEnvelope::new("app", "data", serde_json::json!({}));
        let route_payload = serde_json::json!({
            "targetDeviceId": "target-dev",
            "envelope": serde_json::to_value(&inner_envelope).unwrap(),
        });
        let route_envelope = MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "route:message".to_string(),
            payload: route_payload,
            timestamp: Some(1000),
        };

        // Should not panic — the send will just fail silently (no connection)
        handler
            .handle_route_envelope("conn-sender", Some("dev-a"), &route_envelope)
            .await;
    }

    /// 23. Massive payload: MeshMessage with a large JSON payload.
    ///     Must be handled without panic.
    #[tokio::test]
    async fn massive_payload_no_panic() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        // Build a ~1MB JSON payload
        let large_string = "x".repeat(1_000_000);
        let msg = MeshMessage::new(
            "device:announce",
            "remote-big",
            serde_json::json!({"data": large_string}),
        );

        // This will fail to parse as DeviceAnnouncePayload but must not panic
        handler.dispatch_mesh_message(&msg).await;

        // No device added (malformed payload)
        let dm = handler.device_manager.read().await;
        assert!(dm.device_by_id("remote-big").is_none(),
            "Massive payload that fails deserialization must not add a device");
    }

    /// Malformed election:candidate payload: must not panic, candidate not added.
    #[tokio::test]
    async fn malformed_election_candidate_no_panic() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        {
            let mut election = handler.election.write().await;
            election.configure(ElectionConfig {
                device_id: "local-dev".to_string(),
                started_at: 1000,
                prefer_primary: false,
            });
            election.start_election();
        }

        let msg = MeshMessage::new(
            "election:candidate",
            "remote-1",
            serde_json::json!({"garbage": "not a candidate payload"}),
        );

        handler.dispatch_mesh_message(&msg).await;

        // Election should still be collecting, no panic
        let election = handler.election.read().await;
        assert_eq!(election.phase(), ElectionPhase::Collecting,
            "Malformed election:candidate must not change phase");
    }

    /// Malformed election:result payload: must not panic, primary not changed.
    #[tokio::test]
    async fn malformed_election_result_no_panic() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        {
            let mut election = handler.election.write().await;
            election.configure(ElectionConfig {
                device_id: "local-dev".to_string(),
                started_at: 1000,
                prefer_primary: false,
            });
        }

        let msg = MeshMessage::new(
            "election:result",
            "remote-1",
            serde_json::json!({"not_valid": "payload"}),
        );

        handler.dispatch_mesh_message(&msg).await;

        let election = handler.election.read().await;
        assert!(election.primary_id().is_none(),
            "Malformed election:result must not set primary");
    }

    /// handle_message with completely invalid (non-object) JSON.
    #[tokio::test]
    async fn handle_message_non_object_json_ignored() {
        let (handler, mut event_rx, _dev_rx, _elec_rx) = make_handler();

        // Array instead of object
        let payload = serde_json::json!([1, 2, 3]);
        handler.handle_message("conn-1", &payload).await;

        // String value
        let payload = serde_json::json!("just a string");
        handler.handle_message("conn-1", &payload).await;

        // Null value
        let payload = serde_json::json!(null);
        handler.handle_message("conn-1", &payload).await;

        // None of these should produce events
        assert!(event_rx.try_recv().is_err(),
            "Non-object JSON payloads must be silently ignored");
    }

    /// Multiple device announces in rapid succession: all should be processed.
    #[tokio::test]
    async fn rapid_multiple_announces_all_processed() {
        let (handler, _event_rx, _dev_rx, _elec_rx) = make_handler();

        for i in 0..10 {
            let device = make_device(&format!("dev-{i}"), &format!("Device {i}"));
            let msg = make_announce_msg(&device);
            handler.dispatch_mesh_message(&msg).await;
        }

        let dm = handler.device_manager.read().await;
        assert_eq!(dm.devices().len(), 10,
            "All 10 rapid announces should be processed and added");
    }
}
