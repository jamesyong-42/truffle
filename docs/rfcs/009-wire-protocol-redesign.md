# RFC 009: Wire Protocol Redesign

**Status**: Proposed
**Created**: 2026-03-19
**Depends on**: RFC 003 (Rust Rewrite), RFC 005 (Refactor), RFC 008 (Vision)
**Supersedes**: Wire protocol defined in RFC 008 Appendix C (Mesh Envelope section)

---

## 1. Problem Statement

A comprehensive messaging design review identified 14 structural issues in the current wire protocol. These issues span frame discrimination, codec consistency, type safety, validation, and error handling. Left unaddressed, they create an expanding attack surface and make the protocol fragile as new namespaces and message types are added.

### Issue Catalog

| # | Issue | Severity | Layer |
|---|-------|----------|-------|
| 1 | Heartbeats share wire format with envelopes -- discrimination is a negative-space heuristic (`!has("namespace")`) | High | Transport |
| 2 | Dead `codec.rs` uses `to_vec_named` while live `websocket.rs` uses `to_vec` -- incompatible msgpack encoding | High | Codec |
| 3 | Three different `"type"` fields at different nesting levels (heartbeat, envelope, message) | Medium | Protocol |
| 4 | All messages decode to `serde_json::Value` before typing -- delays validation | High | Dispatch |
| 5 | Silent message drop on `MeshEnvelope` parse failure in `handler.rs` (`Err(_) => return`) | High | Handler |
| 6 | Text frame path has no shared validation with binary path | Medium | Transport |
| 7 | Unbounded envelope nesting in `route:broadcast` -- no depth guard | High | Routing |
| 8 | `route:broadcast` local delivery doesn't filter mesh namespace -- could bypass protocol | High | Routing |
| 9 | Mesh message types are unvalidated strings with `_ => {}` fallback | Medium | Dispatch |
| 10 | All payloads are `serde_json::Value` with late validation | Medium | Types |
| 11 | `unwrap_or_default()` hides serialization failures | Medium | Error handling |
| 12 | Inconsistent message type naming (`store:sync:full` vs `OFFER` vs `device:announce`) | Low | Naming |
| 13 | Pong always sent as msgpack regardless of sender format | Low | Transport |
| 14 | Flags mask ignores unused bits (only checks bits 1-2, ignores bit 0 and bits 3-7) | Low | Codec |

### Root Cause

The current protocol evolved from a TypeScript implementation where messages are JSON objects and discrimination happens by inspecting fields at runtime. Rust's type system can enforce these invariants at compile time, but the current code operates at the `serde_json::Value` level everywhere, throwing away the type safety that Rust provides.

---

## 2. Design Principles

1. **Parse, don't validate.** Bytes on the wire should decode into a Rust enum, not into `serde_json::Value` that is then inspected. If it decodes, it is valid.

2. **Structural discrimination, not content inspection.** Frame types are distinguished by a type byte in the header, not by the presence or absence of JSON fields.

3. **One codec.** A single `encode`/`decode` path. The dead `codec.rs` is removed. `websocket.rs` is the canonical codec.

4. **Fail loudly.** Every unrecognized or malformed message is logged with `tracing::warn!`. No `_ => {}` in match arms without a log statement.

5. **Bounded nesting.** Route envelopes carry exactly one inner envelope. No recursive nesting.

6. **Compiler-checked dispatch.** Message types are Rust enums. Adding a new message type without handling it is a compile error (exhaustive match).

7. **Backward compatibility window.** Old and new nodes coexist during migration via a version handshake and dual-format acceptance period.

8. **Consistent naming.** All message types across all namespaces use `kebab-case` (e.g., `device-announce`, `sync-full`, `file-offer`).

---

## 3. Wire Format

### 3.1 Binary Framing

Every WebSocket **binary** message begins with a 2-byte header:

```
Byte 0: Frame type
Byte 1: Flags
Bytes 2..N: Payload (type-dependent encoding)
```

**Frame type byte (byte 0):**

| Value | Name | Description |
|-------|------|-------------|
| `0x01` | Control | Heartbeat ping/pong, handshake, protocol errors |
| `0x02` | Data | MeshEnvelope (application messages, routing) |
| `0x03` | Error | Protocol-level error notification |

This replaces the current scheme where byte 0 is a format flag and heartbeats are indistinguishable from envelopes at the binary level.

**Flags byte (byte 1):**

| Bit | Meaning |
|-----|---------|
| 0 | Encoding: `0` = MessagePack, `1` = JSON |
| 1 | Reserved (must be 0) |
| 2 | Reserved (must be 0) |
| 3 | Reserved (must be 0) |
| 4 | Reserved (must be 0) |
| 5 | Reserved (must be 0) |
| 6 | Reserved (must be 0) |
| 7 | Reserved (must be 0) |

