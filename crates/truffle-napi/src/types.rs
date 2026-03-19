use std::collections::HashMap;

use napi_derive::napi;

use truffle_core::mesh::node::{IncomingMeshMessage, MeshNodeEvent};
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

/// Convert JS metadata (serde_json::Value) to HashMap.
pub fn value_to_metadata(val: &serde_json::Value) -> Option<HashMap<String, serde_json::Value>> {
    match val {
        serde_json::Value::Object(map) => {
            let result: HashMap<String, serde_json::Value> = map.iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            Some(result)
        }
        _ => None,
    }
}
