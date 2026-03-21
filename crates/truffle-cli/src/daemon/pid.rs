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
        if handle.is_null() {
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

    #[test]
    fn test_is_process_running_nonexistent() {
        // PID 999999 is extremely unlikely to be running in test environments.
        // On macOS/Linux, the max PID is typically 99999 or 4194304, so 999999
        // should not exist. On Windows, PIDs can go higher but this PID is
        // still very unlikely to be in use during tests.
        assert!(
            !is_process_running(999_999),
            "PID 999999 should not be running"
        );
    }

    #[test]
    fn test_pid_file_roundtrip() {
        // Write the current PID, read it back, verify they match.
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("roundtrip.pid");

        write_pid(&pid_path).unwrap();

        let read_back = read_pid(&pid_path).unwrap();
        assert_eq!(
            read_back,
            Some(std::process::id()),
            "Read-back PID should match current process"
        );

        // Also verify that the process identified by the PID file is running
        let pid = read_back.unwrap();
        assert!(
            is_process_running(pid),
            "PID from file should be a running process"
        );
    }

    #[test]
    fn test_pid_stale_detection_with_dead_pid() {
        // Write a PID that is definitely not running, then verify
        // check_stale_pid detects it as stale.
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("dead.pid");

        // Write a PID that cannot be running (very high value)
        fs::write(&pid_path, "999999").unwrap();

        let stale = check_stale_pid(&pid_path).unwrap();
        assert!(stale, "PID 999999 should be detected as stale");

        // Conversely, write the current process PID -- should NOT be stale
        write_pid(&pid_path).unwrap();
        let stale = check_stale_pid(&pid_path).unwrap();
        assert!(!stale, "Current process PID should not be detected as stale");
    }
}
