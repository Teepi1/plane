#!/usr/bin/env python3
"""The Python reference for the Rust port: today's `plane` refresh, with every write blocked.

    python3 -I tools/py_snapshot.py --agents FILE --now SECONDS [--home DIR]

Loads the `plane` script that sits next to this folder (never the live ~/.local/bin/plane),
runs one `Store._refresh` on the given `claude agents` output at a frozen time, and prints
the same snapshot shape as `plane-next snapshot`. Writes are recorded, not made: file
writes, renames, unlinks and mkdirs all raise if anything slips past the stubs.
"""
import argparse
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import subprocess
import sys
import threading
import types

HERE = pathlib.Path(__file__).resolve().parent
SESSION_KEYS = ["id", "sessionId", "name", "kind", "bg", "pid", "branch", "procStart", "needs", "procStatus",
                "jobState", "tempo", "stopped", "usedAt"]
LOST_KEYS = ["sessionId", "jobId", "kind", "cause", "jobState", "needs", "lastActive", "seenAt", "name"]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--agents", required=True)
    ap.add_argument("--now", type=float, required=True)
    ap.add_argument("--home")
    args = ap.parse_args()
    if args.home:
        os.environ["HOME"] = args.home  # plane reads Path.home() at import
    agents = pathlib.Path(args.agents).read_text()

    writes = []
    home = str(pathlib.Path.home())

    def short(p):
        p = str(p)
        return "~" + p[len(home):] if p.startswith(home + "/") else p

    # block every way the refresh could touch the disk
    def refuse(*a, **k):
        raise RuntimeError(f"py_snapshot: blocked a write {a!r}")
    pathlib.Path.write_text = refuse
    pathlib.Path.write_bytes = refuse
    pathlib.Path.mkdir = refuse
    pathlib.Path.rename = refuse
    pathlib.Path.replace = refuse
    os.replace = refuse
    os.rename = refuse
    pathlib.Path.unlink = lambda self, missing_ok=False: writes.append({"op": "unlink", "path": short(self)})

    loader = importlib.machinery.SourceFileLoader("plane", str(HERE.parent / "plane"))
    spec = importlib.util.spec_from_loader("plane", loader)
    P = importlib.util.module_from_spec(spec)
    sys.modules["plane"] = P
    loader.exec_module(P)

    P.write_json = lambda path, data: writes.append({"op": "write", "path": short(path), "data": json.loads(json.dumps(data))})
    P.time = types.SimpleNamespace(time=lambda: args.now, sleep=lambda s: None)
    real_run = subprocess.run

    def run(cmd, *a, **k):
        if len(cmd) > 1 and cmd[1] == "agents":
            return subprocess.CompletedProcess(cmd, 0, agents, "")
        return real_run(cmd, *a, **k)
    P.subprocess = types.SimpleNamespace(run=run, CompletedProcess=subprocess.CompletedProcess,
                                         SubprocessError=subprocess.SubprocessError, Popen=refuse)

    store = types.SimpleNamespace(lock=threading.Lock(), on_ready=lambda h: None, on_waiting=lambda s, st: None,
                                  on_settled=lambda sid: None, _ready_seen=None, _state_seen=None, error="",
                                  sessions=[], handoffs=[], lost=[], usage={})
    try:
        P.Store._refresh(store)
    except Exception as exc:
        print(json.dumps({"error": f"{type(exc).__name__}: {exc}", "writes": writes}, indent=2))
        return

    def row(s):
        r = {k: s.get(k) for k in SESSION_KEYS}
        r["state"] = P.session_state(s)
        return r

    def lost_row(r):
        out = {k: r.get(k) for k in LOST_KEYS}
        out["label"] = P.lost_label(r)
        out["file"] = pathlib.Path(r["path"]).name
        return out

    print(json.dumps({
        "sessions": [row(s) for s in store.sessions],
        "handoffs": [{"file": pathlib.Path(h["path"]).name, "id": h.get("id"), "ready": h.get("ready"),
                      "source": (h.get("source") or {}).get("sessionId")} for h in store.handoffs],
        "lost": [lost_row(r) for r in store.lost],
        "usage": store.usage,
        "writes": writes,
    }, indent=2))


if __name__ == "__main__":
    main()
