//! Output formatting helpers for the truffle CLI.
//!
//! Provides consistent, beautiful terminal output following the cli-design.md
//! principles: Unicode box-drawing, color indicators, aligned tables,
//! structured error messages, and progress bars.

use std::fmt;
use std::fmt::Write;
use std::io::{self, Write as _};

// ═══════════════════════════════════════════════════════════════════════════
// Color mode
// ═══════════════════════════════════════════════════════════════════════════

/// Whether color output is enabled (set once at startup).
static COLOR_ENABLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// Initialize color output based on mode. Call once at startup.
///
/// - `"never"` disables color unconditionally.
/// - `"always"` enables color unconditionally.
/// - `"auto"` (default) enables color when stdout is a TTY.
pub fn init_color(mode: &str) {
    let enabled = match mode {
        "never" => false,
        "always" => true,
        _ => is_tty(),
    };
    COLOR_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

/// Check if stdout is a TTY.
fn is_tty() -> bool {
    unsafe { libc::isatty(libc::STDOUT_FILENO) != 0 }
}

/// Returns `true` if color output is currently enabled.
pub fn color_enabled() -> bool {
    COLOR_ENABLED.load(std::sync::atomic::Ordering::Relaxed)
}

fn apply_ansi(text: &str, code: &str) -> String {
    if color_enabled() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// ANSI color helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Wrap text in ANSI bold.
pub fn bold(text: &str) -> String {
    apply_ansi(text, "1")
}

/// Wrap text in ANSI dim (faint).
pub fn dim(text: &str) -> String {
    apply_ansi(text, "2")
}

/// Wrap text in ANSI green.
pub fn green(text: &str) -> String {
    apply_ansi(text, "32")
}

/// Wrap text in ANSI red.
pub fn red(text: &str) -> String {
    apply_ansi(text, "31")
}

/// Wrap text in ANSI yellow.
pub fn yellow(text: &str) -> String {
    apply_ansi(text, "33")
}

/// Wrap text in ANSI cyan.
pub fn cyan(text: &str) -> String {
    apply_ansi(text, "36")
}

/// Wrap text in bold red.
pub fn bold_red(text: &str) -> String {
    apply_ansi(text, "1;31")
}

/// Wrap text in bold green.
pub fn bold_green(text: &str) -> String {
    apply_ansi(text, "1;32")
}

// ═══════════════════════════════════════════════════════════════════════════
// Status indicators
// ═══════════════════════════════════════════════════════════════════════════

/// A diagnostic/status indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indicator {
    /// Green checkmark (pass).
    Pass,
    /// Red cross (fail).
    Fail,
    /// Yellow warning.
    Warn,
    /// Dim dash (skipped/unavailable).
    Skip,
    /// Green filled circle (online).
    Online,
    /// Dim open circle (offline).
    Offline,
}

impl Indicator {
    /// Render as a colored symbol.
    pub fn symbol(self) -> String {
        match self {
            Indicator::Pass => green("\u{2713}"),    // ✓
            Indicator::Fail => red("\u{2717}"),       // ✗
            Indicator::Warn => yellow("\u{26a0}"),    // ⚠
            Indicator::Skip => dim("\u{2014}"),       // —
            Indicator::Online => green("\u{25cf}"),   // ●
            Indicator::Offline => dim("\u{25cb}"),    // ○
        }
    }
}

impl fmt::Display for Indicator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.symbol())
    }
}

/// Return a colored status indicator dot.
///
/// - Online: green filled circle
/// - Offline: dim open circle
/// - Connecting: yellow filled circle
pub fn status_indicator(status: &str) -> String {
    match status.to_lowercase().as_str() {
        "online" => green("\u{25cf}"),        // ●
        "connecting" => yellow("\u{25cf}"),    // ●
        _ => dim("\u{25cb}"),                  // ○
    }
}

