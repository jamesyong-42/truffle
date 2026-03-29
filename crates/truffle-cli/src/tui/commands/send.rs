//! /send <message> @device — send a chat message to a peer.
//!
//! Syntax: /send hello world @server
//! The message is everything before the last @device token.

use crate::apps::messaging;
use crate::tui::app::{AppState, DisplayItem, SystemLevel};
use crate::tui::commands::CommandResult;

/// Execute the /send command.
pub async fn execute(args: &str, app: &mut AppState) -> CommandResult {
    // Parse: find the last @device token
    let (message, device_name) = match parse_send_args(args) {
        Some(parsed) => parsed,
        None => {
            return CommandResult::Items(vec![DisplayItem::System {
                time: chrono::Local::now(),
                text: "  Usage: /send <message> @device".to_string(),
                level: SystemLevel::Warning,
            }]);
        }
    };

    // Resolve device name to peer ID
    let peer = match resolve_peer(&app, &device_name) {
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

    // Optimistic display: show outgoing message immediately
    app.push_item(DisplayItem::ChatOutgoing {
        time: chrono::Local::now(),
        to: peer.name.clone(),
        text: message.clone(),
    });

    // Send via Node API
    match messaging::send_message(&*app.node, &peer.id, &message).await {
        Ok(()) => CommandResult::Handled,
        Err(e) => {
            app.push_item(DisplayItem::System {
                time: chrono::Local::now(),
                text: format!("  Failed to send: {e}"),
                level: SystemLevel::Error,
            });
            CommandResult::Handled
        }
    }
}

/// Parse "/send <message> @device" — message is everything before the last @token.
///
/// The last whitespace-separated token starting with `@` is the device.
/// Everything before it is the message.
fn parse_send_args(args: &str) -> Option<(String, String)> {
    let args = args.trim();
    if args.is_empty() {
        return None;
    }

    // Find the byte position of the last whitespace-separated token starting with @
    // by searching backwards for the last ` @` boundary.
    let last_at_pos = args.rfind(" @")?;
    let device_token = &args[last_at_pos + 1..]; // includes the @
    let device = device_token.strip_prefix('@')?;

    if device.is_empty() || device.contains(char::is_whitespace) {
        return None;
    }

    let message = args[..last_at_pos].trim();
    if message.is_empty() {
        return None;
    }

    Some((message.to_string(), device.to_string()))
}

/// Resolve a device name to an online PeerInfo (case-insensitive, prefix match).
fn resolve_peer(
    app: &AppState,
    name: &str,
) -> Option<crate::tui::app::PeerInfo> {
    let lower = name.to_lowercase();
    let online_peers: Vec<_> = app.peers.iter().filter(|p| p.online).collect();

    // Exact match (case-insensitive)
    if let Some(peer) = online_peers
        .iter()
        .find(|p| p.name.to_lowercase() == lower)
    {
        return Some((*peer).clone());
    }

    // Prefix match
    let matches: Vec<_> = online_peers
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
    fn test_parse_send_args() {
        assert_eq!(
            parse_send_args("hello @server"),
            Some(("hello".to_string(), "server".to_string()))
        );
        assert_eq!(
            parse_send_args("hello world @server"),
            Some(("hello world".to_string(), "server".to_string()))
        );
        assert_eq!(
            parse_send_args("how are things? @my-laptop"),
            Some(("how are things?".to_string(), "my-laptop".to_string()))
        );
        // Missing @device
        assert_eq!(parse_send_args("hello"), None);
        // Empty message
        assert_eq!(parse_send_args("@server"), None);
        // Empty
        assert_eq!(parse_send_args(""), None);
    }

    #[test]
    fn test_parse_send_args_prefix_ambiguity() {
        // When message contains @mention-like text, only the LAST @token is the device
        assert_eq!(
            parse_send_args("@server is down @pi"),
            Some(("@server is down".to_string(), "pi".to_string()))
        );
        assert_eq!(
            parse_send_args("talk to @work about it @worker"),
            Some(("talk to @work about it".to_string(), "worker".to_string()))
        );
    }
}
