//! `truffle doctor` -- run diagnostics on the truffle installation.
//!
//! Checks:
//! 1. Tailscale installed and version
//! 2. Tailscale connection status
//! 3. Sidecar binary availability
//! 4. Config file validity
//! 5. Mesh connectivity (if daemon running)
//! 6. Key expiry (if connected)

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::output::{self, Indicator};

// ═══════════════════════════════════════════════════════════════════════════
// Check result type
// ═══════════════════════════════════════════════════════════════════════════

/// Result of a single diagnostic check.
struct CheckResult {
    indicator: Indicator,
    label: String,
    detail: String,
    fix: Option<String>,
}

impl CheckResult {
    fn pass(label: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            indicator: Indicator::Pass,
            label: label.into(),
            detail: detail.into(),
            fix: None,
        }
    }

    fn fail(label: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            indicator: Indicator::Fail,
            label: label.into(),
            detail: String::new(),
            fix: Some(fix.into()),
        }
    }

    fn warn(label: impl Into<String>, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            indicator: Indicator::Warn,
            label: label.into(),
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    fn skip(label: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            indicator: Indicator::Skip,
            label: label.into(),
            detail: format!("(skipped: {})", reason.into()),
            fix: None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Main run function
// ═══════════════════════════════════════════════════════════════════════════

/// Run diagnostics on the truffle installation.
pub async fn run(config: &TruffleConfig) -> Result<(), String> {
    println!();
    println!("  {}", output::bold("truffle doctor"));
    println!("  {}", output::dim(&"\u{2500}".repeat(39)));
    println!();

    let mut checks = Vec::new();
    let mut errors = 0u32;
    let mut warnings = 0u32;

    // 1. Check Tailscale installation
    let ts_installed = check_tailscale_installed();
    checks.push(ts_installed);

    // 2. Check Tailscale connection status
    let ts_connected = if checks.last().map(|c| c.indicator) == Some(Indicator::Pass) {
        check_tailscale_connected()
    } else {
        CheckResult::skip("Tailscale connected", "Tailscale not installed")
    };
    let tailscale_connected = ts_connected.indicator == Indicator::Pass;
    checks.push(ts_connected);

    // 3. Check sidecar binary
    checks.push(check_sidecar_binary(config));

    // 4. Check config file
    checks.push(check_config_file());

    // 5. Check mesh connectivity
    if tailscale_connected {
        checks.push(check_mesh_connectivity(config).await);
    } else {
        checks.push(CheckResult::skip(
            "Mesh reachable",
            "Tailscale not connected",
        ));
    }

    // 6. Check key expiry
    if tailscale_connected {
        checks.push(check_key_expiry());
    } else {
        checks.push(CheckResult::skip(
            "Key expiry",
            "Tailscale not connected",
        ));
    }

    // Display results
    for check in &checks {
        output::print_check(check.indicator, &check.label, &check.detail);
        if let Some(fix) = &check.fix {
            output::print_fix_suggestion(fix);
        }
        match check.indicator {
            Indicator::Fail => errors += 1,
            Indicator::Warn => warnings += 1,
            _ => {}
        }
    }

    // Summary
    println!();
    if errors == 0 && warnings == 0 {
        output::print_success("All checks passed. Your mesh is healthy.");
    } else {
        let mut parts = Vec::new();
        if errors > 0 {
            parts.push(format!(
                "{} {}",
                errors,
                if errors == 1 { "error" } else { "errors" }
            ));
        }
        if warnings > 0 {
            parts.push(format!(
                "{} {}",
                warnings,
                if warnings == 1 { "warning" } else { "warnings" }
            ));
        }
        let summary = parts.join(", ");
        if errors > 0 {
            println!(
                "  {}. Fix the {} first.",
                summary,
                if errors == 1 { "error" } else { "errors" }
            );
        } else {
            println!("  {}.", summary);
        }
    }
    println!();

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Individual checks
// ═══════════════════════════════════════════════════════════════════════════

/// Check if Tailscale CLI is installed and get its version.
fn check_tailscale_installed() -> CheckResult {
    match std::process::Command::new("tailscale")
        .arg("version")
        .output()
    {
        Ok(output) if output.status.success() => {
            let version_str = String::from_utf8_lossy(&output.stdout);
            let version = version_str
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            CheckResult::pass("Tailscale installed", &version)
        }
        Ok(_) => CheckResult::fail(
            "Tailscale installed but not working",
            "Reinstall Tailscale from https://tailscale.com/download",
        ),
        Err(_) => CheckResult::fail(
            "Tailscale not installed",
            "Install Tailscale from https://tailscale.com/download",
        ),
    }
}

/// Check if Tailscale is connected.
fn check_tailscale_connected() -> CheckResult {
    match std::process::Command::new("tailscale")
        .arg("status")
        .arg("--json")
        .output()
    {
        Ok(output) if output.status.success() => {
            let status_str = String::from_utf8_lossy(&output.stdout);
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&status_str) {
                let backend_state = json["BackendState"]
                    .as_str()
                    .unwrap_or("Unknown");

                if backend_state == "Running" {
                    let user = json["Self"]["UserID"].as_i64().unwrap_or(0);
                    let detail = if user > 0 {
                        format!("user ID {}", user)
                    } else {
                        "connected".to_string()
                    };
                    CheckResult::pass("Tailscale connected", &detail)
                } else {
                    CheckResult::fail(
                        &format!("Tailscale not connected (state: {})", backend_state),
                        "Run 'tailscale up' to connect",
                    )
                }
            } else {
                CheckResult::pass("Tailscale connected", "")
            }
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("not running") || stderr.contains("not connected") {
                CheckResult::fail(
                    "Tailscale not connected",
                    "Run 'tailscale up' to connect",
                )
            } else {
                CheckResult::fail(
                    "Tailscale status check failed",
                    "Run 'tailscale up' to connect",
                )
            }
        }
        Err(e) => CheckResult::fail(
            &format!("Can't check Tailscale status: {}", e),
            "Ensure Tailscale is installed and running",
        ),
    }
}

/// Check if the sidecar binary is available.
fn check_sidecar_binary(config: &TruffleConfig) -> CheckResult {
    // Check configured path first
    if !config.node.sidecar_path.is_empty() {
        let path = std::path::Path::new(&config.node.sidecar_path);
        if path.exists() {
            return CheckResult::pass(
                "Sidecar binary found",
                &config.node.sidecar_path,
            );
        } else {
            return CheckResult::fail(
                &format!("Sidecar binary not found at {}", config.node.sidecar_path),
                "Check the 'sidecar_path' in your config file",
            );
        }
    }

    // Check common locations
    let search_paths = [
        "/usr/local/bin/truffle-sidecar",
        "/usr/bin/truffle-sidecar",
    ];

    for path in &search_paths {
        if std::path::Path::new(path).exists() {
            return CheckResult::pass("Sidecar binary found", (*path).to_string());
        }
    }

    // Check if it's in PATH via `which`
    if let Ok(output) = std::process::Command::new("which")
        .arg("truffle-sidecar")
        .output()
    {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            return CheckResult::pass("Sidecar binary found", &path);
        }
    }

    // Also check adjacent to our own binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sidecar = dir.join("truffle-sidecar");
            if sidecar.exists() {
                return CheckResult::pass(
                    "Sidecar binary found",
                    &sidecar.display().to_string(),
                );
            }
        }
    }

    CheckResult::warn(
        "Sidecar binary not found",
        "",
        "Build the sidecar with 'cd sidecar && go build' or set 'sidecar_path' in config",
    )
}

