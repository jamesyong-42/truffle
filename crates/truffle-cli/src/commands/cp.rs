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
use crate::daemon::protocol::method;
use crate::output;

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
        file_name.to_string()
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
    let result = client
        .request(
            method::PUSH_FILE,
            serde_json::json!({
                "node": node,
                "local_path": abs_local_path.to_string_lossy(),
                "remote_path": dest_path,
            }),
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
async fn do_download(
    client: &DaemonClient,
    node: &str,
    remote_path: &str,
    local_path: &Path,
    verify: bool,
) -> Result<(), String> {
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

    // Request file from daemon
    let result = client
        .request(
            method::GET_FILE,
            serde_json::json!({
                "node": node,
                "path": remote_path,
            }),
        )
        .await;

    let elapsed = started.elapsed().as_secs_f64();

    match result {
        Ok(resp) => {
            let data_b64 = resp["data_base64"]
                .as_str()
                .unwrap_or("");
            let file_data = base64_decode(data_b64)?;
            let file_size = file_data.len() as u64;

            // Write to local file
            std::fs::write(&dest_path, &file_data)
                .map_err(|e| format!("Can't write to {}: {e}", dest_path.display()))?;

            output::print_progress_complete(file_size, elapsed);

            if verify {
                let local_hash = compute_sha256(&dest_path)?;
                let remote_hash = resp["sha256"].as_str().unwrap_or("");
                if !remote_hash.is_empty() && remote_hash == local_hash {
                    output::print_success("SHA-256 verified");
                } else if remote_hash.is_empty() {
                    // Still compute and display local hash
                    output::print_success(&format!("Downloaded ({} SHA-256: {})", output::format_bytes(file_size), &local_hash[..16]));
                } else {
                    output::print_error(
                        "SHA-256 mismatch",
                        &format!("Local:  {}\nRemote: {}", local_hash, remote_hash),
                        "The file may have been corrupted. Try again.",
                    );
                    return Err("SHA-256 mismatch".to_string());
                }
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
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Compute SHA-256 hash of a file.
fn compute_sha256(path: &Path) -> Result<String, String> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .map_err(|e| format!("Can't open file for hashing: {e}"))?;

    // Simple SHA-256 using a basic implementation.
    // For a production CLI we would use the `sha2` crate, but to avoid
    // adding a dependency we use a manual approach via the system's
    // shasum command, or we include a minimal implementation.
    //
    // For now, shell out to shasum (available on macOS/Linux).
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|e| format!("Can't read file: {e}"))?;

    // Use std::process::Command to call shasum
    let output = std::process::Command::new("shasum")
        .args(["-a", "256"])
        .arg(path.as_os_str())
        .output()
        .map_err(|e| format!("Can't compute SHA-256 (shasum not found?): {e}"))?;

    if !output.status.success() {
        return Err("shasum failed".to_string());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let hash = stdout.split_whitespace().next().unwrap_or("").to_string();
    if hash.len() == 64 {
        Ok(hash)
    } else {
        Err("Unexpected shasum output".to_string())
    }
}

/// Base64 encode bytes.
///
/// Used by `do_download` (Phase 3 will remove) and tests.
#[allow(dead_code)]
fn base64_encode(data: &[u8]) -> String {
    // Simple base64 encoding without external dependency.
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);

    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };

        let n = (b0 << 16) | (b1 << 8) | b2;

        result.push(CHARS[((n >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((n >> 12) & 0x3F) as usize] as char);

        if chunk.len() > 1 {
            result.push(CHARS[((n >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }

        if chunk.len() > 2 {
            result.push(CHARS[(n & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }

    result
}

/// Base64 decode a string to bytes.
fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    fn char_to_val(c: u8) -> Result<u8, String> {
        match c {
            b'A'..=b'Z' => Ok(c - b'A'),
            b'a'..=b'z' => Ok(c - b'a' + 26),
            b'0'..=b'9' => Ok(c - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            b'=' => Ok(0),
            _ => Err(format!("Invalid base64 character: {}", c as char)),
        }
    }

    let input = input.trim();
    if input.is_empty() {
        return Ok(Vec::new());
    }

    let bytes = input.as_bytes();
    if bytes.len() % 4 != 0 {
        return Err("Invalid base64 length".to_string());
    }

    let mut result = Vec::with_capacity(bytes.len() / 4 * 3);

    for chunk in bytes.chunks(4) {
        let a = char_to_val(chunk[0])?;
        let b = char_to_val(chunk[1])?;
        let c = char_to_val(chunk[2])?;
        let d = char_to_val(chunk[3])?;

        let n = ((a as u32) << 18) | ((b as u32) << 12) | ((c as u32) << 6) | (d as u32);

        result.push((n >> 16) as u8);
        if chunk[2] != b'=' {
            result.push((n >> 8) as u8);
        }
        if chunk[3] != b'=' {
            result.push(n as u8);
        }
    }

    Ok(result)
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
    fn test_base64_roundtrip() {
        let data = b"Hello, truffle!";
        let encoded = base64_encode(data);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_base64_empty() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_decode("").unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn test_base64_padding() {
        // 1 byte -> 4 chars with 2 padding
        let encoded = base64_encode(b"A");
        assert_eq!(encoded, "QQ==");
        assert_eq!(base64_decode(&encoded).unwrap(), b"A");

        // 2 bytes -> 4 chars with 1 padding
        let encoded = base64_encode(b"AB");
        assert_eq!(encoded, "QUI=");
        assert_eq!(base64_decode(&encoded).unwrap(), b"AB");

        // 3 bytes -> 4 chars no padding
        let encoded = base64_encode(b"ABC");
        assert_eq!(encoded, "QUJD");
        assert_eq!(base64_decode(&encoded).unwrap(), b"ABC");
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
}