Reserved bits MUST be zero. A receiver MUST reject frames where reserved bits are nonzero (solves issue #14).

**Rust types:**

```rust
/// Frame type discriminant -- first byte of every binary WS message.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameType {
    Control = 0x01,
    Data    = 0x02,
    Error   = 0x03,
}

impl TryFrom<u8> for FrameType {
    type Error = WireError;
    fn try_from(b: u8) -> Result<Self, WireError> {
        match b {
            0x01 => Ok(Self::Control),
            0x02 => Ok(Self::Data),
            0x03 => Ok(Self::Error),
            other => Err(WireError::UnknownFrameType(other)),
        }
    }
}

/// Flags byte -- second byte of every binary WS message.
#[derive(Debug, Clone, Copy)]
pub struct Flags(u8);

impl Flags {
    const ENCODING_MASK: u8 = 0x01;     // bit 0
    const RESERVED_MASK: u8 = 0xFE;     // bits 1-7

    pub fn new(use_json: bool) -> Self {
        Self(if use_json { 1 } else { 0 })
    }

    pub fn from_byte(b: u8) -> Result<Self, WireError> {
        if b & Self::RESERVED_MASK != 0 {
            return Err(WireError::ReservedBitsSet(b));
        }
        Ok(Self(b))
    }

    pub fn is_json(&self) -> bool {
        self.0 & Self::ENCODING_MASK != 0
    }
}
```

### 3.2 Text Frame Handling

Text frames are accepted **only during the migration period** (see Section 11). They are parsed as JSON and routed through the same validation pipeline as binary JSON frames. The text frame path calls the same `decode_data_frame` function, just without reading the 2-byte header:

```rust
fn decode_text_frame(text: &str) -> Result<WireFrame, WireError> {
    let value: serde_json::Value = serde_json::from_str(text)?;

    // Legacy heartbeat detection (migration only)
    if is_legacy_heartbeat(&value) {
        return decode_legacy_heartbeat(&value);
    }

    // Must be a MeshEnvelope
    let envelope: MeshEnvelope = serde_json::from_value(value)?;
    Ok(WireFrame::Data(envelope))
}
```

After the migration period, text frames are rejected with a logged warning.

### 3.3 Maximum Message Size

Unchanged: 16 MB (`MAX_MESSAGE_SIZE = 16 * 1024 * 1024`).

---

## 4. Frame Types

### 4.1 Control Frames (`0x01`)

Control frames carry protocol-level messages that are **not** routed through the message bus. They are consumed by the transport layer.

**Control frame payload:**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ControlMessage {
    /// Heartbeat ping.
    Ping {
        timestamp: u64,
    },
    /// Heartbeat pong (response to ping).
    Pong {
        timestamp: u64,
        echo_timestamp: u64,
    },
    /// Protocol version handshake (sent on connection open).
    Handshake {
        protocol_version: u32,
        device_id: String,
        /// Capabilities this node supports.
        capabilities: Vec<String>,
    },
    /// Handshake acknowledgment.
    HandshakeAck {
        protocol_version: u32,
        device_id: String,
        /// Negotiated protocol version (min of both sides).
        negotiated_version: u32,
    },
}
```

The `#[serde(tag = "type")]` attribute means the discriminant is a `"type"` field in the serialized form, but it is **only** within control frames. There is no ambiguity with the `"type"` field in `MeshEnvelope` or `MeshMessage` because they live in different frame types (byte 0 = `0x01` vs `0x02`). This solves issue #3 (three `"type"` fields at different levels) by making each `"type"` field unambiguous within its frame type.

**Heartbeat flow (solves issues #1 and #13):**

1. Ping is encoded as `[0x01, flags, msgpack/json(ControlMessage::Ping)]`.
2. Pong is encoded using the **same encoding** as the received ping (the `flags` byte is echoed). This solves issue #13.
3. The transport layer reads byte 0, sees `0x01` (Control), and dispatches to the heartbeat handler. It never touches the envelope parser.

### 4.2 Data Frames (`0x02`)

Data frames carry `MeshEnvelope` messages. They flow through the mesh routing and message bus systems.

```
[0x02] [flags] [msgpack or json encoded MeshEnvelope]
```

See Section 5 for the new `MeshEnvelope` format.

### 4.3 Error Frames (`0x03`)

Error frames notify the peer of protocol-level problems. They are informational -- the sender may close the connection after sending an error frame.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProtocolError {
    /// Machine-readable error code.
    pub code: ErrorCode,
    /// Human-readable description.
    pub message: String,
    /// Whether the sender will close the connection.
    pub fatal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    /// Unrecognized frame type byte.
    UnknownFrameType,
    /// Reserved bits set in flags byte.
    InvalidFlags,
    /// Payload failed to deserialize.
    MalformedPayload,
    /// Unknown namespace in envelope.
    UnknownNamespace,
    /// Unknown message type in envelope.
    UnknownMessageType,
    /// Protocol version mismatch.
    VersionMismatch,
    /// Message too large.
    MessageTooLarge,
    /// Rate limit exceeded.
    RateLimited,
}
```

---

## 5. Envelope Format

The new `MeshEnvelope` uses typed enums for namespace and message type instead of raw strings.

### 5.1 MeshEnvelope

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeshEnvelope {
    /// Message namespace.
    pub namespace: Namespace,
    /// Message type within namespace (string on the wire for extensibility).
    #[serde(rename = "type")]
    pub msg_type: String,
    /// Typed payload. Validated during deserialization.
    pub payload: serde_json::Value,
    /// Source device ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Target device ID (for routed messages).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Timestamp (ms since epoch).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<u64>,
    /// Correlation ID for request/response matching.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
}
```

**Key change:** The `from`, `to`, `correlation_id`, and `timestamp` fields move from `MeshMessage` (the inner payload for mesh-namespace messages) into the envelope itself. This eliminates the double-nesting where `MeshEnvelope` contains a `MeshMessage` which duplicates addressing fields.

### 5.2 Namespace

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Namespace {
    /// Internal mesh protocol: device discovery, election, routing.
    Mesh,
    /// Store synchronization.
    Sync,
    /// File transfer signaling.
    FileTransfer,
    /// Application-defined namespace (extensibility escape hatch).
    #[serde(untagged)]
    Custom(String),
}
```

The `Custom(String)` variant uses `#[serde(untagged)]` so that any string not matching a known variant deserializes as `Custom`. Known namespaces get compile-time checking; custom namespaces still work for application-defined pub/sub.

**Wire representation:** `"mesh"`, `"sync"`, `"file-transfer"`, or any other string.

### 5.3 Why `msg_type` Remains a String

The message type within a namespace remains a `String` on the wire rather than an enum in the envelope. This is deliberate:

1. **Namespace extensibility.** Custom namespaces define their own message types. The envelope cannot enumerate them.
2. **Version independence.** A node running v2.1 may send message types that a v2.0 node does not know about. A string allows graceful unknown-type handling.
3. **Type safety happens at dispatch.** Each namespace handler parses the string into its own enum (see Section 6). The envelope carries the string; the handler validates it.

---

## 6. Message Type System

Each known namespace defines a Rust enum for its message types. Dispatch converts the string `msg_type` into the enum at the handler boundary.

### 6.1 Mesh Namespace Messages

```rust
/// Message types for the mesh namespace.
/// Wire strings use kebab-case: "device-announce", "election-start", etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    pub fn from_str(s: &str) -> Option<Self> {
        // Accepts both new kebab-case and legacy colon-separated forms
        match s {
            "device-announce" | "device:announce" => Some(Self::DeviceAnnounce),
            "device-list"     | "device:list"     => Some(Self::DeviceList),
            "device-goodbye"  | "device:goodbye"  => Some(Self::DeviceGoodbye),
            "election-start"  | "election:start"  => Some(Self::ElectionStart),
            "election-candidate" | "election:candidate" => Some(Self::ElectionCandidate),
            "election-result" | "election:result"  => Some(Self::ElectionResult),
            "route-message"   | "route:message"    => Some(Self::RouteMessage),
            "route-broadcast" | "route:broadcast"  => Some(Self::RouteBroadcast),
            _ => None,
        }
    }
}
```

### 6.2 Typed Payloads per Message Type

Each mesh message type maps to a strongly typed payload:

```rust
/// A fully parsed mesh-namespace message.
/// The payload is decoded into the correct type at dispatch time.
#[derive(Debug, Clone)]
pub enum MeshPayload {
    DeviceAnnounce(DeviceAnnouncePayload),
    DeviceList(DeviceListPayload),
    DeviceGoodbye(DeviceGoodbyePayload),
    ElectionStart,  // no payload
    ElectionCandidate(ElectionCandidatePayload),
    ElectionResult(ElectionResultPayload),
    RouteMessage(RouteMessagePayload),
    RouteBroadcast(RouteBroadcastPayload),
}

impl MeshPayload {
    /// Parse a mesh message type string and raw payload into a typed MeshPayload.
    /// Returns an error if the type is unknown or the payload does not match.
    pub fn parse(msg_type: &str, payload: serde_json::Value) -> Result<Self, DispatchError> {
        let typ = MeshMessageType::from_str(msg_type)
            .ok_or_else(|| DispatchError::UnknownMessageType {
                namespace: "mesh".into(),
                msg_type: msg_type.into(),
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
            MeshMessageType::ElectionStart => {
                Ok(Self::ElectionStart)
            }
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
}
```

### 6.3 Sync Namespace Messages

```rust
/// Message types for the sync namespace.
/// Wire strings: "sync-full", "sync-update", "sync-request", "sync-clear".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SyncMessageType {
    SyncFull,
    SyncUpdate,
    SyncRequest,
    SyncClear,
}

impl SyncMessageType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "sync-full"    | "store:sync:full"    => Some(Self::SyncFull),
            "sync-update"  | "store:sync:update"  => Some(Self::SyncUpdate),
            "sync-request" | "store:sync:request"  => Some(Self::SyncRequest),
            "sync-clear"   | "store:sync:clear"    => Some(Self::SyncClear),
            _ => None,
        }
    }
}
```

### 6.4 File Transfer Namespace Messages

```rust
/// Message types for the file-transfer namespace.
/// Wire strings: "file-offer", "file-accept", "file-reject", "file-cancel".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileTransferMessageType {
    FileOffer,
    FileAccept,
    FileReject,
    FileCancel,
}

impl FileTransferMessageType {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "file-offer"  | "OFFER"  => Some(Self::FileOffer),
            "file-accept" | "ACCEPT" => Some(Self::FileAccept),
            "file-reject" | "REJECT" => Some(Self::FileReject),
            "file-cancel" | "CANCEL" => Some(Self::FileCancel),
            _ => None,
        }
    }
}
```

### 6.5 Naming Convention (Solves Issue #12)

All message types across all namespaces use **kebab-case**:

| Old | New |
|-----|-----|
| `device:announce` | `device-announce` |
| `device:list` | `device-list` |
| `election:start` | `election-start` |
| `route:message` | `route-message` |
| `store:sync:full` | `sync-full` |
| `store:sync:update` | `sync-update` |
| `OFFER` | `file-offer` |
| `ACCEPT` | `file-accept` |
| `REJECT` | `file-reject` |
| `CANCEL` | `file-cancel` |

The `from_str` methods accept both old and new forms during the migration period.

---

## 7. Dispatch Chain

Step-by-step processing from WebSocket frame to handler.

### 7.1 Read Pump (Layer 0: Transport)

```
WebSocket binary frame received
    |
    v
[Size check] -- reject if > MAX_MESSAGE_SIZE
    |
    v
[Read byte 0] -- FrameType::try_from(data[0])
    |
    +-- Err => tracing::warn!, send Error frame, continue
    |
    v
[Read byte 1] -- Flags::from_byte(data[1])
    |
    +-- Err (reserved bits) => tracing::warn!, send Error frame, continue
    |
    v
[Decode payload bytes 2..N based on FrameType]
    |
    +-- FrameType::Control => decode ControlMessage
    |       |
    |       +-- Ping => send Pong (same encoding), record activity
    |       +-- Pong => record activity, compute RTT
    |       +-- Handshake => negotiate version, emit DeviceIdentified
    |       +-- HandshakeAck => store negotiated version
    |
    +-- FrameType::Data => decode MeshEnvelope
    |       |
    |       v
    |   [Forward to message dispatcher]
    |
    +-- FrameType::Error => log, optionally close connection
```

### 7.2 Message Dispatcher (Layer 1: Protocol)

```
MeshEnvelope received from read pump
    |
    v
[Match envelope.namespace]
    |
    +-- Namespace::Mesh => MeshHandler::dispatch(envelope)
    |       |
    |       v
    |   [MeshMessageType::from_str(envelope.msg_type)]
    |       |
    |       +-- None => tracing::warn!("unknown mesh type: {}", msg_type)
    |       |
    |       +-- Some(typ) => MeshPayload::parse(msg_type, payload)
    |               |
    |               +-- Err => tracing::warn!("payload validation failed: {}", err)
    |               |
    |               +-- Ok(MeshPayload::DeviceAnnounce(p)) => handle_device_announce(from, p)
    |               +-- Ok(MeshPayload::DeviceList(p))     => handle_device_list(from, p)
    |               +-- Ok(MeshPayload::RouteMessage(p))   => handle_route_message(from, p)
    |               +-- Ok(MeshPayload::RouteBroadcast(p)) => handle_route_broadcast(from, p)
    |               +-- ... (exhaustive match -- compiler enforced)
    |
    +-- Namespace::Sync => SyncHandler::dispatch(envelope)
    |       |
    |       v
    |   [SyncMessageType::from_str(envelope.msg_type)]
    |       +-- dispatch to typed handler methods
    |
    +-- Namespace::FileTransfer => FileTransferHandler::dispatch(envelope)
    |       |
    |       v
    |   [FileTransferMessageType::from_str(envelope.msg_type)]
    |       +-- dispatch to typed handler methods
    |
    +-- Namespace::Custom(ns) => MessageBus::dispatch(envelope)
            |
            v
        [Forward to registered subscribers -- no type validation]
```

### 7.3 Key Invariants

1. **No `serde_json::Value` passthrough.** By the time a handler method is called, the payload is a strongly typed Rust struct. The only exception is `Custom` namespace messages, which pass through to the message bus as `serde_json::Value` because the application defines their shape.

2. **No silent drops.** Every `Err` path in the chain produces a `tracing::warn!` log. The current `Err(_) => return` in `handle_message` (issue #5) and `_ => {}` in `dispatch_mesh_message` (issue #9) are replaced with explicit logging.

3. **Early termination on malformed input.** A frame with bad header bytes is rejected at Layer 0. A frame with a valid header but malformed payload is rejected at Layer 1. Neither reaches the application handlers.

---

## 8. Routing

### 8.1 STAR Topology (Unchanged)

The STAR routing model is unchanged: secondaries send `route-message` and `route-broadcast` to the primary, which forwards to the target(s).

### 8.2 RouteMessagePayload (Revised)

```rust
/// Payload for route-message: route an envelope to a specific device via primary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteMessagePayload {
    /// Target device ID.
    pub target_device_id: String,
    /// The inner envelope to deliver. This is a MeshEnvelope, not raw JSON.
    pub envelope: MeshEnvelope,
}
```

**Key change:** The inner `envelope` is typed as `MeshEnvelope`, not `serde_json::Value`. This means the primary **validates** the inner envelope during deserialization. A malformed inner envelope fails to parse at the routing layer, not silently downstream.

### 8.3 RouteBroadcastPayload (Revised)

```rust
/// Payload for route-broadcast: broadcast an envelope to all devices via primary.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteBroadcastPayload {
    /// The inner envelope to broadcast.
    pub envelope: MeshEnvelope,
}
```

### 8.4 Bounded Nesting (Solves Issue #7)

The inner envelope in `RouteMessagePayload` and `RouteBroadcastPayload` is a `MeshEnvelope`, which contains a `serde_json::Value` payload. However, the routing handler enforces:

1. **The inner envelope's namespace MUST NOT be `Namespace::Mesh`** (solves issue #8). A `route-broadcast` carrying a `mesh`-namespace inner envelope would allow a secondary to inject fake `device-announce` or `election-result` messages into the mesh. The primary rejects this:

```rust
fn handle_route_broadcast(&self, from: &str, payload: RouteBroadcastPayload) {
    if payload.envelope.namespace == Namespace::Mesh {
        tracing::warn!(
            "Rejected route:broadcast from {from} with mesh-namespace inner envelope \
             (type: {}). Mesh messages cannot be routed via broadcast.",
            payload.envelope.msg_type
        );
        return;
    }
    // ... forward to other connections and deliver locally
}
```

2. **The inner envelope MUST NOT itself be a `route-message` or `route-broadcast`** (prevents recursive nesting):

```rust
fn validate_inner_envelope(envelope: &MeshEnvelope) -> Result<(), DispatchError> {
    if envelope.namespace == Namespace::Mesh {
        if let Some(typ) = MeshMessageType::from_str(&envelope.msg_type) {
            if matches!(typ, MeshMessageType::RouteMessage | MeshMessageType::RouteBroadcast) {
                return Err(DispatchError::NestedRouting);
            }
        }
    }
    Ok(())
}
```

This guarantees exactly one level of nesting: an outer route envelope containing one inner data envelope. No deeper.

---

## 9. Validation Rules

### 9.1 Layer 0: Transport (read pump)

| Check | Action on failure |
|-------|-------------------|
| Binary frame size > MAX_MESSAGE_SIZE | `warn!`, close connection |
| Empty binary frame (< 2 bytes) | `warn!`, skip frame |
| Unknown frame type byte | `warn!`, send Error frame, skip |
| Reserved bits set in flags byte | `warn!`, send Error frame, skip |
| Payload deserialization fails | `warn!`, skip frame |
| Text frame (post-migration) | `warn!`, skip frame |

### 9.2 Layer 1: Protocol (message dispatcher)

| Check | Action on failure |
|-------|-------------------|
| Unknown namespace (not a known enum variant, and not a valid Custom string) | Forward to message bus as Custom, `debug!` log |
| Unknown message type within known namespace | `warn!`, skip |
| Payload does not match expected schema for message type | `warn!`, skip |
| Missing required fields (e.g., `from` on mesh messages) | `warn!`, skip |

### 9.3 Layer 2: Routing (primary only)

| Check | Action on failure |
|-------|-------------------|
| Non-primary receives route envelope | `warn!`, skip |
| Inner envelope has mesh namespace | `warn!`, reject (issue #8) |
| Inner envelope is itself a route envelope | `warn!`, reject (issue #7) |
| Target device not connected (route-message) | `warn!`, skip (device offline) |

### 9.4 Layer 3: Application handlers

| Check | Action on failure |
|-------|-------------------|
| Typed payload validation (e.g., DeviceAnnounce with empty device_id) | `warn!`, skip |
| State-dependent validation (e.g., election result from non-participant) | `warn!`, skip |

---

## 10. Error Handling

### 10.1 Principle: Never Panic, Never Silently Swallow

Every error path follows this pattern:

```rust
// BAD (current code):
let envelope: MeshEnvelope = match serde_json::from_value(payload.clone()) {
    Ok(e) => e,
    Err(_) => return,  // SILENT DROP -- issue #5
};

// GOOD (new code):
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
```

### 10.2 `unwrap_or_default()` Audit (Solves Issue #11)

Every `unwrap_or_default()` on a serialization result is replaced with explicit error handling:

```rust
// BAD (current code):
payload: serde_json::to_value(message).unwrap_or_default(),

// GOOD (new code):
let payload = match serde_json::to_value(message) {
    Ok(v) => v,
    Err(e) => {
        tracing::error!(error = %e, "Failed to serialize mesh message");
        return;
    }
};
```

### 10.3 Error Frame Responses

When the transport layer rejects a frame due to a structural error (bad frame type, reserved bits, etc.), it sends an Error frame (`0x03`) back to the peer before discarding the message. This gives the sender diagnostic information:

```rust
async fn send_protocol_error(
    write_tx: &mpsc::Sender<Vec<u8>>,
    code: ErrorCode,
    message: &str,
    fatal: bool,
    use_json: bool,
) {
    let error = ProtocolError {
        code,
        message: message.to_string(),
        fatal,
    };
    if let Ok(payload) = encode_payload(&error, use_json) {
        let mut frame = Vec::with_capacity(2 + payload.len());
        frame.push(FrameType::Error as u8);
        frame.push(Flags::new(use_json).0);
        frame.extend_from_slice(&payload);
        let _ = write_tx.send(frame).await;
    }
}
```

---

## 11. Migration Plan

### 11.1 Protocol Versioning

The protocol version is negotiated during the handshake:

| Version | Description |
|---------|-------------|
| 2 | Current protocol (pre-RFC 009). No frame type byte. Heartbeats are JSON objects. |
| 3 | RFC 009 protocol. 2-byte header. Typed frames. |

### 11.2 Handshake Sequence

On connection open, both sides send a `Handshake` control frame (or, if the peer is v2, a legacy JSON handshake):

```
Node A (v3) ----[0x01][flags][Handshake{version:3}]----> Node B (v3)
Node B (v3) ----[0x01][flags][HandshakeAck{version:3,negotiated:3}]----> Node A

Node A (v3) ----[0x01][flags][Handshake{version:3}]----> Node C (v2)
Node C (v2) (does not understand 0x01 frame, ignores or disconnects)
```

**Fallback for v2 peers:** If a v3 node sends a handshake and gets no response within 2 seconds (one heartbeat interval), it falls back to v2 mode for that connection:

- Send heartbeats as bare `{"type":"ping"}` JSON (no frame header)
- Accept both framed and unframed messages
- Emit envelopes with the old naming convention

### 11.3 Phased Rollout

**Phase A: Accept Both, Send New (2 weeks)**

All nodes are upgraded to understand v3 frames. They accept both v2 (legacy JSON) and v3 (framed binary) messages. They send v3 to peers that completed a v3 handshake, and v2 to peers that did not.

During this phase:
- `from_str` methods accept both old and new naming conventions
- Text frames are still accepted (with a deprecation warning log)
- The dead `codec.rs` is removed
- `websocket.rs` gains the new 2-byte header encode/decode

**Phase B: Send New Only (2 weeks)**

All nodes send v3 frames exclusively. V2 acceptance remains for any stragglers. Text frame acceptance is logged as a warning.

**Phase C: Remove Legacy (permanent)**

V2 code paths are removed:
- `from_str` methods only accept kebab-case
- Text frames are rejected
- The `is_legacy_heartbeat` function is removed
- Protocol version is hardcoded to 3

### 11.4 Feature Flag

The new protocol is gated behind a feature or runtime flag during Phase A:

```rust
/// If true, send v3 framed messages to all connections.
/// If false (default during Phase A), only send v3 to connections
/// that completed a v3 handshake.
pub send_v3: bool,
```

---

## 12. Breaking Changes

### 12.1 truffle-core (Rust library)

| Change | Impact |
|--------|--------|
| `MeshEnvelope.namespace` changes from `String` to `Namespace` enum | All code matching on `envelope.namespace == "mesh"` must change to `envelope.namespace == Namespace::Mesh` |
| `MeshEnvelope` gains `from`, `to`, `correlation_id` fields | Existing envelope construction must add these fields (or use `None`) |
| `MeshMessage` struct is removed | Handlers receive `MeshPayload` enum variants directly |
| `codec.rs` is deleted | Any code importing from `protocol::codec` must switch to `transport::websocket` |
| Heartbeat functions return `ControlMessage` instead of `serde_json::Value` | `heartbeat.rs` API changes |
| Message type strings change (kebab-case) | During migration, both forms accepted |

### 12.2 truffle-napi (Node.js bindings)

| Change | Impact |
|--------|--------|
| Event payloads change shape | `from` and `to` move from inner message to envelope |
| Message type strings change | JS code matching on `"device:announce"` must accept `"device-announce"` |
| Heartbeat is no longer visible to JS | Was never intentionally exposed, but any code inspecting raw WS messages will see different framing |

### 12.3 truffle-tauri-plugin

Same impacts as NAPI. Tauri IPC command payloads that expose `MeshEnvelope` fields need updating.

### 12.4 Electron Example / TypeScript Clients

| Change | Impact |
|--------|--------|
| Wire format adds 2-byte header | TS WebSocket code must read header before parsing payload |
| Message type naming | Match strings need updating |
| Heartbeats are binary-framed | TS heartbeat code must encode/decode with frame type byte |

The `@truffle/core` TypeScript package should be updated in lockstep with Phase A to handle both formats.

---

## 13. Implementation Plan

### Phase 1: Foundation (Non-Breaking)

**Effort:** 3-4 hours
**Breaking changes:** None (additive only)

1. Add `FrameType`, `Flags`, `ControlMessage`, `ProtocolError`, and `ErrorCode` types to `protocol/`
2. Add `Namespace` enum alongside the existing `MESH_NAMESPACE` constant
3. Add `MeshMessageType`, `SyncMessageType`, `FileTransferMessageType` enums
4. Add `MeshPayload` typed dispatch enum
5. Write unit tests for all new types (serde roundtrip, `from_str` for both old and new naming)
6. Delete `protocol/codec.rs` (it is dead code already -- `websocket.rs` is the live codec)

### Phase 2: Codec Upgrade (Non-Breaking)

**Effort:** 2-3 hours
**Breaking changes:** None (old format still accepted)

1. Update `encode_message` in `websocket.rs` to produce 2-byte header frames
2. Update `decode_message` to accept both old (1-byte header) and new (2-byte header) frames
3. Add `encode_control_frame` and `decode_control_frame` functions
4. Update heartbeat to use `ControlMessage::Ping`/`Pong` instead of raw JSON
5. Add reserved-bits validation to flags parsing
6. Update `read_pump` to dispatch on `FrameType` before payload decode

### Phase 3: Typed Dispatch (Breaking for internal APIs)

**Effort:** 4-6 hours
**Breaking changes:** Internal handler signatures change

1. Change `handle_message` in `handler.rs` to accept `MeshEnvelope` (typed namespace) instead of `serde_json::Value`
2. Replace `dispatch_mesh_message` string matching with `MeshPayload::parse()` + exhaustive match
3. Add `tracing::warn!` to every error path that was previously silent
4. Replace all `unwrap_or_default()` with explicit error handling
5. Add mesh-namespace filter to `route:broadcast` inner envelope validation
6. Add nested-routing guard to `route:message` and `route:broadcast`
7. Flatten `MeshMessage` fields into `MeshEnvelope` (remove `MeshMessage` struct)

### Phase 4: Handshake and Migration (Non-Breaking on wire)

**Effort:** 2-3 hours
**Breaking changes:** None (handshake is additive)

1. Implement `Handshake`/`HandshakeAck` flow in connection setup
2. Track per-connection protocol version in `WSConnection`
3. Send v3 frames to v3 peers, v2 frames to v2 peers
4. Add text frame deprecation warning

### Phase 5: TypeScript/NAPI Update

**Effort:** 3-4 hours
**Breaking changes:** NAPI and TS package API

1. Update `truffle-napi` to emit events with new envelope shape
2. Update `@truffle/core` TypeScript package to handle v3 framing
3. Update Electron example to use new message type names
4. Update Tauri plugin IPC commands

### Phase 6: Legacy Removal

**Effort:** 1-2 hours
**Breaking changes:** Drops v2 support

1. Remove v2 acceptance paths from codec
2. Remove legacy `from_str` aliases (colon-separated, SCREAMING_CASE)
3. Remove `is_legacy_heartbeat` and text frame acceptance
4. Remove `MeshMessage` struct if not done in Phase 3
5. Hardcode protocol version to 3

### Summary

| Phase | Effort | Breaking | Can Ship Independently |
|-------|--------|----------|------------------------|
| 1. Foundation | 3-4h | No | Yes |
| 2. Codec upgrade | 2-3h | No | Yes (after 1) |
| 3. Typed dispatch | 4-6h | Internal | Yes (after 2) |
| 4. Handshake | 2-3h | No | Yes (after 2) |
| 5. TS/NAPI update | 3-4h | NAPI/TS | Yes (after 3) |
| 6. Legacy removal | 1-2h | Wire | After Phase A window |

**Total: ~16-22 hours** across 6 phases.

---

## Appendix A: Issue Resolution Matrix

| Issue # | Description | Resolution | Phase |
|---------|-------------|------------|-------|
| 1 | Heartbeats share wire format with envelopes | Frame type byte `0x01` vs `0x02` | 2 |
| 2 | Dead `codec.rs` with incompatible encoding | Delete `codec.rs` | 1 |
| 3 | Three `"type"` fields at different nesting levels | Each frame type has its own `"type"` field, disambiguated by frame type byte; `MeshMessage` eliminated | 2, 3 |
| 4 | All messages decode to `serde_json::Value` | `MeshPayload::parse()` produces typed structs | 3 |
| 5 | Silent drop on envelope parse failure | `tracing::warn!` on every `Err` path | 3 |
| 6 | Text frame path has no shared validation | Text frames go through `decode_text_frame` which calls the same validation pipeline | 2 |
| 7 | Unbounded envelope nesting | Inner envelope cannot be a route envelope (enforced check) | 3 |
| 8 | `route:broadcast` doesn't filter mesh namespace | Inner envelope namespace != Mesh enforced at routing layer | 3 |
| 9 | Mesh message types are unvalidated strings | `MeshMessageType` enum with exhaustive match | 3 |
| 10 | Payloads are `serde_json::Value` with late validation | `MeshPayload` typed payloads; `Custom` namespace remains `Value` | 3 |
| 11 | `unwrap_or_default()` hides serialization failures | Replaced with explicit error handling + `tracing::error!` | 3 |
| 12 | Inconsistent message type naming | All kebab-case, with legacy aliases during migration | 1, 6 |
| 13 | Pong sent as msgpack regardless of sender format | Pong echoes the sender's flags byte | 2 |
| 14 | Flags mask ignores unused bits | Reserved bits must be zero, validated in `Flags::from_byte` | 2 |

---

## Appendix B: Full Frame Examples

### B.1 Heartbeat Ping (v3, msgpack)

```
Bytes: [0x01, 0x00, <msgpack {"type":"ping","timestamp":1710764400000}>]
         ^     ^     ^
         |     |     payload (ControlMessage::Ping)
         |     flags: encoding=msgpack, reserved=0
         frame type: Control
```

### B.2 Heartbeat Ping (v3, JSON)

```
Bytes: [0x01, 0x01, {"type":"ping","timestamp":1710764400000}]
         ^     ^     ^
         |     |     payload (ControlMessage::Ping, JSON-encoded)
         |     flags: encoding=json, reserved=0
         frame type: Control
```

### B.3 Device Announce (v3, msgpack)

```
Bytes: [0x02, 0x00, <msgpack {
    "namespace": "mesh",
    "type": "device-announce",
    "from": "device-abc",
    "payload": {
        "device": {"id": "device-abc", "name": "My Laptop", ...},
        "protocolVersion": 3
    },
    "timestamp": 1710764400000
}>]
```

### B.4 Route Broadcast (v3, JSON, for debugging)

```
Bytes: [0x02, 0x01, {
    "namespace": "mesh",
    "type": "route-broadcast",
    "from": "device-xyz",
    "payload": {
        "envelope": {
            "namespace": "sync",
            "type": "sync-full",
            "from": "device-xyz",
            "payload": {
                "storeId": "tasks",
                "deviceId": "device-xyz",
                "data": {"items": [...]},
                "version": 42,
                "updatedAt": 1710764400000
            }
        }
    }
}]
```

### B.5 Protocol Error (v3, JSON)

```
Bytes: [0x03, 0x01, {
    "code": "malformed-payload",
    "message": "Failed to deserialize MeshEnvelope: missing field `namespace`",
    "fatal": false
}]
```

### B.6 Legacy Heartbeat Ping (v2, for comparison)

```
Bytes: [0x00, <msgpack {"type":"ping","timestamp":1710764400000}>]
         ^     ^
         |     indistinguishable from a MeshEnvelope at the binary level
         old flags byte (0x00 = msgpack)
```

Note: In v2, the receiver must parse the entire payload as JSON and then check `!has("namespace") && type == "ping"` to identify this as a heartbeat. In v3, the receiver reads byte 0 = `0x01` and immediately knows it is a control frame.

---

## Appendix C: Design Decisions and Alternatives Considered

### C.1 Why Not a Full Binary Header with Length Prefix?

WebSocket already provides message framing. Adding a length prefix is redundant for WS binary frames. The 2-byte header (type + flags) is the minimal addition needed for structural discrimination. If truffle ever needs to run over raw TCP (e.g., for file transfer signaling without WS), the framing can be extended with a length prefix at that time.

### C.2 Why Not Protobuf/Cap'n Proto?

Truffle's design principle (RFC 008 Section 9.2) is JSON-over-WebSocket for debuggability. Switching to protobuf would improve performance but hurt observability. The msgpack option already provides binary efficiency for production. The new protocol keeps JSON as a first-class encoding.

### C.3 Why Keep `serde_json::Value` for Custom Namespace Payloads?

Application-defined namespaces cannot have their payload types known at compile time in truffle-core. The typed payload approach (Section 6.2) applies only to known namespaces. Custom namespaces continue to use `serde_json::Value`, which is validated by the application's message bus subscribers.

### C.4 Why Not Make `msg_type` an Enum on the Wire?

Making `msg_type` an enum in the serialized envelope would break forward compatibility. A node running v3.1 that adds a new message type would produce envelopes that v3.0 nodes cannot deserialize at all (unknown enum variant). Keeping it as a string on the wire allows graceful degradation: the v3.0 node logs a warning for the unknown type and continues operating.

### C.5 Why a Handshake Instead of Just Detecting v2 by Frame Shape?

Detecting v2 by frame shape (e.g., "if byte 0 is `{` it's JSON text, if it's `0x00` it's v2 msgpack, if it's `0x01-0x03` it's v3") is fragile. `0x00` is also a valid v3 frame type byte if we ever assign it. An explicit handshake establishes the protocol version unambiguously and opens the door for future negotiation (e.g., compression support, max message size).
