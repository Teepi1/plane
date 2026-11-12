//! `plane-next`: the Rust `plane` command, running beside the Python version until the switch.
//!
//!   plane-next snapshot [--agents FILE] [--now SECONDS] [--home DIR]
//!       What Plane would show right now, and the file writes it would have made (it makes
//!       none). --agents reads `claude agents --json --all` output from a file instead of
//!       running it, so the Python and Rust versions can be compared on the same input.
//!
//!   plane-next open
//!       Bring the app forward, or start it.
//!
//!   plane-next edit PATH[:LINE]
//!       Open a file in the app (at LINE), starting the app if it isn't running. The
//!       "Plane editor" desktop entry runs this, so files open in Plane from elsewhere.
//!
//!   plane-next focus left|right|up|down
//!       Super+arrows: move between the app's sidebar and session; past its edge, or with
//!       no app running, plain Hyprland focus.
//!
//!   plane-next action [--home DIR] [--now SECONDS] NAME ARGS...
//!       One user action, for `tools/actions_check.py` (these do change files):
//!         close JOB · pause JOB · requeue JOB
//!         claim FILE (started SID | not-started)
//!         discard FILE · undo FILE · close-lost SESSION_ID...
//!         recent [LIMIT] · complete TEXT

use std::path::Path;
use std::process::ExitCode;

use plane_core::actions::{self, ClaimError};
use plane_core::ledger::interrupted_sessions;
use plane_core::{control, hypr, pget, snapshot::snapshot, Env, Fs, Mode};
use serde_json::{json, Value};

fn usage() -> ExitCode {
    eprintln!("usage: plane-next snapshot [--agents FILE] [--now SECONDS] [--home DIR]");
    eprintln!("       plane-next open | edit PATH[:LINE] | focus left|right|up|down");
    eprintln!("       plane-next action [--home DIR] [--now SECONDS] NAME ARGS...");
    ExitCode::from(2)
}

fn claude_agents(env: &Env) -> String {
    let local = env.home.join(".local/bin/claude");
    let bin = if local.exists() { local.into_os_string() } else { "claude".into() };
    std::process::Command::new(bin)
        .args(["agents", "--json", "--all"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn now_secs() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

fn result(r: Result<Value, String>) -> Value {
    match r {
        Ok(v) => json!({ "ok": v }),
        Err(e) => json!({ "error": e }),
    }
}

fn action(env: &Env, now: f64, name: &str, rest: &[String]) -> Option<Value> {
    let mut fs = Fs::new(Mode::Real);
    let e = env.clone();
    let mut claude = move |args: &[&str], cwd: &Path| actions::run_claude(&e, args, cwd);
    let arg = |i: usize| rest.get(i).map(String::as_str);
    Some(match (name, arg(0)) {
        ("close", Some(job)) => result(actions::close_session(env, &mut fs, now, &mut claude, job).map(|_| Value::Null)),
        ("pause", Some(job)) => result(actions::pause_session(env, &mut claude, job).map(|_| Value::Null)),
        ("requeue", Some(job)) => result(actions::requeue(env, &mut fs, now, &mut claude, job).map(|_| Value::Null)),
        ("claim", Some(file)) => match actions::claim_handoff(env, &mut fs, now, Path::new(file)) {
            Err(ClaimError::AlreadyStarted) => json!({ "error": "already started" }),
            Err(ClaimError::FolderGone(cwd)) => json!({ "error": format!("its folder is gone: {cwd}") }),
            Ok((done, _)) => {
                match (arg(1), arg(2)) {
                    (Some("started"), Some(sid)) => actions::handoff_started(env, &mut fs, &done, sid),
                    (Some("not-started"), _) => actions::handoff_not_started(env, &mut fs, &done),
                    _ => {}
                }
                json!({ "ok": done.to_string_lossy() })
            }
        },
        ("discard", Some(file)) => json!({ "ok": actions::discard_handoff(env, &mut fs, Path::new(file)).map(|p| p.to_string_lossy().into_owned()) }),
        ("undo", Some(file)) => json!({ "ok": actions::undo_discard(env, &mut fs, Path::new(file)) }),
        ("close-lost", Some(_)) => {
            let lost = interrupted_sessions(env, &mut Fs::new(Mode::Record), now);
            let picked: Vec<Value> = lost.into_iter().filter(|r| rest.iter().any(|id| pget(r, "sessionId").as_str() == Some(id))).collect();
            actions::close_lost(env, &mut fs, &mut claude, &picked);
            json!({ "ok": picked.len() })
        }
        ("recent", limit) => json!({ "ok": actions::recent_projects(env, limit.and_then(|l| l.parse().ok()).unwrap_or(10)) }),
        ("complete", Some(text)) => json!({ "ok": actions::complete_dir(env, text) }),
        _ => return None,
    })
}

/// Starts the app (`plane-gui`, next to this program), detached.
fn start_app() -> bool {
    let gui = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("plane-gui")));
    let Some(gui) = gui.filter(|g| g.exists()) else {
        eprintln!("plane-next: plane-gui isn't built next to plane-next");
        return false;
    };
    let null = std::process::Stdio::null;
    std::process::Command::new("setsid").arg("-f").arg(gui).stdin(null()).stdout(null()).stderr(null()).status().is_ok()
}

/// `PATH` or `PATH:LINE` (a file whose name really ends in `:N` wins), made absolute.
fn edit_target(arg: &str) -> (std::path::PathBuf, u32) {
    let (file, line) = match arg.rsplit_once(':') {
        Some((f, l)) if !Path::new(arg).exists() && !f.is_empty() => match l.parse::<u32>() {
            Ok(n) => (f, n),
            Err(_) => (arg, 0),
        },
        _ => (arg, 0),
    };
    let p = Path::new(file);
    let abs = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.to_path_buf()) };
    (std::fs::canonicalize(&abs).unwrap_or(abs), line)
}

