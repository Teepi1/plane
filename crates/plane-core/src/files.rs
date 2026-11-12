//! Files a session works on: finding the file a terminal link means, reading it safely,
//! saving without losing anyone's text, and what changed (against git, or against what was
//! on disk before). No GUI code; the app's editor tabs are built on this.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use serde_json::Value;

/// Files larger than this open as a plain message, not as text.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;

// ---------------------------------------------------------------- links

/// A link from a terminal: `src/app.rs:12:4`, `~/x.py`, or a `file://` hyperlink.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Link {
    pub file: String,
    pub line: Option<u32>,
    pub col: Option<u32>,
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Some(b) = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Reads a link's text. `None` for links that aren't files (https://…).
pub fn parse_link(text: &str) -> Option<Link> {
    let text = text.trim().trim_end_matches(['.', ',', ';', ')', '\'', '"']);
    if let Some(rest) = text.strip_prefix("file://") {
        // file://host/path: the host is this machine (or empty); the path starts at '/'
        let path_start = rest.find('/')?;
        let rest = &rest[path_start..];
        let (path, frag) = match rest.split_once('#') {
            Some((p, f)) => (p, Some(f)),
            None => (rest, None),
        };
        let line = frag.and_then(|f| f.trim_start_matches(['L', 'l']).split(|c: char| !c.is_ascii_digit()).next()?.parse().ok());
        let mut link = split_line(&percent_decode(path));
        if link.line.is_none() {
            link.line = line;
        }
        return Some(link);
    }
    if text.contains("://") {
        return None;
    }
    Some(split_line(text))
}

/// `a/b.rs:12:4` -> file, line 12, column 4. A trailing `:` alone is dropped.
fn split_line(text: &str) -> Link {
    let mut file = text;
    let mut nums = vec![];
    while let Some((head, tail)) = file.rsplit_once(':') {
        if tail.is_empty() || !tail.chars().all(|c| c.is_ascii_digit()) || nums.len() == 2 {
            break;
        }
        nums.insert(0, tail.parse::<u32>().unwrap_or(0));
        file = head;
    }
    Link { file: file.to_owned(), line: nums.first().copied().filter(|n| *n > 0), col: nums.get(1).copied().filter(|n| *n > 0) }
}

/// Finds the file `file` names, trying each folder in `bases` (the session's folder now,
/// where it started, and so on) and then each one's repository root. Paths shown in git
/// diffs (`a/src/x.rs`) are tried without their `a/` or `b/`.
pub fn resolve(file: &str, bases: &[PathBuf], home: &Path) -> Option<PathBuf> {
    let p = if let Some(rest) = file.strip_prefix("~/") { home.join(rest) } else { PathBuf::from(file) };
    if p.is_absolute() {
        return p.is_file().then_some(p);
    }
    let mut tries: Vec<PathBuf> = vec![];
    for b in bases {
        tries.push(b.join(&p));
    }
    for b in bases {
        if let Some(root) = repo_root(b) {
            tries.push(root.join(&p));
        }
    }
    for prefix in ["a/", "b/"] {
        if let Some(rest) = file.strip_prefix(prefix) {
            for b in bases {
                tries.push(b.join(rest));
            }
        }
    }
    tries.into_iter().find(|t| t.is_file()).map(|t| std::fs::canonicalize(&t).unwrap_or(t))
}

// ---------------------------------------------------------------- reading

/// What's on disk, as last seen: a save first checks the disk still matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    pub len: u64,
    pub hash: u64,
}

fn hash_bytes(b: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    b.hash(&mut h);
    h.finish()
}

