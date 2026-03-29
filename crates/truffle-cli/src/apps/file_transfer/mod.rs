//! File transfer application — Layer 7.
//!
//! Pure application code using the Node API. No HTTP, no axum, no
//! TcpProxyHandler. Signaling via WS (`node.send`), data via raw TCP
//! (`node.open_tcp` / `node.listen_tcp`).
//!
//! # Protocol
//!
//! ## Upload (push)
//! 1. SHA-256 hash the local file
//! 2. `node.send(peer, "ft", OFFER { file_name, size, sha256, save_path, token })`
//! 3. Wait for `ACCEPT { token }` on `node.subscribe("ft")`
//! 4. `stream = node.open_tcp(peer, 0)` — raw TCP
//! 5. Write `[8-byte size][32-byte sha256][file bytes]` to stream
//! 6. Read 1-byte ACK (0x01 = verified OK)
//!
//! ## Download (pull)
//! 1. `node.send(peer, "ft", PULL_REQUEST { path })`
//! 2. Wait for `OFFER` on `node.subscribe("ft")`
//! 3. `node.send(peer, "ft", ACCEPT { token })`
//! 4. `listener = node.listen_tcp(0)`
//! 5. `stream = listener.accept()`
//! 6. Read `[8-byte size][32-byte sha256][file bytes]`
//! 7. Verify SHA-256, write to disk
//! 8. Send 1-byte ACK

pub mod types;
pub mod upload;
pub mod download;
pub mod receive;
