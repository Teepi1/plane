//! The session ledger: one file per session under `~/.local/share/plane/live/`.
//!
//! Every session writes its record from the hooks and marks it ended when it exits. A crash,
//! power loss or reboot never gets that far, so a record whose process is gone and that isn't
//! marked ended is a session that was cut off mid-work.

use std::collections::{HashMap, HashSet};

use serde_json::{Map, Value};

use crate::{boot_id, glob_json, is_headless, num, or, pget, proc_alive, proc_start, py_str, read_json, read_obj, truthy, Env, Fs};

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// Port of `ledger_update(sid, pid, **fields)`.
pub fn ledger_update(env: &Env, fs: &mut Fs, now: f64, sid: &str, pid: &Value, fields: &[(&str, Value)]) {
    let f = env.live().join(format!("{sid}.json"));
    let mut rec = obj(read_obj(&f));
    let info = read_obj(&env.sessions().join(format!("{}.json", py_str(pid))));
    let switched = rec.get("pid").unwrap_or(&Value::Null) != pid;
    if switched {
        rec.remove("ended"); // a new process for this conversation (resumed): it's live again
    }
    for (k, v) in fields {
        if truthy(v) {
            rec.insert((*k).to_owned(), v.clone());
        }
    }
    rec.insert("sessionId".into(), Value::String(sid.to_owned()));
    rec.insert("pid".into(), pid.clone());
    rec.insert("procStart".into(), proc_start(pid).map(Value::String).unwrap_or(Value::Null));
    rec.insert("bootId".into(), Value::String(boot_id()));
    rec.insert("seenAt".into(), now.into());
    for key in ["kind", "jobId", "name"] {
        let v = pget(&info, key);
        if truthy(v) {
            rec.insert(key.into(), v.clone());
        }
    }
    if !rec.contains_key("cwd") {
        // launch dir: where --resume finds it
        let last = fields.iter().find(|(k, _)| *k == "lastCwd").map(|(_, v)| v).unwrap_or(&Value::Null);
        let home = Value::String(env.home.to_string_lossy().into_owned());
        rec.insert("cwd".into(), or(&[pget(&info, "cwd"), last, &home]).clone());
    }
    if !rec.contains_key("startedAt") {
        rec.insert("startedAt".into(), now.into());
    }
    let rec = Value::Object(rec);
    fs.write_json(env, &f, &rec);
    if switched {
        retire_replaced(env, fs, now, &rec);
    }
}

/// A process runs one conversation at a time. A `--resume` start first reports a throwaway
/// session id, then the resumed one, and the throwaway never gets SessionEnd: end it here, or
/// it would come back as a second, cut-off copy of the session after a reboot.
pub fn retire_replaced(env: &Env, fs: &mut Fs, now: f64, rec: &Value) {
    for f in glob_json(&env.live()) {
        let mut old = read_obj(&f);
        if pget(&old, "sessionId") != pget(rec, "sessionId")
            && !truthy(pget(&old, "ended"))
            && pget(&old, "pid") == pget(rec, "pid")
            && pget(&old, "procStart") == pget(rec, "procStart")
            && pget(&old, "bootId") == pget(rec, "bootId")
        {
            old["ended"] = now.into();
            old["endReason"] = "replaced".into();
            fs.write_json(env, &f, &old);
        }
    }
}

/// `claude rm` ends a background session without SessionEnd reaching the hook: mark its
/// records ended here, or a session you closed would come back as cut off by a crash.
pub fn ledger_end(env: &Env, fs: &mut Fs, now: f64, job_id: &str, reason: &str) {
    for f in glob_json(&env.live()) {
        let mut rec = read_obj(&f);
        let matches = pget(&rec, "jobId").as_str() == Some(job_id) || pget(&rec, "sessionId").as_str() == Some(job_id);
        if !truthy(pget(&rec, "ended")) && matches {
            rec["ended"] = now.into();
            rec["endReason"] = reason.into();
            fs.write_json(env, &f, &rec);
        }
    }
}