pub fn stamp_of(bytes: &[u8]) -> Stamp {
    Stamp { len: bytes.len() as u64, hash: hash_bytes(bytes) }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Content {
    Text(String),
    /// Has NUL bytes near the start: an image, an archive, a program.
    Binary,
    /// Larger than [`MAX_BYTES`] (its size).
    TooBig(u64),
    /// Not valid UTF-8, and not binary either.
    NotUtf8,
}

#[derive(Debug)]
pub enum ReadError {
    Missing,
    Other(String),
}

/// Reads a file for the editor, with its stamp.
pub fn load(path: &Path) -> Result<(Content, Stamp), ReadError> {
    let meta = std::fs::metadata(path).map_err(|e| if e.kind() == std::io::ErrorKind::NotFound { ReadError::Missing } else { ReadError::Other(e.to_string()) })?;
    if meta.is_dir() {
        return Err(ReadError::Other("it's a folder".into()));
    }
    if meta.len() > MAX_BYTES {
        return Ok((Content::TooBig(meta.len()), Stamp { len: meta.len(), hash: 0 }));
    }
    let mut bytes = vec![];
    std::fs::File::open(path).and_then(|mut f| f.read_to_end(&mut bytes)).map_err(|e| ReadError::Other(e.to_string()))?;
    let stamp = stamp_of(&bytes);
    if bytes.len() as u64 > MAX_BYTES {
        return Ok((Content::TooBig(bytes.len() as u64), stamp));
    }
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return Ok((Content::Binary, stamp));
    }
    match String::from_utf8(bytes) {
        Ok(s) => Ok((Content::Text(s), stamp)),
        Err(_) => Ok((Content::NotUtf8, stamp)),
    }
}

/// The stamp of what's on disk now, or `None` if the file isn't there.
pub fn disk_stamp(path: &Path) -> Option<Stamp> {
    std::fs::read(path).ok().map(|b| stamp_of(&b))
}

// ---------------------------------------------------------------- saving

#[derive(Debug)]
pub enum SaveError {
    /// The file on disk isn't what the editor last loaded or saved.
    Changed,
    Io(String),
}

/// Writes `text` to `path` without a half-written file ever showing: a temporary file in
/// the same folder, flushed to disk, renamed over the original, keeping its permissions. A
/// symlink is followed, so the link stays a link.
///
/// With `expect`, the file on disk must still match it (`None` inside means "the file was
/// deleted, and that's fine"); otherwise nothing is written and [`SaveError::Changed`] comes
/// back. Without `expect`, the write goes ahead regardless.
pub fn save(path: &Path, text: &str, expect: Option<Option<&Stamp>>) -> Result<Stamp, SaveError> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if let Some(want) = expect {
        let now = disk_stamp(&target);
        let same = match (now.as_ref(), want) {
            (None, None) => true,
            (Some(a), Some(b)) => a == b,
            (None, Some(_)) => true, // deleted since: saving puts it back
            (Some(_), None) => false, // appeared since
        };
        if !same {
            return Err(SaveError::Changed);
        }
    }
    let io = |e: std::io::Error| SaveError::Io(e.to_string());
    let dir = target.parent().ok_or_else(|| SaveError::Io("no folder".into()))?;
    let name = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let perms = std::fs::metadata(&target).ok().map(|m| m.permissions());
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".{name}.plane-{}-{n}.tmp", std::process::id()));
    let written = (|| -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(text.as_bytes())?;
        if let Some(p) = &perms {
            f.set_permissions(p.clone())?;
        }
        f.sync_all()?;
        std::fs::rename(&tmp, &target)?;
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        if e.kind() == std::io::ErrorKind::PermissionDenied && target.exists() {
            // a folder we can't create files in, but a file we may write: write it in place
            std::fs::write(&target, text.as_bytes()).map_err(io)?;
        } else {
            return Err(io(e));
        }
    }
    Ok(stamp_of(text.as_bytes()))
}

// ---------------------------------------------------------------- git

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then_some(out.stdout)
}

pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    let dir = if dir.is_file() { dir.parent()? } else { dir };
    let out = git(dir, &["rev-parse", "--show-toplevel"])?;
    let s = String::from_utf8_lossy(&out).trim().to_owned();
    (!s.is_empty()).then(|| PathBuf::from(s))
}

/// What a file's changes are measured against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Base {
    /// The last commit (git HEAD).
    Head(String),
    /// Not in git (no repository, or the file isn't committed): the text as first opened.
    Opened(String),
}

impl Base {
    pub fn text(&self) -> &str {
        match self {
            Base::Head(t) | Base::Opened(t) => t,
        }
    }
}

