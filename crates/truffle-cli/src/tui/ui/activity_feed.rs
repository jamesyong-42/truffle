//! Activity feed — scrollable list of DisplayItems.

use ratatui::prelude::*;
use ratatui::widgets::Paragraph;

use crate::tui::app::{AppState, DisplayItem, PeerEventKind, SystemLevel};

pub fn render(frame: &mut Frame, area: Rect, app: &AppState) {
    if area.height == 0 {
        return;
    }

    let visible_height = area.height as usize;
    let total_items = app.items.len();

    // Calculate which items to show based on scroll position
    let end = if app.auto_scroll {
        total_items
    } else {
        total_items.saturating_sub(app.scroll_offset)
    };
    let start = end.saturating_sub(visible_height);

    let visible_items = &app.items[start..end];
    let mut lines: Vec<Line> = Vec::new();

    for (i, item) in visible_items.iter().enumerate() {
        // Check if this is a continuation of the same sender (message grouping)
        let is_continuation = if i > 0 {
            is_same_sender(&visible_items[i - 1], item)
        } else if start > 0 {
            // Check against the item just above the viewport
            is_same_sender(&app.items[start - 1], item)
        } else {
            false
        };

        if is_continuation {
            lines.push(render_continuation(item));
        } else {
            lines.push(render_item(item));
        }
    }

    // Pad with empty lines if we have fewer items than the viewport
    while lines.len() < visible_height {
        lines.push(Line::raw(""));
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}

/// Check if two consecutive items are from the same sender (for grouping).
fn is_same_sender(prev: &DisplayItem, curr: &DisplayItem) -> bool {
    match (prev, curr) {
        (
            DisplayItem::ChatOutgoing { to: to1, .. },
            DisplayItem::ChatOutgoing { to: to2, .. },
        ) => to1 == to2,
        (
            DisplayItem::ChatIncoming { from: f1, .. },
            DisplayItem::ChatIncoming { from: f2, .. },
        ) => f1 == f2,
        _ => false,
    }
}

/// Render a continuation message (same sender — no prefix, just indented text).
fn render_continuation(item: &DisplayItem) -> Line<'static> {
    match item {
        DisplayItem::ChatOutgoing { time, text, .. } => {
            let ts = format!("{}", time.format("%H:%M"));
            // Indent to align with the text after "You → device: "
            Line::from(vec![
                Span::styled(format!("  {ts}  "), Style::default().fg(Color::DarkGray)),
                Span::styled(format!("              {text}"), Style::default().fg(Color::Cyan)),
            ])
        }
        DisplayItem::ChatIncoming { time, text, .. } => {
            let ts = format!("{}", time.format("%H:%M"));
            Line::from(vec![
                Span::styled(format!("  {ts}  "), Style::default().fg(Color::DarkGray)),
                Span::raw(format!("              {text}")),
            ])
        }
        // Non-chat items don't get continuation rendering
        other => render_item(other),
    }
}

