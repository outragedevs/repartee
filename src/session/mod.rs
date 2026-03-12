pub mod protocol;
pub mod shim;
pub mod writer;

use std::path::PathBuf;

use crate::constants;

/// Socket path for a given PID: `~/.repartee/sessions/{pid}.sock`.
pub fn socket_path(pid: u32) -> PathBuf {
    constants::sessions_dir().join(format!("{pid}.sock"))
}

/// List all active sessions: `(pid, socket_path)` pairs for living PIDs.
pub fn list_sessions() -> Vec<(u32, PathBuf)> {
    let dir = constants::sessions_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sock") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(pid) = stem.parse::<u32>() else {
            continue;
        };
        if is_pid_alive(pid) {
            sessions.push((pid, path));
        }
    }
    sessions
}

/// Remove socket files for dead PIDs.
pub fn cleanup_stale_sockets() {
    let dir = constants::sessions_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sock") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(pid) = stem.parse::<u32>() else {
            // Not a PID-named socket, remove it
            let _ = std::fs::remove_file(&path);
            continue;
        };
        if !is_pid_alive(pid) {
            tracing::info!(
                "removing stale socket for dead PID {pid}: {}",
                path.display()
            );
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Check if a PID is alive using `kill(pid, 0)`.
///
/// # Safety
/// `libc::kill` with signal 0 is a POSIX-standard liveness check that sends
/// no signal — it only tests whether the process exists and is reachable.
pub fn is_pid_alive(pid: u32) -> bool {
    let Ok(pid_t) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 sends no signal, only checks process existence.
    unsafe { libc::kill(pid_t, 0) == 0 }
}