/// The file's text at git HEAD, if it's committed.
pub fn head_text(path: &Path) -> Option<String> {
    let dir = path.parent()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    let out = git(dir, &["show", &format!("HEAD:./{name}")])?;
    String::from_utf8(out).ok()
}

/// One entry of `git status`: the file and a plain word for what happened to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    pub word: &'static str,
}

/// Uncommitted changes under `dir`'s repository, as `git status` lists them.
pub fn git_changes(dir: &Path) -> Option<Vec<Change>> {
    let root = repo_root(dir)?;
    let out = git(&root, &["status", "--porcelain=v1", "-z", "--untracked-files=all"])?;
    let mut items = out.split(|b| *b == 0).map(|s| String::from_utf8_lossy(s).into_owned());
    let mut changes = vec![];
    while let Some(entry) = items.next() {
        if entry.len() < 4 {
            continue;
        }
        let (xy, file) = entry.split_at(3);
        let xy = xy.trim();
        if xy.starts_with('R') || xy.starts_with('C') {
            items.next(); // the old name
        }
        let word = if xy == "??" || xy.contains('A') {
            "new"
        } else if xy.contains('D') {
            "deleted"
        } else if xy.contains('R') {
            "renamed"
        } else if xy.contains('U') {
            "conflict"
        } else {
            "changed"
        };
        changes.push(Change { path: root.join(file), word });
    }
    Some(changes)
}

/// Paths under `root` that git ignores, folders as a whole (`target/`).
pub fn git_ignored(root: &Path) -> Vec<PathBuf> {
    let Some(out) = git(root, &["ls-files", "--others", "--ignored", "--exclude-standard", "--directory", "-z"]) else { return vec![] };
    out.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| root.join(String::from_utf8_lossy(s).trim_end_matches('/'))).collect()
}

// ---------------------------------------------------------------- diffs

/// What changed on each line of `new` compared with `old`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LineMarks {
    /// 0-based lines of `new` that are new.
    pub added: Vec<usize>,
    /// 0-based lines of `new` that replace lines of `old`.
    pub changed: Vec<usize>,
    /// 0-based lines of `new` above which lines of `old` were removed (len means "at the end"),
    /// with how many.
    pub removed: Vec<(usize, usize)>,
}

pub fn line_marks(old: &str, new: &str) -> LineMarks {
    use similar::{DiffOp, TextDiff};
    let diff = TextDiff::configure().timeout(std::time::Duration::from_millis(500)).diff_lines(old, new);
    let mut m = LineMarks::default();
    for op in diff.ops() {
        match *op {
            DiffOp::Equal { .. } => {}
            DiffOp::Insert { new_index, new_len, .. } => m.added.extend(new_index..new_index + new_len),
            DiffOp::Delete { new_index, old_len, .. } => m.removed.push((new_index, old_len)),
            DiffOp::Replace { new_index, new_len, old_len, .. } => {
                m.changed.extend(new_index..new_index + new_len);
                if old_len > new_len {
                    m.removed.push((new_index + new_len, old_len - new_len));
                }
            }
        }
    }
    m
}

/// The lines of `new` that differ from `old` (added or changed), 0-based.
pub fn changed_lines(old: &str, new: &str) -> Vec<usize> {
    let m = line_marks(old, new);
    let mut v = m.added;
    v.extend(m.changed);
    v.sort_unstable();
    v
}

/// One edit that turns `old` into `new`, by lines: replace old lines `old_start..old_end`
/// with `text`. Listed last first, so applying them in order keeps earlier line numbers right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: usize,
    pub old_end: usize,
    pub text: String,
    /// Lines `text` takes up.
    pub new_len: usize,
}

