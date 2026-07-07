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

use std::path::{Path, PathBuf};

// Pure integrity helpers shared with `build.rs` (which pulls the same file in
// via `include!`). Compiled only under test so `sha2`/`serde_json` stay out of
// the published library's regular dependencies — they are dev-dependencies.
#[cfg(test)]
mod integrity;

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
/// 3. Platform config directory — `~/.config/truffle/bin/` (installer location)
///    or equivalent; on macOS the legacy
///    `~/Library/Application Support/truffle/bin` is also checked
/// 4. Build-time downloaded binary (development — `cargo run`)
/// 5. System paths (`/usr/local/bin/` on Unix) — logs a warning when used
/// 6. Bare `"sidecar-slim"` PATH fallback — last resort, logs a warning
pub fn sidecar_path() -> PathBuf {
    // 1. Runtime override
    if let Ok(path) = std::env::var("TRUFFLE_SIDECAR_PATH") {
        let p = PathBuf::from(&path);
        if p.exists() {
            return p;
        }
    }

    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));

    resolve(
        exe_dir.as_deref(),
        &config_bin_dirs(),
        Path::new(env!("TRUFFLE_SIDECAR_PATH")),
        &system_bin_dirs(),
    )
}

/// Core resolution logic, separated from process/env state so the
/// preference order can be unit-tested.
fn resolve(
    exe_dir: Option<&Path>,
    config_dirs: &[PathBuf],
    build_path: &Path,
    system_dirs: &[PathBuf],
) -> PathBuf {
    // 2. Same directory as the current executable
    if let Some(dir) = exe_dir {
        for name in SIDECAR_NAMES {
            let candidate = dir.join(name);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    // 3. Platform config directories (installer-managed)
    for dir in config_dirs {
        for name in SIDECAR_NAMES {
            let candidate = dir.join(name);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    // 4. Build-time downloaded binary (set by build.rs via cargo:rustc-env).
    // Relative placeholders (e.g. skip-download builds) are ignored so we
    // never resolve against the current working directory.
    if build_path.is_absolute() && build_path.exists() {
        return build_path.to_path_buf();
    }

    // 5. System paths — not managed by truffle, so warn when used.
    for dir in system_dirs {
        for name in SIDECAR_NAMES {
            let candidate = dir.join(name);
            if candidate.exists() {
                eprintln!(
                    "truffle-sidecar: warning: using system sidecar at {} (not managed by truffle)",
                    candidate.display()
                );
                return candidate;
            }
        }
    }

    // 6. Last resort: bare PATH lookup at runtime.
    eprintln!(
        "truffle-sidecar: warning: sidecar not found in trusted locations; falling back to \"sidecar-slim\" on PATH"
    );
    PathBuf::from("sidecar-slim")
}

/// Returns the version of the sidecar this crate was built to download.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Platform config bin directories, most-preferred first.
/// The installer (scripts/install.sh) uses `~/.config/truffle/bin` on all
/// Unix platforms, including macOS; the legacy macOS
/// `~/Library/Application Support/truffle/bin` is kept for back-compat.
fn config_bin_dirs() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var("HOME")
            .ok()
            .map(|h| {
                let home = PathBuf::from(h);
                vec![
                    home.join(".config/truffle/bin"),
                    home.join("Library/Application Support/truffle/bin"),
                ]
            })
            .unwrap_or_default()
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
            .map(|d| vec![d.join("truffle/bin")])
            .unwrap_or_default()
    }

    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA")
            .ok()
            .map(|d| vec![PathBuf::from(d).join("truffle\\bin")])
            .unwrap_or_default()
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        Vec::new()
    }
}

/// System install locations checked as an untrusted fallback (Unix only).
fn system_bin_dirs() -> Vec<PathBuf> {
    #[cfg(not(windows))]
    {
        vec![PathBuf::from("/usr/local/bin")]
    }
    #[cfg(windows)]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Create a fake sidecar binary in `dir` and return its path.
    fn touch_sidecar(dir: &Path) -> PathBuf {
        let p = dir.join(SIDECAR_NAMES[0]);
        fs::write(&p, b"").unwrap();
        p
    }

    #[test]
    fn build_path_preferred_over_system_dir() {
        // Regression test for the resolver-order finding: the build-time
        // downloaded binary must win over /usr/local/bin-style system dirs.
        let build = tempfile::tempdir().unwrap();
        let system = tempfile::tempdir().unwrap();
        let build_bin = touch_sidecar(build.path());
        touch_sidecar(system.path());
        let got = resolve(None, &[], &build_bin, &[system.path().to_path_buf()]);
        assert_eq!(got, build_bin);
    }

    #[test]
    fn exe_dir_preferred_over_all() {
        let exe = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let build = tempfile::tempdir().unwrap();
        let system = tempfile::tempdir().unwrap();
        let exe_bin = touch_sidecar(exe.path());
        touch_sidecar(config.path());
        let build_bin = touch_sidecar(build.path());
        touch_sidecar(system.path());
        let got = resolve(
            Some(exe.path()),
            &[config.path().to_path_buf()],
            &build_bin,
            &[system.path().to_path_buf()],
        );
        assert_eq!(got, exe_bin);
    }

    #[test]
    fn config_dir_preferred_over_build_path() {
        let config = tempfile::tempdir().unwrap();
        let build = tempfile::tempdir().unwrap();
        let config_bin = touch_sidecar(config.path());
        let build_bin = touch_sidecar(build.path());
        let got = resolve(None, &[config.path().to_path_buf()], &build_bin, &[]);
        assert_eq!(got, config_bin);
    }

    #[test]
    fn system_dir_used_when_no_trusted_candidate() {
        let system = tempfile::tempdir().unwrap();
        let system_bin = touch_sidecar(system.path());
        let missing = system.path().join("missing-build-binary");
        let got = resolve(None, &[], &missing, &[system.path().to_path_buf()]);
        assert_eq!(got, system_bin);
    }

    #[test]
    fn falls_back_to_bare_path_name() {
        let empty = tempfile::tempdir().unwrap();
        let missing = empty.path().join("missing-build-binary");
        let got = resolve(None, &[], &missing, &[]);
        assert_eq!(got, PathBuf::from("sidecar-slim"));
    }

    #[test]
    fn relative_build_path_is_ignored() {
        // Skip-download builds set a relative placeholder; it must never be
        // resolved against the current working directory.
        let got = resolve(None, &[], Path::new("truffle-sidecar"), &[]);
        assert_eq!(got, PathBuf::from("sidecar-slim"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_config_dirs_prefer_installer_location() {
        let dirs = config_bin_dirs();
        assert_eq!(dirs.len(), 2);
        assert!(dirs[0].ends_with(".config/truffle/bin"));
        assert!(dirs[1].ends_with("Library/Application Support/truffle/bin"));
    }
}
