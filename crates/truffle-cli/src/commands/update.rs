//! `truffle update` -- update truffle to the latest version.
//!
//! Checks GitHub releases for a newer version and downloads it.
//! Like `claude update` — seamless self-update.

use crate::output;

const REPO: &str = "jamesyong-42/truffle";

pub async fn run() -> Result<(), String> {
    let current_version = env!("CARGO_PKG_VERSION");

    println!();
    println!("  {}", output::bold("truffle update"));
    println!("  {}", output::dim(&"\u{2500}".repeat(39)));
    println!();
    println!("  Current version: {}", output::bold(current_version));
    println!("  Checking for updates...");

    // Check latest release from GitHub API
    let latest = check_latest_version().await?;
    let latest_tag = latest.trim_start_matches('v');

    if latest_tag == current_version {
        println!();
        println!("  {} Already on the latest version", output::green("\u{2713}"));
        println!();
        return Ok(());
    }

    println!("  Latest version:  {}", output::bold(&latest));
    println!();
    println!("  Updating...");

    // Determine platform
    let (os, arch) = detect_platform()?;
    let asset_name = if os == "windows" {
        format!("truffle-{os}-{arch}.zip")
    } else {
        format!("truffle-{os}-{arch}.tar.gz")
    };

    let url = format!(
        "https://github.com/{REPO}/releases/download/{latest}/{asset_name}"
    );

    // Download to temp dir
    let temp_dir = std::env::temp_dir().join("truffle-update");
    std::fs::create_dir_all(&temp_dir)
        .map_err(|e| format!("Failed to create temp dir: {e}"))?;
    let archive_path = temp_dir.join(&asset_name);

    download_file(&url, &archive_path).await?;

    // Find current install location
    let exe_path = std::env::current_exe()
        .map_err(|e| format!("Can't find current binary: {e}"))?;
    let install_dir = exe_path.parent()
        .ok_or("Can't determine install directory")?;

    // Extract new version
    if os == "windows" {
        extract_zip(&archive_path, install_dir)?;
    } else {
        extract_tar(&archive_path, install_dir)?;
    }

    // Cleanup
    let _ = std::fs::remove_dir_all(&temp_dir);

    println!();
    println!("  {} Updated to {}", output::green("\u{2713}"), output::bold(&latest));
    println!();

    Ok(())
}

async fn check_latest_version() -> Result<String, String> {
    // Use GitHub API to get latest release tag
    let output = tokio::process::Command::new("curl")
        .args([
            "-fsSL",
            "-H", "Accept: application/vnd.github.v3+json",
            &format!("https://api.github.com/repos/{REPO}/releases/latest"),
        ])
        .output()
        .await
        .map_err(|e| format!("Failed to check for updates: {e}"))?;

    if !output.status.success() {
        return Err("Failed to check GitHub releases".to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("Failed to parse release info: {e}"))?;

    json["tag_name"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| "No tag_name in release".to_string())
}

fn detect_platform() -> Result<(&'static str, &'static str), String> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        "windows" => "win32",
        other => return Err(format!("Unsupported OS: {other}")),
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => return Err(format!("Unsupported arch: {other}")),
    };
    Ok((os, arch))
}

async fn download_file(url: &str, dest: &std::path::Path) -> Result<(), String> {
    let output = tokio::process::Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(dest)
        .arg(url)
        .output()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Download failed: {stderr}"));
    }
    Ok(())
}

fn extract_tar(archive: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    let output = std::process::Command::new("tar")
        .args(["xzf"])
        .arg(archive)
        .arg("-C")
        .arg(dest)
        .output()
        .map_err(|e| format!("Extract failed: {e}"))?;

    if !output.status.success() {
        return Err("Failed to extract archive".to_string());
    }
    Ok(())
}

#[cfg(windows)]
fn extract_zip(archive: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile", "-Command",
            &format!(
                "Expand-Archive -Force -Path '{}' -DestinationPath '{}'",
                archive.display(),
                dest.display()
            ),
        ])
        .output()
        .map_err(|e| format!("Extract failed: {e}"))?;

    if !output.status.success() {
        return Err("Failed to extract zip".to_string());
    }
    Ok(())
}

#[cfg(not(windows))]
fn extract_zip(archive: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    let output = std::process::Command::new("unzip")
        .args(["-o"])
        .arg(archive)
        .arg("-d")
        .arg(dest)
        .output()
        .map_err(|e| format!("Extract failed: {e}"))?;

    if !output.status.success() {
        return Err("Failed to extract zip".to_string());
    }
    Ok(())
}
