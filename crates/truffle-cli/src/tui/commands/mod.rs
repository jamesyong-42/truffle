//! Slash command registry and dispatch.

use tokio::sync::mpsc;

use crate::tui::app::{AppState, DisplayItem, SystemLevel};
use crate::tui::event::AppEvent;

pub mod broadcast;
pub mod cp;
pub mod send;

/// Result of executing a slash command.
pub enum CommandResult {
    /// Items to append to the activity feed.
    Items(Vec<DisplayItem>),
    /// Command handled — already pushed items to feed directly.
    Handled,
    /// Quit the TUI.
    Quit,
}

/// Parse and dispatch a slash command from the input bar.
pub async fn dispatch(
    input: &str,
    app: &mut AppState,
    event_tx: mpsc::UnboundedSender<AppEvent>,
) -> CommandResult {
    let input = input.trim_start_matches('/');

    let (cmd, args) = match input.split_once(char::is_whitespace) {
        Some((c, a)) => (c.trim(), a.trim()),
        None => (input.trim(), ""),
    };

    match cmd {
        "exit" | "quit" | "q" => CommandResult::Quit,

        "send" => send::execute(args, app).await,

        "broadcast" => broadcast::execute(args, app).await,

        "cp" => cp::execute(args, app, event_tx).await,

        _ => CommandResult::Items(vec![DisplayItem::System {
            time: chrono::Local::now(),
            text: format!("  Unknown command: /{cmd}"),
            level: SystemLevel::Warning,
        }]),
    }
}