fn render_item(item: &DisplayItem) -> Line<'static> {
    match item {
        DisplayItem::System { text, level, .. } => {
            let style = match level {
                SystemLevel::Info => Style::default().fg(Color::DarkGray),
                SystemLevel::Success => Style::default().fg(Color::Green),
                SystemLevel::Warning => Style::default().fg(Color::Yellow),
                SystemLevel::Error => Style::default().fg(Color::Red),
            };
            // Banner lines get no timestamp
            Line::from(Span::styled(text.clone(), style))
        }

        DisplayItem::PeerEvent {
            time,
            kind,
            peer_name,
            detail,
            ..
        } => {
            let ts = format!("{}", time.format("%H:%M"));
            let (indicator, verb, color) = match kind {
                PeerEventKind::Joined => ("\u{25cf}", "joined", Color::Green),
                PeerEventKind::Left => ("\u{25cb}", "left", Color::DarkGray),
                PeerEventKind::Connected => ("\u{25cf}", "connected", Color::Green),
                PeerEventKind::Disconnected => ("\u{25cb}", "disconnected", Color::DarkGray),
            };
            let detail_str = if detail.is_empty() {
                String::new()
            } else {
                format!(" ({detail})")
            };

            Line::from(vec![
                Span::styled(format!("  {ts}  "), Style::default().fg(Color::DarkGray)),
                Span::styled(format!("{indicator} "), Style::default().fg(color)),
                Span::styled(peer_name.clone(), Style::default().bold()),
                Span::raw(format!(" {verb}{detail_str}")),
            ])
        }

        DisplayItem::ChatOutgoing { time, to, text, .. } => {
            let ts = format!("{}", time.format("%H:%M"));
            Line::from(vec![
                Span::styled(format!("  {ts}  "), Style::default().fg(Color::DarkGray)),
                Span::styled("You", Style::default().fg(Color::Cyan).bold()),
                Span::styled(format!(" \u{2192} {to}: "), Style::default().fg(Color::Cyan)),
                Span::styled(text.clone(), Style::default().fg(Color::Cyan)),
            ])
        }

        DisplayItem::ChatIncoming {
            time, from, text, ..
        } => {
            let ts = format!("{}", time.format("%H:%M"));
            Line::from(vec![
                Span::styled(format!("  {ts}  "), Style::default().fg(Color::DarkGray)),
                Span::styled(from.clone(), Style::default().bold()),
                Span::raw(format!(" \u{2192} You: ")),
                Span::raw(text.clone()),
            ])
        }

        DisplayItem::FileTransfer {
            time,
            direction,
            file_name,
            size,
            status,
            ..
        } => {
            let ts = format!("{}", time.format("%H:%M"));
            let arrow = match direction {
                crate::tui::app::TransferDirection::Send => "\u{2b06}",
                crate::tui::app::TransferDirection::Receive => "\u{2b07}",
            };
            let size_str = crate::output::format_bytes(*size);

            match status {
                crate::tui::app::TransferStatus::InProgress { percent, speed_bps } => {
                    // Progress bar: ████████████░░░░░░░░ 45%  1.2 MB/s
                    let bar_width: usize = 24;
                    let filled = (*percent / 100.0 * bar_width as f64) as usize;
                    let empty = bar_width.saturating_sub(filled);
                    let bar_filled = "\u{2588}".repeat(filled);
                    let bar_empty = "\u{2591}".repeat(empty);
                    let speed_str = crate::output::format_speed(*speed_bps);

                    Line::from(vec![
                        Span::styled(format!("  {ts}  "), Style::default().fg(Color::DarkGray)),
                        Span::raw(format!("{arrow} {file_name} ")),
                        Span::styled(bar_filled, Style::default().fg(Color::Cyan)),
                        Span::styled(bar_empty, Style::default().fg(Color::DarkGray)),
                        Span::raw(format!(" {percent:.0}%")),
                        Span::styled(format!("  {speed_str}"), Style::default().fg(Color::DarkGray)),
                    ])
                }
                crate::tui::app::TransferStatus::Complete { sha256 } => {
                    let sha_short = if sha256.len() >= 8 {
                        format!("{}..{}", &sha256[..4], &sha256[sha256.len() - 4..])
                    } else {
                        sha256.clone()
                    };
                    Line::from(vec![
                        Span::styled(format!("  {ts}  "), Style::default().fg(Color::DarkGray)),
                        Span::raw(format!("{arrow} {file_name} ")),
                        Span::styled("\u{2713}", Style::default().fg(Color::Green)),
                        Span::styled(format!("  {size_str}  sha:{sha_short}"), Style::default().fg(Color::DarkGray)),
                    ])
                }
                crate::tui::app::TransferStatus::Failed { reason } => {
                    Line::from(vec![
                        Span::styled(format!("  {ts}  "), Style::default().fg(Color::DarkGray)),
                        Span::raw(format!("{arrow} {file_name} ")),
                        Span::styled("\u{2717}", Style::default().fg(Color::Red)),
                        Span::styled(format!("  {reason}"), Style::default().fg(Color::Red)),
                    ])
                }
            }
        }
    }
}
