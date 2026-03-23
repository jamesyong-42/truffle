//! `truffle cp` -- copy files between nodes.
//!
//! Supports scp-style syntax:
//!   truffle cp file.txt node:/path/   -- upload
//!   truffle cp node:/path/file.txt ./ -- download
//!   truffle cp file.txt node:         -- upload, keep filename

use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::{method, DaemonNotification};
use crate::output;

/// Default remote directory when no path is specified (e.g. `truffle cp file.txt server:`).
const DEFAULT_REMOTE_DIR: &str = "~/Downloads/truffle/";

// ═══════════════════════════════════════════════════════════════════════════
// Remote path parsing
// ═══════════════════════════════════════════════════════════════════════════

/// Represents a file location -- either local or on a remote node.
#[derive(Debug, Clone, PartialEq)]
pub enum FileLocation {
    /// A local file path.
    Local(PathBuf),
    /// A remote file on a node: `(node_name, remote_path)`.
    Remote { node: String, path: String },
}

impl FileLocation {
    /// Returns `true` if this is a remote location.
    pub fn is_remote(&self) -> bool {
        matches!(self, FileLocation::Remote { .. })
    }
}

/// Parse a source/dest string into a `FileLocation`.
///
/// Remote paths use `node:path` syntax (like scp):
///   - `server:/tmp/file.txt` -> Remote { node: "server", path: "/tmp/file.txt" }
///   - `server:` -> Remote { node: "server", path: "" }
///   - `./file.txt` -> Local("./file.txt")
///   - `/tmp/file.txt` -> Local("/tmp/file.txt")
///   - `file.txt` -> Local("file.txt")
///
/// A bare colon after a node name indicates "use the filename from the other side".
pub fn parse_location(s: &str) -> FileLocation {
    // If it starts with "/" or "./" or "../", it's definitely local
    if s.starts_with('/') || s.starts_with("./") || s.starts_with("../") {
        return FileLocation::Local(PathBuf::from(s));
    }

    // Look for node:path pattern (but not C:\windows or similar)
    if let Some((node, path)) = s.split_once(':') {
        // Reject single-char "nodes" (likely Windows drive letters)
        if node.len() > 1 && !node.contains('/') && !node.contains('\\') {
            return FileLocation::Remote {
                node: node.to_string(),
                path: path.to_string(),
            };
        }
    }

    FileLocation::Local(PathBuf::from(s))
}

/// Determine the transfer direction from source and dest locations.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransferDirection {
    Upload,
    Download,
}

// ═══════════════════════════════════════════════════════════════════════════
// Main run function
// ═══════════════════════════════════════════════════════════════════════════

/// Copy a file between nodes.
pub async fn run(
    config: &TruffleConfig,
    source: &str,
    dest: &str,
    verify: bool,
) -> Result<(), String> {
    let src = parse_location(source);
    let dst = parse_location(dest);

    // Validate: exactly one side must be remote
    match (&src, &dst) {
        (FileLocation::Local(_), FileLocation::Local(_)) => {
            output::print_error(
                "Both source and destination are local",
                "Use 'cp' for local copies. 'truffle cp' transfers files between nodes.",
                "truffle cp file.txt server:/tmp/\ntruffle cp server:/tmp/file.txt ./",
            );
            return Err(output::format_error(
                "Both source and destination are local",
                "",
                "",
            ));
        }
        (FileLocation::Remote { .. }, FileLocation::Remote { .. }) => {
            output::print_error(
                "Both source and destination are remote",
                "Direct node-to-node copies are not yet supported.",
                "truffle cp server:/file.txt ./    (download first)\ntruffle cp ./file.txt other:/     (then upload)",
            );
            return Err(output::format_error(
                "Both source and destination are remote",
                "",
                "",
            ));
        }
        _ => {}
    }

    let direction = if src.is_remote() {
        TransferDirection::Download
    } else {
        TransferDirection::Upload
    };

    // Connect to daemon
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| {
            output::format_error(
                "Node is not running",
                &e.to_string(),
                "truffle up    start your node first",
            )
        })?;

    match direction {
        TransferDirection::Upload => {
            let local_path = match &src {
                FileLocation::Local(p) => p,
                _ => unreachable!(),
            };
            let (node, remote_path) = match &dst {
                FileLocation::Remote { node, path } => (node, path),
                _ => unreachable!(),
            };

            do_upload(&client, local_path, node, remote_path, verify).await
        }
        TransferDirection::Download => {
            let (node, remote_path) = match &src {
                FileLocation::Remote { node, path } => (node, path),
                _ => unreachable!(),
            };
            let local_path = match &dst {
                FileLocation::Local(p) => p,
                _ => unreachable!(),
            };

            do_download(&client, node, remote_path, local_path, verify).await
        }
    }
}

