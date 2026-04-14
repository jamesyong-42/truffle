//! `/broadcast <message>` — send a message to all online peers.

use crate::apps::messaging;
use crate::tui::app::{AppState, DisplayItem, SystemLevel};
use crate::tui::commands::CommandResult;

/// Execute the /broadcast command.
pub async fn execute(args: &str, app: &mut AppState) -> CommandResult {
    let message = args.trim();

    if message.is_empty() {
        return CommandResult::Items(vec![DisplayItem::System {
            time: chrono::Local::now(),
            text: "  Usage: /broadcast <message>".to_string(),
            level: SystemLevel::Warning,
        }]);
    }

    let online_count = app.online_peer_count();
    if online_count == 0 {
        return CommandResult::Items(vec![DisplayItem::System {
            time: chrono::Local::now(),
            text: "  No peers online to broadcast to.".to_string(),
            level: SystemLevel::Warning,
        }]);
    }

    // Optimistic display
    app.push_item(DisplayItem::ChatOutgoing {
        time: chrono::Local::now(),
        to: format!("all ({online_count} peers)"),
        text: message.to_string(),
    });

    // Send via Node API
    messaging::broadcast_message(&*app.node, message).await;

    CommandResult::Handled
}
