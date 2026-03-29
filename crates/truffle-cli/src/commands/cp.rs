//! `truffle cp` -- copy files between nodes (like scp).
//!
//! Syntax: `truffle cp <src> <dst>`
//! - Upload: `truffle cp file.txt server:/tmp/`
//! - Download: `truffle cp server:/tmp/file.txt ./`

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::{method, notification, DaemonNotification};
use crate::exit_codes;
use crate::json_output;
use crate::output;

/// Parse a location string into (node, path) or (None, local_path).
fn parse_location(s: &str) -> (Option<&str>, &str) {
    // Look for `node:/path` or `node:path`
    if let Some(colon_pos) = s.find(':') {
        // Make sure it's not a Windows drive letter (C:\...)
        if colon_pos > 1 || !s.as_bytes()[0].is_ascii_alphabetic() {
            let node = &s[..colon_pos];
            let path = &s[colon_pos + 1..];
            return (Some(node), path);
        }
    }
    (None, s)
}

pub async fn run(
    config: &TruffleConfig,
    source: &str,
    dest: &str,
    verify: bool,
    json: bool,
) -> Result<(), (i32, String)> {
    let _ = verify; // SHA-256 is always on in v2

    let (src_node, src_path) = parse_location(source);
    let (dst_node, dst_path) = parse_location(dest);

    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| (exit_codes::NOT_ONLINE, e.to_string()))?;

    match (src_node, dst_node) {
        (None, Some(remote_node)) => {
            // Upload: local -> remote
            upload(&client, config, src_path, remote_node, dst_path, json).await
        }
        (Some(remote_node), None) => {
            // Download: remote -> local
            download(&client, config, remote_node, src_path, dst_path, json).await
        }
        (None, None) => Err((
            exit_codes::USAGE,
            "Both source and destination are local. Use 'cp' instead.".to_string(),
        )),
        (Some(_), Some(_)) => Err((
            exit_codes::USAGE,
            "Direct node-to-node copy is not yet supported. Copy to local first.".to_string(),
        )),
    }
}

async fn upload(
    client: &DaemonClient,
    config: &TruffleConfig,
    local_path: &str,
    remote_node: &str,
    remote_path: &str,
    json: bool,
) -> Result<(), (i32, String)> {
    // Verify local file exists
    if !std::path::Path::new(local_path).exists() {
        return Err((
            exit_codes::NOT_FOUND,
            format!("File not found: {local_path}"),
        ));
    }

    if !json {
        println!();
        println!(
            "  Copying {} -> {}:{}",
            output::bold(local_path),
            output::bold(remote_node),
            remote_path,
        );
        println!();
    }

    let start = std::time::Instant::now();

    let result = client
        .request_with_notifications(
            method::PUSH_FILE,
            serde_json::json!({
                "peer_id": remote_node,
                "local_path": local_path,
                "remote_path": remote_path,
            }),
            |notif: &DaemonNotification| {
                if !json && notif.method == notification::CP_PROGRESS {
                    let current = notif.params["bytes_sent"].as_u64().unwrap_or(0);
                    let total = notif.params["total_bytes"].as_u64().unwrap_or(0);
                    let speed = notif.params["bytes_per_second"].as_f64().unwrap_or(0.0);
                    output::print_progress(current, total, speed);
                }
            },
        )
        .await
        .map_err(|e| (exit_codes::TRANSFER_FAILED, e.to_string()))?;

    let bytes = result["bytes_transferred"].as_u64().unwrap_or(0);
    let sha256 = result["sha256"].as_str().unwrap_or("").to_string();

    if json {
        let mut map = json_output::envelope(&config.node.name);
        map.insert("file".to_string(), serde_json::json!(local_path));
        map.insert("bytes".to_string(), serde_json::json!(bytes));
        if !sha256.is_empty() {
            map.insert("sha256".to_string(), serde_json::json!(sha256));
        }
        json_output::print_json(&serde_json::Value::Object(map));
    } else {
        let elapsed = start.elapsed().as_secs_f64();
        output::print_progress_complete(bytes, elapsed);

        if !sha256.is_empty() {
            output::print_success(&format!(
                "SHA-256 verified: {}...{}",
                &sha256[..8],
                &sha256[sha256.len() - 8..]
            ));
        }

        println!();
    }

    Ok(())
}

async fn download(
    client: &DaemonClient,
    config: &TruffleConfig,
    remote_node: &str,
    remote_path: &str,
    local_path: &str,
    json: bool,
) -> Result<(), (i32, String)> {
    if !json {
        println!();
        println!(
            "  Copying {}:{} -> {}",
            output::bold(remote_node),
            remote_path,
            output::bold(local_path),
        );
        println!();
    }

    let start = std::time::Instant::now();

    let result = client
        .request_with_notifications(
            method::GET_FILE,
            serde_json::json!({
                "peer_id": remote_node,
                "remote_path": remote_path,
                "local_path": local_path,
            }),
            |notif: &DaemonNotification| {
                if !json && notif.method == notification::CP_PROGRESS {
                    let current = notif.params["bytes_received"].as_u64().unwrap_or(0);
                    let total = notif.params["total_bytes"].as_u64().unwrap_or(0);
                    let speed = notif.params["bytes_per_second"].as_f64().unwrap_or(0.0);
                    output::print_progress(current, total, speed);
                }
            },
        )
        .await
        .map_err(|e| (exit_codes::TRANSFER_FAILED, e.to_string()))?;

    let bytes = result["bytes_transferred"].as_u64().unwrap_or(0);
    let sha256 = result["sha256"].as_str().unwrap_or("").to_string();

    if json {
        let mut map = json_output::envelope(&config.node.name);
        map.insert("file".to_string(), serde_json::json!(remote_path));
        map.insert("bytes".to_string(), serde_json::json!(bytes));
        if !sha256.is_empty() {
            map.insert("sha256".to_string(), serde_json::json!(sha256));
        }
        json_output::print_json(&serde_json::Value::Object(map));
    } else {
        let elapsed = start.elapsed().as_secs_f64();
        output::print_progress_complete(bytes, elapsed);

        if !sha256.is_empty() {
            output::print_success(&format!(
                "SHA-256 verified: {}...{}",
                &sha256[..8],
                &sha256[sha256.len() - 8..]
            ));
        }

        println!();
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_location_remote() {
        let (node, path) = parse_location("server:/tmp/file.txt");
        assert_eq!(node, Some("server"));
        assert_eq!(path, "/tmp/file.txt");
    }

    #[test]
    fn test_parse_location_local() {
        let (node, path) = parse_location("./file.txt");
        assert_eq!(node, None);
        assert_eq!(path, "./file.txt");
    }

    #[test]
    fn test_parse_location_local_absolute() {
        let (node, path) = parse_location("/tmp/file.txt");
        assert_eq!(node, None);
        assert_eq!(path, "/tmp/file.txt");
    }
}