/// Upload a local file to a remote node via the daemon.
///
/// The CLI sends only metadata (node, paths) to the daemon via JSON-RPC.
/// The daemon reads the file directly from disk (same machine) and handles
/// the transfer over the mesh network.
async fn do_upload(
    client: &DaemonClient,
    local_path: &Path,
    node: &str,
    remote_path: &str,
    _verify: bool,
) -> Result<(), String> {
    // Verify local file exists
    if !local_path.exists() {
        output::print_error(
            &format!("File not found: {}", local_path.display()),
            "The source file does not exist.",
            "",
        );
        return Err(output::format_error(
            &format!("File not found: {}", local_path.display()),
            "",
            "",
        ));
    }

    let metadata = std::fs::metadata(local_path)
        .map_err(|e| format!("Can't read file metadata: {e}"))?;

    if metadata.is_dir() {
        output::print_error(
            &format!("{} is a directory", local_path.display()),
            "Directory copies are not yet supported.",
            "truffle cp file.txt node:/path/    (copy individual files)",
        );
        return Err(output::format_error(
            &format!("{} is a directory", local_path.display()),
            "",
            "",
        ));
    }

    let file_name = local_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");

    // Determine the final remote path
    let dest_path = if remote_path.is_empty() {
        // `truffle cp file.txt server:` -- no remote path specified.
        // Use a default downloads directory on the remote side.
        format!("{}{}", DEFAULT_REMOTE_DIR, file_name)
    } else if remote_path.ends_with('/') {
        format!("{}{}", remote_path, file_name)
    } else {
        remote_path.to_string()
    };

    // Resolve local_path to absolute (daemon reads from disk directly)
    let abs_local_path = if local_path.is_absolute() {
        local_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("Can't determine working directory: {e}"))?
            .join(local_path)
    };

    // Show transfer info
    println!();
    println!(
        "  {} {} {}:{}",
        local_path.display(),
        output::dim("\u{2192}"),
        node,
        dest_path,
    );

    let started = Instant::now();

    // Send via daemon's push_file command -- no file data, just metadata.
    // The daemon reads the file directly from disk (it runs on the same machine).
    // Use request_with_notifications to receive streaming progress updates.
    let result = client
        .request_with_notifications(
            method::PUSH_FILE,
            serde_json::json!({
                "node": node,
                "local_path": abs_local_path.to_string_lossy(),
                "remote_path": dest_path,
            }),
            |notif| {
                render_progress(notif);
            },
        )
        .await;

    let elapsed = started.elapsed().as_secs_f64();

    match result {
        Ok(resp) => {
            let transferred = resp["bytes_transferred"].as_u64().unwrap_or(0);
            output::print_progress_complete(transferred, elapsed);

            // The daemon performs SHA-256 verification on both sides during transfer.
            // Report the result if available.
            let sha256 = resp["sha256"].as_str().unwrap_or("");
            if !sha256.is_empty() {
                output::print_success(&format!("SHA-256: {}", &sha256[..16.min(sha256.len())]));
            }

            Ok(())
        }
        Err(e) => {
            println!();
            output::print_error(
                &format!("Transfer failed to {}", node),
                &e.to_string(),
                &format!("truffle ping {}    check connectivity\ntruffle doctor      diagnose issues", node),
            );
            Err(e.to_string())
        }
    }
}

