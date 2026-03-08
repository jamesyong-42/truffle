use serde::Serialize;
use tauri::{AppHandle, Emitter, Runtime};

use truffle_core::mesh::node::MeshNodeEvent;
use truffle_core::reverse_proxy::ProxyEvent;
use truffle_core::types::DeviceRole;

// ═══════════════════════════════════════════════════════════════════════════
// Event payloads emitted to the Tauri frontend
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshEventPayload {
    pub event_type: String,
    pub device_id: Option<String>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyEventPayload {
    pub event_type: String,
    pub proxy_id: String,
    pub payload: serde_json::Value,
}

// ═══════════════════════════════════════════════════════════════════════════
// Event emission
// ═══════════════════════════════════════════════════════════════════════════

/// Emit a MeshNodeEvent to the Tauri frontend.
pub fn emit_mesh_event<R: Runtime>(app: &AppHandle<R>, event: &MeshNodeEvent) {
    let payload = match event {
        MeshNodeEvent::Started => MeshEventPayload {
            event_type: "started".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        },
        MeshNodeEvent::Stopped => MeshEventPayload {
            event_type: "stopped".to_string(),
            device_id: None,
            payload: serde_json::Value::Null,
        },
        MeshNodeEvent::AuthRequired(url) => MeshEventPayload {
            event_type: "authRequired".to_string(),
            device_id: None,
            payload: serde_json::Value::String(url.clone()),
        },
        MeshNodeEvent::DeviceDiscovered(device) => MeshEventPayload {
            event_type: "deviceDiscovered".to_string(),
            device_id: Some(device.id.clone()),
            payload: serde_json::to_value(device).unwrap_or(serde_json::Value::Null),
        },
        MeshNodeEvent::DeviceUpdated(device) => MeshEventPayload {
            event_type: "deviceUpdated".to_string(),
            device_id: Some(device.id.clone()),
            payload: serde_json::to_value(device).unwrap_or(serde_json::Value::Null),
        },
        MeshNodeEvent::DeviceOffline(id) => MeshEventPayload {
            event_type: "deviceOffline".to_string(),
            device_id: Some(id.clone()),
            payload: serde_json::Value::Null,
        },
        MeshNodeEvent::DevicesChanged(devices) => MeshEventPayload {
            event_type: "devicesChanged".to_string(),
            device_id: None,
            payload: serde_json::to_value(devices).unwrap_or(serde_json::Value::Null),
        },
        MeshNodeEvent::RoleChanged { role, is_primary } => MeshEventPayload {
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
        MeshNodeEvent::PrimaryChanged(id) => MeshEventPayload {
            event_type: "primaryChanged".to_string(),
            device_id: id.clone(),
            payload: serde_json::Value::Null,
        },
        MeshNodeEvent::Message(msg) => MeshEventPayload {
            event_type: "message".to_string(),
            device_id: msg.from.clone(),
            payload: serde_json::json!({
                "connectionId": msg.connection_id,
                "namespace": msg.namespace,
                "type": msg.msg_type,
                "payload": msg.payload,
            }),
        },
        MeshNodeEvent::Error(err) => MeshEventPayload {
            event_type: "error".to_string(),
            device_id: None,
            payload: serde_json::Value::String(err.clone()),
        },
    };

    if let Err(e) = app.emit("truffle://mesh-event", &payload) {
        tracing::warn!("Failed to emit mesh event to frontend: {e}");
    }
}

/// Emit a ProxyEvent to the Tauri frontend.
pub fn emit_proxy_event<R: Runtime>(app: &AppHandle<R>, event: &ProxyEvent) {
    let payload = match event {
        ProxyEvent::Started { id, port, target_port, url } => ProxyEventPayload {
            event_type: "started".to_string(),
            proxy_id: id.clone(),
            payload: serde_json::json!({
                "port": port,
                "targetPort": target_port,
                "url": url,
            }),
        },
        ProxyEvent::Stopped { id, reason } => ProxyEventPayload {
            event_type: "stopped".to_string(),
            proxy_id: id.clone(),
            payload: serde_json::json!({ "reason": reason }),
        },
        ProxyEvent::Error { id, message, code } => ProxyEventPayload {
            event_type: "error".to_string(),
            proxy_id: id.clone(),
            payload: serde_json::json!({
                "message": message,
                "code": code,
            }),
        },
    };

    if let Err(e) = app.emit("truffle://proxy-event", &payload) {
        tracing::warn!("Failed to emit proxy event to frontend: {e}");
    }
}
