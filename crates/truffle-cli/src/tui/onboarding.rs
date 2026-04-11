//! First-run onboarding TUI — single-screen wizard.
//!
//! Shows on bare `truffle` when no config.toml exists.
//! Flow: Name prompt → Start daemon → Auth (if needed) → Done → Main TUI

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

use crate::config::{self, TruffleConfig};
use crate::daemon::server::DaemonServer;

// ── State machine ────────────────────────────────────────────────────────

#[derive(Clone, PartialEq)]
enum Phase {
    NamePrompt,
    Starting,
    WaitingForAuth { url: String },
    AuthSuccess { ip: String },
    Done,
}

struct OnboardingState {
    phase: Phase,
    name_input: String,
    cursor_pos: usize,
    spinner_frame: usize,
    error: Option<String>,
    started_at: Option<Instant>,
}

impl OnboardingState {
    fn new() -> Self {
        let suggested = config::smart_node_name();
        let cursor = suggested.len();
        Self {
            phase: Phase::NamePrompt,
            name_input: suggested,
            cursor_pos: cursor,
            spinner_frame: 0,
            error: None,
            started_at: None,
        }
    }
}

// ── Entry point ──────────────────────────────────────────────────────────

/// Run the onboarding wizard. Returns the started DaemonServer and updated config.
pub async fn run(base_config: &TruffleConfig) -> Result<(DaemonServer, TruffleConfig), String> {
    let mut state = OnboardingState::new();

    // Initialize terminal
    enable_raw_mode().map_err(|e| format!("Failed to enable raw mode: {e}"))?;
    io::stdout()
        .execute(EnterAlternateScreen)
        .map_err(|e| format!("Failed to enter alternate screen: {e}"))?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))
        .map_err(|e| format!("Failed to create terminal: {e}"))?;

    // Name prompt phase
    loop {
        terminal
            .draw(|f| render(f, &state))
            .map_err(|e| format!("Draw error: {e}"))?;

        if event::poll(Duration::from_millis(100)).map_err(|e| format!("Poll error: {e}"))? {
            if let Event::Key(key) = event::read().map_err(|e| format!("Read error: {e}"))? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        restore_terminal();
                        std::process::exit(0);
                    }
                    KeyCode::Esc => {
                        restore_terminal();
                        std::process::exit(0);
                    }
                    KeyCode::Enter if state.phase == Phase::NamePrompt => {
                        let name = state.name_input.trim().to_string();
                        if name.is_empty() {
                            state.error = Some("Name cannot be empty".to_string());
                        } else if name.len() > 15 {
                            state.error = Some("Name must be 15 characters or less".to_string());
                        } else {
                            state.error = None;
                            state.phase = Phase::Starting;
                            state.started_at = Some(Instant::now());
                            break;
                        }
                    }
                    KeyCode::Char(c) if state.phase == Phase::NamePrompt => {
                        if c.is_alphanumeric() || c == '-' || c == '_' {
                            state.name_input.insert(state.cursor_pos, c);
                            state.cursor_pos += 1;
                            state.error = None;
                        }
                    }
                    KeyCode::Backspace if state.phase == Phase::NamePrompt => {
                        if state.cursor_pos > 0 {
                            state.cursor_pos -= 1;
                            state.name_input.remove(state.cursor_pos);
                            state.error = None;
                        }
                    }
                    KeyCode::Left if state.phase == Phase::NamePrompt => {
                        state.cursor_pos = state.cursor_pos.saturating_sub(1);
                    }
                    KeyCode::Right if state.phase == Phase::NamePrompt => {
                        if state.cursor_pos < state.name_input.len() {
                            state.cursor_pos += 1;
                        }
                    }
                    KeyCode::Home if state.phase == Phase::NamePrompt => {
                        state.cursor_pos = 0;
                    }
                    KeyCode::End if state.phase == Phase::NamePrompt => {
                        state.cursor_pos = state.name_input.len();
                    }
                    _ => {}
                }
            }
        }
    }

    // Save config with chosen name
    let mut config = base_config.clone();
    config.node.name = state.name_input.clone();
    if let Err(e) = config.save(None) {
        restore_terminal();
        return Err(format!("Failed to save config: {e}"));
    }

    // Render starting phase
    terminal
        .draw(|f| render(f, &state))
        .map_err(|e| format!("Draw error: {e}"))?;

    // Set up auth event channel
    let (_auth_tx, mut auth_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    // Start the daemon in a background task
    let config_clone = config.clone();
    let server_handle = tokio::spawn(async move { DaemonServer::start(&config_clone).await });

    // Poll for auth events and daemon completion
    let server = loop {
        // Check for auth events (non-blocking)
        if let Ok(url) = auth_rx.try_recv() {
            state.phase = Phase::WaitingForAuth { url: url.clone() };
            // Auto-open browser
            let _ = open::that(&url);
        }

        // Check if daemon started
        if server_handle.is_finished() {
            match server_handle.await {
                Ok(Ok(server)) => {
                    // Check if we got an IP
                    let info = server.node().local_info();
                    let ip = info.ip.map(|ip| ip.to_string()).unwrap_or_default();
                    if !ip.is_empty() {
                        state.phase = Phase::AuthSuccess { ip };
                    } else {
                        state.phase = Phase::Done;
                    }
                    terminal
                        .draw(|f| render(f, &state))
                        .map_err(|e| format!("Draw error: {e}"))?;

                    // Brief pause to show success
                    tokio::time::sleep(Duration::from_secs(2)).await;

                    state.phase = Phase::Done;
                    break server;
                }
                Ok(Err(e)) => {
                    restore_terminal();
                    return Err(format!("Failed to start: {e}"));
                }
                Err(e) => {
                    restore_terminal();
                    return Err(format!("Daemon task failed: {e}"));
                }
            }
        }

        // Update spinner
        state.spinner_frame = (state.spinner_frame + 1) % 10;

        // Render
        terminal
            .draw(|f| render(f, &state))
            .map_err(|e| format!("Draw error: {e}"))?;

        // Handle Ctrl+C during startup
        if event::poll(Duration::from_millis(200)).unwrap_or(false) {
            if let Ok(Event::Key(key)) = event::read() {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    restore_terminal();
                    std::process::exit(0);
                }
            }
        }
    };

    // Restore terminal — the main TUI will re-init it
    restore_terminal();

    Ok((server, config))
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = io::stdout().execute(LeaveAlternateScreen);
}

