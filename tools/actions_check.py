#!/usr/bin/env python3
"""Check that each user action in Rust makes the same changes as the Python sidebar's.

    python3 -I tools/actions_check.py [--keep]

For every scenario it builds two identical throwaway homes under /tmp, runs the Python action
(the Sidebar method itself, with its prompts answered yes) in one and `plane-next action` in
the other, then compares every file in the two homes and every call made to `claude` (a
stand-in that logs its arguments). Nothing outside /tmp is touched.

Two scenarios differ on purpose, for the two Python bugs the port leaves behind: starting a
handoff whose folder is gone, and worktree folders in the recent-folders list.
"""
import argparse
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent
RUST = REPO / "target/release/plane-next"
TIME_KEYS = {"ended", "launchedAt", "seenAt"}

FAKE_CLAUDE = """#!/usr/bin/env python3
import json, os, sys
with open(os.environ["FAKE_CLAUDE_LOG"], "a") as f:
    f.write(json.dumps({"args": sys.argv[1:], "cwd": os.getcwd()}) + "\\n")
if os.environ.get("FAKE_CLAUDE_FAIL"):
    print("boom: it went wrong\\nsecond line")
    sys.exit(1)
if "--bg" in sys.argv and os.environ.get("FAKE_CLAUDE_ID"):
    print("Session backgrounded · " + os.environ["FAKE_CLAUDE_ID"])
"""

# Runs inside the Python home: the Sidebar's own methods on a stand-in `self`.
DRIVER = r"""
import importlib.machinery, importlib.util, json, os, sys, threading, types
from pathlib import Path
loader = importlib.machinery.SourceFileLoader("plane", sys.argv[1])
spec = importlib.util.spec_from_loader("plane", loader)
P = importlib.util.module_from_spec(spec)
loader.exec_module(P)
S = P.Sidebar
name, args = sys.argv[2], sys.argv[3:]
msgs = []
f = types.SimpleNamespace(attached=None, last_discard=None)
f.confirm = lambda *a, **k: True
f.flash = lambda text, tone="info": msgs.append(text)
f.store = types.SimpleNamespace(poke=lambda: None, lock=threading.Lock(), handoffs=[], lost=[])
f.run_claude = lambda a, cwd: S.run_claude(f, a, cwd)
f.remove = lambda s: S.remove(f, s)
f.handoff_of = lambda s: S.handoff_of(f, s)
started = []
f.start_bg = lambda cwd, title, extra: started.append(extra)
out = None
if name == "close":
    S.close(f, {"bg": True, "id": args[0], "name": "x"})
elif name == "pause":
    S.pause(f, {"bg": True, "id": args[0]})
elif name == "requeue":
    S.requeue(f, {"bg": True, "id": args[0]})
elif name == "claim":
    h = P.read_json(args[0]) or {"id": "gone", "cwd": str(Path.home())}  # the list still shows it
    h.update(path=args[0], source=None, ready=True)
    S.launch_handoff(f, h)
    if started:  # what the session pane runs next: `plane start --handoff done/<file>`
        ns = types.SimpleNamespace(handoff=started[0][1], cwd=None, name=None, prompt=None, resume=None)
        sys.stdin = open(os.devnull)
        real_exec = os.execvp  # pty.spawn's child needs the real one, to run `claude --bg`
        os.execvp = lambda f, a: msgs.append("attach") if a[1:2] == ["attach"] else real_exec(f, a)
        try:
            P.cmd_start(ns)
        except (EOFError, OSError) as e:
            msgs.append(type(e).__name__)
elif name == "discard":
    S.discard_handoff(f, {"path": args[0], "title": "t"})
    out = str(f.last_discard) if f.last_discard else None
elif name == "undo":
    f.last_discard = Path(args[0])
    S.undo_discard(f)
elif name == "close-lost":
    real = P.write_json
    P.write_json = lambda *a: None  # listing them mustn't write (the Rust side lists read-only)
    f.store.lost = P.interrupted_sessions()
    P.write_json = real
    for r in [r for r in f.store.lost if r["sessionId"] in args]:
        S.close_lost(f, r)
elif name == "recent":
    out = P.recent_projects(int(args[0]) if args else 10)
elif name == "complete":
    out = P.complete_dir(args[0])
print("\n" + json.dumps({"out": out, "msgs": msgs}))
"""


