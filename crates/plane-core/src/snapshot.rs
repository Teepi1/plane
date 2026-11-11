//! One refresh: what Plane shows right now. Port of Python `Store._refresh`.
//!
//! The output shape is shared with `tools/py_snapshot.py`, so the Rust and Python versions
//! can be compared on the same input.

use std::collections::HashSet;
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::ledger::{interrupted_sessions, ledger_sync};
use crate::state::session_state;
use crate::{git_branch, glob_json, lost_label, num, or, pget, py_str, read_json, read_obj, truthy, Env, Fs};

/// The session fields the comparison looks at.
pub const SESSION_KEYS: [&str; 14] = [
    "id", "sessionId", "name", "kind", "bg", "pid", "branch", "procStart", "needs", "procStatus", "jobState", "tempo", "stopped", "usedAt",
];
pub const LOST_KEYS: [&str; 9] = ["sessionId", "jobId", "kind", "cause", "jobState", "needs", "lastActive", "seenAt", "name"];

/// When you last used a session: your last message to it (the ledger's lastActive), or its
/// start. Opening one doesn't count: browsing would reshuffle the list under you.
pub fn last_used(env: &Env, s: &Value) -> f64 {
    let sid = pget(s, "sessionId");
    let rec = if truthy(sid) { read_obj(&env.live().join(format!("{}.json", py_str(sid)))) } else { json!({}) };
    let started = num(or(&[pget(s, "startedAt"), &json!(0)])) / 1000.0;
    num(or(&[pget(&rec, "lastActive"), &json!(0)])).max(started)
}

/// Everything one refresh produces, in the order the sidebar shows it.
pub struct Refresh {
    pub sessions: Vec<Value>,
    pub handoffs: Vec<Value>,
    pub lost: Vec<Value>,
    pub usage: Value,
}

/// Port of `Store._refresh`, minus notifications. `agents` is the stdout of
/// `claude agents --json --all`. Errors mirror the places where Python would raise.
pub fn refresh(env: &Env, fs: &mut Fs, now: f64, agents: &str) -> Result<Refresh, String> {
    let raw: Value = serde_json::from_str(if agents.trim().is_empty() { "[]" } else { agents }).map_err(|e| format!("claude agents: {e}"))?;
    let Value::Array(raw) = raw else { return Err("claude agents: not a list".into()) };
    let (mut sessions, mut stopped): (Vec<Value>, Vec<Value>) = (vec![], vec![]);
    for mut s in raw {
        if !s.is_object() {
            return Err("claude agents: a row that isn't an object".into());
        }
        let bg = pget(&s, "kind") == "background";
        s["bg"] = bg.into();
        let cwd = s.get("cwd").and_then(Value::as_str).unwrap_or("").to_owned();
        s["branch"] = git_branch(&cwd).into();
        let pid = pget(&s, "pid").clone();
        let files = if truthy(&pid) { read_obj(&env.sessions().join(format!("{}.json", py_str(&pid)))) } else { json!({}) };
        s["procStart"] = pget(&files, "procStart").clone();
        if bg {
            // Python indexes s["id"] here: a row with no id fails the whole refresh
            let Some(id) = s.get("id").map(py_str) else { return Err("KeyError: 'id'".into()) };
            let job = read_obj(&env.jobs().join(&id).join("state.json"));
            s["needs"] = or(&[pget(&job, "needs"), pget(&job, "detail"), &json!("")]).clone();
            s["procStatus"] = pget(&files, "status").clone();
            s["jobState"] = pget(&job, "state").clone();
            s["tempo"] = pget(&job, "tempo").clone();
        }
        let alive = truthy(&pid) && Path::new(&format!("/proc/{}", py_str(&pid))).exists();
        if alive { sessions.push(s) } else { stopped.push(s) }
    }
    for s in sessions.iter_mut().chain(stopped.iter_mut()) {
        let used = last_used(env, s);
        s["usedAt"] = used.into();
    }
    // most recently used first; sessions in other windows after background ones
    sessions.sort_by(|a, b| {
        let ka = (!truthy(pget(a, "bg")), -num(pget(a, "usedAt")));
        let kb = (!truthy(pget(b, "bg")), -num(pget(b, "usedAt")));
        ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
    });
    ledger_sync(env, fs, now);
    let lost = interrupted_sessions(env, fs, now);
    let lost_ids: HashSet<String> = lost.iter().map(|r| py_str(pget(r, "sessionId"))).collect();
    stopped.retain(|s| !lost_ids.contains(&py_str(pget(s, "sessionId"))));
    stopped.sort_by(|a, b| num(pget(b, "startedAt")).partial_cmp(&num(pget(a, "startedAt"))).unwrap_or(std::cmp::Ordering::Equal));
    for s in &mut stopped {
        s["stopped"] = true.into();
    }

    let mut files = glob_json(&env.handoffs());
    files.sort();
    let mut handoffs = vec![];
    for f in files {
        let Some(mut h) = read_json(&f).filter(truthy) else { continue };
        if !h.is_object() {
            continue;
        }
        h["path"] = Value::String(f.to_string_lossy().into_owned());
        let src = sessions
            .iter()
            .find(|s| {
                pget(s, "pid") == pget(&h, "sourcePid")
                    && (!truthy(pget(&h, "sourceProcStart")) || pget(s, "procStart") == pget(&h, "sourceProcStart"))
            })
            .or_else(|| sessions.iter().find(|s| pget(s, "sessionId") == pget(&h, "sourceSessionId")));
        h["ready"] = src.is_none_or(|s| pget(s, "status") != "busy").into();
        h["source"] = src.cloned().unwrap_or(Value::Null);
        handoffs.push(h);
    }
    let usage = match read_json(&env.usage()) {
        Some(v) if truthy(&v) => v,
        _ => json!({}),
    };
    sessions.extend(stopped);
    Ok(Refresh { sessions, handoffs, lost, usage })
}

fn pick(v: &Value, keys: &[&str]) -> Value {
    let mut m = Map::new();
    for k in keys {
        m.insert((*k).to_owned(), pget(v, k).clone());
    }
    Value::Object(m)
}

/// The comparable snapshot: what the sidebar would show, and what the refresh wrote.
pub fn snapshot(env: &Env, fs: &mut Fs, now: f64, agents: &str) -> Value {
    match refresh(env, fs, now, agents) {
        Err(e) => json!({ "error": e, "writes": fs.writes }),
        Ok(r) => json!({
            "sessions": r.sessions.iter().map(|s| {
                let mut row = pick(s, &SESSION_KEYS);
                row["state"] = session_state(s).into();
                row
            }).collect::<Vec<_>>(),
            "handoffs": r.handoffs.iter().map(|h| json!({
                "file": Path::new(h["path"].as_str().unwrap_or("")).file_name().map(|n| n.to_string_lossy().into_owned()),
                "id": pget(h, "id"),
                "ready": pget(h, "ready"),
                "source": pget(pget(h, "source"), "sessionId"),
            })).collect::<Vec<_>>(),
            "lost": r.lost.iter().map(|l| {
                let mut row = pick(l, &LOST_KEYS);
                row["label"] = lost_label(l).into();
                row["file"] = Path::new(l["path"].as_str().unwrap_or("")).file_name().map(|n| n.to_string_lossy().into_owned()).into();
                row
            }).collect::<Vec<_>>(),
            "usage": r.usage,
            "writes": fs.writes,
        }),
    }
}
