use napi_derive::napi;

use truffle_core::mesh::node::{IncomingMeshMessage, MeshNodeEvent};
use truffle_core::protocol::namespace::Namespace;
use truffle_core::protocol::types::{
    FileTransferMessageType, MeshMessageType, SyncMessageType,
};
use truffle_core::runtime::TruffleEvent;
use truffle_core::types::{BaseDevice, DeviceStatus};

// ═══════════════════════════════════════════════════════════════════════════
// NAPI-exported JS object types
//
// These structs map 1:1 to the JS API surface. They use #[napi(object)]
// so they are passed as plain JS objects (not class instances).
// ═══════════════════════════════════════════════════════════════════════════

/// A mesh event delivered to JS via ThreadsafeFunction.
#[napi(object)]
pub struct NapiMeshEvent {
    pub event_type: String,
    pub device_id: Option<String>,
    pub payload: serde_json::Value,
}

/// Configuration for creating a MeshNode.
#[napi(object)]
pub struct NapiMeshNodeConfig {
    pub device_id: String,
    pub device_name: String,
    pub device_type: String,
    pub hostname_prefix: String,
    /// Path to the Go sidecar binary. Optional — use `resolveSidecarPath()`
    /// from `@vibecook/truffle` for automatic platform detection.
    pub sidecar_path: Option<String>,
    pub state_dir: Option<String>,
    pub auth_key: Option<String>,
    pub prefer_primary: Option<bool>,
    pub static_path: Option<String>,
    pub capabilities: Option<Vec<String>>,
    pub timing: Option<NapiMeshTimingConfig>,
    /// If true, the node is ephemeral (cleaned up when offline).
    pub ephemeral: Option<bool>,
    /// ACL tags to advertise (e.g. ["tag:truffle"]).
    pub tags: Option<Vec<String>>,
}

/// Timing configuration for the mesh.
#[napi(object)]
pub struct NapiMeshTimingConfig {
    pub announce_interval_ms: Option<u32>,
    pub discovery_timeout_ms: Option<u32>,
    pub election_timeout_ms: Option<u32>,
    pub primary_loss_grace_ms: Option<u32>,
    pub heartbeat_ping_ms: Option<u32>,
    pub heartbeat_timeout_ms: Option<u32>,
}

/// A device in the mesh network (JS representation).
#[napi(object)]
pub struct NapiBaseDevice {
    pub id: String,
    pub device_type: String,
    pub name: String,
    pub tailscale_hostname: String,
    pub tailscale_dns_name: Option<String>,
    pub tailscale_ip: Option<String>,
    pub role: Option<String>,
    pub status: String,
    pub capabilities: Vec<String>,
    pub metadata: Option<serde_json::Value>,
    pub last_seen: Option<f64>,
    pub started_at: Option<f64>,
    pub os: Option<String>,
    pub latency_ms: Option<f64>,
}

/// An incoming mesh message from a peer (JS representation).
#[napi(object)]
pub struct NapiIncomingMessage {
    pub from: Option<String>,
    pub connection_id: String,
    pub namespace: String,
    pub msg_type: String,
    pub payload: serde_json::Value,
}

/// Tailnet peer info (JS representation).
#[napi(object)]
pub struct NapiTailnetPeer {
    pub id: String,
    pub hostname: String,
    pub dns_name: String,
    pub tailscale_ips: Vec<String>,
    pub online: bool,
    pub os: Option<String>,
    pub cur_addr: Option<String>,
    pub relay: Option<String>,
    pub last_seen: Option<String>,
    pub key_expiry: Option<String>,
    pub expired: Option<bool>,
}

// ═══════════════════════════════════════════════════════════════════════════
// Conversions: truffle-core types -> NAPI types
// ═══════════════════════════════════════════════════════════════════════════

