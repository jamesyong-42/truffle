use serde::{Deserialize, Serialize};

/// Default namespace for store sync messages on the MessageBus.
pub const STORE_SYNC_NAMESPACE: &str = "sync";

/// Message types for store synchronization.
pub mod message_types {
    pub const SYNC_FULL: &str = "store:sync:full";
    pub const SYNC_UPDATE: &str = "store:sync:update";
    pub const SYNC_REQUEST: &str = "store:sync:request";
    pub const SYNC_CLEAR: &str = "store:sync:clear";
}

/// A device-owned slice of data in a store.
/// Each device only writes to its own slice.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSlice {
    pub device_id: String,
    pub data: serde_json::Value,
    pub updated_at: u64,
    pub version: u64,
}

/// Payload for SYNC_FULL message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncFullPayload {
    pub store_id: String,
    pub device_id: String,
    pub data: serde_json::Value,
    pub version: u64,
    pub updated_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u64>,
}

/// Payload for SYNC_UPDATE message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncUpdatePayload {
    pub store_id: String,
    pub device_id: String,
    pub data: serde_json::Value,
    pub version: u64,
    pub updated_at: u64,
}

/// Payload for SYNC_REQUEST message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncRequestPayload {
    pub store_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_device_id: Option<String>,
}

/// Payload for SYNC_CLEAR message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncClearPayload {
    pub store_id: String,
    pub device_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// An incoming sync message from the MessageBus.
#[derive(Debug, Clone)]
pub struct SyncMessage {
    pub from: Option<String>,
    pub msg_type: String,
    pub payload: serde_json::Value,
}

/// An outgoing sync message to broadcast via the MessageBus.
#[derive(Debug, Clone)]
pub struct OutgoingSyncMessage {
    pub msg_type: String,
    pub payload: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_slice_serde() {
        let slice = DeviceSlice {
            device_id: "dev-1".to_string(),
            data: serde_json::json!({"items": ["a", "b"]}),
            updated_at: 1000,
            version: 1,
        };
        let json = serde_json::to_string(&slice).unwrap();
        assert!(json.contains("deviceId"));
        assert!(json.contains("updatedAt"));

        let parsed: DeviceSlice = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.device_id, "dev-1");
        assert_eq!(parsed.version, 1);
    }

    #[test]
    fn sync_full_payload_serde() {
        let payload = SyncFullPayload {
            store_id: "tasks".to_string(),
            device_id: "dev-1".to_string(),
            data: serde_json::json!({"items": []}),
            version: 3,
            updated_at: 5000,
            schema_version: None,
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(!json.contains("schemaVersion")); // skip_serializing_if
        let parsed: SyncFullPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.store_id, "tasks");
    }

    #[test]
    fn sync_request_payload_serde() {
        let payload = SyncRequestPayload {
            store_id: "settings".to_string(),
            from_device_id: Some("dev-2".to_string()),
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert!(json.contains("fromDeviceId"));
        let parsed: SyncRequestPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.from_device_id, Some("dev-2".to_string()));

        // Without fromDeviceId
        let payload2 = SyncRequestPayload {
            store_id: "tasks".to_string(),
            from_device_id: None,
        };
        let json2 = serde_json::to_string(&payload2).unwrap();
        assert!(!json2.contains("fromDeviceId"));
    }

    #[test]
    fn sync_clear_payload_serde() {
        let payload = SyncClearPayload {
            store_id: "tasks".to_string(),
            device_id: "dev-2".to_string(),
            reason: Some("offline".to_string()),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let parsed: SyncClearPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.reason, Some("offline".to_string()));
    }

    #[test]
    fn message_type_constants() {
        assert_eq!(message_types::SYNC_FULL, "store:sync:full");
        assert_eq!(message_types::SYNC_UPDATE, "store:sync:update");
        assert_eq!(message_types::SYNC_REQUEST, "store:sync:request");
        assert_eq!(message_types::SYNC_CLEAR, "store:sync:clear");
        assert_eq!(STORE_SYNC_NAMESPACE, "sync");
    }
}