/// Record sessions that started before the hooks existed; drop old ended records.
pub fn ledger_sync(env: &Env, fs: &mut Fs, now: f64) {
    for f in glob_json(&env.sessions()) {
        let info = read_obj(&f);
        let (pid, sid) = (pget(&info, "pid"), pget(&info, "sessionId"));
        if !truthy(sid) || !proc_alive(pid, pget(&info, "procStart")) || is_headless(pid) {
            continue;
        }
        let sid = py_str(sid);
        let rec = read_json(&env.live().join(format!("{sid}.json"))).filter(|v| !v.is_null());
        let stale = match &rec {
            None => true,
            Some(r) => pget(r, "pid") == pid && !truthy(pget(r, "ended")) && pget(r, "name") != pget(&info, "name"),
        };
        if stale {
            ledger_update(env, fs, now, &sid, pid, &[("lastCwd", pget(&info, "cwd").clone())]);
        }
    }
    for f in glob_json(&env.live()) {
        let rec = read_obj(&f);
        let ended = pget(&rec, "ended");
        if truthy(ended) && now - num(ended) > 86400.0 {
            fs.unlink(env, &f);
        }
    }
}

/// Sessions whose process is gone without having ended, newest activity first. Those that
/// were waiting on you, not working, come back with cause "standby".
pub fn interrupted_sessions(env: &Env, fs: &mut Fs, now: f64) -> Vec<Value> {
    let (mut live, mut live_jobs) = (HashSet::new(), HashSet::new());
    for f in glob_json(&env.sessions()) {
        let info = read_obj(&f);
        if proc_alive(pget(&info, "pid"), pget(&info, "procStart")) {
            live.insert(py_str(pget(&info, "sessionId")));
            live_jobs.insert(py_str(pget(&info, "jobId")));
        }
    }
    let boot = Value::String(boot_id());
    let mut out: Vec<Value> = vec![];
    for f in glob_json(&env.live()) {
        let Some(mut rec) = read_json(&f).filter(truthy) else { continue };
        if !rec.is_object() || truthy(pget(&rec, "ended")) || live.contains(&py_str(pget(&rec, "sessionId"))) {
            continue;
        }
        let job_id = pget(&rec, "jobId").clone();
        if truthy(&job_id) && live_jobs.contains(&py_str(&job_id)) {
            continue; // its background job is running again
        }
        if pget(&rec, "kind") == "bg" && truthy(&job_id) && !env.jobs().join(py_str(&job_id)).is_dir() {
            // a crash or reboot leaves the job dir; only `claude rm` (closed, anywhere) removes it
            rec["ended"] = now.into();
            rec["endReason"] = "removed".into();
            fs.write_json(env, &f, &rec);
            continue;
        }
        if pget(&rec, "bootId") == &boot && proc_alive(pget(&rec, "pid"), pget(&rec, "procStart")) {
            continue;
        }
        rec["path"] = Value::String(f.to_string_lossy().into_owned());
        rec["cause"] = (if pget(&rec, "bootId") != &boot { "reboot" } else { "crashed" }).into();
        let job = read_obj(&env.jobs().join(py_str(&job_id)).join("state.json"));
        let jstate = pget(&job, "state");
        if pget(&rec, "kind") == "bg" && !jstate.is_null() && jstate != "working" && jstate != "failed" {
            // Claude parks a background session that sits waiting on you, and a reboot does the
            // same to one that wasn't mid-task: nothing was lost, it's on standby
            rec["cause"] = "standby".into();
            rec["jobState"] = jstate.clone();
            rec["needs"] = or(&[pget(&job, "needs"), pget(&job, "detail"), &Value::String(String::new())]).clone();
        }
        out.push(rec);
    }
    let key = |r: &Value| num(or(&[pget(r, "lastActive"), pget(r, "seenAt"), &Value::from(0)]));
    out.sort_by(|a, b| key(b).partial_cmp(&key(a)).unwrap_or(std::cmp::Ordering::Equal));
    // one row per background job: the record that holds its conversation
    let mut best: HashMap<String, usize> = HashMap::new();
    for (i, r) in out.iter().enumerate() {
        let j = pget(r, "jobId");
        if !truthy(j) {
            continue;
        }
        let j = py_str(j);
        let replace = match best.get(&j) {
            None => true,
            Some(&b) => truthy(pget(r, "firstPrompt")) && !truthy(pget(&out[b], "firstPrompt")),
        };
        if replace {
            best.insert(j, i);
        }
    }
    out.iter()
        .enumerate()
        .filter(|(i, r)| {
            let j = pget(r, "jobId");
            !truthy(j) || best.get(&py_str(j)) == Some(i)
        })
        .map(|(_, r)| r.clone())
        .collect()
}