impl From<&BaseDevice> for NapiBaseDevice {
    fn from(d: &BaseDevice) -> Self {
        Self {
            id: d.id.clone(),
            device_type: d.device_type.clone(),
            name: d.name.clone(),
            tailscale_hostname: d.tailscale_hostname.clone(),
            tailscale_dns_name: d.tailscale_dns_name.clone(),
            tailscale_ip: d.tailscale_ip.clone(),
            role: None,
            status: match d.status {
                DeviceStatus::Online => "online".to_string(),
                DeviceStatus::Offline => "offline".to_string(),
                DeviceStatus::Connecting => "connecting".to_string(),
            },
            capabilities: d.capabilities.clone(),
            metadata: d.metadata.as_ref().map(|m| {
                serde_json::to_value(m).unwrap_or(serde_json::Value::Null)
            }),
            last_seen: d.last_seen.map(|v| v as f64),
            started_at: d.started_at.map(|v| v as f64),
            os: d.os.clone(),
            latency_ms: d.latency_ms,
        }
    }
}

impl From<BaseDevice> for NapiBaseDevice {
    fn from(d: BaseDevice) -> Self {
        NapiBaseDevice::from(&d)
    }
}

impl From<&IncomingMeshMessage> for NapiIncomingMessage {
    fn from(m: &IncomingMeshMessage) -> Self {
        let ns = normalize_namespace(&m.namespace);
        let mt = normalize_msg_type(&ns, &m.msg_type);
        Self {
            from: m.from.clone(),
            connection_id: m.connection_id.clone(),
            namespace: ns,
            msg_type: mt,
            payload: m.payload.clone(),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// RFC 009 Phase 5: Namespace/message-type normalization to kebab-case
//
// The internal core layer may still use legacy naming (colon-separated or
// SCREAMING_CASE). These helpers normalize to the canonical kebab-case form
// before delivering to JS, so consumers always see the new convention.
// ═══════════════════════════════════════════════════════════════════════════

/// Normalize a namespace string to its canonical kebab-case form.
///
/// Known namespaces are mapped via the `Namespace` enum. Unknown namespaces
/// are passed through unchanged.
fn normalize_namespace(ns: &str) -> String {
    // Try deserializing as a Namespace enum (handles known values like "mesh",
    // "sync", "file-transfer" as well as custom strings).
    if let Ok(parsed) = serde_json::from_value::<Namespace>(serde_json::Value::String(ns.to_string())) {
        parsed.as_str().to_string()
    } else {
        ns.to_string()
    }
}

/// Normalize a message type string to its canonical kebab-case form.
///
/// Uses the `from_str` methods on the typed message enums which accept both
/// legacy (colon-separated, SCREAMING_CASE) and new (kebab-case) forms.
/// Unknown types are passed through unchanged.
fn normalize_msg_type(namespace: &str, msg_type: &str) -> String {
    // Try each namespace's message type enum to see if it matches
    if let Some(t) = MeshMessageType::from_str(msg_type) {
        return t.as_str().to_string();
    }
    if let Some(t) = SyncMessageType::from_str(msg_type) {
        return t.as_str().to_string();
    }
    if let Some(t) = FileTransferMessageType::from_str(msg_type) {
        return t.as_str().to_string();
    }
    // Unknown: namespace hint is available but we don't use it for now
    let _ = namespace;
    msg_type.to_string()
}

/// Convert a MeshNodeEvent into a NapiMeshEvent for delivery to JS.
///
/// IMPORTANT: This function must NEVER panic. All conversions use
/// fallible operations with fallback values.
pub fn mesh_event_to_napi(event: &MeshNodeEvent) -> NapiMeshEvent {
    match event {
        MeshNodeEvent::Started => NapiMeshEvent {
            event_type: "started".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        },
        MeshNodeEvent::Stopped => NapiMeshEvent {
            event_type: "stopped".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        },
        MeshNodeEvent::AuthRequired(url) => NapiMeshEvent {
            event_type: "authRequired".to_string(),
            device_id: None,
            payload: serde_json::Value::String(url.clone()),
        },
        MeshNodeEvent::AuthComplete => NapiMeshEvent {
            event_type: "authComplete".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        },
        MeshNodeEvent::DeviceDiscovered(device) => NapiMeshEvent {
            event_type: "deviceDiscovered".to_string(),
            device_id: Some(device.id.clone()),
            payload: serde_json::to_value(device).unwrap_or(serde_json::Value::Null),
        },
        MeshNodeEvent::DeviceUpdated(device) => NapiMeshEvent {
            event_type: "deviceUpdated".to_string(),
            device_id: Some(device.id.clone()),
            payload: serde_json::to_value(device).unwrap_or(serde_json::Value::Null),
        },
        MeshNodeEvent::DeviceOffline(id) => NapiMeshEvent {
            event_type: "deviceOffline".to_string(),
            device_id: Some(id.clone()),
            payload: serde_json::Value::Null,
        },
        MeshNodeEvent::DevicesChanged(devices) => NapiMeshEvent {
            event_type: "devicesChanged".to_string(),
            device_id: None,
            payload: serde_json::to_value(devices).unwrap_or(serde_json::Value::Null),
        },
        MeshNodeEvent::Message(msg) => {
            let ns = normalize_namespace(&msg.namespace);
            let mt = normalize_msg_type(&ns, &msg.msg_type);
            NapiMeshEvent {
                event_type: "message".to_string(),
                device_id: msg.from.clone(),
                payload: serde_json::json!({
                    "connectionId": msg.connection_id,
                    "namespace": ns,
                    "type": mt,
                    "payload": msg.payload,
                }),
            }
        },
        MeshNodeEvent::Error(err) => NapiMeshEvent {
            event_type: "error".to_string(),
            device_id: None,
            payload: serde_json::Value::String(err.clone()),
        },
    }
}

/// Convert a TruffleEvent into a NapiMeshEvent (same shape as NapiMeshEvent) for delivery to JS.
///
/// This is the new unified event conversion. All TruffleEvent variants are mapped
/// to the same { event_type, device_id, payload } shape used by NapiMeshEvent,
/// so JS consumers get a single consistent event shape.
///
/// IMPORTANT: This function must NEVER panic. All conversions use
/// fallible operations with fallback values.
pub fn truffle_event_to_napi(event: &TruffleEvent) -> NapiMeshEvent {
    match event {
        TruffleEvent::Started => NapiMeshEvent {
            event_type: "started".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        },
        TruffleEvent::Stopped => NapiMeshEvent {
            event_type: "stopped".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        },
        TruffleEvent::AuthRequired { auth_url } => NapiMeshEvent {
            event_type: "authRequired".to_string(),
            device_id: None,
            payload: serde_json::Value::String(auth_url.clone()),
        },
        TruffleEvent::AuthComplete => NapiMeshEvent {
            event_type: "authComplete".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        },
        TruffleEvent::Online { ip, dns_name } => NapiMeshEvent {
            event_type: "online".to_string(),
            device_id: None,
            payload: serde_json::json!({
                "ip": ip,
                "dnsName": dns_name,
            }),
        },
        TruffleEvent::SidecarStarted => NapiMeshEvent {
            event_type: "sidecarStarted".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        },
        TruffleEvent::SidecarStopped => NapiMeshEvent {
            event_type: "sidecarStopped".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        },
        TruffleEvent::SidecarCrashed { exit_code, stderr_tail } => NapiMeshEvent {
            event_type: "sidecarCrashed".to_string(),
            device_id: None,
            payload: serde_json::json!({
                "exitCode": exit_code,
                "stderrTail": stderr_tail,
            }),
        },
        TruffleEvent::SidecarStateChanged { state } => NapiMeshEvent {
            event_type: "sidecarStateChanged".to_string(),
            device_id: None,
            payload: serde_json::Value::String(state.clone()),
        },
        TruffleEvent::SidecarNeedsApproval => NapiMeshEvent {
            event_type: "sidecarNeedsApproval".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        },
        TruffleEvent::SidecarKeyExpiring { expires_at } => NapiMeshEvent {
            event_type: "sidecarKeyExpiring".to_string(),
            device_id: None,
            payload: serde_json::Value::String(expires_at.clone()),
        },
        TruffleEvent::SidecarHealthWarning { warnings } => NapiMeshEvent {
            event_type: "sidecarHealthWarning".to_string(),
            device_id: None,
            payload: serde_json::to_value(warnings).unwrap_or(serde_json::Value::Null),
        },
        TruffleEvent::DeviceDiscovered(device) => NapiMeshEvent {
            event_type: "deviceDiscovered".to_string(),
            device_id: Some(device.id.clone()),
            payload: serde_json::to_value(device).unwrap_or(serde_json::Value::Null),
        },
        TruffleEvent::DeviceUpdated(device) => NapiMeshEvent {
            event_type: "deviceUpdated".to_string(),
            device_id: Some(device.id.clone()),
            payload: serde_json::to_value(device).unwrap_or(serde_json::Value::Null),
        },
        TruffleEvent::DeviceOffline(id) => NapiMeshEvent {
            event_type: "deviceOffline".to_string(),
            device_id: Some(id.clone()),
            payload: serde_json::Value::Null,
        },
        TruffleEvent::DevicesChanged(devices) => NapiMeshEvent {
            event_type: "devicesChanged".to_string(),
            device_id: None,
            payload: serde_json::to_value(devices).unwrap_or(serde_json::Value::Null),
        },
        TruffleEvent::Message(msg) => {
            let ns = normalize_namespace(&msg.namespace);
            let mt = normalize_msg_type(&ns, &msg.msg_type);
            NapiMeshEvent {
                event_type: "message".to_string(),
                device_id: msg.from.clone(),
                payload: serde_json::json!({
                    "connectionId": msg.connection_id,
                    "namespace": ns,
                    "type": mt,
                    "payload": msg.payload,
                }),
            }
        },
        TruffleEvent::Error(err) => NapiMeshEvent {
            event_type: "error".to_string(),
            device_id: None,
            payload: serde_json::Value::String(err.clone()),
        },
    }
}

/// Convert a NapiTailnetPeer to a truffle-core TailnetPeer.
pub fn napi_peer_to_core(p: &NapiTailnetPeer) -> truffle_core::types::TailnetPeer {
    truffle_core::types::TailnetPeer {
        id: p.id.clone(),
        hostname: p.hostname.clone(),
        dns_name: p.dns_name.clone(),
        tailscale_ips: p.tailscale_ips.clone(),
        online: p.online,
        os: p.os.clone(),
        cur_addr: p.cur_addr.clone(),
        relay: p.relay.clone(),
        last_seen: p.last_seen.clone(),
        key_expiry: p.key_expiry.clone(),
        expired: p.expired.unwrap_or(false),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use truffle_core::mesh::node::IncomingMeshMessage;
    use truffle_core::runtime::TruffleEvent;
    use truffle_core::types::{BaseDevice, DeviceStatus};

    fn test_base_device() -> BaseDevice {
        BaseDevice {
            id: "dev-abc".to_string(),
            device_type: "desktop".to_string(),
            name: "Test Peer".to_string(),
            tailscale_hostname: "app-desktop-dev-abc".to_string(),
            tailscale_dns_name: Some("app-desktop-dev-abc.tail1234.ts.net".to_string()),
            tailscale_ip: Some("100.64.0.2".to_string()),
            status: DeviceStatus::Online,
            capabilities: vec!["pty".to_string()],
            metadata: None,
            last_seen: Some(1710764400),
            started_at: Some(1710760000),
            os: Some("darwin".to_string()),
            latency_ms: Some(12.5),
        }
    }

    fn test_incoming_message() -> IncomingMeshMessage {
        IncomingMeshMessage {
            from: Some("dev-abc".to_string()),
            connection_id: "conn-1".to_string(),
            namespace: "chat".to_string(),
            msg_type: "text".to_string(),
            payload: serde_json::json!({"text": "hello"}),
        }
    }

    #[test]
    fn truffle_event_to_napi_all_variants() {
        // Every TruffleEvent variant must produce a valid NapiMeshEvent
        let events: Vec<TruffleEvent> = vec![
            TruffleEvent::Started,
            TruffleEvent::Stopped,
            TruffleEvent::AuthRequired { auth_url: "https://login.tailscale.com/a/xxx".to_string() },
            TruffleEvent::Online { ip: "100.64.0.1".to_string(), dns_name: "app.tail.ts.net".to_string() },
            TruffleEvent::DeviceDiscovered(test_base_device()),
            TruffleEvent::DeviceUpdated(test_base_device()),
            TruffleEvent::DeviceOffline("dev-abc".to_string()),
            TruffleEvent::Message(test_incoming_message()),
            TruffleEvent::Error("something went wrong".to_string()),
            TruffleEvent::SidecarCrashed { exit_code: Some(1), stderr_tail: "panic".to_string() },
        ];

        for event in &events {
            let napi_event = truffle_event_to_napi(event);
            // event_type must be a non-empty string
            assert!(!napi_event.event_type.is_empty(), "event_type must not be empty for {:?}", event);
        }
    }

    #[test]
    fn napi_event_type_strings_match_contract() {
        // Verify the exact event_type string for each variant matches the JS API contract
        let cases: Vec<(TruffleEvent, &str)> = vec![
            (TruffleEvent::Started, "started"),
            (TruffleEvent::Stopped, "stopped"),
            (TruffleEvent::AuthRequired { auth_url: "url".to_string() }, "authRequired"),
            (TruffleEvent::Online { ip: "1.2.3.4".to_string(), dns_name: "x".to_string() }, "online"),
            (TruffleEvent::DeviceDiscovered(test_base_device()), "deviceDiscovered"),
            (TruffleEvent::DeviceUpdated(test_base_device()), "deviceUpdated"),
            (TruffleEvent::DeviceOffline("id".to_string()), "deviceOffline"),
            (TruffleEvent::Message(test_incoming_message()), "message"),
            (TruffleEvent::Error("err".to_string()), "error"),
            (TruffleEvent::SidecarCrashed { exit_code: Some(1), stderr_tail: "x".to_string() }, "sidecarCrashed"),
        ];

        for (event, expected_type) in &cases {
            let napi = truffle_event_to_napi(event);
            assert_eq!(
                &napi.event_type, expected_type,
                "event_type mismatch for {:?}: got '{}', expected '{}'",
                event, napi.event_type, expected_type,
            );
        }
    }

    #[test]
    fn napi_mesh_event_construction() {
        let event = NapiMeshEvent {
            event_type: "started".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        };
        assert_eq!(event.event_type, "started");
    }

    // ═══════════════════════════════════════════════════════════════════
    // Namespace/message-type normalization tests
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn normalize_namespace_known_values() {
        assert_eq!(normalize_namespace("mesh"), "mesh");
        assert_eq!(normalize_namespace("sync"), "sync");
        assert_eq!(normalize_namespace("file-transfer"), "file-transfer");
    }

    #[test]
    fn normalize_namespace_custom_passthrough() {
        assert_eq!(normalize_namespace("chat"), "chat");
        assert_eq!(normalize_namespace("my-app"), "my-app");
        assert_eq!(normalize_namespace("clipboard"), "clipboard");
    }

    #[test]
    fn normalize_msg_type_mesh_kebab() {
        assert_eq!(normalize_msg_type("mesh", "device-announce"), "device-announce");
        assert_eq!(normalize_msg_type("mesh", "device-list"), "device-list");
        assert_eq!(normalize_msg_type("mesh", "route-broadcast"), "route-broadcast");
    }

    #[test]
    fn normalize_msg_type_legacy_colon_passthrough() {
        // Legacy colon-separated names are no longer recognized and pass through unchanged
        assert_eq!(normalize_msg_type("mesh", "device:announce"), "device:announce");
        assert_eq!(normalize_msg_type("sync", "store:sync:full"), "store:sync:full");
    }

    #[test]
    fn normalize_msg_type_sync_kebab() {
        assert_eq!(normalize_msg_type("sync", "sync-full"), "sync-full");
        assert_eq!(normalize_msg_type("sync", "sync-update"), "sync-update");
    }

    #[test]
    fn normalize_msg_type_file_transfer_legacy_screaming_passthrough() {
        // Legacy SCREAMING_CASE names are no longer recognized and pass through unchanged
        assert_eq!(normalize_msg_type("file-transfer", "OFFER"), "OFFER");
        assert_eq!(normalize_msg_type("file-transfer", "ACCEPT"), "ACCEPT");
    }

    #[test]
    fn normalize_msg_type_file_transfer_kebab() {
        assert_eq!(normalize_msg_type("file-transfer", "file-offer"), "file-offer");
        assert_eq!(normalize_msg_type("file-transfer", "file-accept"), "file-accept");
    }

    #[test]
    fn normalize_msg_type_unknown_passthrough() {
        assert_eq!(normalize_msg_type("chat", "message"), "message");
        assert_eq!(normalize_msg_type("mesh", "unknown-type"), "unknown-type");
        assert_eq!(normalize_msg_type("custom", "anything"), "anything");
    }

    #[test]
    fn truffle_event_message_uses_kebab_case() {
        let msg = IncomingMeshMessage {
            from: Some("dev-1".to_string()),
            connection_id: "conn-1".to_string(),
            namespace: "mesh".to_string(),
            msg_type: "device-announce".to_string(),
            payload: serde_json::json!({"device": {}}),
        };
        let event = TruffleEvent::Message(msg);
        let napi = truffle_event_to_napi(&event);
        assert_eq!(napi.event_type, "message");
        let payload = &napi.payload;
        assert_eq!(payload["namespace"], "mesh");
        assert_eq!(payload["type"], "device-announce");
    }

    #[test]
    fn truffle_event_message_file_transfer_kebab() {
        let msg = IncomingMeshMessage {
            from: Some("dev-2".to_string()),
            connection_id: "conn-2".to_string(),
            namespace: "file-transfer".to_string(),
            msg_type: "file-offer".to_string(),
            payload: serde_json::json!({}),
        };
        let event = TruffleEvent::Message(msg);
        let napi = truffle_event_to_napi(&event);
        let payload = &napi.payload;
        assert_eq!(payload["namespace"], "file-transfer");
        assert_eq!(payload["type"], "file-offer");
    }

    #[test]
    fn napi_incoming_message_kebab_case() {
        let msg = IncomingMeshMessage {
            from: Some("dev-1".to_string()),
            connection_id: "conn-1".to_string(),
            namespace: "sync".to_string(),
            msg_type: "sync-full".to_string(),
            payload: serde_json::json!({}),
        };
        let napi: NapiIncomingMessage = (&msg).into();
        assert_eq!(napi.namespace, "sync");
        assert_eq!(napi.msg_type, "sync-full");
    }

    #[test]
    fn napi_incoming_message_custom_namespace_passthrough() {
        let msg = IncomingMeshMessage {
            from: None,
            connection_id: "conn-x".to_string(),
            namespace: "chat".to_string(),
            msg_type: "text".to_string(),
            payload: serde_json::json!({"text": "hi"}),
        };
        let napi: NapiIncomingMessage = (&msg).into();
        assert_eq!(napi.namespace, "chat");
        assert_eq!(napi.msg_type, "text");
    }

    // ═══════════════════════════════════════════════════════════════════
    // Existing mesh_event_to_napi tests (MeshNodeEvent -- backward compat)
    // ═══════════════════════════════════════════════════════════════════

    #[test]
    fn mesh_event_to_napi_all_variants() {
        // Existing MeshNodeEvent conversion must still work
        let events: Vec<MeshNodeEvent> = vec![
            MeshNodeEvent::Started,
            MeshNodeEvent::Stopped,
            MeshNodeEvent::AuthRequired("https://example.com".to_string()),
            MeshNodeEvent::AuthComplete,
            MeshNodeEvent::DeviceDiscovered(test_base_device()),
            MeshNodeEvent::DeviceUpdated(test_base_device()),
            MeshNodeEvent::DeviceOffline("dev-abc".to_string()),
            MeshNodeEvent::DevicesChanged(vec![test_base_device()]),
            MeshNodeEvent::Message(test_incoming_message()),
            MeshNodeEvent::Error("err".to_string()),
        ];

        for event in &events {
            let napi = mesh_event_to_napi(event);
            assert!(!napi.event_type.is_empty());
        }
    }
}
