//! Plane's session logic, ported from the Python `plane` script.
//!
//! Ported to match the Python version exactly, including its edge cases, so the two can be
//! compared on the same input (`tools/shadow.py`). Python's truthiness, `dict.get` and
//! `str()` rules are spelled out by the helpers here, and every write goes through [`Fs`],
//! which can record writes instead of making them.

pub mod actions;
pub mod control;
pub mod files;
pub mod hypr;
pub mod ledger;
pub mod snapshot;
pub mod state;

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

/// Where Plane's files live. Built from a home folder so tests can use a fake one.
#[derive(Clone, Debug)]
pub struct Env {
    pub home: PathBuf,
    pub claude: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
}

impl Env {
    pub fn new(home: impl Into<PathBuf>) -> Env {
        let home = home.into();
        Env {
            claude: home.join(".claude"),
            data: home.join(".local/share/plane"),
            cache: home.join(".cache/plane"),
            home,
        }
    }

    /// The real home folder, from $HOME as Python's `Path.home()` does.
    pub fn from_env() -> Env {
        Env::new(std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/")))
    }

    pub fn live(&self) -> PathBuf { self.data.join("live") }
    pub fn handoffs(&self) -> PathBuf { self.data.join("handoffs") }
    pub fn usage(&self) -> PathBuf { self.cache.join("usage.json") }
    pub fn sessions(&self) -> PathBuf { self.claude.join("sessions") }
    pub fn jobs(&self) -> PathBuf { self.claude.join("jobs") }

    /// `~/...` for a path under home, as written into recorded writes.
    pub fn short(&self, path: &Path) -> String {
        match path.strip_prefix(&self.home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        }
    }
}

// ---------------------------------------------------------------- Python's rules

static NULL: Value = Value::Null;

/// `d.get(k)`: a missing key and a JSON null both read as None.
pub fn pget<'a>(v: &'a Value, k: &str) -> &'a Value {
    v.get(k).unwrap_or(&NULL)
}

/// Python truthiness: None, False, 0, "", [] and {} are false.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `a or b or ...`: the first truthy value, else the last one.
pub fn or<'a>(vals: &[&'a Value]) -> &'a Value {
    vals.iter().copied().find(|v| truthy(v)).unwrap_or_else(|| vals.last().copied().unwrap_or(&NULL))
}

/// `str(v)` for the values that show up in file names and comparisons.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// A number as f64 (`x or 0` style callers decide what a missing value means).
pub fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or(0.0)
}

// ---------------------------------------------------------------- files

/// `read_json(path)`: None when the file is missing or isn't valid JSON.
pub fn read_json(path: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// `read_json(path, {}) or {}`: always an object.
pub fn read_obj(path: &Path) -> Value {
    match read_json(path) {
        Some(v @ Value::Object(_)) if truthy(&v) => v,
        _ => Value::Object(Map::new()),
    }
}

/// The `*.json` entries of a folder in directory order, as Python's `Path.glob` returns them.
pub fn glob_json(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(".json")))
        .collect()
}

/// How writes are handled: made, or only recorded (the shadow run).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Real,
    Record,
}

/// Every file change goes through here.
pub struct Fs {
    pub mode: Mode,
    pub writes: Vec<Value>,
}

impl Fs {
    pub fn new(mode: Mode) -> Fs {
        Fs { mode, writes: vec![] }
    }

    pub fn write_json(&mut self, env: &Env, path: &Path, data: &Value) {
        match self.mode {
            Mode::Record => self.writes.push(serde_json::json!({ "op": "write", "path": env.short(path), "data": data })),
            Mode::Real => {
                let _ = write_json_atomic(path, data);
            }
        }
    }

