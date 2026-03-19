use serde::{Deserialize, Serialize};
use std::fmt;

use crate::protocol::message_types::{
    DeviceAnnouncePayload, DeviceGoodbyePayload, DeviceListPayload, ElectionCandidatePayload,
    ElectionResultPayload, RouteBroadcastPayload, RouteMessagePayload,
};

// ---------------------------------------------------------------------------
// MeshMessageType
// ---------------------------------------------------------------------------

/// Message types for the `mesh` namespace.
///
/// Wire strings use kebab-case: `"device-announce"`, `"election-start"`, etc.
/// The `from_str` method also accepts legacy colon-separated forms for backward
/// compatibility during the migration period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MeshMessageType {
    DeviceAnnounce,
    DeviceList,
    DeviceGoodbye,
    ElectionStart,
    ElectionCandidate,
    ElectionResult,
    RouteMessage,
    RouteBroadcast,
}

impl MeshMessageType {
    /// Parse a message type string, accepting both new kebab-case and legacy
    /// colon-separated forms.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "device-announce" | "device:announce" => Some(Self::DeviceAnnounce),
            "device-list" | "device:list" => Some(Self::DeviceList),
            "device-goodbye" | "device:goodbye" => Some(Self::DeviceGoodbye),
            "election-start" | "election:start" => Some(Self::ElectionStart),
            "election-candidate" | "election:candidate" => Some(Self::ElectionCandidate),
            "election-result" | "election:result" => Some(Self::ElectionResult),
            "route-message" | "route:message" => Some(Self::RouteMessage),
            "route-broadcast" | "route:broadcast" => Some(Self::RouteBroadcast),
            _ => None,
        }
    }

    /// Returns the canonical kebab-case wire string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DeviceAnnounce => "device-announce",
            Self::DeviceList => "device-list",
            Self::DeviceGoodbye => "device-goodbye",
            Self::ElectionStart => "election-start",
            Self::ElectionCandidate => "election-candidate",
            Self::ElectionResult => "election-result",
            Self::RouteMessage => "route-message",
            Self::RouteBroadcast => "route-broadcast",
        }
    }
}

impl fmt::Display for MeshMessageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ---------------------------------------------------------------------------
// SyncMessageType
// ---------------------------------------------------------------------------

/// Message types for the `sync` namespace.
///
/// Wire strings: `"sync-full"`, `"sync-update"`, `"sync-request"`, `"sync-clear"`.
/// Legacy forms (`"store:sync:full"`, etc.) are accepted during migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncMessageType {
    SyncFull,
    SyncUpdate,
    SyncRequest,
    SyncClear,
}

impl SyncMessageType {
    /// Parse a message type string, accepting both new kebab-case and legacy
    /// colon-separated forms.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "sync-full" | "store:sync:full" => Some(Self::SyncFull),
            "sync-update" | "store:sync:update" => Some(Self::SyncUpdate),
            "sync-request" | "store:sync:request" => Some(Self::SyncRequest),
            "sync-clear" | "store:sync:clear" => Some(Self::SyncClear),
            _ => None,
        }
    }

    /// Returns the canonical kebab-case wire string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SyncFull => "sync-full",
            Self::SyncUpdate => "sync-update",
            Self::SyncRequest => "sync-request",
            Self::SyncClear => "sync-clear",
        }
    }
}

impl fmt::Display for SyncMessageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ---------------------------------------------------------------------------
// FileTransferMessageType
// ---------------------------------------------------------------------------

/// Message types for the `file-transfer` namespace.
///
/// Wire strings: `"file-offer"`, `"file-accept"`, `"file-reject"`, `"file-cancel"`.
/// Legacy SCREAMING_CASE forms (`"OFFER"`, etc.) are accepted during migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileTransferMessageType {
    FileOffer,
    FileAccept,
    FileReject,
    FileCancel,
}

