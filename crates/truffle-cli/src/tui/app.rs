//! Application state for the TUI.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Local};
use truffle_core::file_transfer::types::{FileOffer, OfferResponder};
use truffle_core::network::tailscale::TailscaleProvider;
use truffle_core::node::Node;

use ratatui_explorer::{FileExplorer, FileExplorerBuilder, Theme};

use crate::config::TruffleConfig;
use crate::output;

/// Peer info cached for the device panel.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub id: String,
    pub name: String,
    pub ip: String,
    pub online: bool,
    pub connection: Option<String>,
}

/// A single item in the activity feed.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum DisplayItem {
    System {
        time: DateTime<Local>,
        text: String,
        level: SystemLevel,
    },
    PeerEvent {
        time: DateTime<Local>,
        kind: PeerEventKind,
        peer_name: String,
        detail: String,
    },
    ChatOutgoing {
        time: DateTime<Local>,
        to: String,
        text: String,
    },
    ChatIncoming {
        time: DateTime<Local>,
        from: String,
        text: String,
    },
    FileTransfer {
        time: DateTime<Local>,
        direction: TransferDirection,
        file_name: String,
        size: u64,
        status: TransferStatus,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerEventKind {
    Joined,
    Left,
    Connected,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    Send,
    Receive,
}

#[derive(Debug, Clone)]
pub enum TransferStatus {
    InProgress { percent: f64, speed_bps: f64 },
    Complete { sha256: String },
    Failed { reason: String },
}

/// Toast notification (ephemeral overlay).
#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub created_at: Instant,
}

/// Autocomplete overlay state.
#[derive(Debug, Clone)]
pub struct AutocompleteState {
    pub filter: String,
    pub selected: usize,
    pub kind: AutocompleteKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AutocompleteKind {
    Device,
    Command,
}

/// Phase of the file transfer dialog.
#[derive(Debug, Clone, PartialEq)]
pub enum TransferDialogPhase {
    /// Main [a][s][r][d] prompt.
    Prompt,
    /// Editing the save path.
    SaveAs,
    /// File exists — confirm overwrite [y/n].
    OverwriteConfirm,
}

/// State for the file transfer accept/reject modal dialog.
pub struct TransferDialogState {
    /// The incoming file offer.
    pub offer: FileOffer,
    /// The responder (consumed on accept/reject).
    pub responder: Option<OfferResponder>,
    /// Current dialog phase.
    pub phase: TransferDialogPhase,
    /// Editable save path.
    pub save_path_input: String,
    /// Cursor position in the save path input.
    pub save_path_cursor: usize,
    /// When the dialog was created (for timeout).
    pub created_at: Instant,
}

/// Command definition for slash-command autocomplete.
#[derive(Debug, Clone)]
pub struct SlashCommandDef {
    pub name: &'static str,
    pub usage: &'static str,
    pub description: &'static str,
}

pub const SLASH_COMMANDS: &[SlashCommandDef] = &[
    SlashCommandDef {
        name: "send",
        usage: "/send <msg> @device",
        description: "Send a message",
    },
    SlashCommandDef {
        name: "broadcast",
        usage: "/broadcast <msg>",
        description: "Message all peers",
    },
    SlashCommandDef {
        name: "cp",
        usage: "/cp <path> @device",
        description: "Send a file",
    },
    SlashCommandDef {
        name: "exit",
        usage: "/exit",
        description: "Quit truffle",
    },
];

const MAX_FEED_ITEMS: usize = 10_000;

/// The main application state.
pub struct AppState {
    /// The mesh node (direct access, no IPC).
    pub node: Arc<Node<TailscaleProvider>>,

    /// Activity feed items.
    pub items: Vec<DisplayItem>,
    /// Scroll offset from the bottom (0 = at bottom).
    pub scroll_offset: usize,
    /// Whether to auto-scroll on new items.
    pub auto_scroll: bool,

    /// Input text.
    pub input: String,
    /// Cursor position in the input.
    pub cursor_pos: usize,

    /// Command history.
    pub history: Vec<String>,
    pub history_index: Option<usize>,

