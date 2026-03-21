//! Auto-discovery of the Go sidecar binary.
//!
//! `find_sidecar()` searches a prioritized list of locations so that
//! `truffle up` works zero-config -- no env vars, no config file needed.

use std::path::{Path, PathBuf};

use tracing::debug;

/// Binary names we look for (in order of preference).
const SIDECAR_NAMES: &[&str] = &["truffle-sidecar", "sidecar-slim"];

/// Locate the Go sidecar binary using a prioritized search.
///
/// Search order:
/// 1. Explicit config file override (`config_path`)
/// 2. `TRUFFLE_SIDECAR_PATH` env var
/// 3. Next to the CLI binary (same directory)
/// 4. `~/.config/truffle/bin/sidecar-slim`
/// 5. Workspace: walk up from the binary looking for `packages/sidecar-slim/bin/sidecar-slim`
/// 6. Legacy workspace: `packages/core/bin/sidecar-slim`
/// 7. In `PATH` via `which`
pub fn find_sidecar(config_path: Option<&str>) -> Result<PathBuf, String> {
    // 1. Explicit config override
    if let Some(p) = config_path {
        if !p.is_empty() {
            let path = PathBuf::from(p);
            if path.exists() {
                debug!(path = %path.display(), "sidecar: found via config");
                return Ok(path);
            }
            return Err(format!(
                "Sidecar path from config does not exist: {}",
                path.display()
            ));
        }
    }

    // 2. TRUFFLE_SIDECAR_PATH env var
    if let Ok(env_path) = std::env::var("TRUFFLE_SIDECAR_PATH") {
        if !env_path.is_empty() {
            let path = PathBuf::from(&env_path);
            if path.exists() {
                debug!(path = %path.display(), "sidecar: found via TRUFFLE_SIDECAR_PATH");
                return Ok(path);
            }
            return Err(format!(
                "TRUFFLE_SIDECAR_PATH does not exist: {}",
                env_path
            ));
        }
    }

    // 3. Next to the CLI binary (same directory)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for name in SIDECAR_NAMES {
                let candidate = dir.join(name);
                if candidate.exists() && is_executable(&candidate) {
                    debug!(path = %candidate.display(), "sidecar: found next to CLI binary");
                    return Ok(candidate);
                }
            }
        }
    }

    // 4. ~/.config/truffle/bin/sidecar-slim
    if let Some(config_dir) = dirs::config_dir() {
        let candidate = config_dir.join("truffle").join("bin").join("sidecar-slim");
        if candidate.exists() && is_executable(&candidate) {
            debug!(path = %candidate.display(), "sidecar: found in config bin dir");
            return Ok(candidate);
        }
    }

    // 5. Walk up from the binary looking for packages/sidecar-slim/bin/sidecar-slim
    //    (development mode -- running from workspace target/)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(start) = exe.parent() {
            if let Some(path) = walk_up_for_workspace(start) {
                debug!(path = %path.display(), "sidecar: found in workspace");
                return Ok(path);
            }
        }
    }

    // Also try walking up from the current working directory (common in dev)
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(path) = walk_up_for_workspace(&cwd) {
            debug!(path = %path.display(), "sidecar: found in workspace (from cwd)");
            return Ok(path);
        }
    }

    // 7. In PATH via `which`
    for name in SIDECAR_NAMES {
        if let Some(path) = which_binary(name) {
            debug!(path = %path.display(), "sidecar: found in PATH");
            return Ok(path);
        }
    }

    Err(
        "Could not find sidecar binary. Searched:\n\
         \x20 - next to CLI binary\n\
         \x20 - ~/.config/truffle/bin/\n\
         \x20 - workspace packages/sidecar-slim/bin/\n\
         \x20 - PATH (truffle-sidecar, sidecar-slim)\n\n\
         Fix: build the sidecar with 'cd packages/sidecar-slim && go build -o bin/sidecar-slim'\n\
         \x20    or set sidecar_path in ~/.config/truffle/config.toml"
            .to_string(),
    )
}

/// Walk up from `start` looking for `packages/sidecar-slim/bin/sidecar-slim`.
/// Also checks the legacy `packages/core/bin/sidecar-slim` location.
fn walk_up_for_workspace(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    // Walk up at most 10 levels to avoid infinite loops
    for _ in 0..10 {
        // Primary location
        let candidate = dir
            .join("packages")
            .join("sidecar-slim")
            .join("bin")
            .join("sidecar-slim");
        if candidate.exists() && is_executable(&candidate) {
            return Some(candidate);
        }

        // Legacy location
        let legacy = dir
            .join("packages")
            .join("core")
            .join("bin")
            .join("sidecar-slim");
        if legacy.exists() && is_executable(&legacy) {
            return Some(legacy);
        }

        if !dir.pop() {
            break;
        }
    }
    None
}

/// Check if a file is executable (Unix: has execute permission).
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.exists()
}

/// Find a binary in PATH using the `which` command.
fn which_binary(name: &str) -> Option<PathBuf> {
    std::process::Command::new("which")
        .arg(name)
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path_str.is_empty() {
                    Some(PathBuf::from(path_str))
                } else {
                    None
                }
            } else {
                None
            }
        })
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_sidecar_explicit_missing_errors() {
        let result = find_sidecar(Some("/nonexistent/path/sidecar-slim"));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("does not exist"));
    }

    #[test]
    fn test_find_sidecar_empty_config_path_continues_search() {
        // Empty string should not short-circuit
        let result = find_sidecar(Some(""));
        // It will either find the sidecar in the workspace or fail gracefully
        // -- we just verify it doesn't error with "config path does not exist"
        if let Err(ref e) = result {
            assert!(!e.contains("Sidecar path from config does not exist"));
        }
    }

    #[test]
    fn test_is_executable() {
        // The test binary itself should be executable
        let exe = std::env::current_exe().unwrap();
        assert!(is_executable(&exe));
    }

    #[test]
    fn test_walk_up_finds_workspace_sidecar() {
        // When running tests from the workspace, this should find the sidecar
        if let Ok(cwd) = std::env::current_dir() {
            // This test only passes when run from the truffle workspace
            if let Some(path) = walk_up_for_workspace(&cwd) {
                assert!(path.exists());
                assert!(path.to_string_lossy().contains("sidecar-slim"));
            }
        }
    }
}
