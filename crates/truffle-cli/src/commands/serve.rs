//! `truffle serve` -- publish a local service or directory to your tailnet.
//!
//! One invocation publishes one listen port with one mounted route. The
//! positional target decides the mode: a URL is reverse-proxied, anything
//! else is treated as a directory and served as static files. Multi-route
//! composition (an SPA plus an API on one port) is deliberately left to the
//! JavaScript `mesh.serve({ routes })` API — see RFC 023.
//!
//! `serve` is the v2 face of the `proxy` subsystem (RFC 023 D2): it drives
//! the same daemon `proxy_add`/`proxy_list`/`proxy_remove` methods, so a
//! serve id is a proxy id and `serve status` shows the same rows as the
//! (now hidden) `proxy list`.

use clap::{Args, Subcommand};

use crate::config::TruffleConfig;
use crate::daemon::client::DaemonClient;
use crate::daemon::protocol::method;
use crate::exit_codes;
use crate::json_output;
use crate::output;

/// Long help for `truffle serve`. Lives here as a `const` and is wired onto
/// the `Serve` enum variant in `main.rs`, because clap sources a tuple-variant
/// subcommand's about/long_about from the variant, not from this struct's
/// `#[command]` attributes.
pub const LONG_ABOUT: &str = "Publish a local service or a static directory to your tailnet.

