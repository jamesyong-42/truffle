//! Input bar — Claude Code-style prompt between separator lines.
//!
//! Layout (3 lines):
//!   ─────────────────────────
//!   ❯ user input here_
//!   ─────────────────────────

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::tui::app::AppState;

/// Total height of the input area (separator + prompt + separator).
pub const INPUT_BAR_HEIGHT: u16 = 3;

pub fn render(frame: &mut Frame, area: Rect, app: &AppState) {
    if area.height < 3 {
        return;
    }

    let sep_style = Style::default().fg(Color::DarkGray);
    let sep_line = "\u{2500}".repeat(area.width as usize);

    // Top separator (row 0)
    let top_sep = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(&sep_line, sep_style))),
        top_sep,
    );

    // Prompt line (row 1)
    let prompt_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: 1,
    };
    let prompt = "\u{276f} "; // ❯

    let paragraph = Paragraph::new(Line::from(vec![
        Span::styled(prompt, Style::default().fg(Color::Cyan).bold()),
        Span::raw(&app.input),
    ]));
    frame.render_widget(paragraph, prompt_area);

    // Cursor position
    let text_width_before_cursor: u16 = app
        .input
        .chars()
        .take(app.cursor_pos)
        .map(|c| if c.is_ascii() { 1u16 } else { 2u16 })
        .sum();
    let cursor_x = prompt_area.x + prompt.chars().count() as u16 + text_width_before_cursor;
    let cursor_y = prompt_area.y;
    if cursor_x < prompt_area.x + prompt_area.width {
        frame.set_cursor_position((cursor_x, cursor_y));
    }

    // Bottom separator (row 2)
    let bot_sep = Rect {
        x: area.x,
        y: area.y + 2,
        width: area.width,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(&sep_line, sep_style))),
        bot_sep,
    );
}
