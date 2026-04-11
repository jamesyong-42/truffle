//! NapiFileTransfer — Node.js wrapper for file transfer operations.

use std::sync::Arc;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Status, Unknown};
use napi_derive::napi;
use tokio::sync::Mutex;

use truffle_core::file_transfer::types::OfferResponder;
use truffle_core::network::tailscale::TailscaleProvider;
use truffle_core::{FileTransferEvent, Node};

use crate::types::{
    NapiFileOffer, NapiFileTransferEvent, NapiTransferProgress, NapiTransferResult,
};

/// Responder for accepting or rejecting a file offer from JS.
///
/// The responder is consumed on the first call to `accept()` or `reject()`.
/// If neither is called, the offer times out after 60 seconds.
#[napi]
pub struct NapiOfferResponder {
    inner: Arc<Mutex<Option<OfferResponder>>>,
}

#[napi]
impl NapiOfferResponder {
    /// Accept the file offer, saving to the specified path.
    #[napi]
    pub async fn accept(&self, save_path: String) -> Result<()> {
        let responder = self
            .inner
            .lock()
            .await
            .take()
            .ok_or_else(|| Error::from_reason("Offer already responded to"))?;
        responder.accept(&save_path);
        Ok(())
    }

    /// Reject the file offer with a reason.
    #[napi]
    pub async fn reject(&self, reason: String) -> Result<()> {
        let responder = self
            .inner
            .lock()
            .await
            .take()
            .ok_or_else(|| Error::from_reason("Offer already responded to"))?;
        responder.reject(&reason);
        Ok(())
    }
}

/// File transfer handle exposed to JavaScript.
///
/// Obtained via `NapiNode.fileTransfer()`. Holds an `Arc<Node>` to create
/// fresh `FileTransfer` handles on each method call (the Rust `FileTransfer`
/// borrows `&Node`, so we can't store it across JS boundaries).
#[napi]
pub struct NapiFileTransfer {
    node: Arc<Node<TailscaleProvider>>,
}

impl NapiFileTransfer {
    pub(crate) fn new(node: Arc<Node<TailscaleProvider>>) -> Self {
        Self { node }
    }
}

#[napi]
impl NapiFileTransfer {
    /// Send a file to a peer.
    ///
    /// `peer_id` is the recipient's stable `device_id` (RFC 017). The
    /// underlying `resolve_peer_id` also accepts device names, prefixes,
    /// and legacy Tailscale IDs. Resolves with transfer result on success.
    #[napi]
    pub async fn send_file(
        &self,
        peer_id: String,
        local_path: String,
        remote_path: String,
    ) -> Result<NapiTransferResult> {
        let ft = self.node.file_transfer();
        let result = ft
            .send_file(&peer_id, &local_path, &remote_path)
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(NapiTransferResult {
            bytes_transferred: result.bytes_transferred as f64,
            sha256: result.sha256,
            elapsed_secs: result.elapsed_secs,
        })
    }

    /// Pull (download) a file from a remote peer.
    ///
    /// Resolves with transfer result on success.
    #[napi]
    pub async fn pull_file(
        &self,
        peer_id: String,
        remote_path: String,
        local_path: String,
    ) -> Result<NapiTransferResult> {
        let ft = self.node.file_transfer();
        let result = ft
            .pull_file(&peer_id, &remote_path, &local_path)
            .await
            .map_err(|e| Error::from_reason(e.to_string()))?;
        Ok(NapiTransferResult {
            bytes_transferred: result.bytes_transferred as f64,
            sha256: result.sha256,
            elapsed_secs: result.elapsed_secs,
        })
    }

    /// Auto-accept all incoming file offers, saving files to `output_dir`.
    #[napi]
    pub async fn auto_accept(&self, output_dir: String) -> Result<()> {
        let ft = self.node.file_transfer();
        ft.auto_accept(self.node.clone(), &output_dir).await;
        Ok(())
    }

