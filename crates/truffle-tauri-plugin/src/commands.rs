use serde::{Deserialize, Serialize};
use tauri::{command, State};

use crate::TruffleState;

// ═══════════════════════════════════════════════════════════════════════════
// Command types (serialized to/from frontend)
// ═══════════════════════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
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

impl From<&truffle_core::types::BaseDevice> for DeviceInfo {
    fn from(d: &truffle_core::types::BaseDevice) -> Self {
        Self {
            id: d.id.clone(),
            device_type: d.device_type.clone(),
            name: d.name.clone(),
            tailscale_hostname: d.tailscale_hostname.clone(),
            tailscale_dns_name: d.tailscale_dns_name.clone(),
            tailscale_ip: d.tailscale_ip.clone(),
            role: d.role.map(|r| match r {
                truffle_core::types::DeviceRole::Primary => "primary".to_string(),
                truffle_core::types::DeviceRole::Secondary => "secondary".to_string(),
            }),
            status: match d.status {
                truffle_core::types::DeviceStatus::Online => "online".to_string(),
                truffle_core::types::DeviceStatus::Offline => "offline".to_string(),
                truffle_core::types::DeviceStatus::Connecting => "connecting".to_string(),
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TailnetPeerInput {
    pub id: String,
    pub hostname: String,
    pub dns_name: String,
    pub tailscale_ips: Vec<String>,
    pub online: bool,
    pub os: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyConfigInput {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub target_port: u16,
    pub target_scheme: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProxyInfoOutput {
    pub id: String,
    pub name: String,
    pub port: u16,
    pub target_port: u16,
    pub target_scheme: String,
    pub is_active: bool,
}

// ═══════════════════════════════════════════════════════════════════════════
// MeshNode commands
// ═══════════════════════════════════════════════════════════════════════════

#[command]
pub async fn start(state: State<'_, TruffleState>) -> Result<(), String> {
    let node = state.mesh_node.read().await;
    match node.as_ref() {
        Some(n) => { n.start().await; Ok(()) }
        None => Err("MeshNode not initialized".to_string()),
    }
}

#[command]
pub async fn stop(state: State<'_, TruffleState>) -> Result<(), String> {
    let node = state.mesh_node.read().await;
    match node.as_ref() {
        Some(n) => { n.stop().await; Ok(()) }
        None => Err("MeshNode not initialized".to_string()),
    }
}

#[command]
pub async fn is_running(state: State<'_, TruffleState>) -> Result<bool, String> {
    let node = state.mesh_node.read().await;
    match node.as_ref() {
        Some(n) => Ok(n.is_running().await),
        None => Ok(false),
    }
}

#[command]
pub async fn device_id(state: State<'_, TruffleState>) -> Result<String, String> {
    let node = state.mesh_node.read().await;
    match node.as_ref() {
        Some(n) => Ok(n.device_id().await),
        None => Err("MeshNode not initialized".to_string()),
    }
}

#[command]
pub async fn devices(state: State<'_, TruffleState>) -> Result<Vec<DeviceInfo>, String> {
    let node = state.mesh_node.read().await;
    match node.as_ref() {
        Some(n) => {
            let devs = n.devices().await;
            Ok(devs.iter().map(DeviceInfo::from).collect())
        }
        None => Err("MeshNode not initialized".to_string()),
    }
}

#[command]
pub async fn device_by_id(
    state: State<'_, TruffleState>,
    id: String,
) -> Result<Option<DeviceInfo>, String> {
    let node = state.mesh_node.read().await;
    match node.as_ref() {
        Some(n) => Ok(n.device_by_id(&id).await.as_ref().map(DeviceInfo::from)),
        None => Err("MeshNode not initialized".to_string()),
    }
}

#[command]
pub async fn is_primary(state: State<'_, TruffleState>) -> Result<bool, String> {
    let node = state.mesh_node.read().await;
    match node.as_ref() {
        Some(n) => Ok(n.is_primary().await),
        None => Ok(false),
    }
}

#[command]
pub async fn primary_id(state: State<'_, TruffleState>) -> Result<Option<String>, String> {
    let node = state.mesh_node.read().await;
    match node.as_ref() {
        Some(n) => Ok(n.primary_id().await),
        None => Ok(None),
    }
}

#[command]
pub async fn role(state: State<'_, TruffleState>) -> Result<String, String> {
    let node = state.mesh_node.read().await;
    match node.as_ref() {
        Some(n) => {
            let r = n.role().await;
            Ok(match r {
                truffle_core::types::DeviceRole::Primary => "primary".to_string(),
                truffle_core::types::DeviceRole::Secondary => "secondary".to_string(),
            })
        }
        None => Ok("secondary".to_string()),
    }
}

#[command]
pub async fn send_envelope(
    state: State<'_, TruffleState>,
    device_id: String,
    namespace: String,
    msg_type: String,
    payload: serde_json::Value,
) -> Result<bool, String> {
    let node = state.mesh_node.read().await;
    match node.as_ref() {
        Some(n) => {
            let envelope = truffle_core::protocol::envelope::MeshEnvelope::new(
                namespace, msg_type, payload,
            );
            Ok(n.send_envelope(&device_id, &envelope).await)
        }
        None => Err("MeshNode not initialized".to_string()),
    }
}

#[command]
pub async fn broadcast_envelope(
    state: State<'_, TruffleState>,
    namespace: String,
    msg_type: String,
    payload: serde_json::Value,
) -> Result<(), String> {
    let node = state.mesh_node.read().await;
    match node.as_ref() {
        Some(n) => {
            let envelope = truffle_core::protocol::envelope::MeshEnvelope::new(
                namespace, msg_type, payload,
            );
            n.broadcast_envelope(&envelope).await;
            Ok(())
        }
        None => Err("MeshNode not initialized".to_string()),
    }
}

#[command]
pub async fn handle_tailnet_peers(
    state: State<'_, TruffleState>,
    peers: Vec<TailnetPeerInput>,
) -> Result<(), String> {
    let node = state.mesh_node.read().await;
    match node.as_ref() {
        Some(n) => {
            let core_peers: Vec<truffle_core::types::TailnetPeer> = peers.iter().map(|p| {
                truffle_core::types::TailnetPeer {
                    id: p.id.clone(),
                    hostname: p.hostname.clone(),
                    dns_name: p.dns_name.clone(),
                    tailscale_ips: p.tailscale_ips.clone(),
                    online: p.online,
                    os: p.os.clone(),
                }
            }).collect();
            n.handle_tailnet_peers(&core_peers).await;
            Ok(())
        }
        None => Err("MeshNode not initialized".to_string()),
    }
}

#[command]
pub async fn set_local_online(
    state: State<'_, TruffleState>,
    tailscale_ip: String,
    dns_name: Option<String>,
) -> Result<(), String> {
    let node = state.mesh_node.read().await;
    match node.as_ref() {
        Some(n) => {
            n.set_local_online(&tailscale_ip, dns_name.as_deref()).await;
            Ok(())
        }
        None => Err("MeshNode not initialized".to_string()),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Reverse proxy commands
// ═══════════════════════════════════════════════════════════════════════════

#[command]
pub async fn proxy_add(
    state: State<'_, TruffleState>,
    config: ProxyConfigInput,
) -> Result<(), String> {
    let proxy_manager = state.proxy_manager.read().await;
    match proxy_manager.as_ref() {
        Some(pm) => {
            pm.add(truffle_core::reverse_proxy::ProxyConfig {
                id: config.id,
                name: config.name,
                port: config.port,
                target_port: config.target_port,
                target_scheme: config.target_scheme.unwrap_or_else(|| "http".to_string()),
            }).await
        }
        None => Err("ProxyManager not initialized".to_string()),
    }
}

#[command]
pub async fn proxy_remove(
    state: State<'_, TruffleState>,
    id: String,
) -> Result<(), String> {
    let proxy_manager = state.proxy_manager.read().await;
    match proxy_manager.as_ref() {
        Some(pm) => pm.remove(&id).await,
        None => Err("ProxyManager not initialized".to_string()),
    }
}

#[command]
pub async fn proxy_list(
    state: State<'_, TruffleState>,
) -> Result<Vec<ProxyInfoOutput>, String> {
    let proxy_manager = state.proxy_manager.read().await;
    match proxy_manager.as_ref() {
        Some(pm) => {
            let list = pm.list().await;
            Ok(list.into_iter().map(|p| ProxyInfoOutput {
                id: p.id,
                name: p.name,
                port: p.port,
                target_port: p.target_port,
                target_scheme: p.target_scheme,
                is_active: p.is_active,
            }).collect())
        }
        None => Ok(vec![]),
    }
}