impl FileTransferMessageType {
    /// Parse a message type string, accepting both new kebab-case and legacy
    /// SCREAMING_CASE forms.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "file-offer" | "OFFER" => Some(Self::FileOffer),
            "file-accept" | "ACCEPT" => Some(Self::FileAccept),
            "file-reject" | "REJECT" => Some(Self::FileReject),
            "file-cancel" | "CANCEL" => Some(Self::FileCancel),
            _ => None,
        }
    }

    /// Returns the canonical kebab-case wire string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::FileOffer => "file-offer",
            Self::FileAccept => "file-accept",
            Self::FileReject => "file-reject",
            Self::FileCancel => "file-cancel",
        }
    }
}

impl fmt::Display for FileTransferMessageType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ---------------------------------------------------------------------------
// DispatchError
// ---------------------------------------------------------------------------

/// Errors that can occur when dispatching a typed message payload.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    /// Unknown message type for the given namespace.
    #[error("unknown message type '{msg_type}' in namespace '{namespace}'")]
    UnknownMessageType { namespace: String, msg_type: String },

    /// Payload failed to deserialize into the expected type.
    #[error("payload deserialization failed: {0}")]
    PayloadDeserialize(#[from] serde_json::Error),
}

// ---------------------------------------------------------------------------
// MeshPayload
// ---------------------------------------------------------------------------

/// A fully parsed mesh-namespace message.
///
/// The payload is decoded from `serde_json::Value` into the correct Rust type
/// at dispatch time. This ensures that by the time a handler receives a
/// `MeshPayload`, the data is validated and strongly typed.
#[derive(Debug, Clone)]
pub enum MeshPayload {
    DeviceAnnounce(DeviceAnnouncePayload),
    DeviceList(DeviceListPayload),
    DeviceGoodbye(DeviceGoodbyePayload),
    /// Election start carries no payload.
    ElectionStart,
    ElectionCandidate(ElectionCandidatePayload),
    ElectionResult(ElectionResultPayload),
    RouteMessage(RouteMessagePayload),
    RouteBroadcast(RouteBroadcastPayload),
}

impl MeshPayload {
    /// Parse a mesh message type string and raw payload into a typed `MeshPayload`.
    ///
    /// Returns an error if the type is unknown or the payload does not match
    /// the expected schema for that type.
    pub fn parse(msg_type: &str, payload: serde_json::Value) -> Result<Self, DispatchError> {
        let typ = MeshMessageType::from_str(msg_type).ok_or_else(|| {
            DispatchError::UnknownMessageType {
                namespace: "mesh".into(),
                msg_type: msg_type.into(),
            }
        })?;

        match typ {
            MeshMessageType::DeviceAnnounce => {
                let p = serde_json::from_value(payload)?;
                Ok(Self::DeviceAnnounce(p))
            }
            MeshMessageType::DeviceList => {
                let p = serde_json::from_value(payload)?;
                Ok(Self::DeviceList(p))
            }
            MeshMessageType::DeviceGoodbye => {
                let p = serde_json::from_value(payload)?;
                Ok(Self::DeviceGoodbye(p))
            }
            MeshMessageType::ElectionStart => Ok(Self::ElectionStart),
            MeshMessageType::ElectionCandidate => {
                let p = serde_json::from_value(payload)?;
                Ok(Self::ElectionCandidate(p))
            }
            MeshMessageType::ElectionResult => {
                let p = serde_json::from_value(payload)?;
                Ok(Self::ElectionResult(p))
            }
            MeshMessageType::RouteMessage => {
                let p = serde_json::from_value(payload)?;
                Ok(Self::RouteMessage(p))
            }
            MeshMessageType::RouteBroadcast => {
                let p = serde_json::from_value(payload)?;
                Ok(Self::RouteBroadcast(p))
            }
        }
    }