pub fn hunks(old: &str, new: &str) -> Vec<Hunk> {
    use similar::{DiffOp, TextDiff};
    let diff = TextDiff::configure().timeout(std::time::Duration::from_millis(500)).diff_lines(old, new);
    let new_lines: Vec<&str> = new.split_inclusive('\n').collect();
    let mut out = vec![];
    for op in diff.ops() {
        let (old_start, old_len, new_index, new_len) = match *op {
            DiffOp::Equal { .. } => continue,
            DiffOp::Insert { old_index, new_index, new_len } => (old_index, 0, new_index, new_len),
            DiffOp::Delete { old_index, old_len, new_index } => (old_index, old_len, new_index, 0),
            DiffOp::Replace { old_index, old_len, new_index, new_len } => (old_index, old_len, new_index, new_len),
        };
        let text: String = new_lines[new_index..(new_index + new_len).min(new_lines.len())].concat();
        out.push(Hunk { old_start, old_end: old_start + old_len, text, new_len });
    }
    out.reverse();
    out
}

/// A unified diff for reading, and for each of its lines the line of `new` it shows (1-based),
/// so a click on it can go there.
pub fn unified(old: &str, new: &str, old_name: &str, new_name: &str) -> (String, Vec<Option<u32>>, usize, usize) {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::configure().timeout(std::time::Duration::from_millis(500)).diff_lines(old, new);
    let mut text = format!("--- {old_name}\n+++ {new_name}\n");
    let mut lines: Vec<Option<u32>> = vec![None, None];
    let (mut added, mut removed) = (0, 0);
    for group in diff.grouped_ops(3) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else { continue };
        let (o0, o1) = (first.old_range().start, last.old_range().end);
        let (n0, n1) = (first.new_range().start, last.new_range().end);
        text.push_str(&format!("@@ -{},{} +{},{} @@\n", o0 + 1, o1 - o0, n0 + 1, n1 - n0));
        lines.push(Some(n0 as u32 + 1));
        for op in &group {
            for change in diff.iter_changes(op) {
                let (sign, at) = match change.tag() {
                    ChangeTag::Equal => (' ', change.new_index()),
                    ChangeTag::Insert => {
                        added += 1;
                        ('+', change.new_index())
                    }
                    ChangeTag::Delete => {
                        removed += 1;
                        ('-', None)
                    }
                };
                let value = change.value();
                text.push(sign);
                text.push_str(value.strip_suffix('\n').unwrap_or(value));
                text.push('\n');
                lines.push(at.map(|i| i as u32 + 1).or_else(|| lines.last().copied().flatten()));
            }
        }
    }
    (text, lines, added, removed)
}

// ---------------------------------------------------------------- who edited what

/// Seconds since 1970 for an ISO time like `2026-11-11T05:28:51.663Z` (UTC only).
pub fn iso_secs(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[10] != b'T' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d, h, mi) = (n(0..4)?, n(5..7)?, n(8..10)?, n(11..13)?, n(14..16)?);
    let sec: f64 = s.get(17..).map(|r| r.trim_end_matches('Z')).and_then(|r| r.parse().ok())?;
    // days from civil (Howard Hinnant)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((days * 86400 + h * 3600 + mi * 60) as f64 + sec)
}

/// What one transcript says so far: the files its edits touched (with when), and the folder
/// it's working in. Read a bit more each time, from where the last read stopped.
#[derive(Default, Clone, Debug)]
pub struct TranscriptScan {
    pub offset: u64,
    pub edits: HashMap<PathBuf, f64>,
    pub cwd: String,
}

const EDIT_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

