//! Polls sessions, handoffs and usage on a background thread, like the Python `Store`.
//!
//! Read-only while the kitty version runs: the refresh's own ledger writes (recording old
//! sessions, marking removed ones ended) are recorded and dropped, never made, and nothing
//! is notified. `claude agents` costs about half a second of CPU, so it isn't run on a
//! timer: the thread watches the files sessions write when their state changes and refreshes
//! when one does, at most once a second, with a full refresh every 10 s as a backstop (every
//! 2 min while the window is unfocused or hidden, when session changes also wait 10 s;
//! focusing it again refreshes at once).
//! Handoffs, folders and usage don't come from `claude agents`, so a change to only those
//! files refreshes from disk with the last `claude agents` output. The statusline rewrites
//! usage.json on every redraw, which alone used to cost ~10 calls a minute.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use plane_core::actions::claude_bin;
use plane_core::snapshot::refresh;
use plane_core::{glob_json, Env, Fs, Mode};
use serde_json::Value;

#[derive(Clone, Default)]
pub struct Snap {
    pub sessions: Vec<Value>,
    pub handoffs: Vec<Value>,
    pub lost: Vec<Value>,
    pub usage: Value,
    pub error: String,
    pub loaded: bool,
}

#[derive(Default)]
struct Shared {
    latest: Mutex<Option<Snap>>,
    poked: Mutex<bool>,
    wake: Condvar,
    /// The window is hidden or unfocused: nobody is looking, so the backstop slows down.
    away: AtomicBool,
}

/// A pass that panicked must not take the thread with it: ignore poisoning.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

const MIN_GAP: Duration = Duration::from_secs(1);
const BACKSTOP: Duration = Duration::from_secs(10);
const BACKSTOP_AWAY: Duration = Duration::from_secs(120);

#[derive(Clone)]
pub struct Store {
    shared: Arc<Shared>,
}

impl Store {
    pub fn start(env: Env) -> Store {
        let shared = Arc::new(Shared::default());
        let s = shared.clone();
        std::thread::spawn(move || run(env, s));
        Store { shared }
    }

    /// Refresh now, not at the next change (after an action).
    pub fn poke(&self) {
        *lock(&self.shared.poked) = true;
        self.shared.wake.notify_all();
    }

    /// Whether anyone can see the window. Cheap to call often; coming back refreshes at once.
    pub fn set_seen(&self, seen: bool) {
        let was_away = self.shared.away.swap(!seen, Ordering::Relaxed);
        if was_away && seen {
            self.poke();
        }
    }

    /// The newest refresh, once.
    pub fn take(&self) -> Option<Snap> {
        lock(&self.shared.latest).take()
    }
}

/// The files and their modification times, so a change to any of them is seen.
fn signature(files: Vec<std::path::PathBuf>) -> Vec<(String, std::time::SystemTime)> {
    let mut sig: Vec<_> = files
        .into_iter()
        .filter_map(|f| f.metadata().and_then(|m| m.modified()).ok().map(|t| (f.to_string_lossy().into_owned(), t)))
        .collect();
    sig.sort();
    sig
}

/// Changes whenever a session's status, a job's state or the ledger changes: what
/// `claude agents` has to be asked about again.
fn agents_signature(env: &Env) -> Vec<(String, std::time::SystemTime)> {
    let mut files = glob_json(&env.sessions());
    if let Ok(jobs) = std::fs::read_dir(env.jobs()) {
        files.extend(jobs.filter_map(Result::ok).map(|e| e.path().join("state.json")));
    }
    files.extend(glob_json(&env.live()));
    signature(files)
}

/// Changes whenever the handoff queue, the usage numbers or the folders change: read from
/// disk alone, no `claude agents` needed.
fn disk_signature(env: &Env) -> Vec<(String, std::time::SystemTime)> {
    let mut files = glob_json(&env.handoffs());
    files.push(env.usage());
    files.push(env.folders_file()); // a folder renamed in the kitty sidebar
    signature(files)
}

/// What a wait ended with.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Wake {
    /// Ask `claude agents` again.
    Full,
    /// Only handoffs, usage or folders changed: refresh from disk with the last output.
    Disk,
}

/// Decide after one look: `None` keeps waiting.
fn decide(poked: bool, waited: Duration, away: bool, agents_changed: bool, disk_changed: bool) -> Option<Wake> {
    let backstop = if away { BACKSTOP_AWAY } else { BACKSTOP };
    // Unseen, a session change waits until 10 s after the last ask: nobody is looking, and
    // the change still differs from what was seen, so it isn't lost.
    let agents_due = agents_changed && (!away || waited >= BACKSTOP);
    if poked || agents_due || waited >= backstop {
        Some(Wake::Full)
    } else if disk_changed {
        Some(Wake::Disk)
    } else {
        None
    }
}