/// `plane-next edit PATH[:LINE]`: opens a file in the app, starting the app if it isn't
/// running (then it waits up to 5 s for it to answer).
fn edit(arg: &str) -> ExitCode {
    let (path, line) = edit_target(arg);
    let cmd = format!("edit {line} {}", path.display());
    if control::send(&cmd).is_some() {
        return ExitCode::SUCCESS;
    }
    if !start_app() {
        return ExitCode::FAILURE;
    }
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(150));
        if control::send(&cmd).is_some() {
            return ExitCode::SUCCESS;
        }
    }
    eprintln!("plane-next: the app didn't answer within 5 s");
    ExitCode::FAILURE
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().map(String::as_str) else { return usage() };
    let (mut agents_file, mut now, mut home, mut positional) = (None, None, None, vec![]);
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--agents" if positional.is_empty() => agents_file = it.next().cloned(),
            "--now" if positional.is_empty() => now = it.next().and_then(|n| n.parse::<f64>().ok()),
            "--home" if positional.is_empty() => home = it.next().cloned(),
            _ => positional.push(a.clone()),
        }
    }
    let env = home.map(Env::new).unwrap_or_else(Env::from_env);
    let now = now.unwrap_or_else(now_secs);
    match cmd {
        "snapshot" if positional.is_empty() => {
            let agents = match agents_file {
                Some(f) => match std::fs::read_to_string(&f) {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("plane-next: can't read {f}: {e}");
                        return ExitCode::FAILURE;
                    }
                },
                None => claude_agents(&env),
            };
            let mut fs = Fs::new(Mode::Record); // read-only: writes are recorded, never made
            let snap = snapshot(&env, &mut fs, now, &agents);
            println!("{}", serde_json::to_string_pretty(&snap).unwrap_or_default());
            ExitCode::SUCCESS
        }
        "action" if !positional.is_empty() => match action(&env, now, &positional[0], &positional[1..]) {
            Some(out) => {
                println!("{out}");
                ExitCode::SUCCESS
            }
            None => usage(),
        },
        "open" => {
            if control::send("raise").is_none() && !start_app() {
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        "edit" if positional.len() == 1 => edit(&positional[0]),
        "focus" if matches!(positional.first().map(String::as_str), Some("left" | "right" | "up" | "down")) => {
            let dir = positional[0].as_str();
            if control::send(&format!("focus {dir}")).as_deref() != Some("ok") {
                hypr::move_focus(dir);
            }
            ExitCode::SUCCESS
        }
        _ => usage(),
    }
}
