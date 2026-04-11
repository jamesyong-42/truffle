//! Configuration file loading for the truffle CLI v2.
//!
//! Configuration is loaded from `~/.config/truffle/config.toml`.
//! Missing fields use sensible defaults, and a missing config file
//! results in all-default configuration.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ==========================================================================
// Config types
// ==========================================================================

/// Top-level configuration, deserialized from `config.toml`.
#[derive(Debug, Deserialize, Serialize, Default, Clone)]
pub struct TruffleConfig {
    /// Node identity and behavior settings.
    #[serde(default)]
    pub node: NodeConfig,
    /// Command aliases (e.g., `db = "postgres:5432"`).
    #[serde(default)]
    pub aliases: HashMap<String, String>,
    /// Output format settings.
    #[serde(default)]
    pub output: OutputConfig,
    /// Daemon process settings.
    #[serde(default)]
    pub daemon: DaemonConfig,
    /// Auto-update settings.
    #[serde(default)]
    pub updates: UpdateConfig,
    /// Peer IDs whose file transfers are auto-accepted without prompting.
    #[serde(default)]
    pub auto_accept_peers: Vec<String>,
}

/// Node identity and behavior configuration.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct NodeConfig {
    /// Display name for this node (defaults to hostname).
    #[serde(default = "default_node_name")]
    pub name: String,
    /// Whether to auto-start the daemon when a command needs it.
    #[serde(default = "default_true")]
    pub auto_up: bool,
    /// Path to the Go sidecar binary (auto-detected if empty).
    #[serde(default)]
    pub sidecar_path: String,
    /// Tailscale state directory (defaults to `~/.config/truffle/state`).
    #[serde(default)]
    pub state_dir: String,
    /// Tailscale auth key (for headless setup).
    #[serde(default)]
    pub auth_key: String,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            name: default_node_name(),
            auto_up: true,
            sidecar_path: String::new(),
            state_dir: String::new(),
            auth_key: String::new(),
        }
    }
}

/// Output format configuration.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct OutputConfig {
    /// Output format: "pretty" or "json".
    #[serde(default = "default_pretty")]
    pub format: String,
    /// Color mode: "auto", "always", "never".
    #[serde(default = "default_auto")]
    pub color: String,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            format: default_pretty(),
            color: default_auto(),
        }
    }
}

/// Daemon process configuration.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct DaemonConfig {
    /// Log level: "trace", "debug", "info", "warn", "error".
    #[serde(default = "default_info")]
    pub log_level: String,
    /// Path to log file (empty = stderr).
    #[serde(default)]
    pub log_file: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            log_level: default_info(),
            log_file: String::new(),
        }
    }
}

/// Auto-update configuration.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct UpdateConfig {
    /// Whether to automatically check for and install updates.
    #[serde(default = "default_true")]
    pub auto_update: bool,
    /// Interval between update checks, in seconds (default: 86400 = 24 hours).
    #[serde(default = "default_check_interval")]
    pub check_interval: u64,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            auto_update: true,
            check_interval: default_check_interval(),
        }
    }
}

// ==========================================================================
// Default value functions (for serde)
// ==========================================================================

fn default_node_name() -> String {
    smart_node_name()
}

/// Generate a clean, compact device name from the OS hostname.
///
/// Rules:
/// 1. Strip domain suffixes (.local, .ts.net, .lan, etc.)
/// 2. Lowercase
/// 3. Strip trailing number patterns (-6, -01)
/// 4. Truncate to 15 chars
/// 5. Generic hostnames (localhost, ip-*, DESKTOP-*) → {os}-{hash4}
pub fn smart_node_name() -> String {
    let raw = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "truffle-node".to_string());

    compact_name(&raw)
}

