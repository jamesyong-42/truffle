//! Configuration file loading for the truffle CLI v2.
//!
//! Configuration is loaded from `~/.config/truffle/config.toml`.
//! Missing fields use sensible defaults, and a missing config file
//! results in all-default configuration.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

// ==========================================================================
// Config types
// ==========================================================================

/// Top-level configuration, deserialized from `config.toml`.
#[derive(Debug, Deserialize, Default, Clone)]
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
}

/// Node identity and behavior configuration.
#[derive(Debug, Deserialize, Clone)]
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
#[derive(Debug, Deserialize, Clone)]
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
#[derive(Debug, Deserialize, Clone)]
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

// ==========================================================================
// Default value functions (for serde)
// ==========================================================================

fn default_node_name() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "truffle-node".to_string())
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
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::ReadError(e) => write!(f, "Failed to read config file: {e}"),
            ConfigError::ParseError(e) => write!(f, "Failed to parse config file: {e}"),
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
        let path = path
            .map(PathBuf::from)
            .unwrap_or_else(Self::default_path);

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
    ///
    /// Uses `truffle-v2.sock` to avoid conflicts with the v1 daemon.
    pub fn socket_path() -> PathBuf {
        #[cfg(unix)]
        {
            config_dir().join("truffle-v2.sock")
        }
        #[cfg(windows)]
        {
            PathBuf::from(r"\\.\pipe\truffle-daemon-v2")
        }
    }

    /// The PID file path: `~/.config/truffle/truffle-v2.pid`.
    pub fn pid_path() -> PathBuf {
        config_dir().join("truffle-v2.pid")
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
}
