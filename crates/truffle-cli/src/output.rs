//! Output formatting helpers for the truffle CLI v2.
//!
//! Provides consistent, beautiful terminal output: Unicode box-drawing,
//! color indicators, aligned tables, structured error messages, and
//! progress bars.

use std::fmt;
use std::fmt::Write;
use std::io::{self, Write as _};

// ==========================================================================
// Color mode
// ==========================================================================

/// Whether color output is enabled (set once at startup).
static COLOR_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Initialize color output based on mode. Call once at startup.
pub fn init_color(mode: &str) {
    let enabled = match mode {
        "never" => false,
        "always" => true,
        _ => is_tty() && std::env::var("NO_COLOR").is_err(),
    };
    COLOR_ENABLED.store(enabled, std::sync::atomic::Ordering::Relaxed);
}

fn is_tty() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::isatty(libc::STDOUT_FILENO) != 0 }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE,
        };
        unsafe {
            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut mode = 0;
            GetConsoleMode(handle, &mut mode) != 0
        }
    }
}

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

// ==========================================================================
// ANSI color helpers
// ==========================================================================

pub fn bold(text: &str) -> String {
    apply_ansi(text, "1")
}

pub fn dim(text: &str) -> String {
    apply_ansi(text, "2")
}

pub fn green(text: &str) -> String {
    apply_ansi(text, "32")
}

pub fn red(text: &str) -> String {
    apply_ansi(text, "31")
}

pub fn yellow(text: &str) -> String {
    apply_ansi(text, "33")
}

pub fn cyan(text: &str) -> String {
    apply_ansi(text, "36")
}

// ==========================================================================
// Status indicators
// ==========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Indicator {
    Pass,
    Fail,
    Warn,
    Skip,
    Online,
    Offline,
}

impl Indicator {
    pub fn symbol(self) -> String {
        match self {
            Indicator::Pass => green("\u{2713}"),
            Indicator::Fail => red("\u{2717}"),
            Indicator::Warn => yellow("\u{26a0}"),
            Indicator::Skip => dim("\u{2014}"),
            Indicator::Online => green("\u{25cf}"),
            Indicator::Offline => dim("\u{25cb}"),
        }
    }
}

impl fmt::Display for Indicator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.symbol())
    }
}

pub fn status_indicator(status: &str) -> String {
    match status.to_lowercase().as_str() {
        "online" => green("\u{25cf}"),
        "connecting" => yellow("\u{25cf}"),
        _ => dim("\u{25cb}"),
    }
}

pub fn status_label(status: &str) -> String {
    match status.to_lowercase().as_str() {
        "online" => green("online"),
        "connecting" => yellow("connecting"),
        "offline" => dim("offline"),
        other => dim(other),
    }
}

// ==========================================================================
// Formatting helpers
// ==========================================================================

pub fn format_latency(ms: Option<f64>) -> String {
    match ms {
        Some(v) if v < 1.0 => format!("{v:.2}ms"),
        Some(v) if v < 10.0 => format!("{v:.1}ms"),
        Some(v) => format!("{v:.0}ms"),
        None => "\u{2014}".to_string(),
    }
}

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

pub fn format_connection(connection: Option<&str>) -> String {
    match connection {
        Some("direct") => "direct".to_string(),
        Some(relay) if relay.starts_with("via relay:") || relay.starts_with("relayed") => {
            yellow(relay)
        }
        Some(other) => other.to_string(),
        None => "\u{2014}".to_string(),
    }
}

// ==========================================================================
// Table printer
// ==========================================================================

pub fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    if headers.is_empty() {
        return;
    }

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

    let padding = 2;

    let mut header_line = String::new();
    for (i, h) in headers.iter().enumerate() {
        if i > 0 {
            header_line.push_str(&" ".repeat(padding));
        }
        let _ = write!(header_line, "{:<width$}", h, width = widths[i]);
    }
    println!("  {}", dim(&header_line));

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

// ==========================================================================
// Status / KV output
// ==========================================================================

pub fn print_status(label: &str, value: &str, indicator: Option<Indicator>) {
    let label_formatted = format!("{:<12}", label);
    match indicator {
        Some(ind) => println!("  {} {} {}", dim(&label_formatted), ind, value),
        None => println!("  {}{}", dim(&label_formatted), value),
    }
}

// ==========================================================================
// Section drawing
// ==========================================================================

pub fn print_section(title: &str) {
    println!();
    println!("  {}", bold(title));
    println!("  {}", dim(&"\u{2500}".repeat(title.len().max(4))));
}

pub fn print_title(left: &str, right: &str) {
    let title = format!("{} \u{00b7} {}", left, right);
    println!();
    println!("  {}", bold(&title));
    println!("  {}", dim(&"\u{2500}".repeat(39)));
}

// ==========================================================================
// Progress bar
// ==========================================================================

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
        cyan(&"\u{2588}".repeat(filled)),
        dim(&"\u{2591}".repeat(empty)),
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

    let padded = format!("{:<80}", line);
    print!("\r{padded}");
    let _ = io::stdout().flush();
}

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

// ==========================================================================
// Diagnostic output
// ==========================================================================

pub fn print_check(indicator: Indicator, label: &str, detail: &str) {
    if detail.is_empty() {
        println!("  {} {}", indicator.symbol(), label);
    } else {
        let label_padded = format!("{:<32}", label);
        println!("  {} {}{}", indicator.symbol(), label_padded, dim(detail));
    }
}

pub fn print_fix_suggestion(suggestion: &str) {
    println!("    {} {}", dim("\u{2192}"), suggestion);
}

// ==========================================================================
// Error / Success / Warning messages
// ==========================================================================

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

pub fn print_success(message: &str) {
    println!("  {} {}", green("\u{2713}"), message);
}

pub fn print_warning(message: &str) {
    println!("  {} {}", yellow("\u{26a0}"), message);
}

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

// ==========================================================================
// JSON output
// ==========================================================================

pub fn print_json(value: &serde_json::Value) {
    if let Ok(json) = serde_json::to_string_pretty(value) {
        println!("{json}");
    }
}

// ==========================================================================
// Internal helpers
// ==========================================================================

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
