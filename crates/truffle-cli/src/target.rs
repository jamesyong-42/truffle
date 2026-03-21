//! Unified target addressing for truffle commands.
//!
//! All commands that accept a `<target>` argument use the same addressing
//! format:
//!
//! - `node` -- just a node name
//! - `node:port` -- node with port (e.g., `laptop:8080`)
//! - `node:port/path` -- node with port and path (e.g., `server:3000/ws`)
//! - `node/path` -- node with path but no port
//! - `scheme://node:port/path` -- full URL form (e.g., `ws://chat:3000/events`)

use std::fmt;

/// A parsed target address.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// Node name, alias, or IP address.
    pub node: String,
    /// Optional port number.
    pub port: Option<u16>,
    /// Optional path (always starts with `/`).
    pub path: Option<String>,
    /// Optional scheme (e.g., "tcp", "ws", "wss", "http", "https").
    pub scheme: Option<String>,
}

/// Errors that can occur when parsing a target.
#[derive(Debug, Clone, PartialEq)]
pub enum TargetParseError {
    /// The input string was empty.
    Empty,
    /// The URL scheme was recognized but the URL was malformed.
    InvalidUrl(String),
    /// The node name was missing (e.g., `:8080` without a node).
    MissingNode,
}

impl fmt::Display for TargetParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TargetParseError::Empty => write!(f, "Target string is empty"),
            TargetParseError::InvalidUrl(msg) => write!(f, "Invalid URL: {msg}"),
            TargetParseError::MissingNode => write!(f, "Missing node name in target"),
        }
    }
}

impl std::error::Error for TargetParseError {}

impl Target {
    /// Parse a target string.
    ///
    /// Supported formats:
    /// - `"laptop"` -> node only
    /// - `"laptop:8080"` -> node + port
    /// - `"server:3000/ws"` -> node + port + path
    /// - `"server/ws"` -> node + path
    /// - `"ws://chat:3000/events"` -> scheme + node + port + path
    pub fn parse(s: &str) -> Result<Self, TargetParseError> {
        let s = s.trim();
        if s.is_empty() {
            return Err(TargetParseError::Empty);
        }

        // If it contains "://", parse as a URL
        if s.contains("://") {
            return Self::parse_url(s);
        }

        // If it contains "/", split into node_port and path
        if let Some((node_port, path)) = s.split_once('/') {
            let (node, port) = Self::split_node_port(node_port);
            if node.is_empty() {
                return Err(TargetParseError::MissingNode);
            }
            return Ok(Self {
                node,
                port,
                path: Some(format!("/{path}")),
                scheme: None,
            });
        }

        // Otherwise, it's just node:port or node
        let (node, port) = Self::split_node_port(s);
        if node.is_empty() {
            return Err(TargetParseError::MissingNode);
        }
        Ok(Self {
            node,
            port,
            path: None,
            scheme: None,
        })
    }

    /// Parse a URL-form target (e.g., `ws://chat:3000/events`).
    fn parse_url(s: &str) -> Result<Self, TargetParseError> {
        let url =
            url::Url::parse(s).map_err(|e| TargetParseError::InvalidUrl(e.to_string()))?;

        let node = url
            .host_str()
            .unwrap_or("")
            .to_string();

        if node.is_empty() {
            return Err(TargetParseError::MissingNode);
        }

        let path = {
            let p = url.path().to_string();
            if p.is_empty() || p == "/" { None } else { Some(p) }
        };

        // url::Url::port() returns None for default ports (80 for http, 443
        // for https). Use port_or_known_default() to get the actual port
        // when it was explicitly specified in the input.
        let port = url.port().or_else(|| {
            // Only include the default port if it was explicitly written
            // in the original string (check if the string contains `:port`
            // after the host).
            let host = url.host_str().unwrap_or("");
            let after_scheme = s.split_once("://").map(|(_, rest)| rest).unwrap_or(s);
            let after_host = after_scheme.strip_prefix(host).unwrap_or("");
            if after_host.starts_with(':') {
                url.port_or_known_default()
            } else {
                None
            }
        });

        Ok(Self {
            node,
            port,
            path,
            scheme: Some(url.scheme().to_string()),
        })
    }