/// Return a status label with color.
pub fn status_label(status: &str) -> String {
    match status.to_lowercase().as_str() {
        "online" => green("online"),
        "connecting" => yellow("connecting"),
        "offline" => dim("offline"),
        other => dim(other),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Formatting helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Format a latency value in milliseconds.
///
/// Returns a human-readable string like "2.1ms" or a dash for None.
pub fn format_latency(ms: Option<f64>) -> String {
    match ms {
        Some(v) if v < 1.0 => format!("{v:.2}ms"),
        Some(v) if v < 10.0 => format!("{v:.1}ms"),
        Some(v) => format!("{v:.0}ms"),
        None => "\u{2014}".to_string(), // —
    }
}

/// Format an uptime duration from seconds.
///
/// Returns a human-readable string like "2h 14m" or "just started".
pub fn format_uptime(seconds: u64) -> String {
    if seconds < 5 {
        return "just started".to_string();
    }
    let days = seconds / 86400;
    let hours = (seconds % 86400) / 3600;
    let mins = (seconds % 3600) / 60;
    let secs = seconds % 60;

    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m")
    } else if mins > 0 {
        format!("{mins}m {secs}s")
    } else {
        format!("{secs}s")
    }
}

/// Format a duration in seconds as a human-readable string.
///
/// Alias for `format_uptime` but uses slightly different thresholds
/// appropriate for general durations.
pub fn format_duration(secs: u64) -> String {
    if secs < 60 {
        if secs == 0 {
            return "just started".to_string();
        }
        format!("{secs}s")
    } else if secs < 3600 {
        let mins = secs / 60;
        format!("{mins}m")
    } else if secs < 86400 {
        let hours = secs / 3600;
        let mins = (secs % 3600) / 60;
        format!("{hours}h {mins}m")
    } else {
        let days = secs / 86400;
        if days == 1 {
            "1 day".to_string()
        } else {
            format!("{days} days")
        }
    }
}

/// Format a connection type (direct/relayed).
pub fn format_connection(connection: Option<&str>) -> String {
    match connection {
        Some("direct") => "direct".to_string(),
        Some(relay) if relay.starts_with("via relay:") || relay.starts_with("relayed") => {
            yellow(relay)
        }
        Some(other) => other.to_string(),
        None => "\u{2014}".to_string(), // —
    }
}

/// Format a byte count as human-readable.
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Format a number with k/M suffix for readability.
pub fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Format a transfer speed in bytes/sec as human-readable.
pub fn format_speed(bytes_per_sec: f64) -> String {
    if bytes_per_sec >= 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} GB/s", bytes_per_sec / (1024.0 * 1024.0 * 1024.0))
    } else if bytes_per_sec >= 1024.0 * 1024.0 {
        format!("{:.1} MB/s", bytes_per_sec / (1024.0 * 1024.0))
    } else if bytes_per_sec >= 1024.0 {
        format!("{:.1} KB/s", bytes_per_sec / 1024.0)
    } else {
        format!("{:.0} B/s", bytes_per_sec)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Table printer
// ═══════════════════════════════════════════════════════════════════════════

/// Print a formatted table with headers and rows.
///
/// Headers are printed in dim (metadata-style), columns auto-sized.
///
/// ```text
///   NODE                  STATUS     LATENCY
///   james-macbook         ● online   —
///   living-room-server    ● online   2ms
/// ```
pub fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    if headers.is_empty() {
        return;
    }

    // Calculate column widths (max of header and all row values)
    let col_count = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.len()).collect();

    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            if i < col_count {
                let visible_len = strip_ansi_len(cell);
                if visible_len > widths[i] {
                    widths[i] = visible_len;
                }
            }
        }
    }

    // Add padding between columns
    let padding = 2;

    // Print header (dimmed, per cli-design.md: headers are metadata, not data)
    let mut header_line = String::new();
    for (i, h) in headers.iter().enumerate() {
        if i > 0 {
            header_line.push_str(&" ".repeat(padding));
        }
        let _ = write!(header_line, "{:<width$}", h, width = widths[i]);
    }
    println!("  {}", dim(&header_line));

    // Print rows
    for row in rows {
        let mut line = String::new();
        for (i, cell) in row.iter().enumerate() {
            if i >= col_count {
                break;
            }
            if i > 0 {
                line.push_str(&" ".repeat(padding));
            }
            let visible_len = strip_ansi_len(cell);
            let pad = widths[i].saturating_sub(visible_len);
            line.push_str(cell);
            line.push_str(&" ".repeat(pad));
        }
        println!("  {line}");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Status / KV output
// ═══════════════════════════════════════════════════════════════════════════

/// Print a key-value status line with optional indicator.
///
/// ```text
///   Status      ● online
///   IP          100.64.0.3
/// ```
pub fn print_status(label: &str, value: &str, indicator: Option<Indicator>) {
    let label_formatted = format!("{:<12}", label);
    match indicator {
        Some(ind) => println!("  {} {} {}", dim(&label_formatted), ind, value),
        None => println!("  {}{}", dim(&label_formatted), value),
    }
}

/// Print a key-value line with consistent alignment.
pub fn print_kv(key: &str, value: &str, key_width: usize) {
    println!("  {:<width$}  {}", key, value, width = key_width);
}

// ═══════════════════════════════════════════════════════════════════════════
// Section drawing
// ═══════════════════════════════════════════════════════════════════════════

/// Print a section header with a horizontal rule.
///
/// ```text
///   Mesh
///   ────
/// ```
pub fn print_section(title: &str) {
    println!();
    println!("  {}", bold(title));
    println!("  {}", dim(&"\u{2500}".repeat(title.len().max(4)))); // ─
}

/// Print a title line for a dashboard.
///
/// ```text
///   truffle · james-macbook
///   ───────────────────────────────────────
/// ```
pub fn print_title(left: &str, right: &str) {
    let title = format!("{} \u{00b7} {}", left, right);
    println!();
    println!("  {}", bold(&title));
    println!("  {}", dim(&"\u{2500}".repeat(39)));
}

// ═══════════════════════════════════════════════════════════════════════════
// Progress bar
// ═══════════════════════════════════════════════════════════════════════════

/// Print a progress bar that overwrites the current line.
///
/// ```text
///   ████████████████████████░░░░░░░░  67%  2.4 MB/s  eta 3s
/// ```
pub fn print_progress(current: u64, total: u64, speed_bps: f64) {
    let pct = if total > 0 {
        (current as f64 / total as f64 * 100.0).min(100.0)
    } else {
        0.0
    };
    let bar_width = 32;
    let filled = (pct / 100.0 * bar_width as f64) as usize;
    let empty = bar_width - filled;

    let bar = format!(
        "{}{}",
        cyan(&"\u{2588}".repeat(filled)),  // █
        dim(&"\u{2591}".repeat(empty)),     // ░
    );

    let pct_str = format!("{:.0}%", pct);
    let speed_str = format_speed(speed_bps);

    let eta_str = if pct >= 100.0 {
        "done".to_string()
    } else if speed_bps > 0.0 && current > 0 {
        let remaining = total.saturating_sub(current) as f64;
        let secs = remaining / speed_bps;
        if secs < 60.0 {
            format!("eta {:.0}s", secs)
        } else {
            format!("eta {:.0}m", secs / 60.0)
        }
    } else {
        String::new()
    };

    let mut line = format!("  {}  {}", bar, pct_str);
    if speed_bps > 0.0 {
        line.push_str(&format!("  {}", speed_str));
    }
    if !eta_str.is_empty() {
        line.push_str(&format!("  {}", eta_str));
    }

    // Pad to clear any leftover characters from previous longer lines
    let padded = format!("{:<80}", line);
    print!("\r{padded}");
    let _ = io::stdout().flush();
}

/// Print a progress completion line (replaces the progress bar).
///
/// ```text
///   ████████████████████████████████  100%  done (4.2 MB in 1.8s)
/// ```
pub fn print_progress_complete(total: u64, elapsed_secs: f64) {
    let bar = cyan(&"\u{2588}".repeat(32));
    let size_str = format_bytes(total);
    let time_str = if elapsed_secs < 1.0 {
        format!("{:.0}ms", elapsed_secs * 1000.0)
    } else {
        format!("{:.1}s", elapsed_secs)
    };
    println!(
        "\r  {}  100%  done ({} in {})              ",
        bar, size_str, time_str,
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Diagnostic output
// ═══════════════════════════════════════════════════════════════════════════

/// Print a diagnostic check result line.
///
/// ```text
///   ✓ Tailscale installed            v1.62.0
///   ✗ Tailscale not connected
///     → Run 'tailscale up' to connect
/// ```
pub fn print_check(indicator: Indicator, label: &str, detail: &str) {
    if detail.is_empty() {
        println!("  {} {}", indicator.symbol(), label);
    } else {
        let label_padded = format!("{:<32}", label);
        println!("  {} {}{}", indicator.symbol(), label_padded, dim(detail));
    }
}

/// Print a fix suggestion indented under a check result.
///
/// ```text
///     → Run 'tailscale up' to connect
/// ```
pub fn print_fix_suggestion(suggestion: &str) {
    println!("    {} {}", dim("\u{2192}"), suggestion);
}

// ═══════════════════════════════════════════════════════════════════════════
// Error / Success / Warning messages
// ═══════════════════════════════════════════════════════════════════════════

/// Print a structured error message following the truffle three-part pattern:
///
/// ```text
///   ✗ Can't reach "laptop"
///
///     The node appears to be offline. It was last seen 3 hours ago.
///
///     Try:
///       truffle ls          see who's online
///       truffle doctor      diagnose network issues
/// ```
pub fn print_error(what: &str, why: &str, fix: &str) {
    eprintln!();
    eprintln!("  {} {}", red("\u{2717}"), what);
    if !why.is_empty() {
        eprintln!();
        eprintln!("    {}", why);
    }
    if !fix.is_empty() {
        eprintln!();
        eprintln!("    {}", dim("Try:"));
        for line in fix.lines() {
            eprintln!("      {}", line);
        }
    }
    eprintln!();
}

/// Print a success message with a green checkmark.
///
/// ```text
///   ✓ SHA-256 verified
/// ```
pub fn print_success(message: &str) {
    println!("  {} {}", green("\u{2713}"), message);
}

/// Print a warning message with a yellow indicator.
///
/// ```text
///   ⚠ Key expiring soon
/// ```
pub fn print_warning(message: &str) {
    println!("  {} {}", yellow("\u{26a0}"), message);
}

/// Format an error into the three-part structure and return as a String
/// (for use with `Result<(), String>` error paths).
pub fn format_error(what: &str, why: &str, fix: &str) -> String {
    let mut msg = format!("{} {}", "\u{2717}", what);
    if !why.is_empty() {
        msg.push_str(&format!("\n  {}", why));
    }
    if !fix.is_empty() {
        msg.push_str(&format!("\n  Try: {}", fix));
    }
    msg
}

// ═══════════════════════════════════════════════════════════════════════════
// JSON output
// ═══════════════════════════════════════════════════════════════════════════

/// Print a JSON value to stdout (pretty-printed).
pub fn print_json(value: &serde_json::Value) {
    if let Ok(json) = serde_json::to_string_pretty(value) {
        println!("{json}");
    }
}

/// Print a serializable value as JSON to stdout.
pub fn print_json_value<T: serde::Serialize>(value: &T) {
    if let Ok(json) = serde_json::to_string_pretty(value) {
        println!("{json}");
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Internal helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Calculate the visible length of a string, stripping ANSI escape codes.
fn strip_ansi_len(s: &str) -> usize {
    let mut len = 0;
    let mut in_escape = false;
    for c in s.chars() {
        if in_escape {
            if c == 'm' {
                in_escape = false;
            }
        } else if c == '\x1b' {
            in_escape = true;
        } else {
            len += 1;
        }
    }
    len
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_latency() {
        assert_eq!(format_latency(Some(0.5)), "0.50ms");
        assert_eq!(format_latency(Some(2.1)), "2.1ms");
        assert_eq!(format_latency(Some(14.0)), "14ms");
        assert_eq!(format_latency(None), "\u{2014}");
    }

    #[test]
    fn test_format_uptime() {
        assert_eq!(format_uptime(0), "just started");
        assert_eq!(format_uptime(3), "just started");
        assert_eq!(format_uptime(45), "45s");
        assert_eq!(format_uptime(120), "2m 0s");
        assert_eq!(format_uptime(3661), "1h 1m");
        assert_eq!(format_uptime(90061), "1d 1h 1m");
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(0), "just started");
        assert_eq!(format_duration(30), "30s");
        assert_eq!(format_duration(60), "1m");
        assert_eq!(format_duration(90), "1m");
        assert_eq!(format_duration(3600), "1h 0m");
        assert_eq!(format_duration(3661), "1h 1m");
        assert_eq!(format_duration(86400), "1 day");
        assert_eq!(format_duration(172800), "2 days");
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(2_500_000), "2.4 MB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GB");
    }

    #[test]
    fn test_format_count() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(500), "500");
        assert_eq!(format_count(1_000), "1.0k");
        assert_eq!(format_count(1_200), "1.2k");
        assert_eq!(format_count(1_000_000), "1.0M");
        assert_eq!(format_count(2_500_000), "2.5M");
    }

    #[test]
    fn test_format_speed() {
        assert_eq!(format_speed(500.0), "500 B/s");
        assert_eq!(format_speed(1024.0), "1.0 KB/s");
        assert_eq!(format_speed(2_500_000.0), "2.4 MB/s");
        assert_eq!(format_speed(1024.0 * 1024.0 * 1024.0), "1.0 GB/s");
    }

    #[test]
    fn test_format_error() {
        let msg = format_error("Can't reach \"laptop\"", "The node is offline.", "truffle ls");
        assert!(msg.contains("Can't reach"));
        assert!(msg.contains("offline"));
        assert!(msg.contains("truffle ls"));
    }

    #[test]
    fn test_status_indicator() {
        assert!(!status_indicator("online").is_empty());
        assert!(!status_indicator("offline").is_empty());
        assert!(!status_indicator("connecting").is_empty());
    }

    #[test]
    fn test_indicator_symbols() {
        assert!(!Indicator::Pass.symbol().is_empty());
        assert!(!Indicator::Fail.symbol().is_empty());
        assert!(!Indicator::Warn.symbol().is_empty());
        assert!(!Indicator::Skip.symbol().is_empty());
        assert!(!Indicator::Online.symbol().is_empty());
        assert!(!Indicator::Offline.symbol().is_empty());
    }

    #[test]
    fn test_strip_ansi_len() {
        assert_eq!(strip_ansi_len("hello"), 5);
        assert_eq!(strip_ansi_len("\x1b[32mhello\x1b[0m"), 5);
        assert_eq!(strip_ansi_len("\x1b[1m\x1b[32mhi\x1b[0m"), 2);
        assert_eq!(strip_ansi_len("no escape"), 9);
        assert_eq!(strip_ansi_len("\x1b[1;31mERROR\x1b[0m"), 5);
    }

    #[test]
    fn test_format_connection() {
        assert_eq!(format_connection(Some("direct")), "direct");
        assert_eq!(format_connection(None), "\u{2014}");
    }

    #[test]
    fn test_color_functions() {
        // Verify wrapping works and contains the original text.
        assert!(bold("test").contains("test"));
        assert!(dim("test").contains("test"));
        assert!(green("test").contains("test"));
        assert!(red("test").contains("test"));
        assert!(yellow("test").contains("test"));
        assert!(cyan("test").contains("test"));
        assert!(bold_red("test").contains("test"));
        assert!(bold_green("test").contains("test"));
    }

    #[test]
    fn test_color_disabled() {
        COLOR_ENABLED.store(false, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(bold("test"), "test");
        assert_eq!(dim("test"), "test");
        assert_eq!(red("test"), "test");
        assert_eq!(green("test"), "test");
        // Restore
        COLOR_ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    #[test]
    fn test_color_enabled_wraps() {
        COLOR_ENABLED.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(bold("test").contains("\x1b["));
        assert!(red("test").contains("\x1b[31m"));
        assert!(green("test").contains("\x1b[32m"));
    }
}