    pub fn unlink(&mut self, env: &Env, path: &Path) {
        match self.mode {
            Mode::Record => self.writes.push(serde_json::json!({ "op": "unlink", "path": env.short(path) })),
            Mode::Real => {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    /// `os.replace(from, to)`: atomic, and it fails when `from` is already gone, which is how
    /// a claim tells that someone else got there first.
    pub fn rename(&mut self, env: &Env, from: &Path, to: &Path) -> std::io::Result<()> {
        match self.mode {
            Mode::Record => {
                if !from.exists() {
                    return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
                }
                self.writes.push(serde_json::json!({ "op": "rename", "path": env.short(from), "to": env.short(to) }));
                Ok(())
            }
            Mode::Real => {
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::rename(from, to)
            }
        }
    }

    /// `os.utime(path)`: set its modification time to now.
    pub fn touch(&mut self, env: &Env, path: &Path) {
        match self.mode {
            Mode::Record => self.writes.push(serde_json::json!({ "op": "touch", "path": env.short(path) })),
            Mode::Real => {
                if let Ok(f) = std::fs::File::options().append(true).open(path) {
                    let _ = f.set_modified(std::time::SystemTime::now());
                }
            }
        }
    }
}

/// Writes through a temp file with a name unique to this process and moment, then renames
/// it into place, so two writers can't trip over one shared temp file.
pub fn write_json_atomic(path: &Path, data: &Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.{nanos}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, serde_json::to_string_pretty(data).unwrap_or_default())?;
    std::fs::rename(&tmp, path)
}

// ---------------------------------------------------------------- processes

/// The process start time from /proc (field 22 of stat), as Python's `proc_start` reads it.
pub fn proc_start(pid: &Value) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", py_str(pid))).ok()?;
    let (_, after) = stat.rsplit_once(')')?;
    after.split_whitespace().nth(19).map(str::to_owned)
}

/// `proc_alive(pid, start)`: running, and still the same process when a start time is known.
pub fn proc_alive(pid: &Value, start: &Value) -> bool {
    if !truthy(pid) {
        return false;
    }
    let Some(now) = proc_start(pid) else { return false };
    !truthy(start) || now == py_str(start)
}

pub fn boot_id() -> String {
    std::fs::read_to_string("/proc/sys/kernel/random/boot_id").map(|s| s.trim().to_owned()).unwrap_or_default()
}

pub fn cmdline(pid: &Value) -> Vec<String> {
    match std::fs::read(format!("/proc/{}/cmdline", py_str(pid))) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).split('\0').map(str::to_owned).collect(),
        Err(_) => vec![],
    }
}

/// A `claude -p` run: never tracked.
pub fn is_headless(pid: &Value) -> bool {
    cmdline(pid).iter().any(|a| a == "-p" || a == "--print")
}

// ---------------------------------------------------------------- small helpers

/// The branch a session works on: the worktree folder name, else the checked-out branch.
pub fn git_branch(cwd: &str) -> String {
    // first match of /\.(?:claude/)?worktrees/([^/]+)
    let hit = [cwd.find("/.claude/worktrees/").map(|i| (i, "/.claude/worktrees/")), cwd.find("/.worktrees/").map(|i| (i, "/.worktrees/"))]
        .into_iter()
        .flatten()
        .min_by_key(|(i, _)| *i);
    if let Some((i, pat)) = hit {
        let rest = &cwd[i + pat.len()..];
        let name = rest.split('/').next().unwrap_or("");
        if !name.is_empty() {
            return name.to_owned();
        }
    }
    let mut git = Path::new(cwd).join(".git");
    if git.is_file() {
        let Ok(text) = std::fs::read_to_string(&git) else { return String::new() };
        let Some((_, dir)) = text.split_once("gitdir:") else { return String::new() };
        git = PathBuf::from(dir.trim());
    }
    match std::fs::read_to_string(git.join("HEAD")) {
        Ok(head) => {
            let head = head.trim();
            if head.starts_with("ref:") {
                head.rsplit('/').next().unwrap_or("").to_owned()
            } else {
                head.chars().take(8).collect()
            }
        }
        Err(_) => String::new(),
    }
}

/// The name shown for a cut-off session.
pub fn lost_label(r: &Value) -> String {
    let name = pget(r, "name");
    if truthy(name) {
        return py_str(name);
    }
    let first = pget(r, "firstPrompt");
    if truthy(first) {
        return py_str(first);
    }
    let cwd = r.get("cwd").and_then(Value::as_str).unwrap_or("");
    let base = Path::new(cwd).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if !base.is_empty() {
        return base;
    }
    py_str(pget(r, "sessionId")).chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truthiness_matches_python() {
        for v in [json!(null), json!(false), json!(0), json!(0.0), json!(""), json!([]), json!({})] {
            assert!(!truthy(&v), "{v} should be false");
        }
        for v in [json!(true), json!(1), json!(-1.5), json!("x"), json!([0]), json!({"a": null})] {
            assert!(truthy(&v), "{v} should be true");
        }
    }

    #[test]
    fn or_takes_first_truthy_else_last() {
        assert_eq!(or(&[&json!(""), &json!("b")]), &json!("b"));
        assert_eq!(or(&[&json!(null), &json!("")]), &json!(""));
    }

    #[test]
    fn branch_from_worktree_paths() {
        assert_eq!(git_branch("/x/repo/.claude/worktrees/native-app/crates"), "native-app");
        assert_eq!(git_branch("/x/repo/.worktrees/etsy-access-status"), "etsy-access-status");
        assert_eq!(git_branch("/nonexistent/folder"), "");
    }

    #[test]
    fn lost_label_falls_through() {
        assert_eq!(lost_label(&json!({"name": "A", "firstPrompt": "B"})), "A");
        assert_eq!(lost_label(&json!({"name": "", "firstPrompt": "B"})), "B");
        assert_eq!(lost_label(&json!({"cwd": "/home/n/proj/"})), "proj");
        assert_eq!(lost_label(&json!({"cwd": "/", "sessionId": "abcdef123456"})), "abcdef12");
    }
}
