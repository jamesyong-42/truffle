//! Top-level UI layout — Claude Code-inspired design.
//!
//! Layout (top to bottom):
//!   ╭─── truffle v0.3.4 ──────────────────────────────────╮
//!   │  Logo + Node info     │ Devices                     │
//!   ╰─────────────────────────────────────────────────────╯
//!   (activity feed — full width, scrollable)
//!   ─────────────────────────────────────────────────────────
//!   ❯ input
//!   ─────────────────────────────────────────────────────────
//!   truffle · ● online · 3 peers

use ratatui::prelude::*;

use crate::tui::app::AppState;

pub mod activity_feed;
pub mod autocomplete;
pub mod file_picker;
pub mod input_bar;
pub mod status_bar;
pub mod toast;
pub mod transfer_dialog;
pub mod welcome_box;

/// Render the full TUI layout.
pub fn render(frame: &mut Frame, app: &AppState) {
    let full = frame.area();

    // Vertical split: welcome box | feed | input | status line
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(welcome_box::WELCOME_BOX_HEIGHT), // welcome box
            Constraint::Min(3),                                  // activity feed
            Constraint::Length(input_bar::INPUT_BAR_HEIGHT),     // input (sep + prompt + sep)
            Constraint::Length(1),                               // status line
        ])
        .split(full);

    let welcome_area = chunks[0];
    let feed_area = chunks[1];
    let input_area = chunks[2];
    let status_area = chunks[3];

    // Render each zone
    welcome_box::render(frame, welcome_area, app);
    activity_feed::render(frame, feed_area, app);
    input_bar::render(frame, input_area, app);
    status_bar::render(frame, status_area, app);

    // Overlays — rendered LAST so they appear on top
    file_picker::render(frame, feed_area, app);
    autocomplete::render(frame, input_area, app);
    toast::render_toast(frame, feed_area, app);
    transfer_dialog::render(frame, full, app);
}
