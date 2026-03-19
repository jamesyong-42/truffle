use napi_derive::napi;

use truffle_core::mesh::node::{IncomingMeshMessage, MeshNodeEvent};
use truffle_core::runtime::TruffleEvent;
use truffle_core::types::{BaseDevice, DeviceRole, DeviceStatus};

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
            role: d.role.map(|r| match r {
                DeviceRole::Primary => "primary".to_string(),
                DeviceRole::Secondary => "secondary".to_string(),
            }),
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
        Self {
            from: m.from.clone(),
            connection_id: m.connection_id.clone(),
            namespace: m.namespace.clone(),
            msg_type: m.msg_type.clone(),
            payload: m.payload.clone(),
        }
    }
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
        MeshNodeEvent::RoleChanged { role, is_primary } => NapiMeshEvent {
            event_type: "roleChanged".to_string(),
            device_id: None,
            payload: serde_json::json!({
                "role": match role {
                    DeviceRole::Primary => "primary",
                    DeviceRole::Secondary => "secondary",
                },
                "isPrimary": is_primary,
            }),
        },
        MeshNodeEvent::PrimaryChanged(id) => NapiMeshEvent {
            event_type: "primaryChanged".to_string(),
            device_id: id.clone(),
            payload: serde_json::Value::Null,
        },
        MeshNodeEvent::Message(msg) => NapiMeshEvent {
            event_type: "message".to_string(),
            device_id: msg.from.clone(),
            payload: serde_json::json!({
                "connectionId": msg.connection_id,
                "namespace": msg.namespace,
                "type": msg.msg_type,
                "payload": msg.payload,
            }),
        },
        MeshNodeEvent::Error(err) => NapiMeshEvent {
            event_type: "error".to_string(),
            device_id: None,
            payload: serde_json::Value::String(err.clone()),
        },
    }
}

