//! Name resolution for truffle node addressing.
//!
//! Resolves user-provided names to peer device IDs using the Node's peer list.
//! Priority order:
//! 1. Config aliases
//! 2. Peer device names (exact, case-insensitive)
//! 3. Peer device IDs (exact)
//! 4. IP addresses (match against peer IPs)
//! 5. Fuzzy suggestions if no match

use std::collections::HashMap;

use truffle_core::Peer;

// ==========================================================================
// Types
// ==========================================================================

/// A successfully resolved target.
#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    /// The peer's stable device ID (RFC 017 §5.4 ULID), or the Tailscale
    /// stable ID prior to hello completion. Callers should treat this as
    /// the `device_id` for addressing purposes.
    pub peer_id: String,
    /// The display name for the node (the peer's human-readable
    /// `device_name`, NOT the Tailscale hostname slug).
    pub display_name: String,
    /// The peer's IP address.
    pub ip: String,
    /// How the name was resolved.
    pub resolved_via: ResolvedVia,
}

/// How a name was resolved.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolvedVia {
    Alias,
    PeerName,
    PeerId,
    IpAddress,
}

/// Errors that can occur during name resolution.
#[derive(Debug, Clone)]
pub enum ResolveError {
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

// ==========================================================================
// Name resolver
// ==========================================================================

/// Resolves user-provided node names to device IDs using the v2 Peer list.
pub struct NameResolver {
    aliases: HashMap<String, String>,
    peers: Vec<Peer>,
}

impl NameResolver {
    /// Create a new resolver with aliases and the current peer list.
    pub fn new(aliases: HashMap<String, String>, peers: Vec<Peer>) -> Self {
        Self { aliases, peers }
    }