    /// Returns the `MeshMessageType` for this payload variant.
    pub fn message_type(&self) -> MeshMessageType {
        match self {
            Self::DeviceAnnounce(_) => MeshMessageType::DeviceAnnounce,
            Self::DeviceList(_) => MeshMessageType::DeviceList,
            Self::DeviceGoodbye(_) => MeshMessageType::DeviceGoodbye,
            Self::ElectionStart => MeshMessageType::ElectionStart,
            Self::ElectionCandidate(_) => MeshMessageType::ElectionCandidate,
            Self::ElectionResult(_) => MeshMessageType::ElectionResult,
            Self::RouteMessage(_) => MeshMessageType::RouteMessage,
            Self::RouteBroadcast(_) => MeshMessageType::RouteBroadcast,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ======================================================================
    // MeshMessageType tests
    // ======================================================================

    #[test]
    fn mesh_message_type_from_str_kebab_case() {
        assert_eq!(
            MeshMessageType::from_str("device-announce"),
            Some(MeshMessageType::DeviceAnnounce)
        );
        assert_eq!(
            MeshMessageType::from_str("device-list"),
            Some(MeshMessageType::DeviceList)
        );
        assert_eq!(
            MeshMessageType::from_str("device-goodbye"),
            Some(MeshMessageType::DeviceGoodbye)
        );
        assert_eq!(
            MeshMessageType::from_str("election-start"),
            Some(MeshMessageType::ElectionStart)
        );
        assert_eq!(
            MeshMessageType::from_str("election-candidate"),
            Some(MeshMessageType::ElectionCandidate)
        );
        assert_eq!(
            MeshMessageType::from_str("election-result"),
            Some(MeshMessageType::ElectionResult)
        );
        assert_eq!(
            MeshMessageType::from_str("route-message"),
            Some(MeshMessageType::RouteMessage)
        );
        assert_eq!(
            MeshMessageType::from_str("route-broadcast"),
            Some(MeshMessageType::RouteBroadcast)
        );
    }

    #[test]
    fn mesh_message_type_from_str_legacy_colon() {
        assert_eq!(
            MeshMessageType::from_str("device:announce"),
            Some(MeshMessageType::DeviceAnnounce)
        );
        assert_eq!(
            MeshMessageType::from_str("device:list"),
            Some(MeshMessageType::DeviceList)
        );
        assert_eq!(
            MeshMessageType::from_str("device:goodbye"),
            Some(MeshMessageType::DeviceGoodbye)
        );
        assert_eq!(
            MeshMessageType::from_str("election:start"),
            Some(MeshMessageType::ElectionStart)
        );
        assert_eq!(
            MeshMessageType::from_str("election:candidate"),
            Some(MeshMessageType::ElectionCandidate)
        );
        assert_eq!(
            MeshMessageType::from_str("election:result"),
            Some(MeshMessageType::ElectionResult)
        );
        assert_eq!(
            MeshMessageType::from_str("route:message"),
            Some(MeshMessageType::RouteMessage)
        );
        assert_eq!(
            MeshMessageType::from_str("route:broadcast"),
            Some(MeshMessageType::RouteBroadcast)
        );
    }

    #[test]
    fn mesh_message_type_from_str_unknown() {
        assert_eq!(MeshMessageType::from_str(""), None);
        assert_eq!(MeshMessageType::from_str("unknown"), None);
        assert_eq!(MeshMessageType::from_str("device_announce"), None);
        assert_eq!(MeshMessageType::from_str("DEVICE-ANNOUNCE"), None);
        assert_eq!(MeshMessageType::from_str("device:anounce"), None); // typo
    }

    #[test]
    fn mesh_message_type_as_str() {
        assert_eq!(MeshMessageType::DeviceAnnounce.as_str(), "device-announce");
        assert_eq!(MeshMessageType::DeviceList.as_str(), "device-list");
        assert_eq!(MeshMessageType::DeviceGoodbye.as_str(), "device-goodbye");
        assert_eq!(MeshMessageType::ElectionStart.as_str(), "election-start");
        assert_eq!(
            MeshMessageType::ElectionCandidate.as_str(),
            "election-candidate"
        );
        assert_eq!(MeshMessageType::ElectionResult.as_str(), "election-result");
        assert_eq!(MeshMessageType::RouteMessage.as_str(), "route-message");
        assert_eq!(MeshMessageType::RouteBroadcast.as_str(), "route-broadcast");
    }

    #[test]
    fn mesh_message_type_serde_roundtrip_all_variants() {
        let variants = [
            MeshMessageType::DeviceAnnounce,
            MeshMessageType::DeviceList,
            MeshMessageType::DeviceGoodbye,
            MeshMessageType::ElectionStart,
            MeshMessageType::ElectionCandidate,
            MeshMessageType::ElectionResult,
            MeshMessageType::RouteMessage,
            MeshMessageType::RouteBroadcast,
        ];
        for v in variants {
            let json = serde_json::to_string(&v).unwrap();
            let parsed: MeshMessageType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, v, "Serde roundtrip failed for {v:?}");
        }
    }

    #[test]
    fn mesh_message_type_serde_wire_strings() {
        assert_eq!(
            serde_json::to_string(&MeshMessageType::DeviceAnnounce).unwrap(),
            r#""device-announce""#
        );
        assert_eq!(
            serde_json::to_string(&MeshMessageType::ElectionStart).unwrap(),
            r#""election-start""#
        );
        assert_eq!(
            serde_json::to_string(&MeshMessageType::RouteBroadcast).unwrap(),
            r#""route-broadcast""#
        );
    }

    #[test]
    fn mesh_message_type_display() {
        assert_eq!(format!("{}", MeshMessageType::DeviceAnnounce), "device-announce");
        assert_eq!(format!("{}", MeshMessageType::RouteMessage), "route-message");
    }

    #[test]
    fn mesh_message_type_from_str_roundtrip_via_as_str() {
        let variants = [
            MeshMessageType::DeviceAnnounce,
            MeshMessageType::DeviceList,
            MeshMessageType::DeviceGoodbye,
            MeshMessageType::ElectionStart,
            MeshMessageType::ElectionCandidate,
            MeshMessageType::ElectionResult,
            MeshMessageType::RouteMessage,
            MeshMessageType::RouteBroadcast,
        ];
        for v in variants {
            let s = v.as_str();
            let parsed = MeshMessageType::from_str(s).unwrap();
            assert_eq!(parsed, v);
        }
    }

    // ======================================================================
    // SyncMessageType tests
    // ======================================================================

    #[test]
    fn sync_message_type_from_str_kebab_case() {
        assert_eq!(
            SyncMessageType::from_str("sync-full"),
            Some(SyncMessageType::SyncFull)
        );
        assert_eq!(
            SyncMessageType::from_str("sync-update"),
            Some(SyncMessageType::SyncUpdate)
        );
        assert_eq!(
            SyncMessageType::from_str("sync-request"),
            Some(SyncMessageType::SyncRequest)
        );
        assert_eq!(
            SyncMessageType::from_str("sync-clear"),
            Some(SyncMessageType::SyncClear)
        );
    }

    #[test]
    fn sync_message_type_from_str_legacy_colon() {
        assert_eq!(
            SyncMessageType::from_str("store:sync:full"),
            Some(SyncMessageType::SyncFull)
        );
        assert_eq!(
            SyncMessageType::from_str("store:sync:update"),
            Some(SyncMessageType::SyncUpdate)
        );
        assert_eq!(
            SyncMessageType::from_str("store:sync:request"),
            Some(SyncMessageType::SyncRequest)
        );
        assert_eq!(
            SyncMessageType::from_str("store:sync:clear"),
            Some(SyncMessageType::SyncClear)
        );
    }

    #[test]
    fn sync_message_type_from_str_unknown() {
        assert_eq!(SyncMessageType::from_str(""), None);
        assert_eq!(SyncMessageType::from_str("sync-delete"), None);
        assert_eq!(SyncMessageType::from_str("sync:full"), None);
        assert_eq!(SyncMessageType::from_str("SYNC-FULL"), None);
    }

    #[test]
    fn sync_message_type_as_str() {
        assert_eq!(SyncMessageType::SyncFull.as_str(), "sync-full");
        assert_eq!(SyncMessageType::SyncUpdate.as_str(), "sync-update");
        assert_eq!(SyncMessageType::SyncRequest.as_str(), "sync-request");
        assert_eq!(SyncMessageType::SyncClear.as_str(), "sync-clear");
    }

    #[test]
    fn sync_message_type_serde_roundtrip_all_variants() {
        let variants = [
            SyncMessageType::SyncFull,
            SyncMessageType::SyncUpdate,
            SyncMessageType::SyncRequest,
            SyncMessageType::SyncClear,
        ];
        for v in variants {
            let json = serde_json::to_string(&v).unwrap();
            let parsed: SyncMessageType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, v, "Serde roundtrip failed for {v:?}");
        }
    }