    /// Cached peer list (updated by PeerEvents).
    pub peers: Vec<PeerInfo>,

    /// Toast notifications.
    pub notifications: VecDeque<Toast>,

    /// Autocomplete overlay state.
    pub autocomplete: Option<AutocompleteState>,

    /// File picker overlay (for /cp command).
    pub file_picker: Option<FileExplorer>,

    /// Unread count (items added while scrolled up).
    pub unread_count: usize,

    /// When the TUI started.
    pub started_at: Instant,

    /// Whether we should quit.
    pub should_quit: bool,

    /// Active file transfer dialog (modal overlay).
    pub transfer_dialog: Option<TransferDialogState>,

    /// Queued file offers waiting to be shown (one dialog at a time).
    pub pending_offers: VecDeque<(FileOffer, OfferResponder)>,

    /// Config (for auto_accept_peers and saving).
    pub config: TruffleConfig,
}

impl AppState {
    pub fn new(node: Arc<Node<TailscaleProvider>>, config: TruffleConfig) -> Self {
        let state = Self {
            node,
            items: Vec::new(),
            scroll_offset: 0,
            auto_scroll: true,
            input: String::new(),
            cursor_pos: 0,
            history: Vec::new(),
            history_index: None,
            peers: Vec::new(),
            notifications: VecDeque::new(),
            autocomplete: None,
            file_picker: None,
            unread_count: 0,
            started_at: Instant::now(),
            should_quit: false,
            transfer_dialog: None,
            pending_offers: VecDeque::new(),
            config,
        };

        state
    }

    /// Push a display item to the feed, respecting the cap.
    pub fn push_item(&mut self, item: DisplayItem) {
        self.items.push(item);
        if !self.auto_scroll {
            self.unread_count += 1;
        }
        if self.items.len() > MAX_FEED_ITEMS {
            self.items.remove(0);
            if self.scroll_offset > 0 {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
        }
    }

    /// Scroll to bottom and reset unread count.
    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.auto_scroll = true;
        self.unread_count = 0;
    }

    // -- Autocomplete methods --

    /// Detect `@partial` at a word boundary before the cursor.
    pub fn find_at_prefix(&self) -> Option<(usize, &str)> {
        let byte_pos = super::char_to_byte_pos(&self.input, self.cursor_pos);
        let before_cursor = &self.input[..byte_pos];
        if let Some(at_pos) = before_cursor.rfind('@') {
            if at_pos == 0 || before_cursor.as_bytes()[at_pos - 1] == b' ' {
                let partial = &self.input[at_pos + 1..byte_pos];
                if partial.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
                    return Some((at_pos, partial));
                }
            }
        }
        None
    }

    /// Detect `/partial` at start of input (no space yet).
    pub fn find_slash_prefix(&self) -> Option<&str> {
        let byte_pos = super::char_to_byte_pos(&self.input, self.cursor_pos);
        if self.input.starts_with('/') && !self.input.contains(' ') {
            Some(&self.input[1..byte_pos.min(self.input.len())])
        } else {
            None
        }
    }

    /// Filter online peers by partial name.
    pub fn filtered_devices(&self, partial: &str) -> Vec<String> {
        let lower = partial.to_lowercase();
        self.peers
            .iter()
            .filter(|p| p.online)
            .filter(|p| p.name.to_lowercase().contains(&lower))
            .map(|p| p.name.clone())
            .collect()
    }

