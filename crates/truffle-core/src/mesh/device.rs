use std::collections::HashMap;

use tokio::sync::mpsc;

use crate::protocol::hostname::parse_hostname;
use crate::protocol::message_types::{DeviceAnnouncePayload, DeviceListPayload};
use crate::types::{BaseDevice, DeviceRole, DeviceStatus, TailnetPeer};

/// Identity of the local device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    pub id: String,
    pub device_type: String,
    pub name: String,
    pub tailscale_hostname: String,
}

/// Events emitted by the DeviceManager.
#[derive(Debug, Clone)]
pub enum DeviceEvent {
    DeviceDiscovered(BaseDevice),
    DeviceUpdated(BaseDevice),
    DeviceOffline(String),
    DevicesChanged(Vec<BaseDevice>),
    PrimaryChanged(Option<String>),
    LocalDeviceChanged(BaseDevice),
}

/// Manages device state for the mesh network.
///
/// Generic - no hardcoded device types, hostname prefixes, or app-specific logic.
/// All config provided via constructor injection.
pub struct DeviceManager {
    identity: DeviceIdentity,
    hostname_prefix: String,
    local_device: BaseDevice,
    devices: HashMap<String, BaseDevice>,
    primary_id: Option<String>,
    event_tx: mpsc::Sender<DeviceEvent>,
}

impl DeviceManager {
    pub fn new(
        identity: DeviceIdentity,
        hostname_prefix: String,
        capabilities: Vec<String>,
        metadata: Option<HashMap<String, serde_json::Value>>,
        event_tx: mpsc::Sender<DeviceEvent>,
    ) -> Self {
        let local_device = BaseDevice {
            id: identity.id.clone(),
            device_type: identity.device_type.clone(),
            name: identity.name.clone(),
            tailscale_hostname: identity.tailscale_hostname.clone(),
            tailscale_dns_name: None,
            tailscale_ip: None,
            role: None,
            status: DeviceStatus::Offline,
            capabilities,
            metadata,
            last_seen: None,
            started_at: None,
            os: None,
            latency_ms: None,
        };

        Self {
            identity,
            hostname_prefix,
            local_device,
            devices: HashMap::new(),
            primary_id: None,
            event_tx,
        }
    }

    /// Replace the event channel sender (used when resetting for restart).
    pub fn replace_event_tx(&mut self, tx: mpsc::Sender<DeviceEvent>) {
        self.event_tx = tx;
    }

    // ── Identity ──────────────────────────────────────────────────────────

    pub fn device_id(&self) -> &str {
        &self.identity.id
    }

    pub fn device_type(&self) -> &str {
        &self.identity.device_type
    }

    pub fn device_identity(&self) -> &DeviceIdentity {
        &self.identity
    }

    // ── Local device ──────────────────────────────────────────────────────

    pub fn local_device(&self) -> &BaseDevice {
        &self.local_device
    }

    pub fn set_local_online(&mut self, tailscale_ip: &str, started_at: u64, dns_name: Option<&str>) {
        self.local_device.tailscale_ip = Some(tailscale_ip.to_string());
        self.local_device.status = DeviceStatus::Online;
        self.local_device.started_at = Some(started_at);
        if let Some(dns) = dns_name {
            self.local_device.tailscale_dns_name = Some(dns.to_string());
        }
        self.emit(DeviceEvent::LocalDeviceChanged(self.local_device.clone()));
    }

    pub fn set_local_dns_name(&mut self, dns_name: &str) {
        if self.local_device.tailscale_dns_name.as_deref() != Some(dns_name) {
            self.local_device.tailscale_dns_name = Some(dns_name.to_string());
            self.emit(DeviceEvent::LocalDeviceChanged(self.local_device.clone()));
        }
    }

    pub fn set_local_offline(&mut self) {
        self.local_device.status = DeviceStatus::Offline;
        self.local_device.tailscale_ip = None;
        self.emit(DeviceEvent::LocalDeviceChanged(self.local_device.clone()));
    }

