//! Name resolution for truffle node addressing.
//!
//! Resolves user-provided names to Tailscale addresses using a priority order:
//! 1. Config aliases (e.g., "db" -> "postgres:5432")
//! 2. Mesh device names (device.name field)
//! 3. Tailscale hostnames (device.tailscale_hostname)
//! 4. IP addresses (pass-through)
//! 5. DNS names (device.tailscale_dns_name)
//!
//! If no exact match is found, suggests fuzzy matches.

use std::collections::HashMap;

// ═══════════════════════════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════════════════════════

/// Information about a peer used for name resolution.
#[derive(Debug, Clone)]
pub struct PeerInfo {
    /// Device ID.
    pub id: String,
    /// Device display name.
    pub name: String,
    /// Tailscale hostname.
    pub hostname: String,
    /// Tailscale IP address.
    pub ip: Option<String>,
    /// Full MagicDNS name.
    pub dns_name: Option<String>,
    /// Whether the peer is online.
    pub online: bool,
}

/// A successfully resolved target.
#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    /// The resolved address (IP, hostname, or DNS name).
    pub address: String,
    /// The display name for the node.
    pub display_name: String,
    /// The device ID.
    pub device_id: String,
    /// How the name was resolved.
    pub resolved_via: ResolvedVia,
}

/// How a name was resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedVia {
    /// Matched a config alias.
    Alias,
    /// Matched a mesh device name.
    DeviceName,
    /// Matched a Tailscale hostname.
    Hostname,
    /// Input was already an IP address.
    IpAddress,
    /// Matched a Tailscale DNS name.
    DnsName,
}

