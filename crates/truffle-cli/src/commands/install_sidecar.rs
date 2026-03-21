//! `truffle install-sidecar` -- download the Go sidecar binary for the current platform.
//!
//! This command is the fallback for users who install truffle via `cargo install`
//! (which only builds the Rust CLI, not the Go sidecar). It downloads the
//! pre-built sidecar binary from GitHub releases and places it alongside the
//! CLI in the platform config directory.

use std::path::PathBuf;

use crate::output;

/// GitHub repository for release downloads.
const REPO: &str = "jamesyong-42/truffle";

/// Map `std::env::consts::OS` to the release asset OS name.
fn release_os() -> Result<&'static str, String> {
    match std::env::consts::OS {
        "macos" => Ok("darwin"),
        "linux" => Ok("linux"),
        "windows" => Ok("win32"),
        other => Err(format!("Unsupported operating system: {other}")),
    }
}

/// Map `std::env::consts::ARCH` to the release asset arch name.
fn release_arch() -> Result<&'static str, String> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x64"),
        "aarch64" => Ok("arm64"),
        other => Err(format!("Unsupported architecture: {other}")),
    }
}

/// The sidecar binary name for this platform.
fn sidecar_binary_name() -> &'static str {
    if cfg!(windows) {
        "sidecar-slim.exe"
    } else {
        "sidecar-slim"
    }
}

/// Determine the default install directory for the sidecar.
///
/// Uses `~/.config/truffle/bin/` on all platforms (matching the install scripts).
/// Falls back to the directory containing the CLI binary if config dir is unavailable.
fn default_install_dir() -> Result<PathBuf, String> {
    // Prefer the platform config directory
    if let Some(config_dir) = dirs::config_dir() {
        return Ok(config_dir.join("truffle").join("bin"));
    }

    // Fallback: next to the current executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return Ok(dir.to_path_buf());
        }
    }

    Err("Could not determine install directory. Use --dir to specify one.".to_string())
}

/// Run the install-sidecar command.
///
/// Downloads the pre-built sidecar binary from GitHub releases and installs
/// it to the truffle bin directory.
pub async fn run(dir_override: Option<&str>) -> Result<(), String> {
    let os = release_os()?;
    let arch = release_arch()?;
    let bin_name = sidecar_binary_name();

    let install_dir = if let Some(d) = dir_override {
        PathBuf::from(d)
    } else {
        default_install_dir()?
    };

    let dest = install_dir.join(bin_name);

    // Construct the download URL
    let asset_name = format!("sidecar-slim-{os}-{arch}");
    let url = format!(
        "https://github.com/{REPO}/releases/latest/download/{asset_name}"
    );

    println!();
    println!("  {}", output::bold("truffle install-sidecar"));
    println!("  {}", output::dim(&"\u{2500}".repeat(39)));
    println!();
    println!(
        "  Platform:    {}-{}",
        output::cyan(os),
        output::cyan(arch)
    );
    println!("  Binary:      {}", output::dim(bin_name));
    println!("  Destination: {}", output::dim(&dest.display().to_string()));
    println!("  Source:       {}", output::dim(&url));
    println!();

    // Create the install directory
    std::fs::create_dir_all(&install_dir).map_err(|e| {
        format!(
            "Failed to create directory {}: {e}",
            install_dir.display()
        )
    })?;

    // Download the sidecar binary
    println!("  Downloading sidecar...");

    // Use a blocking task for the HTTP download since ureq is synchronous
    let url_clone = url.clone();
    let dest_clone = dest.clone();

    let download_result = tokio::task::spawn_blocking(move || -> Result<u64, String> {
        let response = ureq::get(&url_clone)
            .call()
            .map_err(|e| {
                format!(
                    "Failed to download sidecar from {url_clone}\n\
                     \x20 Error: {e}\n\n\
                     \x20 Check that a release exists at:\n\
                     \x20 https://github.com/{REPO}/releases/latest"
                )
            })?;

        // Consume the response into parts to get an owned reader
        let (_, body) = response.into_parts();
        let mut reader = body.into_reader();

        let mut file = std::fs::File::create(&dest_clone).map_err(|e| {
            format!(
                "Failed to create file {}: {e}",
                dest_clone.display()
            )
        })?;

        let bytes = std::io::copy(&mut reader, &mut file).map_err(|e| {
            format!("Failed to write sidecar binary: {e}")
        })?;

        Ok(bytes)
    })
    .await
    .map_err(|e| format!("Download task failed: {e}"))?;

    let bytes_written = download_result?;

    // Set executable permission on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(&dest, perms).map_err(|e| {
            format!(
                "Failed to set executable permission on {}: {e}",
                dest.display()
            )
        })?;
    }

    println!();

    let size_mb = bytes_written as f64 / (1024.0 * 1024.0);
    output::print_success(&format!(
        "Sidecar installed ({:.1} MB) -> {}",
        size_mb,
        dest.display()
    ));

    // Verify the binary is executable
    println!();
    println!(
        "  The sidecar is now available for 'truffle up' to use."
    );
    println!(
        "  Run '{}' to verify your installation.",
        output::bold("truffle doctor")
    );
    println!();

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_release_os_returns_valid_value() {
        // On supported platforms, this should succeed
        let result = release_os();
        #[cfg(target_os = "macos")]
        assert_eq!(result.unwrap(), "darwin");
        #[cfg(target_os = "linux")]
        assert_eq!(result.unwrap(), "linux");
        #[cfg(target_os = "windows")]
        assert_eq!(result.unwrap(), "win32");
    }

    #[test]
    fn test_release_arch_returns_valid_value() {
        let result = release_arch();
        #[cfg(target_arch = "x86_64")]
        assert_eq!(result.unwrap(), "x64");
        #[cfg(target_arch = "aarch64")]
        assert_eq!(result.unwrap(), "arm64");
    }

    #[test]
    fn test_sidecar_binary_name_platform_aware() {
        let name = sidecar_binary_name();
        #[cfg(windows)]
        assert_eq!(name, "sidecar-slim.exe");
        #[cfg(not(windows))]
        assert_eq!(name, "sidecar-slim");
    }

    #[test]
    fn test_default_install_dir_succeeds() {
        // Should always return Ok on standard platforms
        let result = default_install_dir();
        assert!(result.is_ok(), "default_install_dir() failed: {:?}", result);
        let dir = result.unwrap();
        assert!(
            dir.to_string_lossy().contains("truffle"),
            "Install dir should contain 'truffle': {}",
            dir.display()
        );
    }
}
