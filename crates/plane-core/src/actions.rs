//! What you can do to a session or a handoff: close, pause, start, put back, discard, undo.
//!
//! Ports of the Python sidebar's actions, without the prompts and messages around them (the
//! app asks and reports). They make the same file changes and the same `claude` calls, which
//! `tools/actions_check.py` checks on made-up homes. Two deliberate differences, both bugs in
//! the Python version:
//! - [`claim_handoff`] checks the handoff's folder before claiming it. The Python sidebar
//!   claimed first and then failed to start in a missing folder, which stranded the handoff
//!   in `done/` (the phone app already checks).
//! - [`recent_projects`] skips `.claude/worktrees` folders and Claude's own folders, as the
//!   phone app does, not only `.worktrees`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::ledger::ledger_end;
use crate::{glob_json, pget, py_str, read_json, truthy, Env, Fs};

impl Env {
    pub fn done(&self) -> PathBuf { self.handoffs().join("done") }
    pub fn discarded(&self) -> PathBuf { self.handoffs().join("discarded") }
}

/// How long a discarded handoff is kept before it's deleted.
pub const DISCARD_KEEP_DAYS: u64 = 14;

/// `shutil.which("claude")`, else `~/.local/bin/claude`.
pub fn claude_bin(env: &Env) -> PathBuf {
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join("claude");
            if is_executable(&p) {
                return p;
            }
        }
    }
    env.home.join(".local/bin/claude")
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Runs `claude <args>` in `cwd`: (exit code, stdout and stderr joined and trimmed).
pub fn run_claude(env: &Env, args: &[&str], cwd: &Path) -> (i32, String) {
    match Command::new(claude_bin(env)).args(args).current_dir(cwd).output() {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.code().unwrap_or(-1), text.trim().to_owned())
        }
        Err(e) => (-1, e.to_string()),
    }
}

/// The `claude` caller, swappable so a test can stand in for it.
pub type Claude<'a> = dyn FnMut(&[&str], &Path) -> (i32, String) + 'a;

/// Close a background session: `claude rm`, then mark its ledger records ended (the hook
/// doesn't hear about `claude rm`). The conversation is kept. Err is the message to show.
pub fn close_session(env: &Env, fs: &mut Fs, now: f64, claude: &mut Claude, job_id: &str) -> Result<(), String> {
    let (code, out) = claude(&["rm", job_id], &env.home);
    if code != 0 {
        let first = out.lines().next().unwrap_or("").chars().take(100).collect::<String>();
        return Err(format!("not closed: {}", if out.is_empty() { "claude rm failed".into() } else { first }));
    }
    ledger_end(env, fs, now, job_id, "closed");
    Ok(())
}

/// Pause a background session: `claude stop`. It stays listed under paused.
pub fn pause_session(env: &Env, claude: &mut Claude, job_id: &str) -> Result<(), String> {
    let (code, out) = claude(&["stop", job_id], &env.home);
    if code != 0 {
        return Err(format!("not paused: {}", out.chars().take(100).collect::<String>()));
    }
    Ok(())
}

/// The folder a handoff starts in, as `plane start` reads it.
pub fn handoff_cwd(env: &Env, h: &Value) -> String {
    let cwd = pget(h, "cwd");
    let cwd = if truthy(cwd) { py_str(cwd) } else { env.home.to_string_lossy().into_owned() };
    expand_user(env, &cwd)
}

