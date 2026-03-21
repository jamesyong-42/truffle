//! PID file management for the truffle daemon.
//!
//! The daemon writes its PID to `~/.config/truffle/truffle.pid` on startup
//! and removes it on graceful shutdown. The client checks this file to
//! determine if a daemon is running before attempting to connect.

use std::fs;
use std::io;
use std::path::Path;

/// Write the current process PID to the given file.
pub fn write_pid(path: &Path) -> io::Result<()> {
    let pid = std::process::id();
    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, pid.to_string())
}

/// Read the PID from the given file.
///
/// Returns `None` if the file does not exist or contains invalid data.
pub fn read_pid(path: &Path) -> io::Result<Option<u32>> {
    match fs::read_to_string(path) {
        Ok(contents) => match contents.trim().parse::<u32>() {
            Ok(pid) => Ok(Some(pid)),
            Err(_) => Ok(None),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Check if a process with the given PID is currently running.
///
/// - Unix: Uses `kill(pid, 0)` which checks existence without sending a signal.
/// - Windows: Uses `OpenProcess` with `PROCESS_QUERY_LIMITED_INFORMATION`.
#[cfg(unix)]
pub fn is_process_running(pid: u32) -> bool {
    // Safety: signal 0 does not kill the process, just checks if it exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(windows)]
pub fn is_process_running(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle == 0 {
            false
        } else {
            CloseHandle(handle);
            true
        }
    }
}

/// Remove the PID file at the given path.
///
/// Does not return an error if the file does not exist.
pub fn remove_pid(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Check if the PID file refers to a stale (no longer running) process.
///
/// Returns `true` if the PID file exists but the process is no longer running.
/// Returns `false` if the PID file does not exist, or the process is still running.
pub fn check_stale_pid(path: &Path) -> io::Result<bool> {
    match read_pid(path)? {
        Some(pid) => Ok(!is_process_running(pid)),
        None => Ok(false),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pid_file_write_read_remove() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("test.pid");

        // Write
        write_pid(&pid_path).unwrap();

        // Read
        let pid = read_pid(&pid_path).unwrap();
        assert_eq!(pid, Some(std::process::id()));

        // Remove
        remove_pid(&pid_path).unwrap();
        let pid = read_pid(&pid_path).unwrap();
        assert_eq!(pid, None);
    }

    #[test]
    fn test_read_pid_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("nonexistent.pid");
        let pid = read_pid(&pid_path).unwrap();
        assert_eq!(pid, None);
    }

    #[test]
    fn test_read_pid_invalid_content() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("bad.pid");
        fs::write(&pid_path, "not-a-number").unwrap();
        let pid = read_pid(&pid_path).unwrap();
        assert_eq!(pid, None);
    }

    #[test]
    fn test_remove_pid_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("nonexistent.pid");
        // Should not error
        remove_pid(&pid_path).unwrap();
    }

    #[test]
    fn test_stale_pid_detection() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("stale.pid");

        // Write a PID that definitely does not exist (use a very high PID)
        fs::write(&pid_path, "4000000000").unwrap();

        // Should detect as stale since PID 4000000000 won't be running
        let stale = check_stale_pid(&pid_path).unwrap();
        assert!(stale, "PID 4000000000 should be detected as stale");
    }

    #[test]
    fn test_stale_pid_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("no-file.pid");

        // No file means not stale (returns false)
        let stale = check_stale_pid(&pid_path).unwrap();
        assert!(!stale);
    }

    #[test]
    fn test_current_process_is_running() {
        let pid = std::process::id();
        assert!(is_process_running(pid));
    }
}
