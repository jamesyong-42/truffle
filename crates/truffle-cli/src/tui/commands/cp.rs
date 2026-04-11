//! /cp <path> @device[:/dest] — file transfer.
//!
//! Syntax: /cp report.pdf @server
//!         /cp report.pdf @server:/tmp/
//! The @device is the last @token. Everything before it is the file path.

use std::path::Path;

use tokio::sync::mpsc;
use truffle_core::file_transfer::types::FileTransferEvent;

use crate::tui::app::{AppState, DisplayItem, SystemLevel, TransferDirection, TransferStatus};
use crate::tui::commands::CommandResult;
use crate::tui::event::AppEvent;

/// Execute the /cp command.
pub async fn execute(
    args: &str,
    app: &mut AppState,
    event_tx: mpsc::UnboundedSender<AppEvent>,
) -> CommandResult {
    let (local_path, device_name, remote_path) = match parse_cp_args(args) {
        Some(parsed) => parsed,
        None => {
            return CommandResult::Items(vec![DisplayItem::System {
                time: chrono::Local::now(),
                text: "  Usage: /cp <path> @device[:/dest]".to_string(),
                level: SystemLevel::Warning,
            }]);
        }
    };

    // Validate local file exists
    if !Path::new(&local_path).exists() {
        return CommandResult::Items(vec![DisplayItem::System {
            time: chrono::Local::now(),
            text: format!("  File not found: {local_path}"),
            level: SystemLevel::Error,
        }]);
    }

    // Resolve device to peer
    let peer = match resolve_online_peer(app, &device_name) {
        Some(p) => p,
        None => {
            let available: Vec<String> = app
                .peers
                .iter()
                .filter(|p| p.online)
                .map(|p| p.name.clone())
                .collect();
            let hint = if available.is_empty() {
                "No peers online.".to_string()
            } else {
                format!("Available: {}", available.join(", "))
            };
            return CommandResult::Items(vec![DisplayItem::System {
                time: chrono::Local::now(),
                text: format!("  Peer '{device_name}' not found. {hint}"),
                level: SystemLevel::Error,
            }]);
        }
    };

    let file_name = Path::new(&local_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file")
        .to_string();

    let file_size = std::fs::metadata(&local_path).map(|m| m.len()).unwrap_or(0);

    // Add initial transfer item to the feed
    app.push_item(DisplayItem::FileTransfer {
        time: chrono::Local::now(),
        direction: TransferDirection::Send,
        file_name: file_name.clone(),
        size: file_size,
        status: TransferStatus::InProgress {
            percent: 0.0,
            speed_bps: 0.0,
        },
    });

    // Spawn the upload in a background task using the core file transfer API.
    let node = app.node.clone();
    let peer_id = peer.id.clone();
    let local = local_path.clone();
    let remote = remote_path.clone();
    let tx = event_tx.clone();
    let fname = file_name.clone();
    let fsize = file_size;

    tokio::spawn(async move {
        // Subscribe to core file transfer events for progress
        let ft = node.file_transfer();
        let mut rx = ft.subscribe();

        let fname_progress = fname.clone();
        let tx_progress = tx.clone();

        // Spawn a forwarder that converts FileTransferEvent::Progress into AppEvents
        let forwarder = tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(FileTransferEvent::Progress(p)) => {
                        let percent = if p.total_bytes > 0 {
                            p.bytes_transferred as f64 / p.total_bytes as f64 * 100.0
                        } else {
                            0.0
                        };
                        let _ = tx_progress.send(AppEvent::TransferProgress {
                            file_name: fname_progress.clone(),
                            percent,
                            speed_bps: p.speed_bps,
                        });
                    }
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });

        let result = ft.send_file(&peer_id, &local, &remote).await;

        // Cancel the forwarder task
        forwarder.abort();

        match result {
            Ok(tr) => {
                let _ = tx.send(AppEvent::TransferComplete {
                    file_name: fname,
                    size: fsize,
                    sha256: tr.sha256,
                });
            }
            Err(e) => {
                let _ = tx.send(AppEvent::TransferFailed {
                    file_name: fname,
                    reason: e.to_string(),
                });
            }
        }
    });

    CommandResult::Handled
}

/// Parse "/cp <path> @device[:/dest]".
fn parse_cp_args(args: &str) -> Option<(String, String, String)> {
    let args = args.trim();
    if args.is_empty() {
        return None;
    }

    // Find the last ` @` boundary
    let at_pos = args.rfind(" @")?;
    let device_part = &args[at_pos + 2..]; // after " @"
    let local_path = args[..at_pos].trim();

    if local_path.is_empty() || device_part.is_empty() {
        return None;
    }

    // Device part may contain ":/dest" for remote path
    let (device, remote_path) = if let Some(colon_pos) = device_part.find(':') {
        let dev = &device_part[..colon_pos];
        let path = &device_part[colon_pos + 1..];
        (dev.to_string(), path.to_string())
    } else {
        (device_part.to_string(), String::new())
    };

    if device.is_empty() {
        return None;
    }

    Some((local_path.to_string(), device, remote_path))
}

fn resolve_online_peer(app: &AppState, name: &str) -> Option<crate::tui::app::PeerInfo> {
    let lower = name.to_lowercase();
    let online: Vec<_> = app.peers.iter().filter(|p| p.online).collect();

    if let Some(p) = online.iter().find(|p| p.name.to_lowercase() == lower) {
        return Some((*p).clone());
    }
    let matches: Vec<_> = online
        .iter()
        .filter(|p| p.name.to_lowercase().starts_with(&lower))
        .collect();
    if matches.len() == 1 {
        return Some((*matches[0]).clone());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cp_args() {
        assert_eq!(
            parse_cp_args("report.pdf @server"),
            Some((
                "report.pdf".to_string(),
                "server".to_string(),
                String::new()
            ))
        );
        assert_eq!(
            parse_cp_args("report.pdf @server:/tmp/"),
            Some((
                "report.pdf".to_string(),
                "server".to_string(),
                "/tmp/".to_string()
            ))
        );
        assert_eq!(
            parse_cp_args("~/Documents/file.txt @my-laptop"),
            Some((
                "~/Documents/file.txt".to_string(),
                "my-laptop".to_string(),
                String::new()
            ))
        );
        assert_eq!(parse_cp_args(""), None);
        assert_eq!(parse_cp_args("@server"), None);
        assert_eq!(parse_cp_args("file.txt"), None);
    }
}
