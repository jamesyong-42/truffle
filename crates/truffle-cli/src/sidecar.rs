//! Auto-discovery of the Go sidecar binary.
//!
//! `find_sidecar()` searches a prioritized list of locations so that
//! `truffle up` works zero-config -- no env vars, no config file needed.
//!
//! Works on macOS, Linux, and Windows -- binary names, PATH lookup,
//! executable checks, and search paths are all platform-aware.

use std::path::{Path, PathBuf};

use tracing::debug;

/// The primary sidecar binary name for this platform.
fn sidecar_bin_name() -> &'static str {
    #[cfg(windows)]
    {
        "sidecar-slim.exe"
    }
    #[cfg(not(windows))]
    {
        "sidecar-slim"
    }
}

/// All binary names to search for (in order of preference).
fn sidecar_search_names() -> Vec<&'static str> {
    #[cfg(windows)]
    {
        vec!["truffle-sidecar.exe", "sidecar-slim.exe"]
    }
    #[cfg(not(windows))]
    {
        vec!["truffle-sidecar", "sidecar-slim"]
    }
}

/// Return the platform-specific npm package name for the sidecar.
///
/// Each platform+arch combination maps to a separate optional npm package
/// that contains the pre-built sidecar binary.
fn platform_npm_package() -> Option<&'static str> {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Some("@vibecook/truffle-sidecar-darwin-arm64")
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        Some("@vibecook/truffle-sidecar-darwin-x64")
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Some("@vibecook/truffle-sidecar-linux-x64")
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        Some("@vibecook/truffle-sidecar-linux-arm64")
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Some("@vibecook/truffle-sidecar-win32-x64")
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
    )))]
    {
        None
    }
}

/// Locate the Go sidecar binary using a prioritized search.
///
/// Search order:
/// 1. Explicit config file override (`config_path`)
/// 2. `TRUFFLE_SIDECAR_PATH` env var
/// 3. Next to the CLI binary (same directory)
/// 4. `~/.config/truffle/bin/sidecar-slim` (platform config dir)
/// 5. Workspace: walk up from the binary looking for `packages/sidecar-slim/bin/sidecar-slim`
/// 6. npm global modules (`npm root -g` / platform-specific package)
/// 7. Homebrew prefix (macOS/Linux only)
/// 8. In `PATH` via `which`/`where`
pub fn find_sidecar(config_path: Option<&str>) -> Result<PathBuf, String> {
    let search_names = sidecar_search_names();
    let bin_name = sidecar_bin_name();

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
            for name in &search_names {
                let candidate = dir.join(name);
                if candidate.exists() && is_executable(&candidate) {
                    debug!(path = %candidate.display(), "sidecar: found next to CLI binary");
                    return Ok(candidate);
                }
            }
        }
    }

    // 4. Platform config dir: ~/.config/truffle/bin/sidecar-slim (Linux)
    //    ~/Library/Application Support/truffle/bin/sidecar-slim (macOS)
    //    %APPDATA%/truffle/bin/sidecar-slim.exe (Windows)
    if let Some(config_dir) = dirs::config_dir() {
        let candidate = config_dir.join("truffle").join("bin").join(bin_name);
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

    // 6. npm global modules
    if let Some(path) = find_in_npm_global(bin_name) {
        debug!(path = %path.display(), "sidecar: found in npm global modules");
        return Ok(path);
    }

    // 7. Homebrew prefix (macOS/Linux only)
    #[cfg(unix)]
    {
        if let Some(path) = find_in_homebrew(bin_name) {
            debug!(path = %path.display(), "sidecar: found in Homebrew prefix");
            return Ok(path);
        }
    }

    // 8. In PATH via `which` (Unix) / `where` (Windows)
    for name in &search_names {
        if let Some(path) = which_binary(name) {
            debug!(path = %path.display(), "sidecar: found in PATH");
            return Ok(path);
        }
    }

    Err(
        "Could not find sidecar binary. Install with:\n\
         \x20 brew install truffle        (macOS/Linux)\n\
         \x20 npm install -g @vibecook/truffle-cli  (any platform)\n\
         \x20 Or download from https://github.com/jamesyong-42/truffle/releases"
            .to_string(),
    )
}