    #[test]
    fn sync_message_type_serde_wire_strings() {
        assert_eq!(
            serde_json::to_string(&SyncMessageType::SyncFull).unwrap(),
            r#""sync-full""#
        );
        assert_eq!(
            serde_json::to_string(&SyncMessageType::SyncUpdate).unwrap(),
            r#""sync-update""#
        );
        assert_eq!(
            serde_json::to_string(&SyncMessageType::SyncRequest).unwrap(),
            r#""sync-request""#
        );
        assert_eq!(
            serde_json::to_string(&SyncMessageType::SyncClear).unwrap(),
            r#""sync-clear""#
        );
    }

    #[test]
    fn sync_message_type_display() {
        assert_eq!(format!("{}", SyncMessageType::SyncFull), "sync-full");
        assert_eq!(format!("{}", SyncMessageType::SyncClear), "sync-clear");
    }

    #[test]
    fn sync_message_type_from_str_roundtrip_via_as_str() {
        let variants = [
            SyncMessageType::SyncFull,
            SyncMessageType::SyncUpdate,
            SyncMessageType::SyncRequest,
            SyncMessageType::SyncClear,
        ];
        for v in variants {
            let s = v.as_str();
            let parsed = SyncMessageType::from_str(s).unwrap();
            assert_eq!(parsed, v);
        }
    }

