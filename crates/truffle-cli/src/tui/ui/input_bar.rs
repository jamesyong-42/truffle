//! Input bar — text input with `> ` prompt at the bottom of the screen.

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::tui::app::AppState;

pub fn render(frame: &mut Frame, area: Rect, app: &AppState) {
    if area.height < 2 {
        return;
    }

    // Separator line (first row of the area)
    let sep_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    let separator = Paragraph::new(Line::from(Span::styled(
        "\u{2500}".repeat(area.width as usize),
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(separator, sep_area);

    // Input line (second row)
    let input_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: 1,
    };

    let prompt = "> ";

    let paragraph = Paragraph::new(Line::from(vec![
        Span::styled(prompt, Style::default().fg(Color::DarkGray).bold()),
        Span::raw(&app.input),
    ]));

    frame.render_widget(paragraph, input_area);

    // Place the cursor — count display width of chars before cursor_pos
    let text_width_before_cursor: u16 = app
        .input
        .chars()
        .take(app.cursor_pos)
        .map(|c| if c.is_ascii() { 1u16 } else { 2u16 })
        .sum();
    let cursor_x = input_area.x + prompt.len() as u16 + text_width_before_cursor;
    let cursor_y = input_area.y;
    if cursor_x < input_area.x + input_area.width {
        frame.set_cursor_position((cursor_x, cursor_y));
    }
}