/// Download a file from a remote node via the daemon.
///
/// The CLI sends only metadata (node, paths) to the daemon via JSON-RPC.
/// The daemon sends a PULL_REQUEST to the remote node, which responds with
/// an OFFER. The daemon auto-accepts and receives the file over the mesh.
async fn do_download(
    client: &DaemonClient,
    node: &str,
    remote_path: &str,
    local_path: &Path,
    _verify: bool,
) -> Result<(), String> {
    // Validate remote path: must specify a file, not just a directory
    if remote_path.is_empty() {
        output::print_error(
            "No remote file specified",
            "You must specify a file path on the remote node.",
            &format!("truffle cp {}:/path/to/file.txt ./", node),
        );
        return Err(output::format_error("No remote file specified", "", ""));
    }

    if remote_path.ends_with('/') {
        output::print_error(
            &format!("Remote path is a directory: {}", remote_path),
            "You must specify a file, not a directory.",
            &format!("truffle cp {}:{}file.txt ./", node, remote_path),
        );
        return Err(output::format_error(
            &format!("Remote path is a directory: {}", remote_path),
            "",
            "",
        ));
    }

    let file_name = Path::new(remote_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");

    // Determine local destination
    let dest_path = if local_path.is_dir() {
        local_path.join(file_name)
    } else if local_path.to_string_lossy() == "." || local_path.to_string_lossy() == "./" {
        PathBuf::from(file_name)
    } else {
        local_path.to_path_buf()
    };

    // Resolve dest_path to absolute (daemon writes to disk directly)
    let abs_dest_path = if dest_path.is_absolute() {
        dest_path.clone()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("Can't determine working directory: {e}"))?
            .join(&dest_path)
    };

    // Show transfer info
    println!();
    println!(
        "  {}:{} {} {}",
        node,
        remote_path,
        output::dim("\u{2192}"),
        dest_path.display(),
    );

    let started = Instant::now();

    // Request file from daemon's get_file command -- no file data in response,
    // the daemon handles receiving the file via the mesh and writing to disk.
    let result = client
        .request_with_notifications(
            method::GET_FILE,
            serde_json::json!({
                "node": node,
                "remote_path": remote_path,
                "local_path": abs_dest_path.to_string_lossy(),
            }),
            |notif| {
                render_progress(notif);
            },
        )
        .await;

    let elapsed = started.elapsed().as_secs_f64();

    match result {
        Ok(resp) => {
            let transferred = resp["bytes_transferred"].as_u64().unwrap_or(0);
            output::print_progress_complete(transferred, elapsed);

            // The daemon performs SHA-256 verification on both sides during transfer.
            // Report the result if available.
            let sha256 = resp["sha256"].as_str().unwrap_or("");
            if !sha256.is_empty() {
                output::print_success(&format!("SHA-256: {}", &sha256[..16.min(sha256.len())]));
            }

            Ok(())
        }
        Err(e) => {
            println!();
            output::print_error(
                &format!("Download failed from {}", node),
                &e.to_string(),
                &format!("truffle ping {}    check connectivity\ntruffle doctor      diagnose issues", node),
            );
            Err(e.to_string())
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Progress rendering
// ═══════════════════════════════════════════════════════════════════════════

/// Render a progress notification as a terminal progress bar.
///
/// Called by `request_with_notifications()` for each `cp.progress`
/// notification received from the daemon during a file transfer.
fn render_progress(notification: &DaemonNotification) {
    let total = notification.params["total_bytes"].as_i64().unwrap_or(0) as u64;
    let current = notification.params["bytes_transferred"].as_i64().unwrap_or(0) as u64;
    let speed = notification.params["bytes_per_second"].as_f64().unwrap_or(0.0);
    output::print_progress(current, total, speed);
}


// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_location_local_paths() {
        assert_eq!(
            parse_location("file.txt"),
            FileLocation::Local(PathBuf::from("file.txt"))
        );
        assert_eq!(
            parse_location("./file.txt"),
            FileLocation::Local(PathBuf::from("./file.txt"))
        );
        assert_eq!(
            parse_location("/tmp/file.txt"),
            FileLocation::Local(PathBuf::from("/tmp/file.txt"))
        );
        assert_eq!(
            parse_location("../dir/file.txt"),
            FileLocation::Local(PathBuf::from("../dir/file.txt"))
        );
    }

    #[test]
    fn test_parse_location_remote_paths() {
        assert_eq!(
            parse_location("server:/tmp/file.txt"),
            FileLocation::Remote {
                node: "server".to_string(),
                path: "/tmp/file.txt".to_string(),
            }
        );
        assert_eq!(
            parse_location("laptop:"),
            FileLocation::Remote {
                node: "laptop".to_string(),
                path: "".to_string(),
            }
        );
        assert_eq!(
            parse_location("my-server:/var/www/app/"),
            FileLocation::Remote {
                node: "my-server".to_string(),
                path: "/var/www/app/".to_string(),
            }
        );
    }

    #[test]
    fn test_parse_location_bare_filename() {
        // A bare filename without a colon is local
        assert_eq!(
            parse_location("report.pdf"),
            FileLocation::Local(PathBuf::from("report.pdf"))
        );
    }

    #[test]
    fn test_transfer_direction() {
        let src = parse_location("file.txt");
        let dst = parse_location("server:/tmp/");
        assert!(!src.is_remote());
        assert!(dst.is_remote());

        let src = parse_location("server:/tmp/file.txt");
        let dst = parse_location("./");
        assert!(src.is_remote());
        assert!(!dst.is_remote());
    }

    #[test]
    fn test_parse_location_empty_remote_path() {
        // `server:` produces an empty path -- used for upload default directory
        let loc = parse_location("server:");
        assert_eq!(
            loc,
            FileLocation::Remote {
                node: "server".to_string(),
                path: "".to_string(),
            }
        );
    }

    #[test]
    fn test_parse_location_remote_trailing_slash() {
        // `server:/tmp/` -- path ends with slash, indicating a directory
        let loc = parse_location("server:/tmp/");
        match loc {
            FileLocation::Remote { path, .. } => {
                assert!(path.ends_with('/'), "Path should preserve trailing slash");
            }
            _ => panic!("Expected Remote location"),
        }
    }

    #[test]
    fn test_parse_location_windows_drive_is_local() {
        // Single-char node names are rejected (likely Windows drive letters)
        assert_eq!(
            parse_location("C:\\Users\\file.txt"),
            FileLocation::Local(PathBuf::from("C:\\Users\\file.txt"))
        );
    }
}