fn compact_name(raw: &str) -> String {
    // Strip domain suffixes (longest first)
    const SUFFIXES: &[&str] = &[
        ".ec2.internal",
        ".compute.internal",
        ".ts.net lan",
        ".ts.net",
        ".local",
        ".lan",
        ".home",
        ".internal",
        ".localdomain",
    ];

    let mut name = raw.to_string();
    for suffix in SUFFIXES {
        if let Some(stripped) = name.strip_suffix(suffix) {
            name = stripped.to_string();
            break;
        }
        // Case-insensitive check
        let lower = name.to_lowercase();
        if let Some(pos) = lower.rfind(&suffix.to_lowercase()) {
            if pos + suffix.len() == name.len() {
                name = name[..pos].to_string();
                break;
            }
        }
    }

    // Lowercase
    name = name.to_lowercase();

    // Strip trailing number/hyphen patterns: -6, -01, -123
    while name.len() > 1 {
        if let Some(stripped) = name.strip_suffix(|c: char| c.is_ascii_digit()) {
            name = stripped.to_string();
        } else if name.ends_with('-') && name.len() > 1 {
            name = name[..name.len() - 1].to_string();
        } else {
            break;
        }
    }

    // Clean up any remaining trailing hyphens
    name = name.trim_end_matches('-').to_string();

    // Check for generic hostnames
    let is_generic = name.is_empty()
        || name == "localhost"
        || name == "truffle-node"
        || name.starts_with("ip-")
        || name.starts_with("desktop-")
        || name.starts_with("win-")
        || name.len() <= 2;

    if is_generic {
        // Use {os}-{hash4}
        let os = std::env::consts::OS;
        let hash = simple_hash(raw);
        name = format!("{os}-{hash}");
    }

    // Truncate to 15 chars at a word boundary if possible
    if name.len() > 15 {
        if let Some(pos) = name[..15].rfind('-') {
            if pos > 3 {
                name = name[..pos].to_string();
            } else {
                name = name[..15].to_string();
            }
        } else {
            name = name[..15].to_string();
        }
    }

    if name.is_empty() {
        "truffle-node".to_string()
    } else {
        name
    }
}

/// Simple 4-char hex hash of a string.
fn simple_hash(s: &str) -> String {
    let mut h: u32 = 5381;
    for b in s.bytes() {
        h = h.wrapping_mul(33).wrapping_add(b as u32);
    }
    format!("{:04x}", h & 0xFFFF)
}

fn default_true() -> bool {
    true
}

fn default_pretty() -> String {
    "pretty".to_string()
}

fn default_auto() -> String {
    "auto".to_string()
}

fn default_info() -> String {
    "info".to_string()
}

fn default_check_interval() -> u64 {
    86400 // 24 hours
}

// ==========================================================================
// Config loading
// ==========================================================================

/// Errors that can occur when loading configuration.
#[derive(Debug)]
pub enum ConfigError {
    /// Failed to read the config file.
    ReadError(std::io::Error),
    /// Failed to parse the TOML content.
    ParseError(toml::de::Error),
    /// Failed to write the config file.
    WriteError(std::io::Error),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::ReadError(e) => write!(f, "Failed to read config file: {e}"),
            ConfigError::ParseError(e) => write!(f, "Failed to parse config file: {e}"),
            ConfigError::WriteError(e) => write!(f, "Failed to write config file: {e}"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl TruffleConfig {
    /// Load configuration from a file path.
    ///
    /// If the file does not exist, returns default configuration.
    /// If the file exists but is invalid TOML, returns an error.
    pub fn load(path: Option<&Path>) -> Result<Self, ConfigError> {
        let path = path.map(PathBuf::from).unwrap_or_else(Self::default_path);

        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                let config: TruffleConfig =
                    toml::from_str(&contents).map_err(ConfigError::ParseError)?;
                Ok(config)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Missing config file is fine -- use defaults
                Ok(TruffleConfig::default())
            }
            Err(e) => Err(ConfigError::ReadError(e)),
        }
    }

    /// The default config file path: `~/.config/truffle/config.toml`.
    pub fn default_path() -> PathBuf {
        config_dir().join("config.toml")
    }

    /// The IPC endpoint path for this platform.
    pub fn socket_path() -> PathBuf {
        #[cfg(unix)]
        {
            config_dir().join("truffle.sock")
        }
        #[cfg(windows)]
        {
            PathBuf::from(r"\\.\pipe\truffle-daemon")
        }
    }

