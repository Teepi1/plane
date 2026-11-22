//! Handoff folders: a project's handoffs filed under names like Launch or Printers.
//! Port of the Python sidebar's folder functions; same files, so both can run at once.
//!
//! - `folders.json` maps a project path to its folder names, in the order they're listed.
//! - A handoff lists its folders in "folders"; files from before several folders were
//!   possible have a single "folder" string, which still reads.
//! - A handoff in several folders is still one handoff: starting or discarding it takes it
//!   out of all of them.

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::{glob_json, num, pget, read_json, truthy, Env, Fs};

impl Env {
    pub fn folders_file(&self) -> PathBuf { self.data.join("folders.json") }
    /// The handoff projects and folders left open, shared with the kitty sidebar.
    pub fn tree_file(&self) -> PathBuf { self.cache.join("tree.json") }
}

/// Every project's folders: [(project path, [names])], in file order.
pub fn all_folders(env: &Env) -> Vec<(String, Vec<String>)> {
    let Some(Value::Object(data)) = read_json(&env.folders_file()) else { return vec![] };
    data.iter()
        .filter_map(|(p, names)| {
            let names = names.as_array()?;
            Some((p.clone(), names.iter().filter_map(Value::as_str).filter(|n| !n.trim().is_empty()).map(str::to_owned).collect()))
        })
        .collect()
}

pub fn project_folders(env: &Env, cwd: &str) -> Vec<String> {
    all_folders(env).into_iter().find(|(p, _)| p == cwd).map(|(_, n)| n).unwrap_or_default()
}

/// A typed folder name, spelled the way the project already spells it (Orders, not orders),
/// with runs of spaces closed up.
pub fn folder_name(env: &Env, cwd: &str, name: &str) -> String {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    project_folders(env, cwd).into_iter().find(|n| n.to_lowercase() == name.to_lowercase()).unwrap_or(name)
}