    /// Split a `node:port` string into (node, optional port).
    ///
    /// If the part after the last colon is not a valid u16, treat the
    /// entire string as the node name (to handle IPv6 or names with colons).
    fn split_node_port(s: &str) -> (String, Option<u16>) {
        match s.rsplit_once(':') {
            Some((node, port_str)) => match port_str.parse::<u16>() {
                Ok(port) => (node.to_string(), Some(port)),
                Err(_) => (s.to_string(), None),
            },
            None => (s.to_string(), None),
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(scheme) = &self.scheme {
            write!(f, "{scheme}://")?;
        }
        write!(f, "{}", self.node)?;
        if let Some(port) = self.port {
            write!(f, ":{port}")?;
        }
        if let Some(path) = &self.path {
            write!(f, "{path}")?;
        }
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_target_parse_node_only() {
        let target = Target::parse("laptop").unwrap();
        assert_eq!(target.node, "laptop");
        assert_eq!(target.port, None);
        assert_eq!(target.path, None);
        assert_eq!(target.scheme, None);
    }

    #[test]
    fn test_target_parse_node_port() {
        let target = Target::parse("laptop:8080").unwrap();
        assert_eq!(target.node, "laptop");
        assert_eq!(target.port, Some(8080));
        assert_eq!(target.path, None);
        assert_eq!(target.scheme, None);
    }

    #[test]
    fn test_target_parse_with_path() {
        let target = Target::parse("server:3000/ws").unwrap();
        assert_eq!(target.node, "server");
        assert_eq!(target.port, Some(3000));
        assert_eq!(target.path, Some("/ws".to_string()));
        assert_eq!(target.scheme, None);
    }

    #[test]
    fn test_target_parse_node_path_no_port() {
        let target = Target::parse("server/ws").unwrap();
        assert_eq!(target.node, "server");
        assert_eq!(target.port, None);
        assert_eq!(target.path, Some("/ws".to_string()));
        assert_eq!(target.scheme, None);
    }

    #[test]
    fn test_target_parse_with_scheme() {
        let target = Target::parse("ws://server:3000/events").unwrap();
        assert_eq!(target.node, "server");
        assert_eq!(target.port, Some(3000));
        assert_eq!(target.path, Some("/events".to_string()));
        assert_eq!(target.scheme, Some("ws".to_string()));
    }

    #[test]
    fn test_target_parse_https_scheme() {
        let target = Target::parse("https://my-server:443/api/v1").unwrap();
        assert_eq!(target.node, "my-server");
        assert_eq!(target.port, Some(443));
        assert_eq!(target.path, Some("/api/v1".to_string()));
        assert_eq!(target.scheme, Some("https".to_string()));
    }

    #[test]
    fn test_target_parse_scheme_no_port() {
        let target = Target::parse("ws://chat/events").unwrap();
        assert_eq!(target.node, "chat");
        assert_eq!(target.port, None);
        assert_eq!(target.path, Some("/events".to_string()));
        assert_eq!(target.scheme, Some("ws".to_string()));
    }

    #[test]
    fn test_target_parse_scheme_no_path() {
        let target = Target::parse("tcp://db:5432").unwrap();
        assert_eq!(target.node, "db");
        assert_eq!(target.port, Some(5432));
        assert_eq!(target.path, None);
        assert_eq!(target.scheme, Some("tcp".to_string()));
    }

    #[test]
    fn test_target_parse_ip_address() {
        let target = Target::parse("100.64.0.3:5432").unwrap();
        assert_eq!(target.node, "100.64.0.3");
        assert_eq!(target.port, Some(5432));
    }

    #[test]
    fn test_target_parse_empty() {
        let result = Target::parse("");
        assert_eq!(result.unwrap_err(), TargetParseError::Empty);
    }

    #[test]
    fn test_target_parse_whitespace_trimmed() {
        let target = Target::parse("  laptop:8080  ").unwrap();
        assert_eq!(target.node, "laptop");
        assert_eq!(target.port, Some(8080));
    }

    #[test]
    fn test_target_display() {
        assert_eq!(
            Target::parse("laptop:8080").unwrap().to_string(),
            "laptop:8080"
        );
        assert_eq!(
            Target::parse("server:3000/ws").unwrap().to_string(),
            "server:3000/ws"
        );
        assert_eq!(
            Target::parse("ws://chat:3000/events").unwrap().to_string(),
            "ws://chat:3000/events"
        );
        assert_eq!(
            Target::parse("laptop").unwrap().to_string(),
            "laptop"
        );
    }

    #[test]
    fn test_target_parse_deep_path() {
        let target = Target::parse("server:8080/api/v2/users").unwrap();
        assert_eq!(target.node, "server");
        assert_eq!(target.port, Some(8080));
        assert_eq!(target.path, Some("/api/v2/users".to_string()));
    }

    #[test]
    fn test_target_parse_name_with_hyphens() {
        let target = Target::parse("my-cool-server:3000").unwrap();
        assert_eq!(target.node, "my-cool-server");
        assert_eq!(target.port, Some(3000));
    }

    #[test]
    fn test_target_parse_dns_name() {
        let target = Target::parse("truffle-desktop-a1b2.tailnet.ts.net:443").unwrap();
        assert_eq!(target.node, "truffle-desktop-a1b2.tailnet.ts.net");
        assert_eq!(target.port, Some(443));
    }
}