    /// The PID file path: `~/.config/truffle/truffle.pid`.
    pub fn pid_path() -> PathBuf {
        config_dir().join("truffle.pid")
    }

    /// Check if a config file exists at the default path.
    pub fn config_exists() -> bool {
        Self::default_path().exists()
    }

    /// Save configuration to a file.
    pub fn save(&self, path: Option<&Path>) -> Result<(), ConfigError> {
        let path = path.map(PathBuf::from).unwrap_or_else(Self::default_path);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(ConfigError::WriteError)?;
        }

        let toml_str = toml::to_string_pretty(self).map_err(|e| {
            ConfigError::WriteError(std::io::Error::new(std::io::ErrorKind::Other, e))
        })?;
        std::fs::write(&path, toml_str).map_err(ConfigError::WriteError)
    }

    /// Resolve an alias from the config, returning the resolved target string
    /// or the original input if no alias matches.
    pub fn resolve_alias<'a>(&'a self, input: &'a str) -> &'a str {
        self.aliases.get(input).map(|s| s.as_str()).unwrap_or(input)
    }
}

/// Get the truffle config directory (`~/.config/truffle`).
///
/// Creates the directory if it does not exist.
fn config_dir() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("truffle");

    // Best-effort create (may fail if we're just reading)
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_load_defaults() {
        let config = TruffleConfig::default();
        assert!(config.node.auto_up);
        assert_eq!(config.output.format, "pretty");
        assert_eq!(config.output.color, "auto");
        assert_eq!(config.daemon.log_level, "info");
        assert!(config.aliases.is_empty());
    }

    #[test]
    fn test_config_load_from_toml() {
        let toml_content = r#"
[node]
name = "my-laptop"
auto_up = false
sidecar_path = "/usr/local/bin/truffle-sidecar"

[aliases]
db = "postgres:5432"
api = "backend:3000"

[output]
format = "json"
color = "never"

[daemon]
log_level = "debug"
log_file = "/tmp/truffle.log"
"#;

        let config: TruffleConfig = toml::from_str(toml_content).unwrap();

        assert_eq!(config.node.name, "my-laptop");
        assert!(!config.node.auto_up);
        assert_eq!(config.node.sidecar_path, "/usr/local/bin/truffle-sidecar");

        assert_eq!(config.aliases.len(), 2);
        assert_eq!(config.aliases["db"], "postgres:5432");

        assert_eq!(config.output.format, "json");
        assert_eq!(config.daemon.log_level, "debug");
    }

    #[test]
    fn test_compact_name_strips_tsnet() {
        assert_eq!(compact_name("Jamess-MBP-6.ts.net lan"), "jamess-mbp");
    }

    #[test]
    fn test_compact_name_strips_local() {
        assert_eq!(compact_name("Jamess-MBP-6.local"), "jamess-mbp");
    }

    #[test]
    fn test_compact_name_strips_trailing_numbers() {
        assert_eq!(compact_name("my-server-01"), "my-server");
        assert_eq!(compact_name("workstation4090"), "workstation");
    }

    #[test]
    fn test_compact_name_generic_ip() {
        let name = compact_name("ip-172-31-75-21.ec2.internal");
        assert!(
            name.starts_with("linux-")
                || name.starts_with("macos-")
                || name.starts_with("windows-")
        );
        assert!(name.len() <= 15);
    }

    #[test]
    fn test_compact_name_generic_localhost() {
        let name = compact_name("localhost");
        assert!(!name.is_empty());
        assert!(name.contains('-')); // os-hash format
    }

    #[test]
    fn test_compact_name_already_clean() {
        assert_eq!(compact_name("myserver"), "myserver");
    }

    #[test]
    fn test_compact_name_truncation() {
        let name = compact_name("very-long-hostname-that-exceeds-limit");
        assert!(name.len() <= 15);
    }

    #[test]
    fn test_compact_name_desktop_pattern() {
        let name = compact_name("DESKTOP-A1B2C3D");
        assert!(name.contains('-')); // os-hash format (generic)
    }
}