/// Check if the config file is valid.
fn check_config_file() -> CheckResult {
    let config_path = TruffleConfig::default_path();

    if !config_path.exists() {
        return CheckResult::pass("Config file", "using defaults (no config file)");
    }

    match TruffleConfig::load(Some(&config_path)) {
        Ok(_) => CheckResult::pass(
            "Config file valid",
            &config_path.display().to_string(),
        ),
        Err(e) => CheckResult::fail(
            &format!("Config file invalid: {}", e),
            &format!("Fix the config at {}", config_path.display()),
        ),
    }
}

/// Check mesh connectivity by querying the daemon for peers.
async fn check_mesh_connectivity(_config: &TruffleConfig) -> CheckResult {
    let client = DaemonClient::new();

    if !client.is_daemon_running() {
        return CheckResult::warn(
            "Mesh reachable",
            "daemon not running",
            "Run 'truffle up' to start your node",
        );
    }

    match client
        .request(method::PEERS, serde_json::json!({}))
        .await
    {
        Ok(result) => {
            let peers = result["peers"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0);

            let online = result["peers"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter(|p| {
                            p["status"]
                                .as_str()
                                .map(|s| s.to_lowercase() == "online")
                                .unwrap_or(false)
                        })
                        .count()
                })
                .unwrap_or(0);

            if peers > 0 {
                CheckResult::pass(
                    "Mesh reachable",
                    &format!("{} {} responding", online, if online == 1 { "node" } else { "nodes" }),
                )
            } else {
                CheckResult::warn(
                    "Mesh reachable",
                    "no peers discovered yet",
                    "Start truffle on other devices to form a mesh",
                )
            }
        }
        Err(e) => CheckResult::fail(
            &format!("Mesh check failed: {}", e),
            "Run 'truffle up' to start your node",
        ),
    }
}

