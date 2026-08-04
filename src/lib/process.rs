//! Liveness of another process, used by `--parent`.
//!
//! When a chart plotter such as OpenCPN starts mayara as a helper process it
//! cannot always clean up after itself: a crash or a kill leaves the child
//! running, holding the radar sockets and the web server port. Watching the
//! plotter's process id lets mayara exit on its own in that case.

use std::time::Duration;

/// How often [`wait_for_exit`] samples the process. Frequent enough that a
/// restarting plotter does not trip over the old web server port, cheap
/// enough to be irrelevant next to the radar traffic.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Returns true while the process is running. A process owned by another
/// user counts as running; one that has exited but not yet been reaped does
/// not.
pub fn is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Signal 0 does the existence and permission checks without
        // delivering anything. Pid 0 and anything beyond `pid_t` would be
        // read as "my process group" or a group id, so reject those outright
        // rather than have `kill` answer a different question than we asked.
        if pid == 0 || pid > i32::MAX as u32 {
            return false;
        }
        if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
            return true;
        }
        // EPERM: the process exists, it just isn't ours to signal.
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };

        unsafe {
            // A handle can outlive the process it refers to, so the exit code
            // decides; opening alone does not prove the process is running.
            let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
                return false;
            };
            let mut exit_code = 0u32;
            // STILL_ACTIVE is an NTSTATUS; exit codes are unsigned.
            let alive = GetExitCodeProcess(handle, &mut exit_code).is_ok()
                && exit_code == STILL_ACTIVE.0 as u32;
            let _ = CloseHandle(handle);
            alive
        }
    }
}

/// Resolves once the process is gone; returns immediately if it never was.
pub async fn wait_for_exit(pid: u32) {
    while is_alive(pid) {
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_process_is_alive() {
        assert!(is_alive(std::process::id()));
    }

    #[cfg(unix)]
    #[test]
    fn process_group_ids_are_not_processes() {
        assert!(!is_alive(0));
        assert!(!is_alive(i32::MAX as u32 + 1));
    }

    #[cfg(unix)]
    #[test]
    fn reaped_child_is_not_alive() {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();

        assert!(!is_alive(pid));
    }
}
