//! File transfer accept/reject modal dialog.
//!
//! Renders a centered overlay when an incoming file offer arrives.
//! The user can accept, save-as, reject, or "don't ask again" for a peer.
//! Only one dialog is shown at a time; additional offers are queued.

use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::tui::app::{AppState, TransferDialogPhase};

/// Render the file transfer dialog overlay centered on `area`.
///
/// Does nothing if no dialog is active.
pub fn render(f: &mut Frame, area: Rect, app: &AppState) {
    let dialog = match &app.transfer_dialog {
        Some(d) => d,
        None => return,
    };

    match dialog.phase {
        TransferDialogPhase::Prompt => render_prompt(f, area, app),
        TransferDialogPhase::SaveAs => render_save_as(f, area, app),
    }
}

/// Render the main prompt dialog.
fn render_prompt(f: &mut Frame, area: Rect, app: &AppState) {
    let dialog = app.transfer_dialog.as_ref().unwrap();
    let offer = &dialog.offer;

    let size_str = crate::output::format_bytes(offer.size);
    let peer_name = resolve_peer_name(app, &offer.from_peer, &offer.from_name);

    // Build dialog content lines
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                &peer_name,
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" wants to send you a file:"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("\u{1F4C4} {}  ({})", offer.file_name, size_str),
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::raw("  Save to: "),
            Span::styled(
                &dialog.save_path_input,
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("[a]", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw(" Accept    "),
            Span::styled("[s]", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(" Save as...    "),
            Span::styled("[r]", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
            Span::raw(" Reject"),
        ]),
        Line::from(vec![
            Span::raw("                          "),
            Span::styled("[d]", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            Span::raw(" Don't ask from "),
            Span::styled(&peer_name, Style::default().fg(Color::Cyan)),
        ]),
        Line::from(""),
    ];

    let dialog_height = lines.len() as u16 + 2; // +2 for borders
    let dialog_width = 56u16;

    let dialog_area = centered_rect(dialog_width, dialog_height, area);

    // Clear behind the dialog
    f.render_widget(Clear, dialog_area);

    let block = Block::default()
        .title(" incoming file ")
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black));

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, dialog_area);
}

/// Render the save-as path editor dialog.
fn render_save_as(f: &mut Frame, area: Rect, app: &AppState) {
    let dialog = app.transfer_dialog.as_ref().unwrap();
    let offer = &dialog.offer;

    // Build path display with cursor
    let path = &dialog.save_path_input;
    let cursor = dialog.save_path_cursor;

    // Split path at cursor for display
    let byte_pos = char_to_byte_pos(path, cursor);
    let before = &path[..byte_pos];
    let after = &path[byte_pos..];

    // Cursor character (block cursor)
    let cursor_char = if after.is_empty() {
        " "
    } else {
        &after[..after.char_indices().nth(1).map(|(i, _)| i).unwrap_or(after.len())]
    };
    let rest = if after.is_empty() {
        ""
    } else {
        &after[after.char_indices().nth(1).map(|(i, _)| i).unwrap_or(after.len())..]
    };

    let lines = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw(format!("  Save {} to:", offer.file_name)),
        ]),
        Line::from(vec![
            Span::raw("  \u{276F} "),
            Span::styled(before, Style::default().fg(Color::White)),
            Span::styled(
                cursor_char,
                Style::default().fg(Color::Black).bg(Color::White),
            ),
            Span::styled(rest, Style::default().fg(Color::White)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("[Enter]", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
            Span::raw(" Confirm    "),
            Span::styled("[Esc]", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(" Back"),
        ]),
        Line::from(""),
    ];

    let dialog_height = lines.len() as u16 + 2; // +2 for borders
    let dialog_width = 56u16;

    let dialog_area = centered_rect(dialog_width, dialog_height, area);

    // Clear behind the dialog
    f.render_widget(Clear, dialog_area);

    let block = Block::default()
        .title(" incoming file ")
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black));

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, dialog_area);
}

/// Resolve a peer ID to a display name, using the peer cache.
fn resolve_peer_name(app: &AppState, peer_id: &str, fallback: &str) -> String {
    app.peers
        .iter()
        .find(|p| p.id == peer_id)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| {
            if fallback.is_empty() {
                peer_id.to_string()
            } else {
                fallback.to_string()
            }
        })
}

/// Create a centered Rect of the given fixed size within `area`.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let w = width.min(area.width);
    let h = height.min(area.height);

    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;

    Rect::new(x, y, w, h)
}

/// Convert a char index to a byte offset in a string.
fn char_to_byte_pos(s: &str, char_pos: usize) -> usize {
    s.char_indices()
        .nth(char_pos)
        .map(|(byte, _)| byte)
        .unwrap_or(s.len())
}
