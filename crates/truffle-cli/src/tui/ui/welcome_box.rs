//! Welcome box — Claude Code-inspired header with logo + devices.
//!
//! Renders a rounded-corner box at the top with:
//!   Left half: block-letter TRUFFLE logo, tagline, node info
//!   Right half: device list with online/offline indicators

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::tui::app::AppState;

/// Height of the welcome box (including borders).
pub const WELCOME_BOX_HEIGHT: u16 = 11;

pub fn render(frame: &mut Frame, area: Rect, app: &AppState) {
    // Outer box with rounded corners and title
    let version = env!("CARGO_PKG_VERSION");
    let title = format!(" truffle v{version} ");
    let block = Block::default()
        .title(title)
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 20 || inner.height < 4 {
        return;
    }

    // Split inner into left (logo + info) and right (devices)
    let halves = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(inner);

    let left = halves[0];
    let right = halves[1];

    // Leave 1 column gap between left content and divider, and 1 after
    let left_content = Rect {
        x: left.x,
        y: left.y,
        width: left.width.saturating_sub(1),
        height: left.height,
    };
    let right_content = Rect {
        x: right.x + 1,
        y: right.y,
        width: right.width.saturating_sub(1),
        height: right.height,
    };

    // -- Left half: logo + node info --
    render_left(frame, left_content, app);

    // -- Vertical divider --
    let divider_x = left.x + left.width;
    for row in 0..inner.height {
        let cell_rect = Rect {
            x: divider_x,
            y: inner.y + row,
            width: 1,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                "\u{2502}",
                Style::default().fg(Color::DarkGray),
            )),
            cell_rect,
        );
    }

    // -- Right half: device list --
    render_right(frame, right_content, app);
}

fn render_left(frame: &mut Frame, area: Rect, app: &AppState) {
    let info = app.node.local_info();
    let node_name = info.device_name.clone();
    let short_device_id: String = info.device_id.chars().take(8).collect();
    let ip_str = info.ip.map(|ip| ip.to_string()).unwrap_or_default();
    let peer_count = app.online_peer_count();
    let uptime = app.uptime_str();

    let mut lines: Vec<Line> = Vec::new();

    // Top margin
    lines.push(Line::raw(""));

    // Logo (3 lines)
    lines.push(Line::from(Span::styled(
        "  \u{2580}\u{2588}\u{2580} \u{2588}\u{2580}\u{2588} \u{2588} \u{2588} \u{2588}\u{2580}\u{2580} \u{2588}\u{2580}\u{2580} \u{2588}   \u{2588}\u{2580}\u{2580}",
        Style::default().bold(),
    )));
    lines.push(Line::from(Span::styled(
        "   \u{2588}  \u{2588}\u{2580}\u{2584} \u{2588} \u{2588} \u{2588}\u{2580}  \u{2588}\u{2580}  \u{2588}   \u{2588}\u{2580}",
        Style::default().bold(),
    )));
    lines.push(Line::from(Span::styled(
        "   \u{2588}  \u{2588} \u{2588} \u{2580}\u{2584}\u{2580} \u{2588}   \u{2588}   \u{2588}\u{2584}\u{2584} \u{2588}\u{2584}\u{2584}",
        Style::default().bold(),
    )));

    // Blank line
    lines.push(Line::raw(""));

    // Tagline
    lines.push(Line::from(Span::styled(
        "  mesh networking for your devices",
        Style::default().fg(Color::DarkGray),
    )));

    // Blank line
    lines.push(Line::raw(""));

    // Node info
    let mut info_spans = vec![
        Span::raw("  "),
        Span::styled(&node_name, Style::default().bold()),
    ];
    if !short_device_id.is_empty() {
        info_spans.push(Span::styled(
            format!(" \u{00b7} {short_device_id}\u{2026}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if !ip_str.is_empty() {
        info_spans.push(Span::raw(" \u{00b7} "));
        info_spans.push(Span::raw(ip_str));
    }
    lines.push(Line::from(info_spans));

    // Status line
    let status_text = if !info.ip.map(|_| true).unwrap_or(false) {
        "connecting"
    } else {
        "online"
    };
    let (indicator, color) = if status_text == "online" {
        ("\u{25cf}", Color::Green)
    } else {
        ("\u{25cf}", Color::Yellow)
    };

    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(indicator, Style::default().fg(color)),
        Span::styled(format!(" {status_text}"), Style::default().fg(color)),
        Span::raw(format!(" \u{00b7} {peer_count} peers")),
        Span::styled(
            format!(" \u{00b7} {uptime}"),
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}

fn render_right(frame: &mut Frame, area: Rect, app: &AppState) {
    let mut lines: Vec<Line> = Vec::new();

    // Top margin (align with left half)
    lines.push(Line::raw(""));

    // Section header
    lines.push(Line::from(Span::styled("Devices", Style::default().bold())));

    // Sort: online first, then alphabetical
    let mut sorted_peers: Vec<_> = app.peers.iter().collect();
    sorted_peers.sort_by(|a, b| b.online.cmp(&a.online).then(a.name.cmp(&b.name)));

    for peer in &sorted_peers {
        let (indicator, style) = if peer.online {
            ("\u{25cf}", Style::default().fg(Color::Green))
        } else {
            ("\u{25cb}", Style::default().fg(Color::DarkGray))
        };

        let conn = if peer.online {
            peer.connection
                .as_deref()
                .map(|c| format!(" ({c})"))
                .unwrap_or_else(|| " (direct)".to_string())
        } else {
            " (offline)".to_string()
        };

        lines.push(Line::from(vec![
            Span::styled(indicator, style),
            Span::raw(" "),
            Span::raw(peer.name.clone()),
            Span::styled(conn, Style::default().fg(Color::DarkGray)),
        ]));
    }

    if sorted_peers.is_empty() {
        lines.push(Line::from(Span::styled(
            "No peers discovered",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}