    pub fn set_local_role(&mut self, role: DeviceRole) {
        if self.local_device.role != Some(role) {
            self.local_device.role = Some(role);
            if role == DeviceRole::Primary {
                self.primary_id = Some(self.identity.id.clone());
            }
            self.emit(DeviceEvent::LocalDeviceChanged(self.local_device.clone()));
        }
    }

    pub fn update_device_name(&mut self, name: &str) {
        self.local_device.name = name.to_string();
        self.emit(DeviceEvent::LocalDeviceChanged(self.local_device.clone()));
    }

    pub fn update_metadata(&mut self, metadata: HashMap<String, serde_json::Value>) {
        let existing = self.local_device.metadata.get_or_insert_with(HashMap::new);
        existing.extend(metadata);
        self.emit(DeviceEvent::LocalDeviceChanged(self.local_device.clone()));
    }

    // ── Remote devices ────────────────────────────────────────────────────

    pub fn devices(&self) -> Vec<BaseDevice> {
        self.devices.values().cloned().collect()
    }

    pub fn device_by_id(&self, id: &str) -> Option<&BaseDevice> {
        if id == self.identity.id {
            return Some(&self.local_device);
        }
        self.devices.get(id)
    }

    pub fn online_devices(&self) -> Vec<BaseDevice> {
        self.devices
            .values()
            .filter(|d| d.status == DeviceStatus::Online)
            .cloned()
            .collect()
    }

    // ── Election support ──────────────────────────────────────────────────

    pub fn set_device_role(&mut self, device_id: &str, role: DeviceRole) {
        if let Some(device) = self.devices.get_mut(device_id) {
            if device.role != Some(role) {
                device.role = Some(role);
                let device_clone = device.clone();
                if role == DeviceRole::Primary {
                    self.primary_id = Some(device_id.to_string());
                    self.emit(DeviceEvent::PrimaryChanged(Some(device_id.to_string())));
                }
                self.emit(DeviceEvent::DeviceUpdated(device_clone));
            }
        }
    }

    pub fn primary_device(&self) -> Option<&BaseDevice> {
        self.primary_id.as_ref().and_then(|id| self.device_by_id(id))
    }

    pub fn primary_id(&self) -> Option<&str> {
        self.primary_id.as_deref()
    }

    // ── Device lifecycle ──────────────────────────────────────────────────

    pub fn add_discovered_peer(&mut self, peer: &TailnetPeer) -> Option<BaseDevice> {
        if peer.hostname == self.identity.tailscale_hostname {
            return None;
        }

        let parsed = parse_hostname(&self.hostname_prefix, &peer.hostname)?;

        let status = if peer.online {
            DeviceStatus::Online
        } else {
            DeviceStatus::Offline
        };

        if let Some(existing) = self.devices.get_mut(&parsed.id) {
            existing.status = status;
            existing.tailscale_ip = peer.tailscale_ips.first().cloned();
            existing.tailscale_dns_name = Some(peer.dns_name.clone());
            if let Some(ref os) = peer.os {
                existing.os = Some(os.clone());
            }
            let device = existing.clone();
            self.emit(DeviceEvent::DeviceUpdated(device.clone()));
            self.emit(DeviceEvent::DevicesChanged(self.devices()));
            return Some(device);
        }

        let device = BaseDevice {
            id: parsed.id.clone(),
            device_type: parsed.device_type,
            name: peer.hostname.clone(),
            tailscale_hostname: peer.hostname.clone(),
            tailscale_dns_name: Some(peer.dns_name.clone()),
            tailscale_ip: peer.tailscale_ips.first().cloned(),
            role: None,
            status,
            capabilities: vec![],
            metadata: None,
            last_seen: None,
            started_at: None,
            os: peer.os.clone(),
            latency_ms: None,
        };

        self.devices.insert(parsed.id, device.clone());
        self.emit(DeviceEvent::DeviceDiscovered(device.clone()));
        self.emit(DeviceEvent::DevicesChanged(self.devices()));
        Some(device)
    }