/// Run `claude agents`: its output, and what went wrong if anything did.
fn ask_agents(env: &Env) -> (String, String) {
    let out = std::process::Command::new(claude_bin(env)).args(["agents", "--json", "--all"]).output();
    match out {
        Ok(o) => {
            let err = if o.status.success() {
                String::new()
            } else {
                let e = String::from_utf8_lossy(&o.stderr).trim().chars().take(80).collect::<String>();
                if e.is_empty() { "claude agents failed".into() } else { e }
            };
            (String::from_utf8_lossy(&o.stdout).into_owned(), err)
        }
        Err(e) => (String::new(), format!("can't run claude: {e}")),
    }
}

fn one(env: &Env, stdout: &str, error: String) -> Snap {
    let mut fs = Fs::new(Mode::Record); // never written while the kitty version runs
    match refresh(env, &mut fs, crate::text::now(), stdout) {
        Ok(r) => Snap { sessions: r.sessions, handoffs: r.handoffs, lost: r.lost, usage: r.usage, error, loaded: true },
        Err(e) => Snap { error: format!("refresh: {e}"), loaded: false, ..Snap::default() },
    }
}

fn run(env: Env, shared: Arc<Shared>) {
    let mut last = (String::new(), String::new()); // the last `claude agents` output and error
    let mut wake = Wake::Full;
    let mut full_at = Instant::now();
    loop {
        let started = Instant::now();
        // One bad pass (a panic in a refresh) must not end the thread: the app would keep
        // showing the last list and look healthy.
        let pass = catch_unwind(AssertUnwindSafe(|| {
            if wake == Wake::Full {
                last = ask_agents(&env);
            }
            one(&env, &last.0, last.1.clone())
        }));
        let snap = pass.unwrap_or_else(|_| Snap { error: "refresh failed".into(), ..Snap::default() });
        if wake == Wake::Full {
            full_at = started;
        }
        *lock(&shared.latest) = Some(snap);
        let seen_agents = agents_signature(&env);
        let seen_disk = disk_signature(&env);
        let mut poked = lock(&shared.poked);
        wake = loop {
            let away = shared.away.load(Ordering::Relaxed);
            let d = if *poked {
                decide(true, full_at.elapsed(), away, false, false)
            } else {
                let (guard, _) = shared.wake.wait_timeout(poked, Duration::from_millis(300)).unwrap_or_else(|e| e.into_inner());
                poked = guard;
                let away = shared.away.load(Ordering::Relaxed);
                decide(*poked, full_at.elapsed(), away, agents_signature(&env) != seen_agents, disk_signature(&env) != seen_disk)
            };
            if let Some(d) = d {
                break d;
            }
        };
        *poked = false;
        drop(poked);
        if let Some(rest) = MIN_GAP.checked_sub(started.elapsed()) {
            std::thread::sleep(rest); // at most one refresh a second
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: fn(u64) -> Duration = Duration::from_secs;

    #[test]
    fn disk_only_changes_skip_claude_agents() {
        assert_eq!(decide(false, S(2), false, false, true), Some(Wake::Disk));
        assert_eq!(decide(false, S(2), true, false, true), Some(Wake::Disk));
    }

    #[test]
    fn session_changes_and_pokes_ask_again() {
        assert_eq!(decide(false, S(2), false, true, false), Some(Wake::Full));
        assert_eq!(decide(false, S(2), false, true, true), Some(Wake::Full));
        assert_eq!(decide(true, S(0), true, false, false), Some(Wake::Full));
    }

    #[test]
    fn unseen_session_changes_wait_for_ten_seconds() {
        assert_eq!(decide(false, S(3), true, true, false), None);
        assert_eq!(decide(false, S(3), true, true, true), Some(Wake::Disk));
        assert_eq!(decide(false, S(10), true, true, false), Some(Wake::Full));
    }

    #[test]
    fn backstop_is_slower_while_nobody_looks() {
        assert_eq!(decide(false, S(9), false, false, false), None);
        assert_eq!(decide(false, S(10), false, false, false), Some(Wake::Full));
        assert_eq!(decide(false, S(60), true, false, false), None);
        assert_eq!(decide(false, S(120), true, false, false), Some(Wake::Full));
    }

    #[test]
    fn coming_back_pokes() {
        let store = Store { shared: Arc::new(Shared::default()) };
        store.set_seen(false);
        assert!(!*lock(&store.shared.poked));
        store.set_seen(true);
        assert!(*lock(&store.shared.poked));
        *lock(&store.shared.poked) = false;
        store.set_seen(true); // already seen: nothing new
        assert!(!*lock(&store.shared.poked));
    }

    #[test]
    fn a_poisoned_lock_is_still_usable() {
        let m = Arc::new(Mutex::new(1));
        let m2 = m.clone();
        let _ = std::thread::spawn(move || {
            let _g = m2.lock().unwrap();
            panic!("bad pass");
        })
        .join();
        assert_eq!(*lock(&m), 1);
    }
}
