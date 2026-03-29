//! File picker overlay — wraps ratatui-explorer in the activity feed area.
//!
//! Shown when the user presses Tab in a `/cp` command.
//! Rendered on top of the activity feed using the full feed area.

use ratatui::prelude::*;
use ratatui::widgets::{Clear, FrameExt as _};

use crate::tui::app::AppState;

/// Render the file picker overlay if active.
pub fn render(frame: &mut Frame, area: Rect, app: &AppState) {
    let explorer = match &app.file_picker {
        Some(e) => e,
        None => return,
    };

    // Clear the area first so the activity feed doesn't bleed through
    frame.render_widget(Clear, area);
    frame.render_widget_ref(explorer.widget(), area);
}
