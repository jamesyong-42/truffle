//! Silent background auto-update.
//!
//! Spawned as a fire-and-forget task from `truffle up`. Checks GitHub for a
//! newer release (respecting a configurable rate limit), downloads and installs
//! it in the background. The new version takes effect on the next launch.
//!
//! All errors are silently caught -- a failed update check must never affect
//! the normal `truffle up` flow.

use crate::commands::update::{
    build_http_client, check_latest_version, download_asset_silent, extract_archive,
    find_install_dir, is_newer, platform_asset, replace_binaries, strip_version_prefix,
};
use crate::config::UpdateConfig;
use crate::output;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};

// ==========================================================================
// Update check cache
// ==========================================================================

/// Persisted state for the update check rate limit.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct UpdateCheckCache {
    /// When the last update check was performed.
    pub last_check: DateTime<Utc>,
    /// The latest version discovered (bare semver, e.g. "0.3.1").
    pub latest_version: String,
    /// Whether the discovered version is newer than the running binary.
    pub update_available: bool,
}

/// Path to the cache file: `~/.config/truffle/update-check.json`.
pub fn cache_path() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("truffle");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("update-check.json")
}

/// Load the cache from disk. Returns `None` if missing or corrupt.
pub fn load_cache(path: &Path) -> Option<UpdateCheckCache> {
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Save the cache to disk.
pub fn save_cache(path: &Path, cache: &UpdateCheckCache) -> Result<(), String> {
    let json = serde_json::to_string_pretty(cache)
        .map_err(|e| format!("Failed to serialize cache: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("Failed to write cache: {e}"))
}

/// Returns `true` if enough time has elapsed since `last_check` for us to
/// check again (based on `check_interval` seconds).
pub fn should_check_now(cache: Option<&UpdateCheckCache>, check_interval: u64) -> bool {
    match cache {
        None => true,
        Some(c) => {
            let elapsed = Utc::now().signed_duration_since(c.last_check).num_seconds();
            elapsed < 0 || elapsed >= check_interval as i64
        }
    }
}

// ==========================================================================
// Notification on launch
// ==========================================================================

/// Check the cache and print an update notification if one was downloaded in
/// a previous background run.
pub fn show_update_notification() {
    if let Some(cache) = load_cache(&cache_path()) {
        if cache.update_available {
            println!(
                "  {} {}",
                output::green("\u{2713}"),
                output::dim(&format!(
                    "Updated to v{} (restart to use new version)",
                    cache.latest_version
                )),
            );
        }
    }
}

// ==========================================================================
// Background updater
// ==========================================================================

/// Spawn a non-blocking, fire-and-forget background update check.
///
/// This should be called from `truffle up` **after** the daemon has started
/// successfully and only when running in background mode (not foreground).
pub fn spawn_background_update(update_config: UpdateConfig) {
    tokio::spawn(async move {
        if let Err(e) = try_background_update(&update_config).await {
            tracing::debug!("auto-update check failed (non-fatal): {e}");
        }
    });
}

async fn try_background_update(
    update_config: &UpdateConfig,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 0. Bail if auto-update is disabled.
    if !update_config.auto_update {
        tracing::debug!("auto-update: disabled in config");
        return Ok(());
    }

    let path = cache_path();
    let cached = load_cache(&path);

    // 1. Check rate limit.
    if !should_check_now(cached.as_ref(), update_config.check_interval) {
        tracing::debug!("auto-update: skipping (checked recently)");
        return Ok(());
    }

    // 2. Query GitHub for the latest version.
    let client = build_http_client()?;
    let release = check_latest_version(&client).await?;
    let latest_version = strip_version_prefix(&release.tag_name).to_string();

    // 3. Compare with current version.
    let current = env!("CARGO_PKG_VERSION");
    if !is_newer(current, &release.tag_name) {
        save_cache(
            &path,
            &UpdateCheckCache {
                last_check: Utc::now(),
                latest_version,
                update_available: false,
            },
        )?;
        tracing::debug!("auto-update: already up to date (v{current})");
        return Ok(());
    }

    // 4. Find the platform asset.
    let asset_name = platform_asset();
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .ok_or_else(|| format!("No release asset for this platform ({asset_name})"))?;

    tracing::info!("auto-update: downloading v{latest_version}");

    // 5. Download to a temp directory.
    let tmp_dir = tempfile::tempdir()?;
    let archive_path = tmp_dir.path().join(asset_name);
    download_asset_silent(&client, asset, &archive_path).await?;

    // 6. Extract.
    let extract_dir = tmp_dir.path().join("extracted");
    std::fs::create_dir_all(&extract_dir)?;
    extract_archive(&archive_path, &extract_dir)?;

    // 7. On Windows, you can't replace a running binary -- just mark the
    //    update as available so the notification is shown on next launch.
    #[cfg(target_os = "windows")]
    {
        save_cache(
            &path,
            &UpdateCheckCache {
                last_check: Utc::now(),
                latest_version: latest_version.clone(),
                update_available: true,
            },
        )?;
        tracing::info!("auto-update: v{latest_version} downloaded (restart to update)");
        return Ok(());
    }

    // 8. Replace binaries atomically.
    #[cfg(not(target_os = "windows"))]
    {
        let install_dir = find_install_dir()?;
        replace_binaries(&extract_dir, &install_dir)?;

        save_cache(
            &path,
            &UpdateCheckCache {
                last_check: Utc::now(),
                latest_version: latest_version.clone(),
                update_available: true,
            },
        )?;

        tracing::info!("auto-update: updated to v{latest_version} (takes effect on next launch)");
    }

    Ok(())
}

// ==========================================================================
// Tests
// ==========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn test_should_check_now_respects_interval() {
        // No cache -> should check.
        assert!(should_check_now(None, 86400));

        // Checked 1 second ago with 24h interval -> should NOT check.
        let recent = UpdateCheckCache {
            last_check: Utc::now() - Duration::seconds(1),
            latest_version: "0.3.0".to_string(),
            update_available: false,
        };
        assert!(!should_check_now(Some(&recent), 86400));

        // Checked 25 hours ago with 24h interval -> should check.
        let old = UpdateCheckCache {
            last_check: Utc::now() - Duration::hours(25),
            latest_version: "0.3.0".to_string(),
            update_available: false,
        };
        assert!(should_check_now(Some(&old), 86400));

        // Checked exactly at the interval boundary -> should check.
        let boundary = UpdateCheckCache {
            last_check: Utc::now() - Duration::seconds(86400),
            latest_version: "0.3.0".to_string(),
            update_available: false,
        };
        assert!(should_check_now(Some(&boundary), 86400));
    }

    #[test]
    fn test_save_and_load_check_result() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update-check.json");

        let cache = UpdateCheckCache {
            last_check: Utc::now(),
            latest_version: "0.4.0".to_string(),
            update_available: true,
        };

        save_cache(&path, &cache).unwrap();
        let loaded = load_cache(&path).unwrap();

        assert_eq!(loaded.latest_version, "0.4.0");
        assert!(loaded.update_available);
        // Timestamps should be within 1 second of each other (serialization
        // may truncate sub-second precision).
        let delta = (loaded.last_check - cache.last_check).num_seconds().abs();
        assert!(delta <= 1, "timestamps diverged by {delta}s");
    }

    #[test]
    fn test_auto_update_skips_when_current() {
        // is_newer already tested in update.rs, but verify the auto-update
        // short-circuit logic matches expectations.
        let current = env!("CARGO_PKG_VERSION");
        assert!(
            !is_newer(current, current),
            "Current version should not be newer than itself"
        );
    }

    #[test]
    fn test_cache_path_is_in_config_dir() {
        let path = cache_path();
        assert!(
            path.to_string_lossy().contains("truffle"),
            "Cache path should be under the truffle config directory"
        );
        assert!(
            path.file_name().unwrap().to_str().unwrap() == "update-check.json",
            "Cache file should be named update-check.json"
        );
    }

    #[test]
    fn test_load_cache_returns_none_for_missing_file() {
        let result = load_cache(Path::new("/nonexistent/path/update-check.json"));
        assert!(result.is_none());
    }

    #[test]
    fn test_load_cache_returns_none_for_corrupt_json() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update-check.json");
        std::fs::write(&path, "not valid json").unwrap();
        let result = load_cache(&path);
        assert!(result.is_none());
    }
}
