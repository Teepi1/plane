//! The app's control socket: how `plane-next open` and `plane-next focus` (bound to
//! Super+arrows in Hyprland) reach a running app. One line per command: `raise`,
//! `focus left|right|up|down`. The app answers `ok` or `edge` (focus is already at that
//! side, so the caller moves on to the next window).

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

pub fn socket_path() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| {
        let uid = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| s.lines().find_map(|l| l.strip_prefix("Uid:").and_then(|r| r.split_whitespace().next().map(str::to_owned))))
            .unwrap_or_else(|| "0".into());
        PathBuf::from(format!("/run/user/{uid}"))
    });
    runtime.join("plane-next.sock")
}

/// Send one command to the running app. None when no app is listening.
pub fn send(cmd: &str) -> Option<String> {
    let mut s = UnixStream::connect(socket_path()).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    s.write_all(format!("{cmd}\n").as_bytes()).ok()?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).ok()?;
    Some(line.trim().to_owned())
}