fn folders_obj(env: &Env) -> Map<String, Value> {
    match read_json(&env.folders_file()) {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

fn names_of(data: &Map<String, Value>, cwd: &str) -> Vec<String> {
    data.get(cwd).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default()
}

pub fn add_project_folder(env: &Env, fs: &mut Fs, cwd: &str, name: &str) {
    let mut data = folders_obj(env);
    let mut names = names_of(&data, cwd);
    if !name.is_empty() && !names.iter().any(|n| n == name) {
        names.push(name.to_owned());
        data.insert(cwd.to_owned(), json!(names));
        fs.write_json(env, &env.folders_file(), &Value::Object(data));
    }
}

/// Renames a folder in its project's list and in every queued handoff filed under it.
/// Renaming onto a folder that already exists merges the two.
pub fn rename_project_folder(env: &Env, fs: &mut Fs, cwd: &str, old: &str, new: &str) {
    let mut data = folders_obj(env);
    let mut names = names_of(&data, cwd);
    if let Some(i) = names.iter().position(|n| n == old) {
        names[i] = new.to_owned();
    } else if !names.iter().any(|n| n == new) {
        names.push(new.to_owned());
    }
    data.insert(cwd.to_owned(), json!(dedup(names)));
    fs.write_json(env, &env.folders_file(), &Value::Object(data));
    let mut files = glob_json(&env.handoffs());
    files.sort();
    for f in files {
        let Some(mut h) = read_json(&f).filter(Value::is_object) else { continue };
        let now = handoff_folders(&h);
        if pget(&h, "cwd").as_str() == Some(cwd) && now.iter().any(|n| n == old) {
            set_handoff_folders(&mut h, &now.iter().map(|n| if n == old { new.to_owned() } else { n.clone() }).collect::<Vec<_>>());
            fs.write_json(env, &f, &h);
        }
    }
}

fn dedup(names: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for n in names {
        if !n.is_empty() && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// The folders a handoff is filed in, in its own order, without repeats.
pub fn handoff_folders(h: &Value) -> Vec<String> {
    let names: Vec<Value> = match pget(h, "folders") {
        Value::Array(a) => a.clone(),
        _ => vec![pget(h, "folder").clone()],
    };
    dedup(names.iter().filter_map(Value::as_str).map(str::to_owned).collect())
}

pub fn set_handoff_folders(h: &mut Value, names: &[String]) {
    let Some(o) = h.as_object_mut() else { return };
    o.remove("folder");
    let names = dedup(names.to_vec());
    if names.is_empty() {
        o.remove("folders");
    } else {
        o.insert("folders".into(), json!(names));
    }
}

/// Files a queued handoff in `names` (replacing its folders). None when the handoff is gone:
/// it was started or discarded meanwhile.
pub fn file_handoff(env: &Env, fs: &mut Fs, path: &Path, names: &[String]) -> Option<Value> {
    let mut h = read_json(path).filter(Value::is_object)?;
    set_handoff_folders(&mut h, names);
    fs.write_json(env, path, &h);
    Some(h)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Node {
    Project,
    Folder,
    Handoff,
}

/// A folder row's id: the project path and the name, as the kitty sidebar spells it.
pub fn folder_id(cwd: &str, name: &str) -> String {
    format!("{cwd}\n{name}")
}

/// The Handoffs section as rows of (kind, obj, depth): a project, then its folders (each
/// listed only while it holds a handoff), then its handoffs that aren't in a folder, the way
/// a file tree lists folders before files. A handoff in several folders is listed in each,
/// as a copy with "_place" saying where (the folder's id, or the project's path).
/// `handoffs` come in display order; projects are ordered newest handoff first; folders
/// keep the project's own order, then any not in it alphabetically.
pub fn handoff_tree(handoffs: &[Value], folders: &[(String, Vec<String>)], is_open: impl Fn(Node, &str) -> bool) -> Vec<(Node, Value, u8)> {
    let mut by_proj: Vec<(String, Vec<&Value>)> = vec![];
    for h in handoffs {
        let p = pget(h, "cwd").as_str().unwrap_or("").to_owned();
        match by_proj.iter_mut().find(|(q, _)| *q == p) {
            Some((_, hs)) => hs.push(h),
            None => by_proj.push((p, vec![h])),
        }
    }
    let newest = |hs: &[&Value]| hs.iter().map(|h| num(pget(h, "createdAt"))).fold(0.0, f64::max);
    by_proj.sort_by(|a, b| newest(&b.1).partial_cmp(&newest(&a.1)).unwrap_or(std::cmp::Ordering::Equal));
    let waiting = |hs: &[&Value]| hs.iter().filter(|h| !h.get("ready").map(truthy).unwrap_or(true)).count();
    let mut rows = vec![];
    for (p, hs) in &by_proj {
        let order = folders.iter().find(|(q, _)| q == p).map(|(_, n)| n.clone()).unwrap_or_default();
        let mut groups: Vec<(String, Vec<&Value>)> = vec![];
        for h in hs {
            let names = handoff_folders(h);
            for n in if names.is_empty() { vec![String::new()] } else { names } {
                match groups.iter_mut().find(|(q, _)| *q == n) {
                    Some((_, g)) => g.push(h),
                    None => groups.push((n, vec![h])),
                }
            }
        }
        let has = |n: &str| groups.iter().any(|(q, _)| q == n);
        let mut names: Vec<String> = order.iter().filter(|n| has(n)).cloned().collect();
        let mut extra: Vec<String> = groups.iter().map(|(n, _)| n.clone()).filter(|n| !n.is_empty() && !order.contains(n)).collect();
        extra.sort_by_key(|n| n.to_lowercase());
        names.extend(extra);
        let name = Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.clone());
        rows.push((Node::Project, json!({ "id": p, "cwd": p, "name": name, "count": hs.len(), "folders": names.len(), "waiting": waiting(hs) }), 0));
        if !is_open(Node::Project, p) {
            continue;
        }
        let group = |n: &str| groups.iter().find(|(q, _)| q == n).map(|(_, g)| g.clone()).unwrap_or_default();
        let placed = |h: &Value, place: &str| {
            let mut h = h.clone();
            h["_place"] = place.into();
            h
        };
        for n in &names {
            let fid = folder_id(p, n);
            let fhs = group(n);
            rows.push((Node::Folder, json!({ "id": fid, "cwd": p, "name": n, "count": fhs.len(), "waiting": waiting(&fhs) }), 1));
            if is_open(Node::Folder, &fid) {
                rows.extend(fhs.iter().map(|h| (Node::Handoff, placed(h, &fid), 2)));
            }
        }
        rows.extend(group("").iter().map(|h| (Node::Handoff, placed(h, p), 1)));
    }
    rows
}

/// The open rows from `tree.json`: [kind, id] pairs, kind "project" or "folder".
pub fn read_tree(env: &Env) -> Vec<(String, String)> {
    let Some(Value::Array(a)) = read_json(&env.tree_file()) else { return vec![] };
    a.iter()
        .filter_map(|p| {
            let p = p.as_array()?;
            Some((p.first()?.as_str()?.to_owned(), p.get(1)?.as_str()?.to_owned()))
        })
        .collect()
}

pub fn write_tree(env: &Env, fs: &mut Fs, open: &[(String, String)]) {
    let mut open = open.to_vec();
    open.sort();
    open.dedup();
    if let Some(dir) = env.tree_file().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    fs.write_json(env, &env.tree_file(), &json!(open.iter().map(|(k, i)| json!([k, i])).collect::<Vec<_>>()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mode;

    fn h(id: &str, cwd: &str, created: f64, folders: Value) -> Value {
        let mut v = json!({ "id": id, "cwd": cwd, "createdAt": created, "ready": true });
        if !folders.is_null() {
            let key = if folders.is_string() { "folder" } else { "folders" };
            v[key] = folders;
        }
        v
    }

    fn shape(rows: &[(Node, Value, u8)]) -> Vec<String> {
        rows.iter().map(|(k, o, d)| format!("{d}{k:?}:{}", pget(o, if *k == Node::Handoff { "id" } else { "name" }).as_str().unwrap())).collect()
    }

    #[test]
    fn old_single_folder_still_reads() {
        assert_eq!(handoff_folders(&json!({ "folder": "Orders" })), vec!["Orders"]);
        assert_eq!(handoff_folders(&json!({ "folders": ["A", "B", "A", ""] })), vec!["A", "B"]);
        assert!(handoff_folders(&json!({})).is_empty());
        let mut x = json!({ "folder": "Old" });
        set_handoff_folders(&mut x, &["A".into(), "B".into()]);
        assert_eq!(x, json!({ "folders": ["A", "B"] }));
        set_handoff_folders(&mut x, &[]);
        assert_eq!(x, json!({}));
    }

    #[test]
    fn tree_lists_folders_before_loose_handoffs_and_hides_empty_ones() {
        let hs = vec![
            h("a", "/p/app", 3.0, json!(["Launch", "Printers"])),
            h("b", "/p/app", 2.0, json!("Orders")),
            h("c", "/p/app", 1.0, Value::Null),
            h("d", "/p/site", 5.0, Value::Null),
        ];
        let folders = vec![("/p/app".to_owned(), vec!["Orders".into(), "Dashboard".into(), "Launch".into(), "Printers".into()])];
        let rows = handoff_tree(&hs, &folders, |_, _| true);
        assert_eq!(
            shape(&rows),
            ["0Project:site", "1Handoff:d", "0Project:app", "1Folder:Orders", "2Handoff:b", "1Folder:Launch", "2Handoff:a", "1Folder:Printers", "2Handoff:a", "1Handoff:c"]
        );
        // the same handoff in two folders: one row each, told apart by _place
        let places: Vec<_> = rows.iter().filter(|r| pget(&r.1, "id") == "a").map(|r| r.1["_place"].as_str().unwrap().to_owned()).collect();
        assert_eq!(places, ["/p/app\nLaunch", "/p/app\nPrinters"]);
        assert_eq!(rows[2].1["count"], 3);
        assert_eq!(rows[2].1["folders"], 3);
    }

    #[test]
    fn closed_rows_hide_what_is_under_them() {
        let hs = vec![h("a", "/p/app", 1.0, json!(["Launch"])), h("c", "/p/app", 1.0, Value::Null)];
        assert_eq!(shape(&handoff_tree(&hs, &[], |_, _| false)), ["0Project:app"]);
        let rows = handoff_tree(&hs, &[], |k, _| k == Node::Project);
        assert_eq!(shape(&rows), ["0Project:app", "1Folder:Launch", "1Handoff:c"]);
    }

    #[test]
    fn rename_merges_and_refiles() {
        let dir = std::env::temp_dir().join(format!("plane-folders-{}", std::process::id()));
        let env = Env::new(&dir);
        std::fs::create_dir_all(env.handoffs()).unwrap();
        let mut fs = Fs::new(Mode::Real);
        std::fs::write(env.folders_file(), r#"{"/p": ["Orders", "Print Queue"]}"#).unwrap();
        std::fs::write(env.handoffs().join("x.json"), r#"{"id": "x", "cwd": "/p", "folder": "Print Queue"}"#).unwrap();
        std::fs::write(env.handoffs().join("y.json"), r#"{"id": "y", "cwd": "/q", "folders": ["Print Queue"]}"#).unwrap();
        assert_eq!(folder_name(&env, "/p", "  orders "), "Orders");
        rename_project_folder(&env, &mut fs, "/p", "Print Queue", "Orders");
        assert_eq!(project_folders(&env, "/p"), ["Orders"]);
        assert_eq!(handoff_folders(&read_json(&env.handoffs().join("x.json")).unwrap()), ["Orders"]);
        assert_eq!(handoff_folders(&read_json(&env.handoffs().join("y.json")).unwrap()), ["Print Queue"], "another project's folder of the same name stays");
        add_project_folder(&env, &mut fs, "/p", "Launch");
        assert_eq!(project_folders(&env, "/p"), ["Orders", "Launch"]);
        assert!(file_handoff(&env, &mut fs, &env.handoffs().join("gone.json"), &["A".into()]).is_none());
        write_tree(&env, &mut fs, &[("folder".into(), "/p\nOrders".into()), ("project".into(), "/p".into())]);
        assert_eq!(read_tree(&env), [("folder".to_owned(), "/p\nOrders".to_owned()), ("project".to_owned(), "/p".to_owned())]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
