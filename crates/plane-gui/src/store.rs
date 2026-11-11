//! Polls sessions, handoffs and usage on a background thread, like the Python `Store`.
//!
//! Read-only while the kitty version runs: the refresh's own ledger writes (recording old
//! sessions, marking removed ones ended) are recorded and dropped, never made, and nothing
//! is notified. `claude agents` costs about half a second of CPU, so it isn't run on a
//! timer: the thread watches the files sessions write when their state changes and refreshes
//! when one does, at most once a second, with a full refresh every 10 s as a backstop.

use std::sync::{Arc, Condvar, Mutex};
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
}

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
        *self.shared.poked.lock().unwrap() = true;
        self.shared.wake.notify_all();
    }

    /// The newest refresh, once.
    pub fn take(&self) -> Option<Snap> {
        self.shared.latest.lock().unwrap().take()
    }
}

/// Changes whenever a session's status, a job's state, the ledger, the handoff queue or the
/// usage numbers change: the files and their modification times.
fn changes_signature(env: &Env) -> Vec<(String, std::time::SystemTime)> {
    let mut files = glob_json(&env.sessions());
    if let Ok(jobs) = std::fs::read_dir(env.jobs()) {
        files.extend(jobs.filter_map(Result::ok).map(|e| e.path().join("state.json")));
    }
    files.extend(glob_json(&env.live()));
    files.extend(glob_json(&env.handoffs()));
    files.push(env.usage());
    let mut sig: Vec<_> = files
        .into_iter()
        .filter_map(|f| f.metadata().and_then(|m| m.modified()).ok().map(|t| (f.to_string_lossy().into_owned(), t)))
        .collect();
    sig.sort();
    sig
}

fn one(env: &Env) -> Snap {
    let out = std::process::Command::new(claude_bin(env)).args(["agents", "--json", "--all"]).output();
    let (stdout, error) = match out {
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
    };
    let mut fs = Fs::new(Mode::Record); // never written while the kitty version runs
    match refresh(env, &mut fs, crate::text::now(), &stdout) {
        Ok(r) => Snap { sessions: r.sessions, handoffs: r.handoffs, lost: r.lost, usage: r.usage, error, loaded: true },
        Err(e) => Snap { error: format!("refresh: {e}"), loaded: false, ..Snap::default() },
    }
}

fn run(env: Env, shared: Arc<Shared>) {
    loop {
        let started = Instant::now();
        let snap = one(&env);
        *shared.latest.lock().unwrap() = Some(snap);
        let seen = changes_signature(&env);
        let mut poked = shared.poked.lock().unwrap();
        loop {
            if *poked {
                break;
            }
            let (guard, _) = shared.wake.wait_timeout(poked, Duration::from_millis(300)).unwrap();
            poked = guard;
            if *poked || started.elapsed() > Duration::from_secs(10) || changes_signature(&env) != seen {
                break;
            }
        }
        *poked = false;
        drop(poked);
        if let Some(rest) = Duration::from_secs(1).checked_sub(started.elapsed()) {
            std::thread::sleep(rest); // at most one refresh a second
        }
    }
}
