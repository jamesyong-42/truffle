//! Unified event system for the TUI.
//!
//! All event sources (crossterm, peer events, tick timer) funnel into
//! a single mpsc channel so the main loop can process them sequentially.

use crossterm::event::{Event as CtEvent, EventStream, KeyEvent};
use futures::StreamExt;
use tokio::sync::mpsc;
use truffle_core::file_transfer::types::FileTransferEvent;
use truffle_core::node::NamespacedMessage;
use truffle_core::session::PeerEvent;

/// All events the TUI can receive.
#[derive(Debug)]
pub enum AppEvent {
    /// A key was pressed.
    Key(KeyEvent),
    /// The terminal was resized.
    Resize(u16, u16),
    /// A peer event from the mesh.
    PeerEvent(PeerEvent),
    /// An incoming chat message.
    IncomingMessage {
        from_id: String,
        from_name: String,
        text: String,
    },
    /// File transfer progress update (from background upload task).
    TransferProgress {
        file_name: String,
        percent: f64,
        speed_bps: f64,
    },
    /// File transfer completed successfully.
    TransferComplete {
        file_name: String,
        size: u64,
        sha256: String,
    },
    /// File transfer failed.
    TransferFailed {
        file_name: String,
        reason: String,
    },
    /// An incoming file offer from a peer.
    IncomingFileOffer {
        from_id: String,
        from_name: String,
        file_name: String,
        size: u64,
    },
    /// Periodic tick (1s) for uptime, toast expiry, etc.
    Tick,
}

/// Spawns event collector tasks and returns the receiving end.
///
/// Also returns the sender so `/cp` can send transfer progress events.
pub fn spawn_event_collectors(
    peer_rx: tokio::sync::broadcast::Receiver<PeerEvent>,
    chat_rx: tokio::sync::broadcast::Receiver<NamespacedMessage>,
    ft_rx: tokio::sync::broadcast::Receiver<FileTransferEvent>,
) -> (mpsc::UnboundedSender<AppEvent>, mpsc::UnboundedReceiver<AppEvent>) {
    let (tx, rx) = mpsc::unbounded_channel();

    // 1. Crossterm terminal events
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut reader = EventStream::new();
            while let Some(Ok(event)) = reader.next().await {
                let app_event = match event {
                    CtEvent::Key(key) => Some(AppEvent::Key(key)),
                    CtEvent::Resize(w, h) => Some(AppEvent::Resize(w, h)),
                    _ => None,
                };
                if let Some(evt) = app_event {
                    if tx.send(evt).is_err() {
                        break;
                    }
                }
            }
        });
    }

    // 2. Peer events from the mesh
    {
        let tx = tx.clone();
        let mut peer_rx = peer_rx;
        tokio::spawn(async move {
            loop {
                match peer_rx.recv().await {
                    Ok(event) => {
                        if tx.send(AppEvent::PeerEvent(event)).is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Peer event stream lagged, missed {n} events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // 3. Chat messages from the mesh ("chat" namespace)
    {
        let tx = tx.clone();
        let mut chat_rx = chat_rx;
        tokio::spawn(async move {
            loop {
                match chat_rx.recv().await {
                    Ok(msg) => {
                        // Extract text from the payload
                        let text = msg
                            .payload
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();

                        if text.is_empty() {
                            continue;
                        }

                        // Use `from` as both ID and fallback name
                        // (the main loop will resolve to a display name from the peer cache)
                        let evt = AppEvent::IncomingMessage {
                            from_id: msg.from.clone(),
                            from_name: msg.from.clone(),
                            text,
                        };
                        if tx.send(evt).is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Chat message stream lagged, missed {n} messages");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // 4. File transfer events from the core (typed FileTransferEvent)
    {
        let tx = tx.clone();
        let mut ft_rx = ft_rx;
        tokio::spawn(async move {
            loop {
                match ft_rx.recv().await {
                    Ok(event) => {
                        match event {
                            FileTransferEvent::OfferReceived(offer) => {
                                let _ = tx.send(AppEvent::IncomingFileOffer {
                                    from_id: offer.from_peer.clone(),
                                    from_name: offer.from_name.clone(),
                                    file_name: offer.file_name,
                                    size: offer.size,
                                });
                            }
                            // Progress, Completed, and Failed are handled by the
                            // /cp command's own event forwarder. We don't need to
                            // duplicate them here (they'd conflict with the /cp task's
                            // events). The core events here are for receive-side
                            // observability — Phase 3 will add TUI display for those.
                            _ => {}
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("FT event stream lagged, missed {n} events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    // 5. Tick timer (1 second)
    {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));
            loop {
                interval.tick().await;
                if tx.send(AppEvent::Tick).is_err() {
                    break;
                }
            }
        });
    }

    (tx, rx)
}
