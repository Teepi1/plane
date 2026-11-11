//! `plane-next`: the Rust `plane` command, running beside the Python version until the switch.
//!
//! Only read-only commands exist so far:
//!
//!   plane-next snapshot [--agents FILE] [--now SECONDS] [--home DIR]
//!       What Plane would show right now, and the file writes it would have made (it makes
//!       none). --agents reads `claude agents --json --all` output from a file instead of
//!       running it, so the Python and Rust versions can be compared on the same input.

use std::process::ExitCode;

use plane_core::{snapshot::snapshot, Env, Fs, Mode};

fn usage() -> ExitCode {
    eprintln!("usage: plane-next snapshot [--agents FILE] [--now SECONDS] [--home DIR]");
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

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) != Some("snapshot") {
        return usage();
    }
    let (mut agents_file, mut now, mut home) = (None, None, None);
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--agents" => agents_file = it.next().cloned(),
            "--now" => now = it.next().and_then(|n| n.parse::<f64>().ok()),
            "--home" => home = it.next().cloned(),
            _ => return usage(),
        }
    }
    let env = home.map(Env::new).unwrap_or_else(Env::from_env);
    let now = now.unwrap_or_else(|| std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0));
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
