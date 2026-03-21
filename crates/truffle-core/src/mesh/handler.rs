//! Handler methods for the MeshNode transport event loop.
//!
//! Extracted from the inline closure in `start_event_loop()` for testability.

use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

use crate::protocol::envelope::{MeshEnvelope, MESH_NAMESPACE};
use crate::protocol::message_types::{
    DeviceAnnouncePayload, MeshMessage,
};
use crate::protocol::types::MeshPayload;
use crate::transport::connection::ConnectionManager;
use super::device::DeviceManager;
use super::message_bus::{BusMessage, MeshMessageBus};
use super::node::{IncomingMeshMessage, MeshNodeConfig, MeshNodeEvent};

/// Holds shared references needed by event handlers.
/// All methods are `&self` -- locks are acquired per-call.
pub(crate) struct TransportHandler {
    pub config: MeshNodeConfig,
    pub connection_manager: Arc<ConnectionManager>,
    pub device_manager: Arc<RwLock<DeviceManager>>,
    pub event_tx: broadcast::Sender<MeshNodeEvent>,
    pub message_bus: Arc<MeshMessageBus>,
}

impl TransportHandler {
    // -- Envelope helpers --

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

    // -- Transport event handlers --

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
            "device-announce",
            &self.config.device_id,
            announce_payload,
        );
        if let Some(envelope) = Self::wrap_mesh_message(&announce) {
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
                tracing::warn!(
                    connection_id,
                    msg_type = %envelope.msg_type,
                    "Unknown mesh namespace message type"
                );
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
        if mesh_msg.msg_type == "device-announce" {
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
            }
            Ok(MeshPayload::DeviceGoodbye(_payload)) => {
                self.device_manager
                    .write()
                    .await
                    .handle_device_goodbye(&msg.from);
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

    async fn handle_app_message(&self, connection_id: &str, from_device_id: Option<String>, envelope: &MeshEnvelope) {
        self.deliver_app_message(connection_id, from_device_id.clone(), envelope);
        self.message_bus.dispatch(&BusMessage {
            from: from_device_id,
            namespace: envelope.namespace.clone(),
            msg_type: envelope.msg_type.clone(),
            payload: envelope.payload.clone(),
        }).await;
    }

    fn deliver_app_message(&self, connection_id: &str, from: Option<String>, envelope: &MeshEnvelope) {
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
    use tokio::sync::mpsc;

    use crate::mesh::device::{DeviceEvent, DeviceIdentity};
    use crate::mesh::node::MeshTimingConfig;
    use crate::protocol::message_types::{DeviceAnnouncePayload, DeviceListPayload, MeshMessage};
    use crate::transport::connection::TransportConfig;
    use crate::types::{BaseDevice, DeviceStatus};

    fn make_handler() -> (TransportHandler, broadcast::Receiver<MeshNodeEvent>, mpsc::Receiver<DeviceEvent>) {
        let config = MeshNodeConfig {
            device_id: "local-dev".to_string(),
            device_name: "Local Device".to_string(),
            device_type: "desktop".to_string(),
            hostname_prefix: "app".to_string(),
            capabilities: vec![],
            metadata: None,
            timing: MeshTimingConfig::default(),
        };
        let (connection_manager, _transport_rx) = ConnectionManager::new(TransportConfig::default());
        let connection_manager = Arc::new(connection_manager);
        let (device_event_tx, device_event_rx) = mpsc::channel(256);
        let device_manager = DeviceManager::new(
            DeviceIdentity { id: "local-dev".to_string(), device_type: "desktop".to_string(), name: "Local Device".to_string(), tailscale_hostname: "app-desktop-local-dev".to_string() },
            "app".to_string(), vec![], None, device_event_tx,
        );
        let device_manager = Arc::new(RwLock::new(device_manager));
        let (event_tx, event_rx) = broadcast::channel(256);
        let (bus_event_tx, _bus_event_rx) = mpsc::channel(64);
        let message_bus = Arc::new(MeshMessageBus::new(bus_event_tx));
        let handler = TransportHandler { config, connection_manager, device_manager, event_tx, message_bus };
        (handler, event_rx, device_event_rx)
    }

    fn make_device(id: &str, name: &str) -> BaseDevice {
        BaseDevice {
            id: id.to_string(), device_type: "desktop".to_string(), name: name.to_string(),
            tailscale_hostname: format!("app-desktop-{id}"), tailscale_dns_name: None,
            tailscale_ip: Some("100.64.0.2".to_string()), status: DeviceStatus::Online,
            capabilities: vec![], metadata: None, last_seen: None, started_at: Some(1000),
            os: None, latency_ms: None,
        }
    }

    fn make_announce_msg(device: &BaseDevice) -> MeshMessage {
        MeshMessage::new("device-announce", &device.id, serde_json::to_value(&DeviceAnnouncePayload { device: device.clone(), protocol_version: 2 }).unwrap())
    }

    #[tokio::test]
    async fn test_device_announce_adds_device() {
        let (handler, _event_rx, _dev_rx) = make_handler();
        let device = make_device("remote-1", "Remote 1");
        handler.dispatch_mesh_message(&make_announce_msg(&device)).await;
        let dm = handler.device_manager.read().await;
        assert!(dm.device_by_id("remote-1").is_some());
        assert_eq!(dm.device_by_id("remote-1").unwrap().name, "Remote 1");
    }

    #[tokio::test]
    async fn test_device_announce_emits_discovered_event() {
        let (handler, _event_rx, mut dev_rx) = make_handler();
        handler.dispatch_mesh_message(&make_announce_msg(&make_device("remote-1", "R1"))).await;
        let mut found = false;
        while let Ok(event) = dev_rx.try_recv() {
            if let DeviceEvent::DeviceDiscovered(d) = event { assert_eq!(d.id, "remote-1"); found = true; }
        }
        assert!(found, "Must emit DeviceDiscovered");
    }

    #[tokio::test]
    async fn test_device_list_adds_multiple_devices() {
        let (handler, _event_rx, _dev_rx) = make_handler();
        let devices = vec![make_device("dev-a", "A"), make_device("dev-b", "B"), make_device("dev-c", "C")];
        let msg = MeshMessage::new("device-list", "primary-node", serde_json::to_value(&DeviceListPayload { devices, primary_id: "primary-node".to_string() }).unwrap());
        handler.dispatch_mesh_message(&msg).await;
        let dm = handler.device_manager.read().await;
        assert!(dm.device_by_id("dev-a").is_some());
        assert!(dm.device_by_id("dev-b").is_some());
        assert!(dm.device_by_id("dev-c").is_some());
    }

    #[tokio::test]
    async fn test_device_goodbye_marks_offline() {
        let (handler, _event_rx, mut dev_rx) = make_handler();
        handler.dispatch_mesh_message(&make_announce_msg(&make_device("remote-1", "R1"))).await;
        while dev_rx.try_recv().is_ok() {}
        let goodbye = MeshMessage::new("device-goodbye", "remote-1", serde_json::to_value(&crate::protocol::message_types::DeviceGoodbyePayload { device_id: "remote-1".to_string(), reason: "shutdown".to_string() }).unwrap());
        handler.dispatch_mesh_message(&goodbye).await;
        let dm = handler.device_manager.read().await;
        assert_eq!(dm.device_by_id("remote-1").unwrap().status, DeviceStatus::Offline);
    }

    #[tokio::test]
    async fn test_unknown_mesh_message_type_ignored() {
        let (handler, _event_rx, _dev_rx) = make_handler();
        handler.dispatch_mesh_message(&MeshMessage::new("totally-unknown", "r1", serde_json::json!({}))).await;
    }

    #[tokio::test]
    async fn test_non_mesh_envelope_delivered_as_app_message() {
        let (handler, mut event_rx, _dev_rx) = make_handler();
        let envelope = MeshEnvelope { namespace: "custom-app".to_string(), msg_type: "user-action".to_string(), payload: serde_json::json!({"key": "value"}), timestamp: Some(1000) };
        handler.handle_message("conn-1", &serde_json::to_value(&envelope).unwrap()).await;
        let event = event_rx.recv().await.unwrap();
        match event { MeshNodeEvent::Message(msg) => { assert_eq!(msg.namespace, "custom-app"); assert_eq!(msg.msg_type, "user-action"); } other => panic!("Expected Message, got: {other:?}") }
    }

    #[tokio::test]
    async fn test_invalid_envelope_ignored() {
        let (handler, _event_rx, _dev_rx) = make_handler();
        handler.handle_message("conn-1", &serde_json::json!("not an envelope")).await;
    }

    #[tokio::test]
    async fn test_wrap_mesh_message_produces_correct_envelope() {
        let msg = MeshMessage::new("device-announce", "dev-1", serde_json::json!({}));
        let envelope = TransportHandler::wrap_mesh_message(&msg).unwrap();
        assert_eq!(envelope.namespace, "mesh");
        assert_eq!(envelope.msg_type, "message");
    }

    #[tokio::test]
    async fn malformed_device_announce_no_panic() {
        let (handler, _event_rx, _dev_rx) = make_handler();
        handler.dispatch_mesh_message(&MeshMessage::new("device-announce", "r1", serde_json::json!("bad"))).await;
        assert!(handler.device_manager.read().await.device_by_id("r1").is_none());
    }

    #[tokio::test]
    async fn handle_message_non_object_json_ignored() {
        let (handler, _event_rx, _dev_rx) = make_handler();
        handler.handle_message("conn-1", &serde_json::json!(42)).await;
        handler.handle_message("conn-1", &serde_json::json!([1,2,3])).await;
        handler.handle_message("conn-1", &serde_json::json!(true)).await;
        handler.handle_message("conn-1", &serde_json::json!(null)).await;
    }

    #[tokio::test]
    async fn rapid_multiple_announces_all_processed() {
        let (handler, _event_rx, _dev_rx) = make_handler();
        for i in 0..50 {
            handler.dispatch_mesh_message(&make_announce_msg(&make_device(&format!("dev-{i}"), &format!("D{i}")))).await;
        }
        assert_eq!(handler.device_manager.read().await.devices().len(), 50);
    }
}
