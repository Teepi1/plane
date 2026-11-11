#!/usr/bin/env python3
"""Compare Rust and Python on made-up homes that reach every branch the live data rarely does.

    python3 -I tools/fixtures_check.py [--keep]

Builds throwaway home folders under /tmp, starts a few sleeping helper processes to stand in
for live sessions (one with `-p` in its arguments, like a `claude -p` run), and runs
tools/shadow.py against each home. Nothing outside /tmp is touched.
"""
import argparse
import json
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time

HERE = pathlib.Path(__file__).resolve().parent


def proc_start(pid):
    return pathlib.Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()[19]


def boot_id():
    return pathlib.Path("/proc/sys/kernel/random/boot_id").read_text().strip()


def put(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(data if isinstance(data, str) else json.dumps(data, indent=2))


def helper(*extra):
    return subprocess.Popen([sys.executable, "-c", "import time; time.sleep(600)", *extra])


def full_home(root, pids):
    """Every write branch and every state branch at once."""
    a, c, d, headless = pids
    now, boot = time.time(), boot_id()
    claude, data = root / ".claude", root / ".local/share/plane"
    live, jobs, sess, hand = data / "live", claude / "jobs", claude / "sessions", data / "handoffs"
    repo = root / "proj"
    put(repo / ".git/HEAD", "ref: refs/heads/main\n")
    put(root / "wt/.git", f"gitdir: {root}/proj/.git\n")

    # sessions/: the processes Claude knows about
    put(sess / f"{a}.json", {"pid": a, "sessionId": "sa", "procStart": proc_start(a), "status": "busy", "kind": "bg", "jobId": "ja",
                             "name": "Alpha", "cwd": f"{root}/proj/.claude/worktrees/feat"})
    put(sess / f"{c}.json", {"pid": c, "sessionId": "sc", "procStart": proc_start(c), "status": "shell", "kind": "bg", "jobId": "jc",
                             "name": "New name", "cwd": str(repo)})
    put(sess / f"{headless}.json", {"pid": headless, "sessionId": "sh", "procStart": proc_start(headless), "status": "busy"})
    put(sess / "99999990.json", {"pid": 99999990, "sessionId": "sdead", "procStart": "1"})

    # live/: the ledger
    put(live / "sc.json", {"sessionId": "sc", "pid": c, "name": "Old name", "lastActive": now - 30, "cwd": str(repo)})
    put(live / "old.json", {"sessionId": "old", "pid": a, "procStart": proc_start(a), "bootId": boot})  # replaced by sa
    put(live / "ended-old.json", {"sessionId": "eo", "ended": now - 90000})
    put(live / "ended-new.json", {"sessionId": "en", "ended": now - 100})
    put(live / "removed.json", {"sessionId": "srm", "kind": "bg", "jobId": "gone", "pid": 99999991, "bootId": boot})
    put(live / "crash.json", {"sessionId": "scrash", "kind": "bg", "jobId": "jcrash", "pid": 99999992, "bootId": boot,
                              "lastActive": now - 500, "name": "", "firstPrompt": "fix the tests", "cwd": str(repo)})
    put(live / "standby.json", {"sessionId": "sstand", "kind": "bg", "jobId": "jstand", "pid": 99999993, "bootId": "old-boot",
                                "seenAt": now - 900, "cwd": f"{root}/somewhere/"})
    put(live / "dupe1.json", {"sessionId": "sd1", "kind": "bg", "jobId": "jdup", "pid": 99999994, "bootId": "old-boot", "lastActive": now - 100})
    put(live / "dupe2.json", {"sessionId": "sd2", "kind": "bg", "jobId": "jdup", "pid": 99999995, "bootId": "old-boot",
                              "lastActive": now - 50, "firstPrompt": "hello"})
    put(live / "nojob.json", {"sessionId": "snojob", "kind": "fg", "pid": 99999996, "bootId": "old-boot"})
    put(live / "empty.json", {})
    put(live / "broken.json", "{not json")

    # jobs/: background job state
    put(jobs / "ja/state.json", {"state": "working", "tempo": "active", "needs": ""})
    put(jobs / "jc/state.json", {"state": "blocked", "tempo": "blocked", "detail": "Pick one of two layouts"})
    put(jobs / "jx/state.json", {"state": "failed"})
    put(jobs / "jcrash/state.json", {"state": "working"})
    put(jobs / "jstand/state.json", {"state": "blocked", "needs": "", "detail": "Which database?"})
    (jobs / "jdup").mkdir(parents=True)

    put(root / "agents.json", [
        {"id": "ja", "pid": a, "kind": "background", "sessionId": "sa", "name": "Alpha", "status": "busy", "state": "working",
         "cwd": f"{root}/proj/.claude/worktrees/feat", "startedAt": int((now - 3600) * 1000)},
        {"id": "jc", "pid": c, "kind": "background", "sessionId": "sc", "name": "Charlie", "status": "busy", "state": "blocked",
         "cwd": str(repo), "startedAt": int((now - 7200) * 1000)},
        {"pid": d, "kind": "interactive", "sessionId": "sd", "name": "In a window", "status": "busy", "cwd": f"{root}/wt",
         "startedAt": int((now - 60) * 1000)},
        {"id": "jx", "pid": 99999997, "kind": "background", "sessionId": "sx", "name": "Failed one", "state": "failed",
         "cwd": str(repo), "startedAt": int((now - 400) * 1000)},
        {"id": "jp", "kind": "background", "sessionId": "sp", "name": "Paused one", "state": "done", "cwd": str(repo),
         "startedAt": int((now - 300) * 1000)},
        {"id": "jcrash", "kind": "background", "sessionId": "scrash", "name": "Crashed", "state": "working",
         "startedAt": int((now - 200) * 1000)},
    ])

    put(hand / "20261101-1-a.json", {"id": "h1", "title": "From Alpha", "sourcePid": a, "sourceProcStart": proc_start(a)})
    put(hand / "20261101-2-b.json", {"id": "h2", "title": "From Charlie", "sourceSessionId": "sc", "sourcePid": 12})
    put(hand / "20261101-3-c.json", {"id": "h3", "title": "No source"})
    put(hand / "20261101-4-d.json", "{broken")
    put(hand / "20261101-5-e.json", {})
    put(root / ".cache/plane/usage.json", {"five_hour": {"pct": 41.0, "resetsAt": now + 3600}, "seven_day": None, "updatedAt": now})


def no_id_home(root, pids):
    """A background row with no id: Python's refresh fails; the port must fail the same way."""
    put(root / "agents.json", [{"pid": pids[0], "kind": "background", "sessionId": "s1", "name": "No id"}])


def empty_home(root, pids):
    put(root / "agents.json", "")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--keep", action="store_true", help="leave the made-up homes in /tmp")
    ap.add_argument("--show", action="store_true", help="print what the Rust version made of each home")
    args = ap.parse_args()
    procs = [helper(), helper(), helper(), helper("-p")]
    time.sleep(0.3)
    pids = [p.pid for p in procs]
    failed = 0
    try:
        for make in (full_home, no_id_home, empty_home):
            root = pathlib.Path(tempfile.mkdtemp(prefix=f"plane-{make.__name__}-"))
            make(root, pids)
            r = subprocess.run([sys.executable, "-I", str(HERE / "shadow.py"), "--home", str(root)], capture_output=True, text=True)
            print(f"{make.__name__}: {(r.stdout or r.stderr).strip()}")
            failed |= r.returncode != 0
            if args.show:
                out = subprocess.run([str(HERE.parent / "target/release/plane-next"), "snapshot", "--home", str(root),
                                      "--agents", str(root / "agents.json")], capture_output=True, text=True).stdout
                d = json.loads(out)
                if "error" in d:
                    print(f"  error: {d['error']}")
                for x in d.get("sessions", []):
                    print(f"  session  {x['name']:<12} {x['state']:<10} branch={x['branch']!r} stopped={x['stopped']}")
                for x in d.get("handoffs", []):
                    print(f"  handoff  {x['id']}  {'ready' if x['ready'] else 'not ready'}  source={x['source']}")
                for x in d.get("lost", []):
                    print(f"  cut off  {x['label']:<14} {x['cause']:<8} needs={x['needs']!r}")
                for x in d.get("writes", []):
                    data = x.get("data") or {}
                    print(f"  write    {x['op']} {x['path'].split('/')[-1]} {data.get('endReason') or ''} {data.get('name') or ''}")
            if args.keep:
                print(f"  kept {root}")
            else:
                shutil.rmtree(root)
    finally:
        for p in procs:
            p.kill()
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