    // ======================================================================
    // FileTransferMessageType tests
    // ======================================================================

    #[test]
    fn file_transfer_message_type_from_str_kebab_case() {
        assert_eq!(
            FileTransferMessageType::from_str("file-offer"),
            Some(FileTransferMessageType::FileOffer)
        );
        assert_eq!(
            FileTransferMessageType::from_str("file-accept"),
            Some(FileTransferMessageType::FileAccept)
        );
        assert_eq!(
            FileTransferMessageType::from_str("file-reject"),
            Some(FileTransferMessageType::FileReject)
        );
        assert_eq!(
            FileTransferMessageType::from_str("file-cancel"),
            Some(FileTransferMessageType::FileCancel)
        );
    }

    #[test]
    fn file_transfer_message_type_from_str_legacy_screaming() {
        assert_eq!(
            FileTransferMessageType::from_str("OFFER"),
            Some(FileTransferMessageType::FileOffer)
        );
        assert_eq!(
            FileTransferMessageType::from_str("ACCEPT"),
            Some(FileTransferMessageType::FileAccept)
        );
        assert_eq!(
            FileTransferMessageType::from_str("REJECT"),
            Some(FileTransferMessageType::FileReject)
        );
        assert_eq!(
            FileTransferMessageType::from_str("CANCEL"),
            Some(FileTransferMessageType::FileCancel)
        );
    }

    #[test]
    fn file_transfer_message_type_from_str_unknown() {
        assert_eq!(FileTransferMessageType::from_str(""), None);
        assert_eq!(FileTransferMessageType::from_str("file-download"), None);
        assert_eq!(FileTransferMessageType::from_str("offer"), None);
        assert_eq!(FileTransferMessageType::from_str("FILE-OFFER"), None);
    }