/// `os.path.expanduser` for `~` and `~/...`.
pub fn expand_user(env: &Env, p: &str) -> String {
    if p == "~" {
        env.home.to_string_lossy().into_owned()
    } else if let Some(rest) = p.strip_prefix("~/") {
        env.home.join(rest).to_string_lossy().into_owned()
    } else {
        p.to_owned()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ClaimError {
    /// Someone (a second Enter, the phone) started it first.
    AlreadyStarted,
    /// Its folder doesn't exist; nothing was moved.
    FolderGone(String),
}

/// Claim a queued handoff before starting it: move it into `done/` (atomic, so it can't be
/// started twice) and stamp `launchedAt`. Returns the claimed file and the handoff.
pub fn claim_handoff(env: &Env, fs: &mut Fs, now: f64, path: &Path) -> Result<(PathBuf, Value), ClaimError> {
    let Some(h) = read_json(path).filter(Value::is_object) else { return Err(ClaimError::AlreadyStarted) };
    let cwd = handoff_cwd(env, &h);
    if !Path::new(&cwd).is_dir() {
        return Err(ClaimError::FolderGone(cwd));
    }
    let done = env.done().join(path.file_name().unwrap_or_default());
    if fs.rename(env, path, &done).is_err() {
        return Err(ClaimError::AlreadyStarted);
    }
    let mut h = h;
    h["launchedAt"] = now.into();
    fs.write_json(env, &done, &h);
    Ok((done, h))
}

/// The started session's id goes into the claimed handoff, for "Put back in queue".
pub fn handoff_started(env: &Env, fs: &mut Fs, done: &Path, session_id: &str) {
    let Some(mut h) = read_json(done).filter(Value::is_object) else { return };
    h["launchedSessionId"] = session_id.into();
    fs.write_json(env, done, &h);
}

/// No session came of it: back into the queue so it can be started again.
pub fn handoff_not_started(env: &Env, fs: &mut Fs, done: &Path) {
    let Some(mut h) = read_json(done).filter(Value::is_object) else { return };
    if let Some(m) = h.as_object_mut() {
        m.remove("launchedAt");
    }
    fs.write_json(env, &env.handoffs().join(done.file_name().unwrap_or_default()), &h);
    fs.unlink(env, done);
}

/// The started handoff (in `done/`) that a session came from, if any.
pub fn handoff_of(env: &Env, job_id: &str) -> Option<PathBuf> {
    glob_json(&env.done()).into_iter().find(|f| {
        read_json(f).is_some_and(|h| truthy(&h) && pget(&h, "launchedSessionId").as_str() == Some(job_id))
    })
}

/// Undo starting a handoff: close its session, and only once that worked (a session still
/// running could otherwise be started twice) put the handoff back in the queue.
pub fn requeue(env: &Env, fs: &mut Fs, now: f64, claude: &mut Claude, job_id: &str) -> Result<(), String> {
    let Some(f) = handoff_of(env, job_id) else { return Err("no handoff to put back".into()) };
    close_session(env, fs, now, claude, job_id)?;
    let mut h = read_json(&f).unwrap_or(Value::Null);
    if let Some(m) = h.as_object_mut() {
        m.remove("launchedAt");
        m.remove("launchedSessionId");
    }
    fs.write_json(env, &env.handoffs().join(f.file_name().unwrap_or_default()), &h);
    fs.unlink(env, &f);
    Ok(())
}

/// Discard a queued handoff: it waits in `discarded/` for [`DISCARD_KEEP_DAYS`], counted
/// from now. Returns where it went, for undo; None if it was already gone.
pub fn discard_handoff(env: &Env, fs: &mut Fs, path: &Path) -> Option<PathBuf> {
    let dest = env.discarded().join(path.file_name()?);
    fs.rename(env, path, &dest).ok()?;
    fs.touch(env, &dest);
    Some(dest)
}

/// Bring the last discarded handoff back into the queue. False if it's gone.
pub fn undo_discard(env: &Env, fs: &mut Fs, discarded: &Path) -> bool {
    let Some(name) = discarded.file_name() else { return false };
    discarded.exists() && fs.rename(env, discarded, &env.handoffs().join(name)).is_ok()
}

/// Done with cut-off or standby sessions: stop offering them. A background job that's still
/// there is removed too, or it would come back under paused. The conversations are kept.
pub fn close_lost(env: &Env, fs: &mut Fs, claude: &mut Claude, records: &[Value]) {
    for r in records {
        let job = pget(r, "jobId");
        if pget(r, "kind") == "bg" && truthy(job) && env.jobs().join(py_str(job)).is_dir() {
            claude(&["rm", &py_str(job)], &env.home);
        }
        if let Some(p) = r.get("path").and_then(Value::as_str) {
            fs.unlink(env, Path::new(p));
        }
    }
}

/// Folders you've worked in lately, newest first, from Claude's prompt history.
pub fn recent_projects(env: &Env, limit: usize) -> Vec<String> {
    let mut seen: Vec<String> = vec![];
    let Ok(text) = std::fs::read(env.claude.join("history.jsonl")) else { return seen };
    let text = String::from_utf8_lossy(&text);
    let home = env.home.to_string_lossy();
    let own = format!("{}/", env.claude.to_string_lossy());
    for line in text.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let Some(p) = v.get("project").and_then(Value::as_str) else { continue };
        if p.is_empty() || seen.iter().any(|s| s == p) || p == home || p.contains("/.worktrees/")
            || p.contains("/.claude/worktrees/") || p.starts_with(&own)
        {
            continue;
        }
        if Path::new(p).is_dir() {
            seen.push(p.to_owned());
        }
        if seen.len() >= limit {
            break;
        }
    }
    seen
}

/// Tab completion for a typed folder: extend it to the longest prefix the matching folders
/// share, with a `/` once only one matches. Hidden folders only when you typed the dot.
pub fn complete_dir(env: &Env, text: &str) -> String {
    let base = expand_user(env, if text.is_empty() { "~/" } else { text });
    // os.path.split: "/x" splits into "/" and "x"
    let (parent, stem) = match base.rfind('/') {
        Some(0) => ("/", &base[1..]),
        Some(i) => (&base[..i], &base[i + 1..]),
        None => ("", base.as_str()),
    };
    let Ok(entries) = std::fs::read_dir(if parent.is_empty() { "." } else { parent }) else { return text.to_owned() };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with(stem) && (stem.starts_with('.') || !n.starts_with('.')))
        .collect();
    if names.is_empty() {
        return text.to_owned();
    }
    names.sort();
    let mut common: Vec<char> = names[0].chars().collect();
    for n in &names[1..] {
        let len = common.iter().zip(n.chars()).take_while(|(a, b)| **a == *b).count();
        common.truncate(len);
    }
    let common: String = common.into_iter().collect();
    let joined = if parent.is_empty() { common } else if parent.ends_with('/') { format!("{parent}{common}") } else { format!("{parent}/{common}") };
    let done = if names.len() == 1 { format!("{joined}/") } else { joined };
    if text.starts_with('~') || text.is_empty() {
        done.replacen(&*env.home.to_string_lossy(), "~", 1)
    } else {
        done
    }
}

