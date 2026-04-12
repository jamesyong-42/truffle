//! Types for the CrdtDoc subsystem.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// base64_vec — serde helper for Vec<u8> ↔ base64 string
// ---------------------------------------------------------------------------

mod base64_vec {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::{self, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &Vec<u8>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let encoded = STANDARD.encode(bytes);
        serializer.serialize_str(&encoded)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        STANDARD.decode(&s).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// CrdtSyncMessage — wire protocol for CRDT document synchronization
// ---------------------------------------------------------------------------

/// Wire protocol messages sent on namespace `"crdt:{doc_id}"`.
///
/// Binary payloads (Loro exports) are encoded as base64 strings for JSON
/// transport over the existing envelope system.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum CrdtSyncMessage {
    /// An incremental update (from a local change).
    Update {
        #[serde(with = "base64_vec")]
        data: Vec<u8>,
    },

    /// Request synchronization — includes our version vector so the peer
    /// can compute a delta.
    SyncRequest {
        #[serde(with = "base64_vec")]
        version_vector: Vec<u8>,
    },

    /// Response to a SyncRequest — incremental updates since the requester's
    /// version vector.
    SyncResponse {
        #[serde(with = "base64_vec")]
        data: Vec<u8>,
    },

    /// A full snapshot of the document.
    Snapshot {
        #[serde(with = "base64_vec")]
        data: Vec<u8>,
    },
}

// ---------------------------------------------------------------------------
// CrdtDocEvent — change events emitted to subscribers
// ---------------------------------------------------------------------------

/// Events emitted by a [`CrdtDoc`](super::CrdtDoc).
#[derive(Debug, Clone)]
pub enum CrdtDocEvent {
    /// A local change was applied and broadcast.
    LocalChange,

    /// A remote change was received and applied.
    RemoteChange { from: String },

    /// A peer completed sync (SyncResponse or Snapshot received).
    PeerSynced { peer_id: String },

    /// A peer left the network.
    PeerLeft { peer_id: String },
}

// ---------------------------------------------------------------------------
// CrdtDocError — error type
// ---------------------------------------------------------------------------

/// Errors from the CrdtDoc subsystem.
#[derive(Debug, thiserror::Error)]
pub enum CrdtDocError {
    /// Error from the Loro CRDT engine.
    #[error("loro error: {0}")]
    Loro(String),

    /// Failed to encode CRDT data.
    #[error("encode error: {0}")]
    Encode(String),

    /// Failed to decode CRDT data.
    #[error("decode error: {0}")]
    Decode(String),

    /// Persistence backend error.
    #[error("backend error: {0}")]
    Backend(String),
}