    #[test]
    fn file_transfer_message_type_as_str() {
        assert_eq!(FileTransferMessageType::FileOffer.as_str(), "file-offer");
        assert_eq!(FileTransferMessageType::FileAccept.as_str(), "file-accept");
        assert_eq!(FileTransferMessageType::FileReject.as_str(), "file-reject");
        assert_eq!(FileTransferMessageType::FileCancel.as_str(), "file-cancel");
    }

    #[test]
    fn file_transfer_message_type_serde_roundtrip_all_variants() {
        let variants = [
            FileTransferMessageType::FileOffer,
            FileTransferMessageType::FileAccept,
            FileTransferMessageType::FileReject,
            FileTransferMessageType::FileCancel,
        ];
        for v in variants {
            let json = serde_json::to_string(&v).unwrap();
            let parsed: FileTransferMessageType = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, v, "Serde roundtrip failed for {v:?}");
        }
    }

    #[test]
    fn file_transfer_message_type_serde_wire_strings() {
        assert_eq!(
            serde_json::to_string(&FileTransferMessageType::FileOffer).unwrap(),
            r#""file-offer""#
        );
        assert_eq!(
            serde_json::to_string(&FileTransferMessageType::FileAccept).unwrap(),
            r#""file-accept""#
        );
        assert_eq!(
            serde_json::to_string(&FileTransferMessageType::FileReject).unwrap(),
            r#""file-reject""#
        );
        assert_eq!(
            serde_json::to_string(&FileTransferMessageType::FileCancel).unwrap(),
            r#""file-cancel""#
        );
    }

    #[test]
    fn file_transfer_message_type_display() {
        assert_eq!(
            format!("{}", FileTransferMessageType::FileOffer),
            "file-offer"
        );
        assert_eq!(
            format!("{}", FileTransferMessageType::FileCancel),
            "file-cancel"
        );
    }

    #[test]
    fn file_transfer_message_type_from_str_roundtrip_via_as_str() {
        let variants = [
            FileTransferMessageType::FileOffer,
            FileTransferMessageType::FileAccept,
            FileTransferMessageType::FileReject,
            FileTransferMessageType::FileCancel,
        ];
        for v in variants {
            let s = v.as_str();
            let parsed = FileTransferMessageType::from_str(s).unwrap();
            assert_eq!(parsed, v);
        }
    }

    // ======================================================================
    // DispatchError tests
    // ======================================================================

    #[test]
    fn dispatch_error_unknown_message_type_display() {
        let err = DispatchError::UnknownMessageType {
            namespace: "mesh".into(),
            msg_type: "device:explode".into(),
        };
        let msg = format!("{}", err);
        assert!(msg.contains("device:explode"));
        assert!(msg.contains("mesh"));
    }

    #[test]
    fn dispatch_error_payload_deserialize_display() {
        let json_err = serde_json::from_str::<String>("not-json").unwrap_err();
        let err = DispatchError::PayloadDeserialize(json_err);
        let msg = format!("{}", err);
        assert!(msg.contains("payload deserialization failed"));
    }

    // ======================================================================
    // MeshPayload tests
    // ======================================================================

    #[test]
    fn mesh_payload_parse_device_announce() {
        let payload = serde_json::json!({
            "device": {
                "id": "dev-1",
                "type": "desktop",
                "name": "My Laptop",
                "tailscaleHostname": "app-desktop-dev1",
                "status": "online",
                "capabilities": ["mesh", "sync"]
            },
            "protocolVersion": 3
        });
        let result = MeshPayload::parse("device-announce", payload).unwrap();
        assert!(matches!(result, MeshPayload::DeviceAnnounce(_)));
        assert_eq!(result.message_type(), MeshMessageType::DeviceAnnounce);
    }

    #[test]
    fn mesh_payload_parse_device_announce_legacy_name() {
        let payload = serde_json::json!({
            "device": {
                "id": "dev-1",
                "type": "desktop",
                "name": "My Laptop",
                "tailscaleHostname": "app-desktop-dev1",
                "status": "online",
                "capabilities": []
            }
        });
        let result = MeshPayload::parse("device:announce", payload).unwrap();
        assert!(matches!(result, MeshPayload::DeviceAnnounce(_)));
    }

    #[test]
    fn mesh_payload_parse_device_list() {
        let payload = serde_json::json!({
            "devices": [
                {
                    "id": "dev-1",
                    "type": "desktop",
                    "name": "Laptop",
                    "tailscaleHostname": "app-desktop-dev1",
                    "status": "online",
                    "capabilities": []
                }
            ],
            "primaryId": "dev-1"
        });
        let result = MeshPayload::parse("device-list", payload).unwrap();
        assert!(matches!(result, MeshPayload::DeviceList(_)));
        assert_eq!(result.message_type(), MeshMessageType::DeviceList);
    }

    #[test]
    fn mesh_payload_parse_device_goodbye() {
        let payload = serde_json::json!({
            "deviceId": "dev-1",
            "reason": "shutdown"
        });
        let result = MeshPayload::parse("device-goodbye", payload).unwrap();
        assert!(matches!(result, MeshPayload::DeviceGoodbye(_)));
        assert_eq!(result.message_type(), MeshMessageType::DeviceGoodbye);
    }

    #[test]
    fn mesh_payload_parse_device_goodbye_legacy() {
        let payload = serde_json::json!({
            "deviceId": "dev-1",
            "reason": "crash"
        });
        let result = MeshPayload::parse("device:goodbye", payload).unwrap();
        assert!(matches!(result, MeshPayload::DeviceGoodbye(_)));
    }

    #[test]
    fn mesh_payload_parse_election_start() {
        let result = MeshPayload::parse("election-start", serde_json::json!(null)).unwrap();
        assert!(matches!(result, MeshPayload::ElectionStart));
        assert_eq!(result.message_type(), MeshMessageType::ElectionStart);
    }

    #[test]
    fn mesh_payload_parse_election_start_legacy() {
        let result = MeshPayload::parse("election:start", serde_json::json!({})).unwrap();
        assert!(matches!(result, MeshPayload::ElectionStart));
    }

    #[test]
    fn mesh_payload_parse_election_candidate() {
        let payload = serde_json::json!({
            "deviceId": "dev-1",
            "uptime": 120000,
            "userDesignated": false
        });
        let result = MeshPayload::parse("election-candidate", payload).unwrap();
        assert!(matches!(result, MeshPayload::ElectionCandidate(_)));
        assert_eq!(result.message_type(), MeshMessageType::ElectionCandidate);
    }

    #[test]
    fn mesh_payload_parse_election_result() {
        let payload = serde_json::json!({
            "newPrimaryId": "dev-2",
            "reason": "election"
        });
        let result = MeshPayload::parse("election-result", payload).unwrap();
        assert!(matches!(result, MeshPayload::ElectionResult(_)));
        assert_eq!(result.message_type(), MeshMessageType::ElectionResult);
    }

    #[test]
    fn mesh_payload_parse_route_message() {
        let payload = serde_json::json!({
            "targetDeviceId": "dev-3",
            "envelope": {"namespace": "sync", "type": "sync-full", "payload": {}}
        });
        let result = MeshPayload::parse("route-message", payload).unwrap();
        assert!(matches!(result, MeshPayload::RouteMessage(_)));
        assert_eq!(result.message_type(), MeshMessageType::RouteMessage);
    }

    #[test]
    fn mesh_payload_parse_route_broadcast() {
        let payload = serde_json::json!({
            "envelope": {"namespace": "sync", "type": "sync-update", "payload": {}}
        });
        let result = MeshPayload::parse("route-broadcast", payload).unwrap();
        assert!(matches!(result, MeshPayload::RouteBroadcast(_)));
        assert_eq!(result.message_type(), MeshMessageType::RouteBroadcast);
    }

    #[test]
    fn mesh_payload_parse_route_message_legacy() {
        let payload = serde_json::json!({
            "targetDeviceId": "dev-3",
            "envelope": {"namespace": "sync", "type": "sync-full", "payload": {}}
        });
        let result = MeshPayload::parse("route:message", payload).unwrap();
        assert!(matches!(result, MeshPayload::RouteMessage(_)));
    }

    #[test]
    fn mesh_payload_parse_unknown_type() {
        let result = MeshPayload::parse("device:explode", serde_json::json!({}));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, DispatchError::UnknownMessageType { .. }));
    }

    #[test]
    fn mesh_payload_parse_bad_payload() {
        // device-announce expects a specific payload shape
        let result = MeshPayload::parse("device-announce", serde_json::json!("not-an-object"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, DispatchError::PayloadDeserialize(_)));
    }

    #[test]
    fn mesh_payload_parse_device_announce_missing_fields() {
        let payload = serde_json::json!({
            "device": {"id": "dev-1"}
            // Missing required fields: type, name, tailscaleHostname, status, capabilities
        });
        let result = MeshPayload::parse("device-announce", payload);
        assert!(result.is_err());
    }

    #[test]
    fn mesh_payload_message_type_all_variants() {
        // Ensure message_type() returns the correct variant for each payload type
        let announce = MeshPayload::parse(
            "device-announce",
            serde_json::json!({
                "device": {
                    "id": "d", "type": "desktop", "name": "N",
                    "tailscaleHostname": "h", "status": "online", "capabilities": []
                }
            }),
        )
        .unwrap();
        assert_eq!(announce.message_type(), MeshMessageType::DeviceAnnounce);

        let election_start =
            MeshPayload::parse("election-start", serde_json::json!(null)).unwrap();
        assert_eq!(
            election_start.message_type(),
            MeshMessageType::ElectionStart
        );
    }

    // ======================================================================
    // MessagePack roundtrip tests for message type enums
    // ======================================================================

    #[test]
    fn mesh_message_type_msgpack_roundtrip() {
        let variants = [
            MeshMessageType::DeviceAnnounce,
            MeshMessageType::DeviceList,
            MeshMessageType::DeviceGoodbye,
            MeshMessageType::ElectionStart,
            MeshMessageType::ElectionCandidate,
            MeshMessageType::ElectionResult,
            MeshMessageType::RouteMessage,
            MeshMessageType::RouteBroadcast,
        ];
        for v in variants {
            let packed = rmp_serde::to_vec_named(&v).unwrap();
            let parsed: MeshMessageType = rmp_serde::from_slice(&packed).unwrap();
            assert_eq!(parsed, v, "Msgpack roundtrip failed for {v:?}");
        }
    }

    #[test]
    fn sync_message_type_msgpack_roundtrip() {
        let variants = [
            SyncMessageType::SyncFull,
            SyncMessageType::SyncUpdate,
            SyncMessageType::SyncRequest,
            SyncMessageType::SyncClear,
        ];
        for v in variants {
            let packed = rmp_serde::to_vec_named(&v).unwrap();
            let parsed: SyncMessageType = rmp_serde::from_slice(&packed).unwrap();
            assert_eq!(parsed, v, "Msgpack roundtrip failed for {v:?}");
        }
    }

    #[test]
    fn file_transfer_message_type_msgpack_roundtrip() {
        let variants = [
            FileTransferMessageType::FileOffer,
            FileTransferMessageType::FileAccept,
            FileTransferMessageType::FileReject,
            FileTransferMessageType::FileCancel,
        ];
        for v in variants {
            let packed = rmp_serde::to_vec_named(&v).unwrap();
            let parsed: FileTransferMessageType = rmp_serde::from_slice(&packed).unwrap();
            assert_eq!(parsed, v, "Msgpack roundtrip failed for {v:?}");
        }
    }
}
