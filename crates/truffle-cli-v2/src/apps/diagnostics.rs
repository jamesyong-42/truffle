//! Diagnostics application — status, doctor checks using Node API.
//!
//! All data comes from `node.peers()`, `node.ping()`, `node.health()`,
//! and `node.local_info()`. Zero direct network calls.

use std::path::PathBuf;

use truffle_core_v2::network::NetworkProvider;
use truffle_core_v2::node::Node;

/// A diagnostic check result.
#[derive(Debug)]
#[allow(dead_code)]
pub struct CheckResult {
    pub label: String,
    pub passed: bool,
    pub detail: String,
    pub fix: Option<String>,
}

/// Run all diagnostic checks.
#[allow(dead_code)]
pub async fn run_diagnostics<N: NetworkProvider + 'static>(
    node: &Node<N>,
    sidecar_path: Option<&str>,
) -> Vec<CheckResult> {
    let mut checks = Vec::new();

    // 1. Check sidecar binary exists
    let sidecar_check = if let Some(path) = sidecar_path {
        let exists = PathBuf::from(path).exists();
        CheckResult {
            label: "Sidecar binary".to_string(),
            passed: exists,
            detail: if exists {
                path.to_string()
            } else {
                "not found".to_string()
            },
            fix: if exists {
                None
            } else {
                Some("Run 'truffle install-sidecar' to download it".to_string())
            },
        }
    } else {
        CheckResult {
            label: "Sidecar binary".to_string(),
            passed: false,
            detail: "path not configured".to_string(),
            fix: Some("Set node.sidecar_path in config or run 'truffle install-sidecar'".to_string()),
        }
    };
    checks.push(sidecar_check);

    // 2. Check Tailscale/network health
    let health = node.health().await;
    checks.push(CheckResult {
        label: "Network connection".to_string(),
        passed: health.healthy,
        detail: health.state.clone(),
        fix: if health.healthy {
            None
        } else {
            Some("Check Tailscale status and network connectivity".to_string())
        },
    });

    // 3. Check key expiry
    if let Some(ref expiry) = health.key_expiry {
        checks.push(CheckResult {
            label: "Key expiry".to_string(),
            passed: true,
            detail: expiry.clone(),
            fix: None,
        });
    }

    // 4. Check health warnings
    for warning in &health.warnings {
        checks.push(CheckResult {
            label: "Health warning".to_string(),
            passed: false,
            detail: warning.clone(),
            fix: None,
        });
    }

    // 5. Check mesh reachability
    let peers = node.peers().await;
    let online_count = peers.iter().filter(|p| p.online).count();
    checks.push(CheckResult {
        label: "Mesh peers".to_string(),
        passed: !peers.is_empty(),
        detail: format!(
            "{} total, {} online",
            peers.len(),
            online_count
        ),
        fix: if peers.is_empty() {
            Some("Start truffle on other devices to discover peers".to_string())
        } else {
            None
        },
    });

    // 6. Check config file
    let config_path = crate::config::TruffleConfig::default_path();
    checks.push(CheckResult {
        label: "Config file".to_string(),
        passed: config_path.exists(),
        detail: config_path.display().to_string(),
        fix: if config_path.exists() {
            None
        } else {
            Some("Config will use defaults; create config.toml for customization".to_string())
        },
    });

    checks
}