def put(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(data if isinstance(data, str) else json.dumps(data, indent=2))


NOW = time.time()  # one clock for every home, so the two sides start identical


def make_home(root):
    now = NOW
    data, claude = root / ".local/share/plane", root / ".claude"
    hand, live = data / "handoffs", data / "live"
    (root / "proj").mkdir(parents=True)
    for d in ("proj-one", "proj-two", ".hidden", "solo"):
        (root / d).mkdir()
    (claude / "jobs/j1").mkdir(parents=True)
    (claude / "jobs/j2").mkdir(parents=True)
    (claude / "sessions").mkdir(parents=True)
    put(live / "s1.json", {"sessionId": "s1", "jobId": "j1", "kind": "bg", "pid": 99999990, "bootId": "old-boot", "lastActive": now - 10})
    put(live / "s1b.json", {"sessionId": "s1b", "jobId": "j1", "kind": "bg", "pid": 99999992, "bootId": "old-boot", "ended": now - 50})
    put(live / "s2.json", {"sessionId": "s2", "jobId": "j2", "kind": "bg", "pid": 99999993, "bootId": "old-boot"})
    put(live / "s3.json", {"sessionId": "s3", "kind": "fg", "pid": 99999991, "bootId": "old-boot", "cwd": str(root / "proj")})
    put(hand / "h1.json", {"id": "h1", "title": "First", "cwd": str(root / "proj"), "prompt": "do the thing", "createdAt": now - 60})
    put(hand / "h2.json", {"id": "h2", "title": "Gone", "cwd": "/nonexistent/plane-check", "prompt": "x", "createdAt": now - 30})
    put(hand / "h3.json", {"id": "h3", "title": "Home", "cwd": "~", "prompt": "in home", "createdAt": now - 20})
    put(hand / "done/d1.json", {"id": "d1", "title": "Started", "cwd": str(root / "proj"), "launchedAt": now - 99, "launchedSessionId": "j1"})
    put(hand / "discarded/old.json", {"id": "old", "title": "Old"})
    hist = [str(root / "proj"), str(root / "proj/.claude/worktrees/feat"), str(root / "solo"), str(root),
            str(root / "proj/.worktrees/x"), str(root / ".claude/jobs/j1"), str(root / "missing"), str(root / "proj")]
    put(claude / "history.jsonl", "\n".join(json.dumps({"project": p}) for p in hist) + "\n")


def tree(root):
    out = {}
    for p in sorted(root.rglob("*")):
        rel = str(p.relative_to(root))
        if rel.startswith(("bin", "claude.log")) or p.is_dir():
            continue
        try:
            data = json.loads(p.read_text())
            if isinstance(data, dict):
                data = {k: ("T" if k in TIME_KEYS and isinstance(v, (int, float)) else v) for k, v in data.items()}
        except ValueError:
            data = p.read_text()
        data = json.loads(json.dumps(data).replace(str(root), "~"))  # the two homes differ only in where they are
        fresh = time.time() - p.stat().st_mtime < 120 if "discarded" in rel else None
        out[rel] = {"data": data, "fresh": fresh}
    return out


def calls(root):
    log = root / "claude.log"
    if not log.exists():
        return []
    return [dict(c, cwd=c["cwd"].replace(str(root), "~")) for c in map(json.loads, log.read_text().splitlines())]


def run(side, root, name, args, env_extra):
    env = {"HOME": str(root), "PATH": f"{root}/bin:/usr/bin:/bin", "FAKE_CLAUDE_LOG": str(root / "claude.log"),
           "KITTY_LISTEN_ON": f"unix:{root}/no-kitty.sock", **env_extra}
    args = [a.replace("{home}", str(root)) for a in args]
    if side == "py":
        cmd = [sys.executable, "-I", "-c", DRIVER, str(REPO / "plane"), name, *args]
    else:
        cmd = [str(RUST), "action", "--home", str(root), name, *args]
    r = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=60, cwd=str(root))
    try:
        return dict(json.loads(r.stdout.strip().splitlines()[-1]), _raw=(r.stdout + r.stderr)[-600:])
    except (ValueError, IndexError):
        return {"crash": (r.stdout + r.stderr)[-400:]}


SCENARIOS = [
    # (name, action, args, env, expected difference or None)
    ("close", "close", ["j1"], {}, None),
    ("close fails", "close", ["j1"], {"FAKE_CLAUDE_FAIL": "1"}, None),
    ("pause", "pause", ["j2"], {}, None),
    ("pause fails", "pause", ["j2"], {"FAKE_CLAUDE_FAIL": "1"}, None),
    ("put back in queue", "requeue", ["j1"], {}, None),
    ("put back fails", "requeue", ["j1"], {"FAKE_CLAUDE_FAIL": "1"}, None),
    ("put back, not from a handoff", "requeue", ["j2"], {}, None),
    ("start handoff", "claim", ["{home}/.local/share/plane/handoffs/h1.json", "started", "abc123"], {"FAKE_CLAUDE_ID": "abc123"}, None),
    ("start handoff in ~", "claim", ["{home}/.local/share/plane/handoffs/h3.json", "started", "abc123"], {"FAKE_CLAUDE_ID": "abc123"}, None),
    ("handoff starts nothing", "claim", ["{home}/.local/share/plane/handoffs/h1.json", "not-started"], {}, None),
    ("handoff already started", "claim", ["{home}/.local/share/plane/handoffs/nope.json"], {}, None),
    ("handoff folder gone", "claim", ["{home}/.local/share/plane/handoffs/h2.json"], {}, "folder"),
    ("discard", "discard", ["{home}/.local/share/plane/handoffs/h1.json"], {}, None),
    ("undo discard", "undo", ["{home}/.local/share/plane/handoffs/discarded/old.json"], {}, None),
    ("undo, nothing there", "undo", ["{home}/.local/share/plane/handoffs/discarded/nope.json"], {}, None),
    ("close cut-off sessions", "close-lost", ["s1", "s3"], {}, None),
    ("recent folders", "recent", ["10"], {}, "worktrees"),
    ("complete a/b", "complete", ["~/pro"], {}, None),
    ("complete one", "complete", ["~/so"], {}, None),
    ("complete hidden", "complete", ["~/.hi"], {}, None),
    ("complete none", "complete", ["~/zz"], {}, None),
    ("complete empty", "complete", [""], {}, None),
    ("complete absolute", "complete", ["{home}/proj-"], {}, None),
]