    /// Filter slash commands by partial name.
    pub fn filtered_commands(partial: &str) -> Vec<&'static SlashCommandDef> {
        let lower = partial.to_lowercase();
        SLASH_COMMANDS
            .iter()
            .filter(|c| c.name.to_lowercase().starts_with(&lower))
            .collect()
    }

    /// Update autocomplete state based on current input.
    pub fn update_autocomplete(&mut self) {
        if let Some(partial) = self.find_slash_prefix() {
            let cmds = Self::filtered_commands(partial);
            if !cmds.is_empty() {
                let selected = self.autocomplete.as_ref().map(|a| a.selected.min(cmds.len() - 1)).unwrap_or(0);
                self.autocomplete = Some(AutocompleteState { filter: partial.to_string(), selected, kind: AutocompleteKind::Command });
                return;
            }
        }
        if let Some((_at_pos, partial)) = self.find_at_prefix() {
            let devices = self.filtered_devices(partial);
            if !devices.is_empty() {
                let selected = self.autocomplete.as_ref().map(|a| a.selected.min(devices.len() - 1)).unwrap_or(0);
                self.autocomplete = Some(AutocompleteState { filter: partial.to_string(), selected, kind: AutocompleteKind::Device });
                return;
            }
        }
        self.autocomplete = None;
    }

    /// Accept the selected autocomplete item. Returns true if something was completed.
    pub fn accept_autocomplete(&mut self) -> bool {
        let ac = match self.autocomplete.take() {
            Some(ac) => ac,
            None => return false,
        };
        match ac.kind {
            AutocompleteKind::Command => {
                let cmds = Self::filtered_commands(&ac.filter);
                if let Some(cmd) = cmds.get(ac.selected) {
                    self.input = format!("/{} ", cmd.name);
                    self.cursor_pos = self.input.chars().count();
                    return true;
                }
            }
            AutocompleteKind::Device => {
                if let Some((at_pos, _partial)) = self.find_at_prefix() {
                    let devices = self.filtered_devices(&ac.filter);
                    if let Some(name) = devices.get(ac.selected) {
                        let byte_pos = super::char_to_byte_pos(&self.input, self.cursor_pos);
                        let after_cursor = self.input[byte_pos..].to_string();
                        self.input = format!("{}@{}{}", &self.input[..at_pos], name, after_cursor);
                        self.cursor_pos = self.input[..at_pos].chars().count() + 1 + name.chars().count();
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Navigate autocomplete selection.
    pub fn autocomplete_next(&mut self) {
        let max = match &self.autocomplete {
            Some(ac) => match ac.kind {
                AutocompleteKind::Command => Self::filtered_commands(&ac.filter).len(),
                AutocompleteKind::Device => {
                    let filter = ac.filter.clone();
                    self.filtered_devices(&filter).len()
                }
            },
            None => return,
        };
        if let Some(ref mut ac) = self.autocomplete {
            if ac.selected + 1 < max {
                ac.selected += 1;
            }
        }
    }

    pub fn autocomplete_prev(&mut self) {
        if let Some(ref mut ac) = self.autocomplete {
            if ac.selected > 0 {
                ac.selected -= 1;
            }
        }
    }

    /// Get uptime in seconds.
    pub fn uptime_secs(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }

    /// Load command history from disk.
    pub fn load_history(&mut self) {
        let path = history_path();
        if let Ok(content) = std::fs::read_to_string(&path) {
            self.history = content
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect();
            // Keep only the last 1000 entries
            if self.history.len() > 1000 {
                self.history = self.history.split_off(self.history.len() - 1000);
            }
        }
    }

    /// Append a command to the history file.
    pub fn save_history_entry(&self, entry: &str) {
        let path = history_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            let _ = writeln!(f, "{}", entry);
        }
    }

    /// Get the formatted uptime string.
    pub fn uptime_str(&self) -> String {
        output::format_uptime(self.uptime_secs())
    }

    /// Open the file picker overlay.
    pub fn open_file_picker(&mut self) {
        let theme = Theme::default()
            .add_default_title();
        if let Ok(explorer) = FileExplorerBuilder::build_with_theme(theme) {
            self.file_picker = Some(explorer);
        }
    }

    /// Close the file picker and return the selected file path (if any).
    pub fn close_file_picker_with_selection(&mut self) -> Option<String> {
        let explorer = self.file_picker.take()?;
        let file = explorer.current();
        if file.is_dir {
            None // Don't select directories
        } else {
            Some(file.path.to_string_lossy().to_string())
        }
    }

    /// Count online peers.
    pub fn online_peer_count(&self) -> usize {
        self.peers.iter().filter(|p| p.online).count()
    }
}

/// Path to the command history file.
fn history_path() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("truffle")
        .join("history")
}