/// Convert a TruffleEvent into a NapiTruffleEvent (same shape as NapiMeshEvent) for delivery to JS.
///
/// This is the new unified event conversion. All TruffleEvent variants are mapped
/// to the same { event_type, device_id, payload } shape used by NapiMeshEvent,
/// so JS consumers get a single consistent event shape.
///
/// IMPORTANT: This function must NEVER panic. All conversions use
/// fallible operations with fallback values.
pub fn truffle_event_to_napi(event: &TruffleEvent) -> NapiTruffleEvent {
    match event {
        TruffleEvent::Started => NapiTruffleEvent {
            event_type: "started".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        },
        TruffleEvent::Stopped => NapiTruffleEvent {
            event_type: "stopped".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        },
        TruffleEvent::AuthRequired { auth_url } => NapiTruffleEvent {
            event_type: "authRequired".to_string(),
            device_id: None,
            payload: serde_json::Value::String(auth_url.clone()),
        },
        TruffleEvent::AuthComplete => NapiTruffleEvent {
            event_type: "authComplete".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        },
        TruffleEvent::Online { ip, dns_name } => NapiTruffleEvent {
            event_type: "online".to_string(),
            device_id: None,
            payload: serde_json::json!({
                "ip": ip,
                "dnsName": dns_name,
            }),
        },
        TruffleEvent::SidecarStarted => NapiTruffleEvent {
            event_type: "sidecarStarted".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        },
        TruffleEvent::SidecarStopped => NapiTruffleEvent {
            event_type: "sidecarStopped".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        },
        TruffleEvent::SidecarCrashed { exit_code, stderr_tail } => NapiTruffleEvent {
            event_type: "sidecarCrashed".to_string(),
            device_id: None,
            payload: serde_json::json!({
                "exitCode": exit_code,
                "stderrTail": stderr_tail,
            }),
        },
        TruffleEvent::SidecarStateChanged { state } => NapiTruffleEvent {
            event_type: "sidecarStateChanged".to_string(),
            device_id: None,
            payload: serde_json::Value::String(state.clone()),
        },
        TruffleEvent::SidecarNeedsApproval => NapiTruffleEvent {
            event_type: "sidecarNeedsApproval".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        },
        TruffleEvent::SidecarKeyExpiring { expires_at } => NapiTruffleEvent {
            event_type: "sidecarKeyExpiring".to_string(),
            device_id: None,
            payload: serde_json::Value::String(expires_at.clone()),
        },
        TruffleEvent::SidecarHealthWarning { warnings } => NapiTruffleEvent {
            event_type: "sidecarHealthWarning".to_string(),
            device_id: None,
            payload: serde_json::to_value(warnings).unwrap_or(serde_json::Value::Null),
        },
        TruffleEvent::DeviceDiscovered(device) => NapiTruffleEvent {
            event_type: "deviceDiscovered".to_string(),
            device_id: Some(device.id.clone()),
            payload: serde_json::to_value(device).unwrap_or(serde_json::Value::Null),
        },
        TruffleEvent::DeviceUpdated(device) => NapiTruffleEvent {
            event_type: "deviceUpdated".to_string(),
            device_id: Some(device.id.clone()),
            payload: serde_json::to_value(device).unwrap_or(serde_json::Value::Null),
        },
        TruffleEvent::DeviceOffline(id) => NapiTruffleEvent {
            event_type: "deviceOffline".to_string(),
            device_id: Some(id.clone()),
            payload: serde_json::Value::Null,
        },
        TruffleEvent::DevicesChanged(devices) => NapiTruffleEvent {
            event_type: "devicesChanged".to_string(),
            device_id: None,
            payload: serde_json::to_value(devices).unwrap_or(serde_json::Value::Null),
        },
        TruffleEvent::RoleChanged { role, is_primary } => NapiTruffleEvent {
            event_type: "roleChanged".to_string(),
            device_id: None,
            payload: serde_json::json!({
                "role": match role {
                    DeviceRole::Primary => "primary",
                    DeviceRole::Secondary => "secondary",
                },
                "isPrimary": is_primary,
            }),
        },
        TruffleEvent::PrimaryChanged { primary_id } => NapiTruffleEvent {
            event_type: "primaryChanged".to_string(),
            device_id: primary_id.clone(),
            payload: serde_json::Value::Null,
        },
        TruffleEvent::Message(msg) => NapiTruffleEvent {
            event_type: "message".to_string(),
            device_id: msg.from.clone(),
            payload: serde_json::json!({
                "connectionId": msg.connection_id,
                "namespace": msg.namespace,
                "type": msg.msg_type,
                "payload": msg.payload,
            }),
        },
        TruffleEvent::Error(err) => NapiTruffleEvent {
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

/// Type alias: NapiMeshEvent is the canonical name, but NapiTruffleEvent
/// is accepted as an alias for forward-compatibility with RFC 008 Phase 3.
pub type NapiTruffleEvent = NapiMeshEvent;

#[cfg(test)]
mod tests {
    use super::*;
    use truffle_core::mesh::node::IncomingMeshMessage;
    use truffle_core::runtime::TruffleEvent;
    use truffle_core::types::{BaseDevice, DeviceRole, DeviceStatus};

    fn test_base_device() -> BaseDevice {
        BaseDevice {
            id: "dev-abc".to_string(),
            device_type: "desktop".to_string(),
            name: "Test Peer".to_string(),
            tailscale_hostname: "app-desktop-dev-abc".to_string(),
            tailscale_dns_name: Some("app-desktop-dev-abc.tail1234.ts.net".to_string()),
            tailscale_ip: Some("100.64.0.2".to_string()),
            role: Some(DeviceRole::Secondary),
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
        // Every TruffleEvent variant must produce a valid NapiTruffleEvent
        let events: Vec<TruffleEvent> = vec![
            TruffleEvent::Started,
            TruffleEvent::Stopped,
            TruffleEvent::AuthRequired { auth_url: "https://login.tailscale.com/a/xxx".to_string() },
            TruffleEvent::Online { ip: "100.64.0.1".to_string(), dns_name: "app.tail.ts.net".to_string() },
            TruffleEvent::DeviceDiscovered(test_base_device()),
            TruffleEvent::DeviceUpdated(test_base_device()),
            TruffleEvent::DeviceOffline("dev-abc".to_string()),
            TruffleEvent::RoleChanged { role: DeviceRole::Primary, is_primary: true },
            TruffleEvent::PrimaryChanged { primary_id: Some("dev-abc".to_string()) },
            TruffleEvent::PrimaryChanged { primary_id: None },
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
            (TruffleEvent::RoleChanged { role: DeviceRole::Primary, is_primary: true }, "roleChanged"),
            (TruffleEvent::PrimaryChanged { primary_id: None }, "primaryChanged"),
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
    fn napi_mesh_event_alias_still_works() {
        // NapiTruffleEvent type alias must compile and be assignable from NapiMeshEvent
        let event: NapiTruffleEvent = NapiMeshEvent {
            event_type: "started".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        };
        assert_eq!(event.event_type, "started");

        // Can also use it in function signatures
        fn accepts_truffle_event(e: &NapiTruffleEvent) -> &str {
            &e.event_type
        }
        assert_eq!(accepts_truffle_event(&event), "started");
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
            MeshNodeEvent::RoleChanged { role: DeviceRole::Primary, is_primary: true },
            MeshNodeEvent::PrimaryChanged(Some("dev-abc".to_string())),
            MeshNodeEvent::Message(test_incoming_message()),
            MeshNodeEvent::Error("err".to_string()),
        ];

        for event in &events {
            let napi = mesh_event_to_napi(event);
            assert!(!napi.event_type.is_empty());
        }
    }
}