/// Check Tailscale key expiry.
fn check_key_expiry() -> CheckResult {
    match std::process::Command::new("tailscale")
        .arg("status")
        .arg("--json")
        .output()
    {
        Ok(output) if output.status.success() => {
            let status_str = String::from_utf8_lossy(&output.stdout);
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&status_str) {
                if let Some(expiry_str) = json["Self"]["KeyExpiry"].as_str() {
                    // Parse the ISO 8601 expiry date
                    // KeyExpiry is like "2026-06-18T12:00:00Z"
                    if let Some(days) = parse_days_until(expiry_str) {
                        if days < 0 {
                            return CheckResult::fail(
                                "Key expired",
                                "Run 'tailscale up --reset' to renew your key",
                            );
                        } else if days <= 7 {
                            return CheckResult::warn(
                                "Key expiring soon",
                                &format!("{} days remaining", days),
                                "Run 'tailscale up --reset' to renew your key",
                            );
                        } else {
                            return CheckResult::pass(
                                "Key expiry",
                                &format!("{} days remaining", days),
                            );
                        }
                    }
                }

                // If no KeyExpiry field, the key does not expire (auth key / admin)
                CheckResult::pass("Key expiry", "no expiry set")
            } else {
                CheckResult::pass("Key expiry", "could not parse status")
            }
        }
        _ => CheckResult::skip("Key expiry", "could not check"),
    }
}

/// Parse a rough "days until" from an ISO 8601 date string.
///
/// Returns `None` if parsing fails. This is a simplified parser that
/// handles the common `YYYY-MM-DDTHH:MM:SSZ` format from Tailscale.
fn parse_days_until(iso_date: &str) -> Option<i64> {
    // Extract YYYY-MM-DD from the beginning
    let date_part = iso_date.split('T').next()?;
    let parts: Vec<&str> = date_part.split('-').collect();
    if parts.len() != 3 {
        return None;
    }

    let year: i64 = parts[0].parse().ok()?;
    let month: i64 = parts[1].parse().ok()?;
    let day: i64 = parts[2].parse().ok()?;

    // Get current date using a simple approach
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    let now_days = now.as_secs() as i64 / 86400;

    // Rough conversion of expiry to days since epoch
    // (not precise but good enough for "days remaining" display)
    let expiry_days = rough_days_since_epoch(year, month, day);

    Some(expiry_days - now_days)
}

/// Very rough conversion from (year, month, day) to days since Unix epoch.
fn rough_days_since_epoch(year: i64, month: i64, day: i64) -> i64 {
    // Simplified -- good enough for "days remaining" estimates
    let y = if month <= 2 { year - 1 } else { year };
    let m = if month <= 2 { month + 9 } else { month - 3 };
    let c = y / 100;
    let ya = y - 100 * c;

    // Julian day number calculation (simplified)
    let jdn = (146097 * c) / 4 + (1461 * ya) / 4 + (153 * m + 2) / 5 + day + 1721119;

    // Unix epoch is Julian day 2440588
    jdn - 2440588
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_result_constructors() {
        let pass = CheckResult::pass("Test", "detail");
        assert_eq!(pass.indicator, Indicator::Pass);
        assert_eq!(pass.label, "Test");
        assert_eq!(pass.detail, "detail");
        assert!(pass.fix.is_none());

        let fail = CheckResult::fail("Test", "fix it");
        assert_eq!(fail.indicator, Indicator::Fail);
        assert!(fail.fix.is_some());

        let warn = CheckResult::warn("Test", "detail", "fix");
        assert_eq!(warn.indicator, Indicator::Warn);

        let skip = CheckResult::skip("Test", "reason");
        assert_eq!(skip.indicator, Indicator::Skip);
        assert!(skip.detail.contains("skipped"));
    }

    #[test]
    fn test_rough_days_since_epoch() {
        // Unix epoch: 1970-01-01 should be ~0
        let epoch = rough_days_since_epoch(1970, 1, 1);
        assert!(epoch.abs() <= 1, "epoch should be near 0, got {}", epoch);

        // 2000-01-01 should be ~10957 days
        let y2k = rough_days_since_epoch(2000, 1, 1);
        assert!(
            (y2k - 10957).abs() <= 1,
            "Y2K should be ~10957, got {}",
            y2k
        );
    }

    #[test]
    fn test_parse_days_until() {
        // A far-future date should return a positive number
        let days = parse_days_until("2099-12-31T00:00:00Z");
        assert!(days.is_some());
        assert!(days.unwrap() > 0);

        // A past date should return a negative number
        let days = parse_days_until("2000-01-01T00:00:00Z");
        assert!(days.is_some());
        assert!(days.unwrap() < 0);

        // Invalid format
        assert!(parse_days_until("not-a-date").is_none());
        assert!(parse_days_until("").is_none());
    }
}