impl TranscriptScan {
    pub fn update(&mut self, path: &Path) {
        let Ok(mut f) = std::fs::File::open(path) else { return };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        if len < self.offset {
            *self = TranscriptScan::default(); // rewritten
        }
        use std::io::Seek;
        if f.seek(std::io::SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut buf = vec![];
        if f.read_to_end(&mut buf).is_err() {
            return;
        }
        let Some(end) = buf.iter().rposition(|b| *b == b'\n') else { return };
        for line in buf[..end].split(|b| *b == b'\n') {
            self.line(line);
        }
        self.offset += end as u64 + 1;
    }

    fn line(&mut self, line: &[u8]) {
        let has = |needle: &[u8]| line.windows(needle.len()).any(|w| w == needle);
        if let Some(i) = find(line, b"\"cwd\":\"") {
            let rest = &line[i + 7..];
            if let Some(j) = rest.iter().position(|b| *b == b'"') {
                self.cwd = String::from_utf8_lossy(&rest[..j]).into_owned();
            }
        }
        if !has(b"\"tool_use\"") || !(has(b"\"file_path\"") || has(b"\"notebook_path\"")) {
            return;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else { return };
        let ts = v.get("timestamp").and_then(Value::as_str).and_then(iso_secs).unwrap_or(0.0);
        let Some(content) = v.pointer("/message/content").and_then(Value::as_array) else { return };
        for c in content {
            if c.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let name = c.get("name").and_then(Value::as_str).unwrap_or("");
            if !EDIT_TOOLS.contains(&name) {
                continue;
            }
            let input = c.get("input");
            let file = input.and_then(|i| i.get("file_path").or_else(|| i.get("notebook_path"))).and_then(Value::as_str);
            if let Some(file) = file {
                let e = self.edits.entry(PathBuf::from(file)).or_insert(0.0);
                *e = e.max(ts);
            }
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

/// The folder a transcript's latest entry was in (a session that entered a worktree works
/// there now). Reads only the end of the file.
pub fn last_cwd(transcript: &Path) -> Option<String> {
    use std::io::Seek;
    let mut f = std::fs::File::open(transcript).ok()?;
    let len = f.metadata().ok()?.len();
    let mut want = 256 * 1024u64;
    loop {
        let start = len.saturating_sub(want);
        f.seek(std::io::SeekFrom::Start(start)).ok()?;
        let mut buf = vec![];
        f.read_to_end(&mut buf).ok()?;
        if let Some(i) = find(&buf, b"\"cwd\":\"") {
            let rest = &buf[i + 7..];
            let j = rest.iter().position(|b| *b == b'"')?;
            return Some(String::from_utf8_lossy(&rest[..j]).into_owned());
        }
        if start == 0 || want >= 8 * 1024 * 1024 {
            return None;
        }
        want *= 4;
    }
}

/// A session's transcripts: the main one and its agents', newest folder first.
pub fn transcripts(claude_dir: &Path, session_id: &str) -> Vec<PathBuf> {
    let mut out = vec![];
    let Ok(projects) = std::fs::read_dir(claude_dir.join("projects")) else { return out };
    for p in projects.filter_map(Result::ok) {
        let main = p.path().join(format!("{session_id}.jsonl"));
        if main.is_file() {
            out.push(main);
        }
        if let Ok(agents) = std::fs::read_dir(p.path().join(session_id).join("subagents")) {
            out.extend(agents.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "jsonl")));
        }
    }
    out.sort_by_key(|p| std::cmp::Reverse(p.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH)));
    out
}

// ---------------------------------------------------------------- finding a file by name

/// Most files listed for a folder that isn't a repository.
pub const LIST_CAP: usize = 20_000;

/// Every file under `root`, relative to it, for finding one by name. In a repository: what
/// git tracks plus untracked files it doesn't ignore. Elsewhere: a walk that leaves out
/// `.git` and `node_modules` and stops at [`LIST_CAP`] files.
pub fn file_list(root: &Path) -> Vec<String> {
    if repo_root(root).is_some() {
        if let Some(out) = git(root, &["ls-files", "-co", "--exclude-standard", "-z"]) {
            let mut seen = std::collections::HashSet::new();
            return out
                .split(|b| *b == 0)
                .filter(|s| !s.is_empty())
                .map(|s| String::from_utf8_lossy(s).into_owned())
                .filter(|p| root.join(p).is_file() && seen.insert(p.clone())) // not deleted ones
                .collect();
        }
    }
    let mut out = vec![];
    let mut dirs = vec![root.to_path_buf()];
    while let Some(d) = dirs.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.filter_map(Result::ok) {
            let Ok(t) = e.file_type() else { continue };
            let p = e.path();
            if t.is_dir() {
                let n = e.file_name();
                if n != ".git" && n != "node_modules" {
                    dirs.push(p);
                }
            } else if t.is_file() || (t.is_symlink() && p.is_file()) {
                if let Ok(rel) = p.strip_prefix(root) {
                    out.push(rel.to_string_lossy().into_owned());
                }
                if out.len() >= LIST_CAP {
                    return out;
                }
            }
        }
    }
    out
}

/// Letters of `q` in order in `text` (any case): a score, or None if they aren't all there.
/// A letter at the start of a word or right after the one before counts extra.
fn in_order(q: &[char], text: &str) -> Option<i64> {
    let chars: Vec<char> = text.chars().collect();
    let (mut score, mut qi, mut prev) = (0i64, 0usize, None::<usize>);
    for (i, c) in chars.iter().enumerate() {
        if qi == q.len() {
            break;
        }
        if c.to_lowercase().eq(q[qi].to_lowercase()) {
            let word_start = i == 0 || matches!(chars[i - 1], '/' | '_' | '-' | '.' | ' ') || (c.is_uppercase() && chars[i - 1].is_lowercase());
            score += 1 + if word_start { 8 } else { 0 } + if prev.is_some_and(|p| p + 1 == i) { 5 } else { 0 };
            prev = Some(i);
            qi += 1;
        }
    }
    (qi == q.len()).then_some(score)
}

/// How well `query` matches a file's relative `path`, higher is better, or None. A match in
/// the file name beats one spread over its folders; the name starting with, or containing,
/// what you typed beats letters picked out of it; shorter paths win ties.
pub fn fuzzy_score(query: &str, path: &str) -> Option<i64> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).collect();
    if q.is_empty() {
        return Some(0);
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    let typed: String = q.iter().collect::<String>().to_lowercase();
    let short = -(path.chars().count() as i64);
    if let Some(s) = in_order(&q, name) {
        let lname = name.to_lowercase();
        let bonus = if lname.starts_with(&typed) { 300 } else if lname.contains(&typed) { 150 } else { 0 };
        return Some(10_000 + bonus + s * 4 + short);
    }
    let bonus = if path.to_lowercase().contains(&typed) { 150 } else { 0 };
    in_order(&q, path).map(|s| bonus + s * 4 + short)
}