The positional target decides the mode:
  - a URL (http://localhost:3000, or the bare host:port form localhost:3000)
    is reverse-proxied to that backend;
  - anything else is a directory path, served as static files (index.html at
    directory roots, ETag/Range for free, no directory listing, dotfiles denied).

One `serve` invocation publishes one listen port with one mounted route. To
compose several routes on a single port (an SPA plus an API, say), use the
JavaScript `mesh.serve({ routes })` API — the CLI stays one-route-per-command
on purpose.

TLS terminates on the listener by default (automatic MagicDNS certificates);
pass --no-tls for a plain-HTTP listener. Targets must be loopback unless
--allow-non-loopback is given.

Examples:
  truffle serve http://localhost:3000 --port 443
  truffle serve ./public --port 443 --fallback /index.html
  truffle serve http://localhost:8000 --port 443 --path /api --strip-prefix

Manage running serves with `truffle serve status` and `truffle serve stop <id>`.";

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct ServeArgs {
    /// URL (http://localhost:3000) or directory (./public) to publish
    #[arg(required = true, value_name = "TARGET")]
    target: Option<String>,

    /// Port to listen on across the tailnet (443 for a suffix-free https URL)
    #[arg(long, required = true)]
    port: Option<u16>,

    /// Mount the target under this path prefix
    #[arg(long, default_value = "/", value_name = "PREFIX")]
    path: String,

    /// SPA fallback for static-directory misses (directory targets only)
    #[arg(long, value_name = "FILE")]
    fallback: Option<String>,

    /// Strip the mount prefix before forwarding (URL targets only)
    #[arg(long)]
    strip_prefix: bool,

    /// Restrict access to matching Tailscale login names (glob; repeatable)
    #[arg(long, value_name = "GLOB")]
    allow: Vec<String>,

    /// Serve plain HTTP instead of terminating TLS on the listener
    #[arg(long)]
    no_tls: bool,

    /// Allow a non-loopback target (turns this node into a gateway to it)
    #[arg(long)]
    allow_non_loopback: bool,

    /// Display name for this serve (defaults to `serve-<port>`)
    #[arg(long, value_name = "NAME")]
    name: Option<String>,

    /// Stable id for this serve (defaults to the name)
    #[arg(long, value_name = "ID")]
    id: Option<String>,

    #[command(subcommand)]
    command: Option<ServeCommand>,
}

#[derive(Subcommand)]
pub enum ServeCommand {
    /// List active serves on this node
    Status,
    /// Stop an active serve
    Stop {
        /// Serve id (or name) to stop
        id: String,
    },
}

/// The positional target, classified into the two serve modes.
enum Target {
    /// Reverse-proxy to this URL (kept verbatim for the routes shape).
    Url(String),
    /// Serve this absolute directory path as static files.
    Dir(String),
}

pub async fn run(config: &TruffleConfig, args: ServeArgs, json: bool) -> Result<(), (i32, String)> {
    // Each branch starts the daemon itself, so `serve <target>` can reject a
    // bad target or option combination before spawning/awaiting one.
    match args.command {
        Some(ServeCommand::Status) => serve_status(config, json).await,
        Some(ServeCommand::Stop { ref id }) => serve_stop(config, id, json).await,
        None => serve_start(config, args, json).await,
    }
}

async fn serve_start(
    config: &TruffleConfig,
    args: ServeArgs,
    json: bool,
) -> Result<(), (i32, String)> {
    // clap's `required = true` guarantees both are present when no subcommand
    // was matched (`subcommand_negates_reqs` waives them for status/stop).
    let target_raw = args
        .target
        .expect("clap requires a target when no subcommand is given");
    let port = args
        .port
        .expect("clap requires --port when no subcommand is given");

    let path = normalize_prefix(&args.path);
    let target = classify_target(&target_raw).map_err(|e| (exit_codes::USAGE, e))?;

    // Option/target compatibility (friendlier than the core's add-time check).
    match &target {
        Target::Url(_) if args.fallback.is_some() => {
            return Err((
                exit_codes::USAGE,
                "--fallback only applies to directory targets".to_string(),
            ));
        }
        Target::Dir(_) if args.strip_prefix => {
            return Err((
                exit_codes::USAGE,
                "--strip-prefix only applies to URL targets".to_string(),
            ));
        }
        _ => {}
    }
    if args.allow.iter().any(|g| g.trim().is_empty()) {
        return Err((
            exit_codes::USAGE,
            "--allow globs must not be empty".to_string(),
        ));
    }

    let tls = !args.no_tls;
    let allow_non_loopback = args.allow_non_loopback;
    let allow = args.allow.clone();

    let name = args.name.clone().unwrap_or_else(|| format!("serve-{port}"));
    let serve_id = args.id.clone().unwrap_or_else(|| name.clone());

    // Fields common to every wire shape. The v2 fields (tls / allow /
    // allow_non_loopback / routes) are always sent; a v1 sidecar sees them at
    // their defaults for a simple proxy and behaves exactly as before, while
    // any non-default value already requires a v2 sidecar (enforced in core).
    let mut params = serde_json::json!({
        "id": serve_id,
        "name": name,
        "listen_port": port,
        "tls": tls,
        "allow_non_loopback": allow_non_loopback,
        "allow": allow,
        "announce": true,
    });
    let obj = params
        .as_object_mut()
        .expect("json object literal is always an object");

    match &target {
        Target::Url(url) => {
            // Old-sidecar compatibility: the simplest whole-service proxy
            // (root mount, no prefix stripping, no allow-list, TLS on,
            // loopback target, and a URL with no path of its own) still fits
            // the v1 single-target shape — target_host/port/scheme with empty
            // routes — which a pre-RFC-023 sidecar understands. Any v2-only
            // option forces the routes shape (which requires a v2 sidecar
            // anyway).
            let v1_eligible =
                path == "/" && !args.strip_prefix && allow.is_empty() && tls && !allow_non_loopback;
            match v1_eligible.then(|| parse_simple_url(url)).flatten() {
                Some((scheme, host, target_port)) => {
                    obj.insert("target_host".to_string(), serde_json::json!(host));
                    obj.insert("target_port".to_string(), serde_json::json!(target_port));
                    obj.insert("target_scheme".to_string(), serde_json::json!(scheme));
                }
                None => {
                    obj.insert(
                        "routes".to_string(),
                        serde_json::json!([{
                            "prefix": path,
                            "target_url": url,
                            "strip_prefix": args.strip_prefix,
                        }]),
                    );
                }
            }
        }
        Target::Dir(abs) => {
            let mut route = serde_json::json!({
                "prefix": path,
                "dir": abs,
            });
            if let Some(fallback) = &args.fallback {
                route
                    .as_object_mut()
                    .expect("json object literal is always an object")
                    .insert(
                        "fallback".to_string(),
                        serde_json::json!(normalize_prefix(fallback)),
                    );
            }
            obj.insert("routes".to_string(), serde_json::json!([route]));
        }
    }

    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| (exit_codes::NOT_ONLINE, e.to_string()))?;

    let result = client
        .request(method::PROXY_ADD, params)
        .await
        .map_err(|e| (exit_codes::ERROR, e.to_string()))?;

    if json {
        let mut map = json_output::envelope(&config.node.device_name);
        map.insert("serve".to_string(), result.clone());
        json_output::print_json(&serde_json::Value::Object(map));
    } else {
        let url = result["url"].as_str().unwrap_or("unknown");
        let what = match &target {
            Target::Url(u) => u.clone(),
            Target::Dir(abs) => abs.clone(),
        };
        output::print_success(&format!("Serving {} on port {}", output::bold(&what), port,));
        println!("  URL: {}", url);
        println!(
            "  {} truffle serve stop {}",
            output::dim("stop with"),
            serve_id,
        );
    }

    Ok(())
}

