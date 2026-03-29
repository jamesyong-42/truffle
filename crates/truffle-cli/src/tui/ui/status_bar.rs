//! Status bar — 1-line pinned header with node info.

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::tui::app::AppState;

pub fn render(frame: &mut Frame, area: Rect, app: &AppState) {
    let info = app.node.local_info();
    let ip_str = info.ip.map(|ip| ip.to_string()).unwrap_or_default();
    let status = if !ip_str.is_empty() { "online" } else { "connecting" };
    let peer_count = app.online_peer_count();
    let uptime = app.uptime_str();

    let mut spans = vec![
        Span::styled("truffle", Style::default().bold()),
        Span::raw(" \u{00b7} "),
        Span::raw(&info.name),
        Span::raw(" \u{00b7} "),
    ];

    // Status indicator
    match status {
        "online" => {
            spans.push(Span::styled("\u{25cf} online", Style::default().fg(Color::Green)));
        }
        _ => {
            spans.push(Span::styled(
                "\u{25cf} connecting",
                Style::default().fg(Color::Yellow),
            ));
        }
    }

    if !ip_str.is_empty() {
        spans.push(Span::raw(" \u{00b7} "));
        spans.push(Span::raw(ip_str));
    }

    spans.push(Span::raw(" \u{00b7} "));
    spans.push(Span::raw(format!("{peer_count} peers")));
    spans.push(Span::raw(" \u{00b7} "));
    spans.push(Span::styled(uptime, Style::default().fg(Color::DarkGray)));

    // Unread indicator (right-aligned)
    if app.unread_count > 0 {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("\u{2193} {} new", app.unread_count),
            Style::default().fg(Color::Yellow),
        ));
    }

    let line = Line::from(spans);
    let paragraph = Paragraph::new(line).style(Style::default());

    frame.render_widget(paragraph, area);
}
