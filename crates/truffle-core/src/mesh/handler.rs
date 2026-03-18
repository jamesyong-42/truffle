//! Handler methods for the MeshNode transport event loop.
//!
//! Extracted from the inline closure in `start_event_loop()` for testability.

use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

use crate::protocol::envelope::{MeshEnvelope, MESH_NAMESPACE};
use crate::protocol::message_types::{
    DeviceAnnouncePayload, DeviceListPayload, ElectionCandidatePayload, ElectionResultPayload,
    MeshMessage,
};
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
    fn wrap_mesh_message(message: &MeshMessage) -> MeshEnvelope {
        MeshEnvelope {
            namespace: MESH_NAMESPACE.to_string(),
            msg_type: "message".to_string(),
            payload: serde_json::to_value(message).unwrap_or_default(),
            timestamp: Some(crate::util::current_timestamp_ms()),
        }
    }

    /// Serialize and send an envelope to a connection.
    async fn send_envelope_to_conn(&self, conn_id: &str, envelope: &MeshEnvelope) {
        if let Ok(value) = serde_json::to_value(envelope) {
            let _ = self.connection_manager.send(conn_id, &value).await;
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
        let announce = MeshMessage::new(
            "device:announce",
            &self.config.device_id,
            serde_json::to_value(&DeviceAnnouncePayload {
                device: local_device.clone(),
                protocol_version: 2,
            })
            .unwrap_or_default(),
        );
        let envelope = Self::wrap_mesh_message(&announce);
        self.send_envelope_to_conn(&conn.id, &envelope).await;

        // If primary, send device list to new connection
        if self.election.read().await.is_primary() {
            let devices = {
                let dm = self.device_manager.read().await;
                let mut devs = vec![local_device];
                devs.extend(dm.devices());
                devs
            };
            let list = MeshMessage::new(
                "device:list",
                &self.config.device_id,
                serde_json::to_value(&DeviceListPayload {
                    devices,
                    primary_id: self.config.device_id.clone(),
                })
                .unwrap_or_default(),
            );
            let envelope = Self::wrap_mesh_message(&list);
            self.send_envelope_to_conn(&conn.id, &envelope).await;
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
            Err(_) => return,
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
            Err(_) => return,
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
        match msg.msg_type.as_str() {
            "device:announce" => {
                if let Ok(payload) =
                    serde_json::from_value::<DeviceAnnouncePayload>(msg.payload.clone())
                {
                    self.device_manager
                        .write()
                        .await
                        .handle_device_announce(&msg.from, &payload);
                }
            }
            "device:list" => {
                if let Ok(payload) =
                    serde_json::from_value::<DeviceListPayload>(msg.payload.clone())
                {
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
            }
            "device:goodbye" => {
                self.device_manager
                    .write()
                    .await
                    .handle_device_goodbye(&msg.from);
            }
            "election:start" => {
                self.election
                    .write()
                    .await
                    .handle_election_start(&msg.from);
            }
            "election:candidate" => {
                if let Ok(payload) =
                    serde_json::from_value::<ElectionCandidatePayload>(msg.payload.clone())
                {
                    self.election
                        .write()
                        .await
                        .handle_election_candidate(&msg.from, &payload);
                }
            }
            "election:result" => {
                if let Ok(payload) =
                    serde_json::from_value::<ElectionResultPayload>(msg.payload.clone())
                {
                    self.election
                        .write()
                        .await
                        .handle_election_result(&msg.from, &payload);
                }
            }
            _ => {
                tracing::debug!("Unknown mesh message type: {}", msg.msg_type);
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
            "route:message" => {
                if let (Some(target), Some(inner)) = (
                    envelope
                        .payload
                        .get("targetDeviceId")
                        .and_then(|v| v.as_str()),
                    envelope.payload.get("envelope"),
                ) {
                    if let Ok(inner_env) =
                        serde_json::from_value::<MeshEnvelope>(inner.clone())
                    {
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
                        }
                    }
                }
            }
            "route:broadcast" => {
                if let Some(inner) = envelope.payload.get("envelope") {
                    if let Ok(inner_env) =
                        serde_json::from_value::<MeshEnvelope>(inner.clone())
                    {
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
                }
            }
            _ => {}
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