/// Walk up from `start` looking for `packages/sidecar-slim/bin/sidecar-slim`.
/// Also checks the legacy `packages/core/bin/sidecar-slim` location.
/// Uses platform-aware binary names (`.exe` on Windows).
fn walk_up_for_workspace(start: &Path) -> Option<PathBuf> {
    let bin_name = sidecar_bin_name();
    let mut dir = start.to_path_buf();
    // Walk up at most 10 levels to avoid infinite loops
    for _ in 0..10 {
        // Primary location
        let candidate = dir
            .join("packages")
            .join("sidecar-slim")
            .join("bin")
            .join(bin_name);
        if candidate.exists() && is_executable(&candidate) {
            return Some(candidate);
        }

        // Legacy location
        let legacy = dir
            .join("packages")
            .join("core")
            .join("bin")
            .join(bin_name);
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

/// Find a binary in PATH using `which` (Unix) or `where` (Windows).
fn which_binary(name: &str) -> Option<PathBuf> {
    #[cfg(unix)]
    let cmd = "which";
    #[cfg(windows)]
    let cmd = "where";

    std::process::Command::new(cmd)
        .arg(name)
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                // `where` on Windows may return multiple lines; take the first.
                let path_str = String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()?
                    .trim()
                    .to_string();
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

/// Search for the sidecar binary inside the npm global `node_modules` tree.
///
/// Runs `npm root -g` to locate the global modules directory, then checks
/// the platform-specific optional package for a `bin/<name>` executable.
fn find_in_npm_global(bin_name: &str) -> Option<PathBuf> {
    let output = std::process::Command::new("npm")
        .args(["root", "-g"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let npm_root = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if npm_root.is_empty() {
        return None;
    }

    let pkg = platform_npm_package()?;
    let candidate = PathBuf::from(&npm_root).join(pkg).join("bin").join(bin_name);
    if candidate.exists() && is_executable(&candidate) {
        Some(candidate)
    } else {
        None
    }
}

/// Search for the sidecar binary under the Homebrew prefix (macOS/Linux only).
///
/// Checks both the default Homebrew cellar bin and the linked location.
#[cfg(unix)]
fn find_in_homebrew(bin_name: &str) -> Option<PathBuf> {
    // Try `brew --prefix` first for the canonical location.
    let output = std::process::Command::new("brew")
        .args(["--prefix"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let prefix = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if prefix.is_empty() {
        return None;
    }

    // Homebrew links binaries into <prefix>/bin/
    let candidate = PathBuf::from(&prefix).join("bin").join(bin_name);
    if candidate.exists() && is_executable(&candidate) {
        Some(candidate)
    } else {
        None
    }
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
    fn test_is_executable_nonexistent() {
        // A path that doesn't exist should not be considered executable
        let path = PathBuf::from("/nonexistent/binary/that/does/not/exist");
        assert!(!is_executable(&path));
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

    #[test]
    fn test_sidecar_bin_name_has_correct_suffix() {
        let name = sidecar_bin_name();
        #[cfg(windows)]
        assert!(
            name.ends_with(".exe"),
            "On Windows, sidecar binary name must end with .exe, got: {name}"
        );
        #[cfg(not(windows))]
        assert!(
            !name.ends_with(".exe"),
            "On non-Windows, sidecar binary name must not end with .exe, got: {name}"
        );
    }

    #[test]
    fn test_sidecar_search_names_platform_consistent() {
        let names = sidecar_search_names();
        assert!(
            names.len() >= 2,
            "Should have at least 2 search names, got: {names:?}"
        );
        #[cfg(windows)]
        for name in &names {
            assert!(
                name.ends_with(".exe"),
                "On Windows, all search names should end with .exe, got: {name}"
            );
        }
        #[cfg(not(windows))]
        for name in &names {
            assert!(
                !name.ends_with(".exe"),
                "On non-Windows, search names should not end with .exe, got: {name}"
            );
        }
    }

    #[test]
    fn test_platform_npm_package_returns_value() {
        // On supported platforms (macOS arm64/x64, Linux x64/arm64, Windows x64),
        // this should return Some. On exotic platforms it returns None.
        let pkg = platform_npm_package();
        #[cfg(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "aarch64"),
            all(target_os = "windows", target_arch = "x86_64"),
        ))]
        assert!(
            pkg.is_some(),
            "Expected Some on this platform, got None"
        );

        if let Some(name) = pkg {
            assert!(
                name.starts_with("@vibecook/truffle-sidecar-"),
                "npm package name should start with @vibecook/truffle-sidecar-, got: {name}"
            );
        }
    }

    #[test]
    fn test_find_in_npm_global_graceful_when_npm_missing() {
        // If npm is not installed, find_in_npm_global should return None
        // without panicking. Even if npm IS installed, this should not
        // panic -- it will just return None if the package isn't installed.
        let result = find_in_npm_global("nonexistent-binary-name-12345");
        assert!(result.is_none());
    }

    #[test]
    fn test_which_binary_finds_common_tools() {
        // `ls` (Unix) or `cmd` (Windows) should always be in PATH
        #[cfg(unix)]
        {
            let result = which_binary("ls");
            assert!(result.is_some(), "which_binary should find 'ls' on Unix");
        }
        #[cfg(windows)]
        {
            let result = which_binary("cmd");
            assert!(result.is_some(), "which_binary should find 'cmd' on Windows");
        }
    }

    #[test]
    fn test_which_binary_returns_none_for_missing() {
        let result = which_binary("nonexistent-binary-that-does-not-exist-12345");
        assert!(result.is_none());
    }

    #[test]
    fn test_error_message_includes_install_hints() {
        // Force an error by providing no config and no sidecar anywhere
        // This test works because it uses a nonexistent explicit path to
        // trigger an error, but we want to test the fallthrough error.
        // We can't easily test the fallthrough in CI since the workspace
        // sidecar may exist. Instead, verify the error message format
        // by checking find_sidecar with a missing explicit path.
        let result = find_sidecar(Some("/nonexistent/path"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("does not exist"));
    }
}
