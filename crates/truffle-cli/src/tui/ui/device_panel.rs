//! Device panel — kept for future use.
#![allow(dead_code)]
//! Device panel — always-visible peer list on the right side.

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::tui::app::AppState;

pub fn render(frame: &mut Frame, area: Rect, app: &AppState) {
    let block = Block::default()
        .title(" DEVICES ")
        .title_alignment(Alignment::Center)
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let mut lines: Vec<Line> = Vec::new();

    // Online peers first, then offline
    let mut sorted_peers: Vec<_> = app.peers.iter().collect();
    sorted_peers.sort_by(|a, b| b.online.cmp(&a.online).then(a.name.cmp(&b.name)));

    for peer in &sorted_peers {
        let (indicator, style) = if peer.online {
            ("\u{25cf}", Style::default().fg(Color::Green))
        } else {
            ("\u{25cb}", Style::default().fg(Color::DarkGray))
        };

        // Truncate name to fit panel width (leave room for indicator + space)
        let max_name_len = (inner.width as usize).saturating_sub(3);
        let name: String = if peer.name.chars().count() > max_name_len {
            peer.name.chars().take(max_name_len).collect()
        } else {
            peer.name.clone()
        };

        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(indicator, style),
            Span::raw(" "),
            Span::raw(name),
        ]));
    }

    if sorted_peers.is_empty() {
        lines.push(Line::from(Span::styled(
            " no peers",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}