// ── Rendering ────────────────────────────────────────────────────────────

const SPINNERS: &[&str] = &["◐", "◓", "◑", "◒"];

fn render(frame: &mut Frame, state: &OnboardingState) {
    let area = frame.area();

    // Calculate centered box
    let box_width = 60u16.min(area.width.saturating_sub(4));
    let box_height = 18u16.min(area.height.saturating_sub(2));
    let box_x = (area.width.saturating_sub(box_width)) / 2;
    let box_y = (area.height.saturating_sub(box_height)) / 2;
    let box_area = Rect::new(box_x, box_y, box_width, box_height);

    let block = Block::default()
        .title(" truffle \u{2500}\u{2500} first time setup ")
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(box_area);
    frame.render_widget(block, box_area);

    let mut lines: Vec<Line> = Vec::new();

    // Logo (always shown)
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "        \u{2580}\u{2588}\u{2580} \u{2588}\u{2580}\u{2588} \u{2588} \u{2588} \u{2588}\u{2580}\u{2580} \u{2588}\u{2580}\u{2580} \u{2588}   \u{2588}\u{2580}\u{2580}",
        Style::default().bold(),
    )));
    lines.push(Line::from(Span::styled(
        "         \u{2588}  \u{2588}\u{2580}\u{2584} \u{2588} \u{2588} \u{2588}\u{2580}  \u{2588}\u{2580}  \u{2588}   \u{2588}\u{2580}",
        Style::default().bold(),
    )));
    lines.push(Line::from(Span::styled(
        "         \u{2588}  \u{2588} \u{2588} \u{2580}\u{2584}\u{2580} \u{2588}   \u{2588}   \u{2588}\u{2584}\u{2584} \u{2588}\u{2584}\u{2584}",
        Style::default().bold(),
    )));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "        mesh networking for your devices",
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::raw(""));

    // Separator
    let sep_width = (inner.width as usize).saturating_sub(4);
    lines.push(Line::from(Span::styled(
        format!("  {}", "\u{2504}".repeat(sep_width)),
        Style::default().fg(Color::DarkGray),
    )));
    lines.push(Line::raw(""));

    // Phase-specific content
    match &state.phase {
        Phase::NamePrompt => {
            lines.push(Line::from(Span::raw("  Name this device:")));
            lines.push(Line::from(vec![
                Span::styled("  \u{276f} ", Style::default().fg(Color::Cyan).bold()),
                Span::raw(&state.name_input),
            ]));
            lines.push(Line::raw(""));
            if let Some(err) = &state.error {
                lines.push(Line::from(Span::styled(
                    format!("  {err}"),
                    Style::default().fg(Color::Red),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    "  This name identifies your device on the mesh.",
                    Style::default().fg(Color::DarkGray),
                )));
                lines.push(Line::from(Span::styled(
                    "  Press Enter to confirm, or type a different name.",
                    Style::default().fg(Color::DarkGray),
                )));
            }

            // Place cursor
            let cursor_x = inner.x + 4 + state.cursor_pos as u16;
            let cursor_y = inner.y + 9; // line index of the input
            if cursor_x < inner.x + inner.width {
                frame.set_cursor_position((cursor_x, cursor_y));
            }
        }

        Phase::Starting => {
            let spinner = SPINNERS[state.spinner_frame % SPINNERS.len()];
            lines.push(Line::from(vec![
                Span::styled("  \u{2713} ", Style::default().fg(Color::Green)),
                Span::raw(format!("Device name: {}", state.name_input)),
            ]));
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled(format!("  {spinner} "), Style::default().fg(Color::Yellow)),
                Span::raw("Starting truffle..."),
            ]));
        }

        Phase::WaitingForAuth { url } => {
            let spinner = SPINNERS[state.spinner_frame % SPINNERS.len()];
            lines.push(Line::from(vec![
                Span::styled("  \u{2713} ", Style::default().fg(Color::Green)),
                Span::raw(format!("Device name: {}", state.name_input)),
            ]));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::raw("  Open this URL to authenticate:")));
            lines.push(Line::from(Span::styled(
                format!("  {url}"),
                Style::default().fg(Color::Cyan).underlined(),
            )));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "  [Browser opened automatically]",
                Style::default().fg(Color::DarkGray),
            )));
            lines.push(Line::from(vec![
                Span::styled(format!("  {spinner} "), Style::default().fg(Color::Yellow)),
                Span::raw("Waiting for authentication..."),
            ]));
        }

        Phase::AuthSuccess { ip } => {
            lines.push(Line::from(vec![
                Span::styled("  \u{2713} ", Style::default().fg(Color::Green)),
                Span::raw(format!("Device name: {}", state.name_input)),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  \u{2713} ", Style::default().fg(Color::Green)),
                Span::raw("Connected to Tailscale"),
            ]));
            lines.push(Line::from(vec![
                Span::styled("  \u{2713} ", Style::default().fg(Color::Green)),
                Span::raw(format!("IP: {ip}")),
            ]));
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                "  Launching truffle...",
                Style::default().fg(Color::DarkGray),
            )));
        }

        Phase::Done => {
            lines.push(Line::from(Span::styled(
                "  Launching truffle...",
                Style::default().fg(Color::DarkGray),
            )));
        }
    }

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}