    /// Resolve a user-provided name to a target.
    pub fn resolve(&self, name: &str) -> Result<ResolvedTarget, ResolveError> {
        let name_lower = name.to_lowercase();

        // 1. Check config aliases
        if let Some(target) = self.aliases.get(name) {
            // Alias may resolve to a peer name -- try to find the peer
            for peer in &self.peers {
                if peer.device_name.to_lowercase() == target.to_lowercase() {
                    return Ok(ResolvedTarget {
                        peer_id: peer.device_id.clone(),
                        display_name: peer.device_name.clone(),
                        ip: peer.ip.to_string(),
                        resolved_via: ResolvedVia::Alias,
                    });
                }
            }
            // If alias doesn't match a peer, return not found
            return Err(ResolveError::NotFound {
                input: name.to_string(),
                suggestion: None,
            });
        }

        // 2. Check peer device names (case-insensitive)
        for peer in &self.peers {
            if peer.device_name.to_lowercase() == name_lower {
                return Ok(ResolvedTarget {
                    peer_id: peer.device_id.clone(),
                    display_name: peer.device_name.clone(),
                    ip: peer.ip.to_string(),
                    resolved_via: ResolvedVia::PeerName,
                });
            }
        }

        // 3. Check peer device IDs (exact match)
        for peer in &self.peers {
            if peer.device_id == name {
                return Ok(ResolvedTarget {
                    peer_id: peer.device_id.clone(),
                    display_name: peer.device_name.clone(),
                    ip: peer.ip.to_string(),
                    resolved_via: ResolvedVia::PeerId,
                });
            }
        }

        // 4. Check IP addresses
        if let Ok(addr) = name.parse::<std::net::IpAddr>() {
            for peer in &self.peers {
                if peer.ip == addr {
                    return Ok(ResolvedTarget {
                        peer_id: peer.device_id.clone(),
                        display_name: peer.device_name.clone(),
                        ip: peer.ip.to_string(),
                        resolved_via: ResolvedVia::IpAddress,
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

    fn suggest(&self, name: &str) -> Option<String> {
        let name_lower = name.to_lowercase();
        let mut best: Option<(String, usize)> = None;

        let mut candidates: Vec<String> = Vec::new();
        for (alias, _) in &self.aliases {
            candidates.push(alias.clone());
        }
        for peer in &self.peers {
            candidates.push(peer.device_name.clone());
        }

        for candidate in &candidates {
            let dist = edit_distance(&name_lower, &candidate.to_lowercase());
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
}

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
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[m]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    fn make_peers() -> Vec<Peer> {
        vec![
            Peer {
                // Legacy compat fields: hostname slug + tailscale id.
                id: "laptop-ts-001".to_string(),
                name: "cli-alice-s-macbook".to_string(),
                // RFC 017 primary identity fields — distinct from the legacy
                // compat fields so tests actually prove the resolver reads
                // `device_id` / `device_name`.
                device_id: "01J4K9M2Z8AB3RNYQPW6H5TC0X".to_string(),
                device_name: "Alice's MacBook".to_string(),
                tailscale_id: "laptop-ts-001".to_string(),
                ip: "100.64.0.3".parse::<IpAddr>().unwrap(),
                online: true,
                ws_connected: false,
                connection_type: "direct".to_string(),
                os: Some("macos".to_string()),
                last_seen: None,
            },
            Peer {
                id: "server-ts-002".to_string(),
                name: "cli-prod-server".to_string(),
                device_id: "01J4K9M2Z8AB3RNYQPW6H5TC0Y".to_string(),
                device_name: "Prod Server".to_string(),
                tailscale_id: "server-ts-002".to_string(),
                ip: "100.64.0.1".parse::<IpAddr>().unwrap(),
                online: true,
                ws_connected: true,
                connection_type: "direct".to_string(),
                os: Some("linux".to_string()),
                last_seen: None,
            },
        ]
    }

    #[test]
    fn test_resolve_peer_name() {
        let resolver = NameResolver::new(HashMap::new(), make_peers());
        let result = resolver.resolve("Alice's MacBook").unwrap();
        assert_eq!(result.peer_id, "01J4K9M2Z8AB3RNYQPW6H5TC0X");
        assert_eq!(result.display_name, "Alice's MacBook");
        assert_eq!(result.resolved_via, ResolvedVia::PeerName);
    }

    #[test]
    fn test_resolve_peer_name_case_insensitive() {
        let resolver = NameResolver::new(HashMap::new(), make_peers());
        let result = resolver.resolve("alice's macbook").unwrap();
        assert_eq!(result.peer_id, "01J4K9M2Z8AB3RNYQPW6H5TC0X");
    }

    #[test]
    fn test_resolve_peer_id() {
        let resolver = NameResolver::new(HashMap::new(), make_peers());
        let result = resolver.resolve("01J4K9M2Z8AB3RNYQPW6H5TC0Y").unwrap();
        assert_eq!(result.display_name, "Prod Server");
        assert_eq!(result.resolved_via, ResolvedVia::PeerId);
    }

    /// Regression: the resolver must read `device_id` / `device_name`,
    /// NOT the legacy compat fields (which are the Tailscale hostname slug).
    /// Resolving by the legacy hostname must fail, and resolving by the
    /// legacy tailscale_id must also fail — only `device_*` fields count.
    #[test]
    fn test_resolver_ignores_legacy_compat_fields() {
        let resolver = NameResolver::new(HashMap::new(), make_peers());

        // Legacy hostname slug should NOT resolve.
        let err = resolver.resolve("cli-alice-s-macbook").unwrap_err();
        assert!(matches!(err, ResolveError::NotFound { .. }));

        // Legacy tailscale id should NOT resolve via PeerId (that path
        // now reads peer.device_id).
        let err = resolver.resolve("laptop-ts-001").unwrap_err();
        assert!(matches!(err, ResolveError::NotFound { .. }));
    }

    #[test]
    fn test_resolve_not_found() {
        let resolver = NameResolver::new(HashMap::new(), make_peers());
        let err = resolver.resolve("nonexistent").unwrap_err();
        match err {
            ResolveError::NotFound { input, .. } => {
                assert_eq!(input, "nonexistent");
            }
        }
    }

    #[test]
    fn test_resolve_fuzzy_suggestion() {
        let resolver = NameResolver::new(HashMap::new(), make_peers());
        let err = resolver.resolve("Prod Servr").unwrap_err();
        match err {
            ResolveError::NotFound { suggestion, .. } => {
                assert_eq!(suggestion, Some("Prod Server".to_string()));
            }
        }
    }
}