/// Delete discarded handoffs older than [`DISCARD_KEEP_DAYS`]. Not called by the app while
/// the kitty version runs, since that one already does it.
pub fn purge_discarded(env: &Env, fs: &mut Fs) {
    let cutoff = std::time::SystemTime::now() - std::time::Duration::from_secs(DISCARD_KEEP_DAYS * 86400);
    for f in glob_json(&env.discarded()) {
        if f.metadata().and_then(|m| m.modified()).is_ok_and(|t| t < cutoff) {
            fs.unlink(env, &f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mode;
    use serde_json::json;

    fn home(tag: &str) -> Env {
        let root = std::env::temp_dir().join(format!("plane-actions-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".local/share/plane/handoffs/done")).unwrap();
        Env::new(root)
    }

    #[test]
    fn claim_checks_the_folder_first() {
        let env = home("claim");
        let f = env.handoffs().join("a.json");
        crate::write_json_atomic(&f, &json!({"id": "a", "cwd": "/nonexistent/folder"})).unwrap();
        let mut fs = Fs::new(Mode::Real);
        assert_eq!(claim_handoff(&env, &mut fs, 1.0, &f).unwrap_err(), ClaimError::FolderGone("/nonexistent/folder".into()));
        assert!(f.exists(), "a handoff with a missing folder stays queued");
        crate::write_json_atomic(&f, &json!({"id": "a", "cwd": "~"})).unwrap();
        let (done, h) = claim_handoff(&env, &mut fs, 5.0, &f).unwrap();
        assert!(!f.exists() && done.exists());
        assert_eq!(h["launchedAt"], json!(5.0));
        assert_eq!(claim_handoff(&env, &mut fs, 6.0, &f).unwrap_err(), ClaimError::AlreadyStarted);
        handoff_not_started(&env, &mut fs, &done);
        let back = read_json(&f).unwrap();
        assert!(back.get("launchedAt").is_none() && !done.exists());
    }

    #[test]
    fn completes_folders() {
        let env = home("complete");
        for d in ["proj-one", "proj-two", ".hidden", "solo"] {
            std::fs::create_dir_all(env.home.join(d)).unwrap();
        }
        assert_eq!(complete_dir(&env, "~/pro"), "~/proj-");
        assert_eq!(complete_dir(&env, "~/so"), "~/solo/");
        assert_eq!(complete_dir(&env, "~/.hi"), "~/.hidden/");
        assert_eq!(complete_dir(&env, "~/zz"), "~/zz");
        let abs = format!("{}/so", env.home.display());
        assert_eq!(complete_dir(&env, &abs), format!("{}/solo/", env.home.display()));
    }
}
