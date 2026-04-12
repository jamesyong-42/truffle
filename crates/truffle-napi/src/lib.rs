//! NAPI-RS bindings for truffle-core.
//!
//! Exposes the truffle-core `Node<TailscaleProvider>` API to Node.js via
//! two main classes:
//!
//! - [`NapiNode`] — peer discovery, messaging, diagnostics
//! - [`NapiFileTransfer`] — file send/receive/pull

pub mod crdt_doc;
pub mod file_transfer;
pub mod node;
pub mod synced_store;
pub mod types;