/// Errors that can occur during name resolution.
#[derive(Debug, Clone)]
pub enum ResolveError {
    /// No match found. May contain a suggestion.
    NotFound {
        input: String,
        suggestion: Option<String>,
    },
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::NotFound { input, suggestion } => {
                write!(f, "Can't find a node named \"{input}\".")?;
                if let Some(suggest) = suggestion {
                    write!(f, " Did you mean \"{suggest}\"?")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for ResolveError {}

// ═══════════════════════════════════════════════════════════════════════════
// Name resolver
// ═══════════════════════════════════════════════════════════════════════════

/// Resolves user-provided node names to Tailscale addresses.
pub struct NameResolver {
    /// Config aliases (name -> target).
    aliases: HashMap<String, String>,
    /// Known mesh peers.
    peers: Vec<PeerInfo>,
}

impl NameResolver {
    /// Create a new resolver with the given aliases and peer list.
    pub fn new(aliases: HashMap<String, String>, peers: Vec<PeerInfo>) -> Self {
        Self { aliases, peers }
    }

    /// Create a resolver from daemon response data.
    pub fn from_daemon_data(
        aliases: &HashMap<String, String>,
        peers_json: &[serde_json::Value],
    ) -> Self {
        let peers = peers_json
            .iter()
            .map(|p| PeerInfo {
                id: p["id"].as_str().unwrap_or("").to_string(),
                name: p["name"].as_str().unwrap_or("").to_string(),
                hostname: p["hostname"].as_str().unwrap_or("").to_string(),
                ip: p["tailscale_ip"].as_str().map(|s| s.to_string()),
                dns_name: p["tailscale_dns_name"].as_str().map(|s| s.to_string()),
                online: p["status"].as_str() == Some("Online"),
            })
            .collect();

        Self {
            aliases: aliases.clone(),
            peers,
        }
    }

    /// Resolve a user-provided name to a target address.
    ///
    /// Resolution order:
    /// 1. Config aliases
    /// 2. Mesh device names (exact, case-insensitive)
    /// 3. Tailscale hostnames (exact, case-insensitive)
    /// 4. IP address pass-through
    /// 5. DNS names (exact, case-insensitive)
    ///
    /// If no match is found, returns a `ResolveError` with a fuzzy suggestion.
    pub fn resolve(&self, name: &str) -> Result<ResolvedTarget, ResolveError> {
        let name_lower = name.to_lowercase();

        // 1. Check config aliases
        if let Some(target) = self.aliases.get(name) {
            // The alias value may itself be a node name, but we treat it as
            // a resolved address directly
            return Ok(ResolvedTarget {
                address: target.clone(),
                display_name: name.to_string(),
                device_id: String::new(),
                resolved_via: ResolvedVia::Alias,
            });
        }

        // 2. Check mesh device names
        for peer in &self.peers {
            if peer.name.to_lowercase() == name_lower {
                let address = peer
                    .ip
                    .clone()
                    .or_else(|| peer.dns_name.clone())
                    .unwrap_or_else(|| peer.hostname.clone());
                return Ok(ResolvedTarget {
                    address,
                    display_name: peer.name.clone(),
                    device_id: peer.id.clone(),
                    resolved_via: ResolvedVia::DeviceName,
                });
            }
        }

        // 3. Check Tailscale hostnames
        for peer in &self.peers {
            if peer.hostname.to_lowercase() == name_lower {
                let address = peer
                    .ip
                    .clone()
                    .or_else(|| peer.dns_name.clone())
                    .unwrap_or_else(|| peer.hostname.clone());
                return Ok(ResolvedTarget {
                    address,
                    display_name: peer.name.clone(),
                    device_id: peer.id.clone(),
                    resolved_via: ResolvedVia::Hostname,
                });
            }
        }

        // 4. IP address pass-through
        if is_ip_address(name) {
            // Check if any peer has this IP
            for peer in &self.peers {
                if peer.ip.as_deref() == Some(name) {
                    return Ok(ResolvedTarget {
                        address: name.to_string(),
                        display_name: peer.name.clone(),
                        device_id: peer.id.clone(),
                        resolved_via: ResolvedVia::IpAddress,
                    });
                }
            }
            // Even if no peer matches, pass through the IP
            return Ok(ResolvedTarget {
                address: name.to_string(),
                display_name: name.to_string(),
                device_id: String::new(),
                resolved_via: ResolvedVia::IpAddress,
            });
        }

        // 5. Check DNS names
        for peer in &self.peers {
            if let Some(dns) = &peer.dns_name {
                if dns.to_lowercase() == name_lower
                    || dns.to_lowercase().trim_end_matches('.') == name_lower
                {
                    let address = peer
                        .ip
                        .clone()
                        .unwrap_or_else(|| dns.clone());
                    return Ok(ResolvedTarget {
                        address,
                        display_name: peer.name.clone(),
                        device_id: peer.id.clone(),
                        resolved_via: ResolvedVia::DnsName,
                    });
                }
            }
        }

        // No match found -- try fuzzy suggestions
        let suggestion = self.suggest(name);
        Err(ResolveError::NotFound {
            input: name.to_string(),
            suggestion,
        })
    }

    /// Suggest a correction for a misspelled name using edit distance.
    pub fn suggest(&self, name: &str) -> Option<String> {
        let name_lower = name.to_lowercase();
        let mut best: Option<(String, usize)> = None;

        // Collect all known names
        let mut candidates: Vec<String> = Vec::new();
        for (alias, _) in &self.aliases {
            candidates.push(alias.clone());
        }
        for peer in &self.peers {
            candidates.push(peer.name.clone());
            candidates.push(peer.hostname.clone());
        }

        for candidate in &candidates {
            let dist = edit_distance(&name_lower, &candidate.to_lowercase());
            // Only suggest if reasonably close (within ~40% of the name length)
            let threshold = (name.len() / 3).max(2);
            if dist <= threshold {
                if let Some((_, best_dist)) = &best {
                    if dist < *best_dist {
                        best = Some((candidate.clone(), dist));
                    }
                } else {
                    best = Some((candidate.clone(), dist));
                }
            }
        }

        // Also check for substring matches
        if best.is_none() {
            for candidate in &candidates {
                let cand_lower = candidate.to_lowercase();
                if cand_lower.contains(&name_lower) || name_lower.contains(&cand_lower) {
                    return Some(candidate.clone());
                }
            }
        }

        best.map(|(name, _)| name)
    }

    /// Get all known peer names (for autocomplete, listing, etc.).
    pub fn peer_names(&self) -> Vec<&str> {
        self.peers.iter().map(|p| p.name.as_str()).collect()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Helper functions
// ═══════════════════════════════════════════════════════════════════════════

/// Check if a string looks like an IP address (v4 or v6).
fn is_ip_address(s: &str) -> bool {
    s.parse::<std::net::IpAddr>().is_ok()
}

/// Compute the Levenshtein edit distance between two strings.
fn edit_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let n = a_chars.len();
    let m = b_chars.len();

    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }

    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0; m + 1];

    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (prev[j] + 1)
                .min(curr[j - 1] + 1)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[m]
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn make_peers() -> Vec<PeerInfo> {
        vec![
            PeerInfo {
                id: "abc-123".to_string(),
                name: "laptop".to_string(),
                hostname: "truffle-desktop-abc123".to_string(),
                ip: Some("100.64.0.3".to_string()),
                dns_name: Some("truffle-desktop-abc123.tailnet.ts.net.".to_string()),
                online: true,
            },
            PeerInfo {
                id: "def-456".to_string(),
                name: "server".to_string(),
                hostname: "truffle-server-def456".to_string(),
                ip: Some("100.64.0.1".to_string()),
                dns_name: Some("truffle-server-def456.tailnet.ts.net.".to_string()),
                online: true,
            },
            PeerInfo {
                id: "ghi-789".to_string(),
                name: "old-laptop".to_string(),
                hostname: "truffle-desktop-ghi789".to_string(),
                ip: None,
                dns_name: None,
                online: false,
            },
        ]
    }

    fn make_aliases() -> HashMap<String, String> {
        let mut aliases = HashMap::new();
        aliases.insert("db".to_string(), "server:5432".to_string());
        aliases.insert("api".to_string(), "server:3000".to_string());
        aliases
    }

    #[test]
    fn test_resolve_alias() {
        let resolver = NameResolver::new(make_aliases(), make_peers());
        let result = resolver.resolve("db").unwrap();
        assert_eq!(result.address, "server:5432");
        assert_eq!(result.resolved_via, ResolvedVia::Alias);
    }

    #[test]
    fn test_resolve_device_name() {
        let resolver = NameResolver::new(make_aliases(), make_peers());
        let result = resolver.resolve("laptop").unwrap();
        assert_eq!(result.address, "100.64.0.3");
        assert_eq!(result.display_name, "laptop");
        assert_eq!(result.device_id, "abc-123");
        assert_eq!(result.resolved_via, ResolvedVia::DeviceName);
    }

    #[test]
    fn test_resolve_device_name_case_insensitive() {
        let resolver = NameResolver::new(make_aliases(), make_peers());
        let result = resolver.resolve("Laptop").unwrap();
        assert_eq!(result.display_name, "laptop");
        assert_eq!(result.resolved_via, ResolvedVia::DeviceName);
    }

    #[test]
    fn test_resolve_hostname() {
        let resolver = NameResolver::new(make_aliases(), make_peers());
        let result = resolver.resolve("truffle-desktop-abc123").unwrap();
        assert_eq!(result.address, "100.64.0.3");
        assert_eq!(result.display_name, "laptop");
        assert_eq!(result.resolved_via, ResolvedVia::Hostname);
    }

    #[test]
    fn test_resolve_ip_address_known() {
        let resolver = NameResolver::new(make_aliases(), make_peers());
        let result = resolver.resolve("100.64.0.3").unwrap();
        assert_eq!(result.address, "100.64.0.3");
        assert_eq!(result.display_name, "laptop");
        assert_eq!(result.resolved_via, ResolvedVia::IpAddress);
    }

    #[test]
    fn test_resolve_ip_address_unknown() {
        let resolver = NameResolver::new(make_aliases(), make_peers());
        let result = resolver.resolve("100.64.0.99").unwrap();
        assert_eq!(result.address, "100.64.0.99");
        assert_eq!(result.display_name, "100.64.0.99");
        assert_eq!(result.resolved_via, ResolvedVia::IpAddress);
    }

    #[test]
    fn test_resolve_dns_name() {
        let resolver = NameResolver::new(make_aliases(), make_peers());
        let result = resolver
            .resolve("truffle-desktop-abc123.tailnet.ts.net")
            .unwrap();
        assert_eq!(result.address, "100.64.0.3");
        assert_eq!(result.resolved_via, ResolvedVia::DnsName);
    }

    #[test]
    fn test_resolve_not_found_with_suggestion() {
        let resolver = NameResolver::new(make_aliases(), make_peers());
        let err = resolver.resolve("lapton").unwrap_err();
        match err {
            ResolveError::NotFound { input, suggestion } => {
                assert_eq!(input, "lapton");
                assert_eq!(suggestion, Some("laptop".to_string()));
            }
        }
    }

    #[test]
    fn test_resolve_not_found_no_suggestion() {
        let resolver = NameResolver::new(make_aliases(), make_peers());
        let err = resolver.resolve("zzzzzzzzzzzzz").unwrap_err();
        match err {
            ResolveError::NotFound { suggestion, .. } => {
                assert!(suggestion.is_none());
            }
        }
    }

    #[test]
    fn test_fuzzy_suggest_substring() {
        let resolver = NameResolver::new(HashMap::new(), make_peers());
        // "lap" is a substring of "laptop"
        let suggestion = resolver.suggest("lap");
        assert_eq!(suggestion, Some("laptop".to_string()));
    }

    #[test]
    fn test_edit_distance() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("abc", "abd"), 1);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("laptop", "lapton"), 1);
    }

    #[test]
    fn test_is_ip_address() {
        assert!(is_ip_address("100.64.0.3"));
        assert!(is_ip_address("::1"));
        assert!(is_ip_address("192.168.1.1"));
        assert!(!is_ip_address("laptop"));
        assert!(!is_ip_address("server:5432"));
    }

    #[test]
    fn test_peer_names() {
        let resolver = NameResolver::new(HashMap::new(), make_peers());
        let names = resolver.peer_names();
        assert_eq!(names, vec!["laptop", "server", "old-laptop"]);
    }
}