/// The best `limit` matches for `query` among `paths`, best first.
pub fn find_files(query: &str, paths: &[String], limit: usize) -> Vec<String> {
    let mut hits: Vec<(i64, &String)> = paths.iter().filter_map(|p| fuzzy_score(query, p).map(|s| (s, p))).collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
    hits.into_iter().take(limit).map(|(_, p)| p.clone()).collect()
}

// ---------------------------------------------------------------- new, rename

/// What's wrong with a typed file or folder name, in words for beside the field.
pub fn name_problem(name: &str) -> Option<&'static str> {
    if name.trim().is_empty() {
        Some("type a name")
    } else if name.contains('/') {
        Some("names can't contain /")
    } else if name == "." || name == ".." {
        Some("that name means a folder itself")
    } else if name.contains('\0') {
        Some("names can't contain that character")
    } else {
        None
    }
}

fn taken(path: &Path) -> Option<String> {
    let meta = path.symlink_metadata().ok()?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    Some(format!("a {} called {name} already exists", if meta.is_dir() { "folder" } else { "file" }))
}

/// Makes a new empty file or folder. Never replaces anything already there.
pub fn create(path: &Path, is_dir: bool) -> Result<(), String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if let Some(p) = name_problem(&name) {
        return Err(p.into());
    }
    if let Some(t) = taken(path) {
        return Err(t);
    }
    let r = if is_dir { std::fs::create_dir(path) } else { std::fs::OpenOptions::new().write(true).create_new(true).open(path).map(|_| ()) };
    r.map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => taken(path).unwrap_or_else(|| "that name is taken".into()),
        _ => e.to_string(),
    })
}

