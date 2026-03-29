//! Status line — bottom bar showing node info and unread count.
//!
//! Rendered below the input bar, outside the main content area.
//! Like Claude Code's "truffle git:(main) ✗" footer.

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::tui::app::AppState;

pub fn render(frame: &mut Frame, area: Rect, app: &AppState) {
    let info = app.node.local_info();
    let node_name = info.name.trim_start_matches("truffle-").to_string();
    let peer_count = app.online_peer_count();

    let status = if info.ip.is_some() { "online" } else { "connecting" };
    let (indicator, color) = if status == "online" {
        ("\u{25cf}", Color::Green)
    } else {
        ("\u{25cf}", Color::Yellow)
    };

    let mut spans = vec![
        Span::styled("  truffle", Style::default().fg(Color::DarkGray)),
        Span::styled(" \u{00b7} ", Style::default().fg(Color::DarkGray)),
        Span::styled(indicator, Style::default().fg(color)),
        Span::styled(format!(" {status}", ), Style::default().fg(color)),
        Span::styled(format!(" \u{00b7} {peer_count} peers"), Style::default().fg(Color::DarkGray)),
    ];

    // Unread indicator (right side)
    if app.unread_count > 0 {
        // Calculate padding to right-align
        let left_len: usize = 10 + 3 + status.len() + 2 + format!("{peer_count}").len() + 6;
        let remaining = (area.width as usize).saturating_sub(left_len + 10);
        if remaining > 0 {
            spans.push(Span::raw(" ".repeat(remaining)));
        }
        spans.push(Span::styled(
            format!("\u{2193} {} new", app.unread_count),
            Style::default().fg(Color::Yellow),
        ));
    }

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line);
    frame.render_widget(paragraph, area);
}
