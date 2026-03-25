//! PID file management for the truffle daemon.

use std::fs;
use std::io;
use std::path::Path;

pub fn write_pid(path: &Path) -> io::Result<()> {
    let pid = std::process::id();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, pid.to_string())
}

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

#[cfg(unix)]
pub fn is_process_running(pid: u32) -> bool {
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

pub fn remove_pid(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

pub fn check_stale_pid(path: &Path) -> io::Result<bool> {
    match read_pid(path)? {
        Some(pid) => Ok(!is_process_running(pid)),
        None => Ok(false),
    }
}