    pub fn handle_device_announce(&mut self, _from: &str, payload: &DeviceAnnouncePayload) {
        let device = &payload.device;
        let existing = self.devices.get(&device.id);

        let mut new_device = device.clone();

        // Preserve DNS name if the announce doesn't include it
        if new_device.tailscale_dns_name.is_none() {
            if let Some(existing) = existing {
                new_device.tailscale_dns_name.clone_from(&existing.tailscale_dns_name);
            }
        }

        let is_new = !self.devices.contains_key(&device.id);
        self.devices.insert(device.id.clone(), new_device.clone());

        if is_new {
            self.emit(DeviceEvent::DeviceDiscovered(new_device));
        } else {
            self.emit(DeviceEvent::DeviceUpdated(new_device));
        }
        self.emit(DeviceEvent::DevicesChanged(self.devices()));
    }

    pub fn handle_device_list(&mut self, _from: &str, payload: &DeviceListPayload) {
        for device in &payload.devices {
            if device.id == self.identity.id {
                continue;
            }

            let mut new_device = device.clone();
            // Preserve DNS name
            if new_device.tailscale_dns_name.is_none() {
                if let Some(existing) = self.devices.get(&device.id) {
                    new_device.tailscale_dns_name.clone_from(&existing.tailscale_dns_name);
                }
            }
            self.devices.insert(device.id.clone(), new_device);
        }

        if !payload.primary_id.is_empty() {
            self.primary_id = Some(payload.primary_id.clone());

            if payload.primary_id == self.identity.id {
                self.local_device.role = Some(DeviceRole::Primary);
            } else {
                self.local_device.role = Some(DeviceRole::Secondary);
            }

            for device in self.devices.values_mut() {
                device.role = if device.id == payload.primary_id {
                    Some(DeviceRole::Primary)
                } else {
                    Some(DeviceRole::Secondary)
                };
            }

            self.emit(DeviceEvent::PrimaryChanged(Some(payload.primary_id.clone())));
        }

        self.emit(DeviceEvent::DevicesChanged(self.devices()));
    }

    pub fn handle_device_goodbye(&mut self, device_id: &str) {
        self.mark_device_offline(device_id);
    }

    pub fn mark_device_offline(&mut self, device_id: &str) {
        if let Some(device) = self.devices.get_mut(device_id) {
            device.status = DeviceStatus::Offline;
            self.emit(DeviceEvent::DeviceOffline(device_id.to_string()));
            self.emit(DeviceEvent::DevicesChanged(self.devices()));

            if self.primary_id.as_deref() == Some(device_id) {
                self.primary_id = None;
                self.emit(DeviceEvent::PrimaryChanged(None));
            }
        }
    }

    pub fn clear(&mut self) {
        self.devices.clear();
        self.primary_id = None;
        self.emit(DeviceEvent::DevicesChanged(vec![]));
    }

    // ── Internal ──────────────────────────────────────────────────────────