# What the Rust side reports for a failure, against the Python message
RUST_MSG = {"close fails": "not closed: boom: it went wrong", "pause fails": "not paused: boom: it went wrong\nsecond line",
            "put back fails": "not closed: boom: it went wrong", "put back, not from a handoff": "no handoff to put back"}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true")
    ap.add_argument("--verbose", action="store_true", help="show what each side printed when they differ")
    a = ap.parse_args()
    if not RUST.exists():
        sys.exit(f"build it first: cargo build --release ({RUST} is missing)")
    base = pathlib.Path(tempfile.mkdtemp(prefix="plane-actions-"))
    failed = 0
    for label, name, args, env, expect in SCENARIOS:
        homes = {}
        for side in ("py", "rs"):
            root = base / label.replace(" ", "-").replace(",", "") / side
            make_home(root)
            put(root / "bin/claude", FAKE_CLAUDE)
            (root / "bin/claude").chmod(0o755)
            res = run(side, root, name, args, env)
            homes[side] = (res, tree(root), calls(root), root)
        (pres, ptree, pcalls, proot), (rres, rtree, rcalls, rroot) = homes["py"], homes["rs"]
        problems = []
        if "crash" in rres or "crash" in pres:
            problems.append(f"crashed: py={pres.get('crash', '')!r} rs={rres.get('crash', '')!r}")
        elif expect == "folder":
            # Python strands the handoff in done/; Rust leaves it queued and changes nothing
            h2 = ".local/share/plane/handoffs/h2.json"
            if h2 in ptree or ".local/share/plane/handoffs/done/h2.json" not in ptree:
                problems.append("expected Python to strand the handoff in done/ (the bug); it didn't")
            if rtree != tree_of_fresh(base):
                problems.append("Rust changed files for a handoff whose folder is gone")
            if "folder is gone" not in rres.get("error", ""):
                problems.append(f"Rust didn't say why: {rres}")
        elif expect == "worktrees":
            want = [p.replace(str(proot), "~") for p in pres["out"]
                    if "/.claude/worktrees/" not in p and not p.startswith(str(proot / ".claude") + "/")]
            got = [p.replace(str(rroot), "~") for p in rres["ok"]]
            if got != want:
                problems.append(f"recent folders: python (minus worktrees) {want} rust {got}")
            if want == [p.replace(str(proot), "~") for p in pres["out"]]:
                problems.append("expected Python to list a worktree folder (the bug); it didn't")
        else:
            if ptree != rtree:
                for k in sorted(set(ptree) | set(rtree)):
                    if ptree.get(k) != rtree.get(k):
                        problems.append(f"{k}: python {ptree.get(k)} rust {rtree.get(k)}")
            # the stand-in's own calls for `--bg` and `attach` only happen on the Python side,
            # where cmd_start runs them; the app runs those itself
            pc = [c for c in pcalls if "--bg" not in c["args"] and "attach" not in c["args"]]
            if pc != rcalls:
                problems.append(f"claude calls: python {pc} rust {rcalls}")
            if name in ("recent", "complete"):
                po = pres["out"]
                ro = rres.get("ok")
                po = [p.replace(str(proot), "~") for p in po] if isinstance(po, list) else str(po).replace(str(proot), "~")
                ro = [p.replace(str(rroot), "~") for p in ro] if isinstance(ro, list) else str(ro).replace(str(rroot), "~")
                if po != ro:
                    problems.append(f"result: python {po!r} rust {ro!r}")
            if label in RUST_MSG:
                if rres.get("error") != RUST_MSG[label] or not any(RUST_MSG[label] == m for m in pres["msgs"]):
                    problems.append(f"message: python {pres['msgs']} rust {rres.get('error')!r}")
        print(f"{'ok  ' if not problems else 'DIFF'} {label}")
        for p in problems[:4]:
            print(f"     {p}")
        if problems and a.verbose:
            print(f"     python said: {pres.get('_raw')!r}\n     rust said: {rres.get('_raw')!r}")
        failed += bool(problems)
    if not a.keep:
        shutil.rmtree(base, ignore_errors=True)
    else:
        print(f"homes kept in {base}")
    print(f"{len(SCENARIOS) - failed} of {len(SCENARIOS)} match")
    sys.exit(1 if failed else 0)


def tree_of_fresh(base):
    root = base / "fresh"
    if not root.exists():
        make_home(root)
    return tree(root)


if __name__ == "__main__":
    main()
