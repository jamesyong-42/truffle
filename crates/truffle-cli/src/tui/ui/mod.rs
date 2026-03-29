//! Top-level UI layout for the TUI.
//!
//! Three pinned zones + device panel:
//!   - Status bar (1 line, top)
//!   - Activity feed (center, fills remaining space)
//!   - Input bar (2 lines, bottom: separator + prompt)
//!   - Device panel (right side, 20 chars)

use ratatui::prelude::*;

use crate::tui::app::AppState;

pub mod activity_feed;
pub mod autocomplete;
pub mod device_panel;
pub mod input_bar;
pub mod status_bar;
pub mod toast;

/// Render the full TUI layout.
pub fn render(frame: &mut Frame, app: &AppState) {
    let full = frame.area();

    // Split horizontally: main area (left) | device panel (right, 20 chars)
    let h_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(30),     // main area (takes remaining)
            Constraint::Length(20),   // device panel
        ])
        .split(full);

    let main_area = h_chunks[0];
    let device_area = h_chunks[1];

    // Split main area vertically: status (1) | feed (fill) | input (2)
    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),   // status bar
            Constraint::Min(3),      // activity feed
            Constraint::Length(2),   // input bar (separator + prompt)
        ])
        .split(main_area);

    let status_area = v_chunks[0];
    let feed_area = v_chunks[1];
    let input_area = v_chunks[2];

    // Render each zone
    status_bar::render(frame, status_area, app);
    activity_feed::render(frame, feed_area, app);
    input_bar::render(frame, input_area, app);
    device_panel::render(frame, device_area, app);

    // Autocomplete overlay
    autocomplete::render(frame, input_area, app);

    // Toast notification overlay — rendered LAST so it appears on top of everything
    toast::render_toast(frame, feed_area, app);
}
