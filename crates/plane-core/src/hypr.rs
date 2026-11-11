//! Hyprland: focusing windows. Optional: without hyprctl these do nothing.

use std::process::Command;

use serde_json::Value;

fn hypr(args: &[&str]) -> Option<(bool, String)> {
    let o = Command::new("hyprctl").args(args).output().ok()?;
    let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&o.stderr));
    Some((o.status.success(), text))
}

pub fn available() -> bool {
    hypr(&["version"]).is_some()
}

/// Hyprland 0.56+ takes Lua dispatches; older versions take the classic form.
fn dispatch(lua: &str, classic: &[&str]) {
    let Some((ok, out)) = hypr(&["dispatch", lua]) else { return };
    let out = out.to_lowercase();
    if !ok || out.contains("invalid") || out.contains("error") {
        let mut args = vec!["dispatch"];
        args.extend_from_slice(classic);
        hypr(&args);
    }
}

/// Focus a window: `class:^(plane-next)$`, `pid:1234`.
pub fn focus(selector: &str) {
    dispatch(&format!("hl.dsp.focus({{ window = \"{selector}\" }})"), &["focuswindow", selector]);
}

/// Move focus to the window beside this one: left, right, up or down.
pub fn move_focus(direction: &str) {
    let short = &direction[..1.min(direction.len())];
    dispatch(&format!("hl.dsp.focus({{ direction = \"{direction}\" }})"), &["movefocus", short]);
}

pub fn clients() -> Vec<Value> {
    hypr(&["clients", "-j"])
        .and_then(|(_, out)| serde_json::from_str::<Value>(&out).ok())
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
}

pub fn active_class() -> Option<String> {
    let (_, out) = hypr(&["activewindow", "-j"])?;
    serde_json::from_str::<Value>(&out).ok()?.get("class")?.as_str().map(str::to_owned)
}

/// The parent of a process, from /proc.
pub fn ppid_of(pid: i64) -> i64 {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|s| s.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().nth(1).and_then(|p| p.parse().ok())))
        .unwrap_or(0)
}

/// Focus the window a process runs in (a session in its own terminal window): the nearest
/// ancestor that Hyprland knows as a window. False when there's none.
pub fn focus_process_window(pid: i64) -> bool {
    let pids: Vec<i64> = clients().iter().filter_map(|c| c.get("pid").and_then(Value::as_i64)).collect();
    let mut p = pid;
    while p > 1 && !pids.contains(&p) {
        p = ppid_of(p);
    }
    if p > 1 {
        focus(&format!("pid:{p}"));
        return true;
    }
    false
}