/// Renames (or moves) `from` to `to`. Never replaces anything already at `to`.
pub fn rename(from: &Path, to: &Path) -> Result<(), String> {
    let name = to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if let Some(p) = name_problem(&name) {
        return Err(p.into());
    }
    if from == to {
        return Ok(());
    }
    if let Some(t) = taken(to) {
        return Err(t);
    }
    std::fs::rename(from, to).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy() {
        // the file name first, then shorter paths
        let paths: Vec<String> = ["crates/plane-gui/src/app.rs", "crates/plane-core/src/actions.rs", "docs/apps/readme.md", "src/a/p/p.rs"].iter().map(|s| s.to_string()).collect();
        assert_eq!(find_files("app", &paths, 10)[0], "crates/plane-gui/src/app.rs");
        assert_eq!(find_files("APP.RS", &paths, 10)[0], "crates/plane-gui/src/app.rs");
        assert!(fuzzy_score("acts", "crates/plane-core/src/actions.rs").is_some());
        assert_eq!(fuzzy_score("xyz", "crates/plane-gui/src/app.rs"), None);
        // spread over folders still matches, below name matches
        assert!(fuzzy_score("guiapp", "crates/plane-gui/src/app.rs").is_some());
        assert!(fuzzy_score("app", "src/app.rs").unwrap() > fuzzy_score("app", "docs/apps/readme.md").unwrap_or(i64::MIN));
        assert_eq!(find_files("", &paths, 2).len(), 2);
    }

    #[test]
    fn listing_files() {
        let d = std::env::temp_dir().join(format!("plane-list-{}", std::process::id()));
        std::fs::create_dir_all(d.join("a/b")).unwrap();
        std::fs::create_dir_all(d.join("node_modules/x")).unwrap();
        std::fs::create_dir_all(d.join(".git")).unwrap();
        std::fs::write(d.join("a/b/c.txt"), "").unwrap();
        std::fs::write(d.join("top.md"), "").unwrap();
        std::fs::write(d.join("node_modules/x/i.js"), "").unwrap();
        // a folder named .git that isn't a real repository: walked, minus .git and node_modules
        let mut l = file_list(&d);
        l.sort();
        assert_eq!(l, vec!["a/b/c.txt".to_string(), "top.md".to_string()]);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn new_and_rename() {
        let d = std::env::temp_dir().join(format!("plane-new-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(create(&d.join("x.txt"), false), Ok(()));
        assert!(d.join("x.txt").is_file());
        assert_eq!(create(&d.join("x.txt"), false), Err("a file called x.txt already exists".into()));
        assert_eq!(create(&d.join("sub"), true), Ok(()));
        assert_eq!(create(&d.join("sub"), false), Err("a folder called sub already exists".into()));
        assert_eq!(name_problem("a/b"), Some("names can't contain /"));
        assert_eq!(name_problem("  "), Some("type a name"));
        std::fs::write(d.join("x.txt"), "keep").unwrap();
        // rename never replaces
        assert_eq!(rename(&d.join("sub"), &d.join("x.txt")), Err("a file called x.txt already exists".into()));
        assert_eq!(rename(&d.join("x.txt"), &d.join("y.txt")), Ok(()));
        assert_eq!(std::fs::read_to_string(d.join("y.txt")).unwrap(), "keep");
        assert!(!d.join("x.txt").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn links() {
        assert_eq!(parse_link("src/app.rs:12:4"), Some(Link { file: "src/app.rs".into(), line: Some(12), col: Some(4) }));
        assert_eq!(parse_link("src/app.rs:12"), Some(Link { file: "src/app.rs".into(), line: Some(12), col: None }));
        assert_eq!(parse_link("src/app.rs"), Some(Link { file: "src/app.rs".into(), line: None, col: None }));
        assert_eq!(parse_link("src/app.rs:12."), Some(Link { file: "src/app.rs".into(), line: Some(12), col: None }));
        assert_eq!(parse_link("file:///home/n/a%20b.rs#L7"), Some(Link { file: "/home/n/a b.rs".into(), line: Some(7), col: None }));
        assert_eq!(parse_link("file://host/home/n/x.rs:3"), Some(Link { file: "/home/n/x.rs".into(), line: Some(3), col: None }));
        assert_eq!(parse_link("https://example.com/x.rs"), None);
    }

    #[test]
    fn resolving() {
        let d = std::env::temp_dir().join(format!("plane-files-{}", std::process::id()));
        let wt = d.join("wt");
        std::fs::create_dir_all(wt.join("src")).unwrap();
        std::fs::write(wt.join("src/x.rs"), "x").unwrap();
        let bases = vec![d.clone(), wt.clone()];
        assert_eq!(resolve("src/x.rs", &bases, &d), Some(std::fs::canonicalize(wt.join("src/x.rs")).unwrap()));
        assert_eq!(resolve("b/src/x.rs", &[wt.clone()], &d), Some(std::fs::canonicalize(wt.join("src/x.rs")).unwrap()));
        assert_eq!(resolve("src/nope.rs", &bases, &d), None);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn loading_and_saving() {
        let d = std::env::temp_dir().join(format!("plane-save-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("a.txt");
        std::fs::write(&f, "one\n").unwrap();
        let (c, st) = load(&f).unwrap();
        assert_eq!(c, Content::Text("one\n".into()));
        // a save over an unchanged file works
        let st2 = save(&f, "two\n", Some(Some(&st))).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "two\n");
        // someone else writes: the next save with the old stamp is refused, nothing written
        std::fs::write(&f, "theirs\n").unwrap();
        assert!(matches!(save(&f, "mine\n", Some(Some(&st2))), Err(SaveError::Changed)));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "theirs\n");
        // saving anyway
        save(&f, "mine\n", None).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "mine\n");
        // no temp files left
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
        // binary and big
        std::fs::write(d.join("b.bin"), [0u8, 1, 2]).unwrap();
        assert_eq!(load(&d.join("b.bin")).unwrap().0, Content::Binary);
        std::fs::write(d.join("l.txt"), [0xffu8, 0xfe, b'a']).unwrap();
        assert_eq!(load(&d.join("l.txt")).unwrap().0, Content::NotUtf8);
        assert!(matches!(load(&d.join("gone")), Err(ReadError::Missing)));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn permissions_and_symlinks_kept() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("plane-perm-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("run.sh");
        std::fs::write(&f, "a").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = d.join("link.sh");
        std::os::unix::fs::symlink(&f, &link).unwrap();
        save(&link, "b", None).unwrap();
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "b");
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o755);
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn marks_and_hunks() {
        let old = "a\nb\nc\nd\n";
        let new = "a\nB\nc\nx\ny\n";
        let m = line_marks(old, new);
        assert_eq!(m.changed, vec![1, 3, 4]);
        assert!(m.added.is_empty());
        let m = line_marks("a\nb\n", "a\nb\nc\n");
        assert_eq!(m.added, vec![2]);
        let m = line_marks("a\nb\nc\n", "a\nc\n");
        assert_eq!(m.removed, vec![(1, 1)]);
        // applying the hunks (last first) turns old into new
        let mut lines: Vec<String> = old.split_inclusive('\n').map(str::to_owned).collect();
        for h in hunks(old, new) {
            let repl: Vec<String> = h.text.split_inclusive('\n').map(str::to_owned).collect();
            lines.splice(h.old_start..h.old_end, repl);
        }
        assert_eq!(lines.concat(), new);
        let (text, at, added, removed) = unified(old, new, "HEAD", "yours");
        assert!(text.contains("-b\n+B\n"));
        assert_eq!((added, removed), (3, 2));
        assert_eq!(at.len(), text.lines().count());
    }

    #[test]
    fn iso() {
        assert_eq!(iso_secs("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(iso_secs("2026-11-11T05:28:51.5Z"), Some(1794374931.5));
    }

    #[test]
    fn transcript_edits() {
        let d = std::env::temp_dir().join(format!("plane-tx-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let t = d.join("s.jsonl");
        let line = r#"{"type":"assistant","cwd":"/w/t","timestamp":"2026-11-11T05:28:51Z","message":{"content":[{"type":"tool_use","name":"Edit","input":{"file_path":"/w/t/a.rs","old_string":"x"}}]}}"#;
        std::fs::write(&t, format!("{line}\n{{\"partial")).unwrap();
        let mut s = TranscriptScan::default();
        s.update(&t);
        assert_eq!(s.cwd, "/w/t");
        assert_eq!(s.edits.get(Path::new("/w/t/a.rs")), Some(&1794374931.0));
        let before = s.offset;
        s.update(&t); // nothing new: the partial line waits
        assert_eq!(s.offset, before);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