    fn emit(&self, event: DeviceEvent) {
        if let Err(mpsc::error::TrySendError::Full(event)) = self.event_tx.try_send(event) {
            tracing::warn!(
                "DeviceEvent channel full, dropping {:?}",
                std::mem::discriminant(&event)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_identity() -> DeviceIdentity {
        DeviceIdentity {
            id: "local-1".to_string(),
            device_type: "desktop".to_string(),
            name: "My Desktop".to_string(),
            tailscale_hostname: "app-desktop-local-1".to_string(),
        }
    }

    fn make_manager() -> (DeviceManager, mpsc::Receiver<DeviceEvent>) {
        let (tx, rx) = mpsc::channel(256);
        let mgr = DeviceManager::new(
            test_identity(),
            "app".to_string(),
            vec![],
            None,
            tx,
        );
        (mgr, rx)
    }

    #[test]
    fn initial_state() {
        let (mgr, _rx) = make_manager();
        assert_eq!(mgr.device_id(), "local-1");
        assert_eq!(mgr.local_device().status, DeviceStatus::Offline);
        assert!(mgr.devices().is_empty());
        assert!(mgr.primary_id().is_none());
    }

    #[test]
    fn set_local_online() {
        let (mut mgr, mut rx) = make_manager();
        mgr.set_local_online("100.64.0.1", 1000, Some("app-desktop-local-1.tailnet.ts.net"));
        assert_eq!(mgr.local_device().status, DeviceStatus::Online);
        assert_eq!(mgr.local_device().tailscale_ip.as_deref(), Some("100.64.0.1"));
        assert_eq!(mgr.local_device().started_at, Some(1000));

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, DeviceEvent::LocalDeviceChanged(_)));
    }

    #[test]
    fn add_discovered_peer() {
        let (mut mgr, _rx) = make_manager();

        let peer = TailnetPeer {
            id: "peer-ts-id".to_string(),
            hostname: "app-mobile-peer-2".to_string(),
            dns_name: "app-mobile-peer-2.tailnet.ts.net".to_string(),
            tailscale_ips: vec!["100.64.0.2".to_string()],
            online: true,
            os: Some("android".to_string()),
        };

        let device = mgr.add_discovered_peer(&peer).unwrap();
        assert_eq!(device.id, "peer-2");
        assert_eq!(device.device_type, "mobile");
        assert_eq!(device.status, DeviceStatus::Online);

        assert_eq!(mgr.devices().len(), 1);
    }

    #[test]
    fn add_peer_wrong_prefix_returns_none() {
        let (mut mgr, _rx) = make_manager();

        let peer = TailnetPeer {
            id: "p".to_string(),
            hostname: "other-prefix-desktop-xyz".to_string(),
            dns_name: "other.ts.net".to_string(),
            tailscale_ips: vec!["100.64.0.3".to_string()],
            online: true,
            os: None,
        };

        assert!(mgr.add_discovered_peer(&peer).is_none());
    }

    #[test]
    fn add_self_returns_none() {
        let (mut mgr, _rx) = make_manager();

        let peer = TailnetPeer {
            id: "self".to_string(),
            hostname: "app-desktop-local-1".to_string(),
            dns_name: "app-desktop-local-1.ts.net".to_string(),
            tailscale_ips: vec!["100.64.0.1".to_string()],
            online: true,
            os: None,
        };

        assert!(mgr.add_discovered_peer(&peer).is_none());
    }

    #[test]
    fn handle_device_announce() {
        let (mut mgr, _rx) = make_manager();

        let device = BaseDevice {
            id: "remote-1".to_string(),
            device_type: "server".to_string(),
            name: "Server 1".to_string(),
            tailscale_hostname: "app-server-remote-1".to_string(),
            tailscale_dns_name: None,
            tailscale_ip: Some("100.64.0.5".to_string()),
            role: None,
            status: DeviceStatus::Online,
            capabilities: vec![],
            metadata: None,
            last_seen: None,
            started_at: Some(500),
            os: None,
            latency_ms: None,
        };

        let payload = DeviceAnnouncePayload {
            device: device.clone(),
            protocol_version: 2,
        };

        mgr.handle_device_announce("remote-1", &payload);
        assert_eq!(mgr.devices().len(), 1);
        assert_eq!(mgr.device_by_id("remote-1").unwrap().name, "Server 1");
    }

    #[test]
    fn handle_device_list_sets_primary() {
        let (mut mgr, _rx) = make_manager();

        let device = BaseDevice {
            id: "remote-1".to_string(),
            device_type: "desktop".to_string(),
            name: "Remote".to_string(),
            tailscale_hostname: "app-desktop-remote-1".to_string(),
            tailscale_dns_name: None,
            tailscale_ip: None,
            role: None,
            status: DeviceStatus::Online,
            capabilities: vec![],
            metadata: None,
            last_seen: None,
            started_at: None,
            os: None,
            latency_ms: None,
        };

        let payload = DeviceListPayload {
            devices: vec![device],
            primary_id: "remote-1".to_string(),
        };

        mgr.handle_device_list("remote-1", &payload);
        assert_eq!(mgr.primary_id(), Some("remote-1"));
        assert_eq!(mgr.local_device().role, Some(DeviceRole::Secondary));
    }

    #[test]
    fn mark_device_offline_clears_primary() {
        let (mut mgr, _rx) = make_manager();

        // Add a device
        let peer = TailnetPeer {
            id: "p".to_string(),
            hostname: "app-desktop-dev2".to_string(),
            dns_name: "app-desktop-dev2.ts.net".to_string(),
            tailscale_ips: vec!["100.64.0.2".to_string()],
            online: true,
            os: None,
        };
        mgr.add_discovered_peer(&peer);
        mgr.set_device_role("dev2", DeviceRole::Primary);
        assert_eq!(mgr.primary_id(), Some("dev2"));

        mgr.mark_device_offline("dev2");
        assert!(mgr.primary_id().is_none());
    }

    #[test]
    fn clear_resets() {
        let (mut mgr, _rx) = make_manager();

        let peer = TailnetPeer {
            id: "p".to_string(),
            hostname: "app-desktop-dev2".to_string(),
            dns_name: "app-desktop-dev2.ts.net".to_string(),
            tailscale_ips: vec!["100.64.0.2".to_string()],
            online: true,
            os: None,
        };
        mgr.add_discovered_peer(&peer);
        assert!(!mgr.devices().is_empty());

        mgr.clear();
        assert!(mgr.devices().is_empty());
        assert!(mgr.primary_id().is_none());
    }

    // ── CS-4/CS-8 additional tests ────────────────────────────────────────

    /// handle_device_goodbye marks the device offline.
    #[test]
    fn handle_device_goodbye_marks_offline() {
        let (mut mgr, mut rx) = make_manager();

        let peer = TailnetPeer {
            id: "p".to_string(),
            hostname: "app-desktop-dev2".to_string(),
            dns_name: "app-desktop-dev2.ts.net".to_string(),
            tailscale_ips: vec!["100.64.0.2".to_string()],
            online: true,
            os: None,
        };
        mgr.add_discovered_peer(&peer);

        // Drain discovery events
        while rx.try_recv().is_ok() {}

        mgr.handle_device_goodbye("dev2");

        let device = mgr.device_by_id("dev2").unwrap();
        assert_eq!(device.status, DeviceStatus::Offline,
            "handle_device_goodbye must mark device offline");

        // Should emit DeviceOffline event
        let mut found_offline = false;
        while let Ok(event) = rx.try_recv() {
            if let DeviceEvent::DeviceOffline(id) = event {
                assert_eq!(id, "dev2");
                found_offline = true;
            }
        }
        assert!(found_offline, "Must emit DeviceOffline event on goodbye");
    }

    /// set_local_offline emits LocalDeviceChanged.
    #[test]
    fn set_local_offline_emits_event() {
        let (mut mgr, mut rx) = make_manager();
        mgr.set_local_online("100.64.0.1", 1000, None);
        while rx.try_recv().is_ok() {} // drain

        mgr.set_local_offline();

        assert_eq!(mgr.local_device().status, DeviceStatus::Offline);
        assert!(mgr.local_device().tailscale_ip.is_none());

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, DeviceEvent::LocalDeviceChanged(_)));
    }

    /// set_local_role only emits event when role actually changes.
    #[test]
    fn set_local_role_emits_on_change() {
        let (mut mgr, mut rx) = make_manager();

        mgr.set_local_role(DeviceRole::Primary);
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, DeviceEvent::LocalDeviceChanged(_)));
        assert_eq!(mgr.local_device().role, Some(DeviceRole::Primary));
        assert_eq!(mgr.primary_id(), Some("local-1"));

        // Setting same role again should not emit
        mgr.set_local_role(DeviceRole::Primary);
        assert!(rx.try_recv().is_err(), "Should not emit when role unchanged");
    }

    /// replace_event_tx redirects events to new channel.
    #[test]
    fn replace_event_tx_works() {
        let (mut mgr, _rx1) = make_manager();

        let (tx2, mut rx2) = mpsc::channel(256);
        mgr.replace_event_tx(tx2);

        mgr.set_local_online("100.64.0.1", 1000, None);

        let event = rx2.try_recv().unwrap();
        assert!(matches!(event, DeviceEvent::LocalDeviceChanged(_)));
    }

    /// ARCH-6: emit() does not panic when channel is full.
    #[test]
    fn emit_does_not_panic_on_full_channel() {
        let (tx, _rx) = mpsc::channel(1);
        let mut mgr = DeviceManager::new(
            test_identity(),
            "app".to_string(),
            vec![],
            None,
            tx,
        );

        // Fill the channel
        mgr.set_local_online("100.64.0.1", 1000, None);

        // Should not panic even with full channel
        mgr.set_local_online("100.64.0.2", 2000, None);
    }

    /// update_metadata merges new keys into existing metadata.
    #[test]
    fn update_metadata_merges() {
        let (mut mgr, _rx) = make_manager();

        let mut meta1 = HashMap::new();
        meta1.insert("key1".to_string(), serde_json::json!("val1"));
        mgr.update_metadata(meta1);

        let mut meta2 = HashMap::new();
        meta2.insert("key2".to_string(), serde_json::json!("val2"));
        mgr.update_metadata(meta2);

        let metadata = mgr.local_device().metadata.clone().unwrap();
        assert_eq!(metadata.get("key1").unwrap(), &serde_json::json!("val1"));
        assert_eq!(metadata.get("key2").unwrap(), &serde_json::json!("val2"));
    }

    /// update_device_name updates the name and emits event.
    #[test]
    fn update_device_name_works() {
        let (mut mgr, mut rx) = make_manager();

        mgr.update_device_name("New Name");
        assert_eq!(mgr.local_device().name, "New Name");

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, DeviceEvent::LocalDeviceChanged(_)));
    }

    /// set_local_dns_name only emits when DNS name changes.
    #[test]
    fn set_local_dns_name_emits_on_change() {
        let (mut mgr, mut rx) = make_manager();

        mgr.set_local_dns_name("app.ts.net");
        let event = rx.try_recv().unwrap();
        assert!(matches!(event, DeviceEvent::LocalDeviceChanged(_)));

        // Same name: no event
        mgr.set_local_dns_name("app.ts.net");
        assert!(rx.try_recv().is_err(), "Should not emit when DNS name unchanged");
    }

    /// online_devices returns only devices with Online status.
    #[test]
    fn online_devices_filters_correctly() {
        let (mut mgr, _rx) = make_manager();

        let online_peer = TailnetPeer {
            id: "p1".to_string(),
            hostname: "app-desktop-on1".to_string(),
            dns_name: "app-desktop-on1.ts.net".to_string(),
            tailscale_ips: vec!["100.64.0.2".to_string()],
            online: true,
            os: None,
        };
        let offline_peer = TailnetPeer {
            id: "p2".to_string(),
            hostname: "app-desktop-off1".to_string(),
            dns_name: "app-desktop-off1.ts.net".to_string(),
            tailscale_ips: vec!["100.64.0.3".to_string()],
            online: false,
            os: None,
        };

        mgr.add_discovered_peer(&online_peer);
        mgr.add_discovered_peer(&offline_peer);

        let online = mgr.online_devices();
        assert_eq!(online.len(), 1);
        assert_eq!(online[0].id, "on1");
    }

    /// mark_device_offline on primary clears primary_id and emits PrimaryChanged(None).
    #[test]
    fn mark_primary_offline_emits_primary_changed_none() {
        let (mut mgr, mut rx) = make_manager();

        let peer = TailnetPeer {
            id: "p".to_string(),
            hostname: "app-desktop-dev2".to_string(),
            dns_name: "app-desktop-dev2.ts.net".to_string(),
            tailscale_ips: vec!["100.64.0.2".to_string()],
            online: true,
            os: None,
        };
        mgr.add_discovered_peer(&peer);
        mgr.set_device_role("dev2", DeviceRole::Primary);
        while rx.try_recv().is_ok() {} // drain

        mgr.mark_device_offline("dev2");

        assert!(mgr.primary_id().is_none());

        // Should have emitted PrimaryChanged(None)
        let mut found = false;
        while let Ok(event) = rx.try_recv() {
            if let DeviceEvent::PrimaryChanged(None) = event {
                found = true;
            }
        }
        assert!(found, "mark_device_offline on primary must emit PrimaryChanged(None)");
    }
}
