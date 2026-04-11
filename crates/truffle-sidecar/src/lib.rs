//! # truffle-sidecar
//!
//! Provides the truffle mesh networking sidecar binary.
//!
//! This crate downloads the platform-specific Go sidecar binary at build time
//! from truffle's GitHub releases. At runtime, [`sidecar_path`] resolves the
//! binary location using a smart search order that works in both development
//! (`cargo run`) and production (installed binary) scenarios.
//!
//! ## Usage
//!
//! ```rust,no_run
//! use truffle_sidecar::sidecar_path;
//!
//! let path = sidecar_path();
//! println!("Sidecar at: {}", path.display());
//! ```
//!
//! ## Override
//!
//! Set `TRUFFLE_SIDECAR_PATH` environment variable at build time or runtime
//! to use a custom sidecar binary:
//!
//! ```bash
//! TRUFFLE_SIDECAR_PATH=/usr/local/bin/sidecar-slim cargo build
//! ```
//!
//! Set `TRUFFLE_SIDECAR_SKIP_DOWNLOAD=1` to skip the build-time download
//! (useful in CI where the sidecar is provided separately).

use std::path::PathBuf;

/// Known sidecar binary names (platform-aware).
const SIDECAR_NAMES: &[&str] = if cfg!(windows) {
    &[
        "sidecar-slim.exe",
        "truffle-sidecar.exe",
        "sidecar-slim",
        "truffle-sidecar",
    ]
} else {
    &["sidecar-slim", "truffle-sidecar"]
};

/// Returns the path to the truffle sidecar binary.
///
/// Resolution order:
/// 1. `TRUFFLE_SIDECAR_PATH` environment variable (runtime override)
/// 2. Same directory as the current executable (production install)
/// 3. Platform config directory (`~/.config/truffle/bin/` or equivalent)
/// 4. System paths (`/usr/local/bin/` on Unix)
/// 5. Build-time downloaded binary (development — `cargo run`)
/// 6. Falls back to `"sidecar-slim"` (assumes on PATH)
pub fn sidecar_path() -> PathBuf {
    // 1. Runtime override
    if let Ok(path) = std::env::var("TRUFFLE_SIDECAR_PATH") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return p;
        }
    }

    // 2. Same directory as the current executable
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));

    if let Some(ref dir) = exe_dir {
        for name in SIDECAR_NAMES {
            let candidate = dir.join(name);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    // 3. Platform config directory
    if let Some(config_dir) = config_bin_dir() {
        for name in SIDECAR_NAMES {
            let candidate = config_dir.join(name);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    // 4. System paths (Unix only)
    #[cfg(not(windows))]
    {
        for path in &[
            "/usr/local/bin/sidecar-slim",
            "/usr/local/bin/truffle-sidecar",
        ] {
            let p = PathBuf::from(path);
            if p.exists() {
                return p;
            }
        }
    }

    // 5. Build-time path (set by build.rs via cargo:rustc-env)
    let build_path = PathBuf::from(env!("TRUFFLE_SIDECAR_PATH"));
    if build_path.exists() {
        return build_path;
    }

    // 6. Fall back to PATH lookup at runtime
    PathBuf::from("sidecar-slim")
}

/// Returns the version of the sidecar this crate was built to download.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Platform config bin directory: `~/.config/truffle/bin/` (or equivalent).
fn config_bin_dir() -> Option<PathBuf> {
    // Use XDG_CONFIG_HOME on Linux, ~/Library/Application Support on macOS,
    // %APPDATA% on Windows — via the same logic as the `dirs` crate but
    // without adding a dependency.
    #[cfg(target_os = "macos")]
    {
        std::env::var("HOME")
            .ok()
            .map(|h| PathBuf::from(h).join("Library/Application Support/truffle/bin"))
    }

    #[cfg(target_os = "linux")]
    {
        std::env::var("XDG_CONFIG_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".config"))
            })
            .map(|d| d.join("truffle/bin"))
    }

    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA")
            .ok()
            .map(|d| PathBuf::from(d).join("truffle\\bin"))
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        None
    }
}