async fn serve_status(config: &TruffleConfig, json: bool) -> Result<(), (i32, String)> {
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| (exit_codes::NOT_ONLINE, e.to_string()))?;

    let result = client
        .request(method::PROXY_LIST, serde_json::json!({}))
        .await
        .map_err(|e| (exit_codes::ERROR, e.to_string()))?;

    let serves = result["proxies"].as_array();

    if json {
        let mut map = json_output::envelope(&config.node.device_name);
        map.insert(
            "serves".to_string(),
            serves
                .map(|s| serde_json::Value::Array(s.clone()))
                .unwrap_or(serde_json::Value::Array(vec![])),
        );
        json_output::print_json(&serde_json::Value::Object(map));
    } else {
        match serves {
            Some(arr) if !arr.is_empty() => {
                println!("{:<16} {:<8} {:<10} URL", "NAME", "PORT", "STATUS");
                for serve in arr {
                    let name = serve["name"].as_str().unwrap_or("-");
                    let port = serve["listen_port"].as_u64().unwrap_or(0);
                    let status = serve["status"].as_str().unwrap_or("-");
                    let url = serve["url"].as_str().unwrap_or("-");
                    println!("{:<16} {:<8} {:<10} {}", name, port, status, url);
                }
            }
            _ => {
                println!("No active serves.");
                println!("  Use 'truffle serve <target> --port <n>' to start one.");
            }
        }
    }

    Ok(())
}

async fn serve_stop(config: &TruffleConfig, id: &str, json: bool) -> Result<(), (i32, String)> {
    let client = DaemonClient::new();
    client
        .ensure_running(config)
        .await
        .map_err(|e| (exit_codes::NOT_ONLINE, e.to_string()))?;

    client
        .request(method::PROXY_REMOVE, serde_json::json!({ "id": id }))
        .await
        .map_err(|e| (exit_codes::ERROR, e.to_string()))?;

    if json {
        let mut map = json_output::envelope(&config.node.device_name);
        map.insert("stopped".to_string(), serde_json::json!(id));
        json_output::print_json(&serde_json::Value::Object(map));
    } else {
        output::print_success(&format!("Serve {} stopped.", output::bold(id)));
    }

    Ok(())
}

/// Decide whether the positional target is a URL (reverse proxy) or a
/// filesystem directory (static serving).
///
/// - `http://` / `https://` prefix -> URL, kept verbatim.
/// - bare `host:port` (e.g. `localhost:3000`) -> URL, default scheme `http`.
/// - anything else -> a directory path, resolved to an absolute path and
///   required to exist. Prefix a name that would otherwise look like a
///   `host:port` or a `serve` subcommand with `./` (e.g. `./status`) to force
///   directory interpretation.
fn classify_target(raw: &str) -> Result<Target, String> {
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return Ok(Target::Url(raw.to_string()));
    }
    if looks_like_host_port(raw) {
        return Ok(Target::Url(format!("http://{raw}")));
    }

    // Directory target: resolve to an absolute path (the daemon/sidecar may
    // run with a different cwd, so the path must be absolute on the wire) and
    // confirm it is a real directory.
    let abs =
        std::fs::canonicalize(raw).map_err(|_| format!("no such file or directory: {raw}"))?;
    if !abs.is_dir() {
        return Err(format!("not a directory: {raw}"));
    }
    let abs = abs
        .to_str()
        .ok_or_else(|| format!("directory path is not valid UTF-8: {raw}"))?
        .to_string();
    Ok(Target::Dir(abs))
}

