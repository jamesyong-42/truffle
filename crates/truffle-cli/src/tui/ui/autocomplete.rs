//! Autocomplete overlay rendering — popup above the input bar.
//!
//! Renders a floating overlay with filtered options (device names or commands).
//! Must be rendered LAST in the draw function so it appears on top.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::tui::app::{AppState, AutocompleteKind};

/// Render the autocomplete overlay above the input bar, if active.
///
/// `input_area` is the Rect of the input bar, so we position the overlay just above it.
pub fn render(f: &mut Frame, input_area: Rect, state: &AppState) {
    let ac = match &state.autocomplete {
        Some(ac) => ac,
        None => return,
    };

    let (items, title): (Vec<(String, String)>, &str) = match ac.kind {
        AutocompleteKind::Device => {
            let devices = state.filtered_devices(&ac.filter);
            let items: Vec<(String, String)> = devices
                .iter()
                .map(|name| {
                    let ip = state
                        .peers
                        .iter()
                        .find(|p| &p.device_name == name)
                        .map(|p| p.ip.clone())
                        .unwrap_or_default();
                    (format!("@{name}"), ip)
                })
                .collect();
            (items, " Devices ")
        }
        AutocompleteKind::Command => {
            let cmds = AppState::filtered_commands(&ac.filter);
            let items: Vec<(String, String)> = cmds
                .iter()
                .map(|c| (c.usage.to_string(), c.description.to_string()))
                .collect();
            (items, " Commands ")
        }
    };

    if items.is_empty() {
        return;
    }

    // Calculate overlay dimensions.
    let max_items = 6;
    let visible_count = items.len().min(max_items);
    let overlay_height = visible_count as u16 + 2; // +2 for borders

    // Width: fit the longest item + padding.
    let max_left_width = items.iter().map(|(l, _)| l.len()).max().unwrap_or(10);
    let max_right_width = items.iter().map(|(_, r)| r.len()).max().unwrap_or(0);
    let content_width = if max_right_width > 0 {
        max_left_width + 3 + max_right_width // 3 for " - " separator
    } else {
        max_left_width
    };
    let overlay_width = (content_width as u16 + 4).min(input_area.width); // +4 for borders + padding

    // Position: above the input bar, aligned to the left.
    let overlay_x = input_area.x;
    let overlay_y = input_area.y.saturating_sub(overlay_height);

    let overlay_rect = Rect::new(overlay_x, overlay_y, overlay_width, overlay_height);

    // Clear the area behind the overlay.
    f.render_widget(Clear, overlay_rect);

    // Build the lines.
    let lines: Vec<Line> = items
        .iter()
        .enumerate()
        .take(visible_count)
        .map(|(i, (left, right))| {
            let is_selected = i == ac.selected;

            let style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            let right_style = if is_selected {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            };

            let mut spans = vec![Span::styled(format!(" {left}"), style)];
            if !right.is_empty() {
                // Pad to align the descriptions.
                let pad = max_left_width.saturating_sub(left.len());
                spans.push(Span::styled(" ".repeat(pad + 1), style));
                spans.push(Span::styled(right, right_style));
            }
            // Fill remaining width with the style (for selected highlight).
            if is_selected {
                let current_len: usize = spans.iter().map(|s| s.content.len()).sum();
                let remaining = (overlay_width as usize).saturating_sub(current_len + 2);
                if remaining > 0 {
                    spans.push(Span::styled(" ".repeat(remaining), style));
                }
            }

            Line::from(spans)
        })
        .collect();

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, overlay_rect);
}