    /// Auto-reject all incoming file offers.
    #[napi]
    pub async fn auto_reject(&self) -> Result<()> {
        let ft = self.node.file_transfer();
        ft.auto_reject(self.node.clone()).await;
        Ok(())
    }

    /// Subscribe to incoming file offers with a callback.
    ///
    /// The callback receives a `NapiFileOffer`. Use `autoAccept()` or
    /// `autoReject()` for automated handling, or use `onOffer()` for
    /// manual inspection of offers.
    /// Subscribe to incoming file offers with a callback.
    ///
    /// The callback receives `(offer, responder)` — call `responder.accept(path)`
    /// or `responder.reject(reason)` to handle the offer. If neither is called
    /// within 60 seconds, the offer is auto-rejected.
    #[napi(ts_args_type = "callback: (offer: FileOffer, responder: NapiOfferResponder) => void")]
    pub fn on_offer(
        &self,
        callback: ThreadsafeFunction<
            (NapiFileOffer, NapiOfferResponder),
            Unknown<'static>,
            (NapiFileOffer, NapiOfferResponder),
            Status,
            false,
        >,
    ) -> Result<()> {
        let node = self.node.clone();

        napi::bindgen_prelude::spawn(async move {
            let ft = node.file_transfer();
            let mut rx = ft.offer_channel(node.clone()).await;

            while let Some((offer, responder)) = rx.recv().await {
                let napi_offer = NapiFileOffer {
                    from_peer: offer.from_peer.clone(),
                    from_name: offer.from_name.clone(),
                    file_name: offer.file_name.clone(),
                    size: offer.size as f64,
                    sha256: offer.sha256.clone(),
                    suggested_path: offer.suggested_path.clone(),
                    token: offer.token.clone(),
                };

                let napi_responder = NapiOfferResponder {
                    inner: Arc::new(Mutex::new(Some(responder))),
                };

                let status = callback.call(
                    (napi_offer, napi_responder),
                    ThreadsafeFunctionCallMode::NonBlocking,
                );
                if status != Status::Ok {
                    break;
                }
            }
        });

        Ok(())
    }

