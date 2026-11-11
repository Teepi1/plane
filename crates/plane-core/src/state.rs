//! What Plane says about a session: working, needs you, your turn, paused or failed.

use serde_json::Value;

use crate::{pget, truthy};

pub const WORKING: &str = "working";
pub const NEEDS_YOU: &str = "needs you";
pub const YOUR_TURN: &str = "your turn";
pub const PAUSED: &str = "paused";
pub const FAILED: &str = "failed";

/// Port of Python `session_state`.
///
/// A background session reports "busy" (`claude agents`) for as long as any of its tasks
/// runs, a background shell or a CI watch included, even after its turn has ended with a
/// question for you. Its own status file (`procStatus`) tells "busy" (thinking) from "shell"
/// (only a task running), and its job file says whether the turn ended blocked on you.
pub fn session_state(s: &Value) -> &'static str {
    if truthy(pget(s, "stopped")) {
        return if pget(s, "state") == "failed" { FAILED } else { PAUSED };
    }
    let bg = truthy(pget(s, "bg"));
    if !bg || !truthy(pget(s, "procStatus")) {
        if pget(s, "status") == "busy" {
            return WORKING;
        }
        return if bg && pget(s, "state") == "blocked" { NEEDS_YOU } else { YOUR_TURN };
    }
    let (proc, job, tempo) = (pget(s, "procStatus"), pget(s, "jobState"), pget(s, "tempo"));
    if proc == "waiting" {
        return NEEDS_YOU; // a permission prompt
    }
    if proc == "busy" {
        return WORKING;
    }
    if tempo == "blocked" || (job == "blocked" && tempo != "active") {
        return NEEDS_YOU;
    }
    if proc == "shell" && job == "working" {
        return WORKING; // waiting on its own task; it carries on after
    }
    YOUR_TURN
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every branch of the Python function, plus the cases that once went wrong.
    #[test]
    fn state_table() {
        let cases = [
            (json!({"stopped": true}), PAUSED),
            (json!({"stopped": true, "state": "failed"}), FAILED),
            (json!({"stopped": true, "status": "busy"}), PAUSED),
            // not background, or no status file: `claude agents` decides
            (json!({"bg": false, "status": "busy"}), WORKING),
            (json!({"bg": false, "status": "idle", "state": "blocked"}), YOUR_TURN),
            (json!({"bg": true, "status": "busy"}), WORKING),
            (json!({"bg": true, "status": "idle", "state": "blocked"}), NEEDS_YOU),
            (json!({"bg": true, "procStatus": "", "state": "blocked"}), NEEDS_YOU),
            (json!({"bg": true, "status": "idle", "state": "done"}), YOUR_TURN),
            // background with a status file
            (json!({"bg": true, "procStatus": "waiting", "status": "idle"}), NEEDS_YOU),
            (json!({"bg": true, "procStatus": "busy"}), WORKING),
            (json!({"bg": true, "procStatus": "idle", "tempo": "blocked"}), NEEDS_YOU),
            (json!({"bg": true, "procStatus": "idle", "jobState": "blocked", "tempo": "idle"}), NEEDS_YOU),
            (json!({"bg": true, "procStatus": "idle", "jobState": "blocked"}), NEEDS_YOU),
            // job.state lags: "blocked" while tempo is still "active" mid-turn
            (json!({"bg": true, "procStatus": "idle", "jobState": "blocked", "tempo": "active"}), YOUR_TURN),
            (json!({"bg": true, "procStatus": "shell", "jobState": "working"}), WORKING),
            // the "your turn" bug: busy only because a background task runs, turn ended with a question
            (json!({"bg": true, "status": "busy", "procStatus": "shell", "jobState": "blocked", "tempo": "blocked"}), NEEDS_YOU),
            (json!({"bg": true, "status": "busy", "procStatus": "shell", "jobState": "done", "tempo": "idle"}), YOUR_TURN),
            (json!({"bg": true, "procStatus": "idle", "jobState": "working"}), YOUR_TURN),
        ];
        for (s, want) in cases {
            assert_eq!(session_state(&s), want, "for {s}");
        }
    }
}
