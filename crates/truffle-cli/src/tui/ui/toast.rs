//! Toast notification overlay.
//!
//! Renders the most recent toast as a bordered box in the top-right corner
//! of the given area. Uses `Clear` to blank the region first so the toast
//! floats on top of the activity feed.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::tui::app::AppState;

/// Maximum width of the toast (including border).
const TOAST_MAX_WIDTH: u16 = 36;

/// Render the most recent toast notification in the top-right corner of `area`.
///
/// Does nothing if there are no active toasts.
pub fn render_toast(f: &mut Frame, area: Rect, app: &AppState) {
    let toast = match app.notifications.back() {
        Some(t) => t,
        None => return,
    };

    // Calculate toast dimensions
    let text_len = toast.text.chars().count() as u16;
    // +4 for border (left border + space + space + right border)
    let width = (text_len + 4).min(TOAST_MAX_WIDTH).min(area.width);
    let height: u16 = 3; // top border + text + bottom border

    if area.width < 6 || area.height < 3 {
        return; // Too small to render
    }

    // Position: top-right corner of the area, with 1 cell margin
    let x = area.x + area.width.saturating_sub(width + 1);
    let y = area.y + 1;

    let toast_area = Rect::new(x, y, width, height);

    // Truncate text if needed
    let max_text = (width as usize).saturating_sub(4);
    let display_text = if toast.text.chars().count() > max_text {
        let truncated: String = toast.text.chars().take(max_text.saturating_sub(3)).collect();
        format!("{truncated}...")
    } else {
        toast.text.clone()
    };

    // Clear the area behind the toast
    f.render_widget(Clear, toast_area);

    // Render the toast
    let paragraph = Paragraph::new(Line::from(display_text)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    );

    f.render_widget(paragraph, toast_area);
}