    /// Subscribe to file transfer events.
    ///
    /// The callback receives `NapiFileTransferEvent` objects for all
    /// transfer lifecycle events (hashing, progress, completed, failed, etc.).
    #[napi(ts_args_type = "callback: (event: FileTransferEvent) => void")]
    pub fn on_event(
        &self,
        callback: ThreadsafeFunction<
            NapiFileTransferEvent,
            Unknown<'static>,
            NapiFileTransferEvent,
            Status,
            false,
        >,
    ) -> Result<()> {
        let ft = self.node.file_transfer();
        let mut rx = ft.subscribe();

        napi::bindgen_prelude::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        let napi_event = convert_ft_event(&event);
                        let status =
                            callback.call(napi_event, ThreadsafeFunctionCallMode::NonBlocking);
                        if status != Status::Ok {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(missed = n, "on_event lagged");
                        continue;
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        Ok(())
    }

    /// Set the maximum allowed transfer size in bytes.
    #[napi]
    pub fn set_max_transfer_size(&self, bytes: f64) {
        let ft = self.node.file_transfer();
        ft.set_max_transfer_size(bytes as u64);
    }
}

// ---------------------------------------------------------------------------
// FileTransferEvent conversion
// ---------------------------------------------------------------------------

fn direction_str(d: &truffle_core::file_transfer::TransferDirection) -> String {
    use truffle_core::file_transfer::TransferDirection;
    match d {
        TransferDirection::Send => "send".to_string(),
        TransferDirection::Receive => "receive".to_string(),
    }
}

fn convert_ft_event(event: &FileTransferEvent) -> NapiFileTransferEvent {
    match event {
        FileTransferEvent::OfferReceived(offer) => NapiFileTransferEvent {
            event_type: "offer_received".to_string(),
            token: Some(offer.token.clone()),
            file_name: Some(offer.file_name.clone()),
            direction: None,
            progress: None,
            offer: Some(NapiFileOffer {
                from_peer: offer.from_peer.clone(),
                from_name: offer.from_name.clone(),
                file_name: offer.file_name.clone(),
                size: offer.size as f64,
                sha256: offer.sha256.clone(),
                suggested_path: offer.suggested_path.clone(),
                token: offer.token.clone(),
            }),
            bytes_transferred: None,
            sha256: None,
            elapsed_secs: None,
            reason: None,
            bytes_hashed: None,
            total_bytes: None,
        },

        FileTransferEvent::Hashing {
            token,
            file_name,
            bytes_hashed,
            total_bytes,
        } => NapiFileTransferEvent {
            event_type: "hashing".to_string(),
            token: Some(token.clone()),
            file_name: Some(file_name.clone()),
            direction: None,
            progress: None,
            offer: None,
            bytes_transferred: None,
            sha256: None,
            elapsed_secs: None,
            reason: None,
            bytes_hashed: Some(*bytes_hashed as f64),
            total_bytes: Some(*total_bytes as f64),
        },

        FileTransferEvent::WaitingForAccept { token, file_name } => NapiFileTransferEvent {
            event_type: "waiting_for_accept".to_string(),
            token: Some(token.clone()),
            file_name: Some(file_name.clone()),
            direction: None,
            progress: None,
            offer: None,
            bytes_transferred: None,
            sha256: None,
            elapsed_secs: None,
            reason: None,
            bytes_hashed: None,
            total_bytes: None,
        },

        FileTransferEvent::Progress(p) => NapiFileTransferEvent {
            event_type: "progress".to_string(),
            token: Some(p.token.clone()),
            file_name: Some(p.file_name.clone()),
            direction: Some(direction_str(&p.direction)),
            progress: Some(NapiTransferProgress {
                token: p.token.clone(),
                direction: direction_str(&p.direction),
                file_name: p.file_name.clone(),
                bytes_transferred: p.bytes_transferred as f64,
                total_bytes: p.total_bytes as f64,
                speed_bps: p.speed_bps,
            }),
            offer: None,
            bytes_transferred: None,
            sha256: None,
            elapsed_secs: None,
            reason: None,
            bytes_hashed: None,
            total_bytes: None,
        },

        FileTransferEvent::Completed {
            token,
            direction,
            file_name,
            bytes_transferred,
            sha256,
            elapsed_secs,
        } => NapiFileTransferEvent {
            event_type: "completed".to_string(),
            token: Some(token.clone()),
            file_name: Some(file_name.clone()),
            direction: Some(direction_str(direction)),
            progress: None,
            offer: None,
            bytes_transferred: Some(*bytes_transferred as f64),
            sha256: Some(sha256.clone()),
            elapsed_secs: Some(*elapsed_secs),
            reason: None,
            bytes_hashed: None,
            total_bytes: None,
        },

        FileTransferEvent::Rejected {
            token,
            file_name,
            reason,
        } => NapiFileTransferEvent {
            event_type: "rejected".to_string(),
            token: Some(token.clone()),
            file_name: Some(file_name.clone()),
            direction: None,
            progress: None,
            offer: None,
            bytes_transferred: None,
            sha256: None,
            elapsed_secs: None,
            reason: Some(reason.clone()),
            bytes_hashed: None,
            total_bytes: None,
        },

        FileTransferEvent::Failed {
            token,
            direction,
            file_name,
            reason,
        } => NapiFileTransferEvent {
            event_type: "failed".to_string(),
            token: Some(token.clone()),
            file_name: Some(file_name.clone()),
            direction: Some(direction_str(direction)),
            progress: None,
            offer: None,
            bytes_transferred: None,
            sha256: None,
            elapsed_secs: None,
            reason: Some(reason.clone()),
            bytes_hashed: None,
            total_bytes: None,
        },
    }
}