/// True for a bare `host:port` authority: host-label characters, a colon,
/// then a run of digits, and nothing else (no scheme, no path).
fn looks_like_host_port(s: &str) -> bool {
    match s.rsplit_once(':') {
        Some((host, port)) => {
            !host.is_empty()
                && host
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
                && !port.is_empty()
                && port.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

/// Decompose a URL into `(scheme, host, port)` for the v1 single-target proxy
/// shape. Returns `None` when the URL carries a path, uses a bracketed IPv6
/// host, or is otherwise not expressible as host/port/scheme — the caller then
/// falls back to the routes shape, which passes the URL string verbatim.
fn parse_simple_url(url: &str) -> Option<(String, String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return None;
    }

    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, path) = rest.split_at(authority_end);
    if !(path.is_empty() || path == "/") {
        return None; // a non-root path can't ride the whole-port v1 shape
    }
    if authority.is_empty() || authority.starts_with('[') {
        return None; // empty or bracketed IPv6 -> defer to the routes shape
    }

    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => {
            if host.is_empty() {
                return None;
            }
            (host.to_string(), port.parse().ok()?)
        }
        None => {
            let port = if scheme == "https" { 443 } else { 80 };
            (authority.to_string(), port)
        }
    };
    Some((scheme, host, port))
}

/// Ensure a mount/fallback path begins with `/`. Core rejects relative
/// prefixes; we fix the common `--path api` slip instead of erroring.
fn normalize_prefix(p: &str) -> String {
    if p.starts_with('/') {
        p.to_string()
    } else {
        format!("/{p}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port_shorthand_is_a_url() {
        assert!(looks_like_host_port("localhost:3000"));
        assert!(looks_like_host_port("127.0.0.1:8080"));
        assert!(looks_like_host_port("dev-box.local:5173"));
    }

    #[test]
    fn paths_and_names_are_not_host_port() {
        assert!(!looks_like_host_port("./public"));
        assert!(!looks_like_host_port("public"));
        assert!(!looks_like_host_port("status"));
        assert!(!looks_like_host_port("/var/www"));
        assert!(!looks_like_host_port("./weird:name")); // host segment has '/'
        assert!(!looks_like_host_port("localhost:")); // empty port
        assert!(!looks_like_host_port("localhost:http")); // non-numeric port
    }

    #[test]
    fn parse_simple_url_extracts_parts() {
        assert_eq!(
            parse_simple_url("http://localhost:3000"),
            Some(("http".into(), "localhost".into(), 3000))
        );
        assert_eq!(
            parse_simple_url("http://localhost:3000/"),
            Some(("http".into(), "localhost".into(), 3000))
        );
        assert_eq!(
            parse_simple_url("https://127.0.0.1"),
            Some(("https".into(), "127.0.0.1".into(), 443))
        );
        assert_eq!(
            parse_simple_url("http://localhost"),
            Some(("http".into(), "localhost".into(), 80))
        );
    }

    #[test]
    fn parse_simple_url_defers_complex_urls() {
        // A path of its own can't ride the whole-port v1 shape.
        assert_eq!(parse_simple_url("http://localhost:8000/api"), None);
        // Bracketed IPv6 -> routes shape (verbatim URL).
        assert_eq!(parse_simple_url("http://[::1]:3000"), None);
        // No scheme separator.
        assert_eq!(parse_simple_url("localhost:3000"), None);
        // Non-numeric port.
        assert_eq!(parse_simple_url("http://localhost:abc"), None);
    }

    #[test]
    fn normalize_prefix_forces_leading_slash() {
        assert_eq!(normalize_prefix("/api"), "/api");
        assert_eq!(normalize_prefix("api"), "/api");
        assert_eq!(normalize_prefix("/"), "/");
    }
}
