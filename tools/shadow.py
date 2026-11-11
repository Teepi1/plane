#!/usr/bin/env python3
"""Compare the Rust port with today's Python `plane` on real data, read-only.

    python3 -I tools/shadow.py [--home DIR] [--quiet]

Captures one `claude agents --json --all` output and one moment in time, runs both the
Python reference (tools/py_snapshot.py) and `plane-next snapshot` on them, and compares
what each would show and what each would have written. Neither writes anything.

A difference that might be timing (a process starting or stopping between the two runs) is
rechecked once. Differences that remain are saved to ~/.cache/plane/shadow/ with both
outputs, and the exit status is 1.
"""
import argparse
import json
import math
import os
import pathlib
import subprocess
import sys
import tempfile
import time

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent
RUST = ROOT / "target/release/plane-next"
OUT = pathlib.Path.home() / ".cache/plane/shadow"


def claude_bin():
    local = pathlib.Path.home() / ".local/bin/claude"
    return str(local) if local.exists() else "claude"


def diffs(a, b, path="", out=None):
    """Every place two JSON values differ (numbers within 1e-6 count as equal)."""
    out = [] if out is None else out
    if isinstance(a, bool) or isinstance(b, bool):
        if a is not b:
            out.append((path, a, b))
    elif isinstance(a, (int, float)) and isinstance(b, (int, float)):
        if not math.isclose(a, b, rel_tol=0, abs_tol=1e-6):
            out.append((path, a, b))
    elif isinstance(a, dict) and isinstance(b, dict):
        for k in sorted(set(a) | set(b)):
            if k not in a or k not in b:
                out.append((f"{path}.{k}", a.get(k, "<missing>"), b.get(k, "<missing>")))
            else:
                diffs(a[k], b[k], f"{path}.{k}", out)
    elif isinstance(a, list) and isinstance(b, list):
        if len(a) != len(b):
            out.append((f"{path}[len]", len(a), len(b)))
        for i, (x, y) in enumerate(zip(a, b)):
            diffs(x, y, f"{path}[{i}]", out)
    elif a != b:
        out.append((path, a, b))
    return out


def run_both(home):
    env = dict(os.environ)
    if home:
        env["HOME"] = home
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as f:
        agents = subprocess.run([claude_bin(), "agents", "--json", "--all"], capture_output=True, text=True,
                                timeout=30).stdout if not home else (pathlib.Path(home) / "agents.json").read_text()
        f.write(agents)
        agents_file = f.name
    now = f"{time.time():.6f}"
    try:
        py = subprocess.run([sys.executable, "-I", str(HERE / "py_snapshot.py"), "--agents", agents_file, "--now", now],
                            capture_output=True, text=True, env=env, timeout=60)
        rs = subprocess.run([str(RUST), "snapshot", "--agents", agents_file, "--now", now] + (["--home", home] if home else []),
                            capture_output=True, text=True, env=env, timeout=60)
    finally:
        os.unlink(agents_file)
    if py.returncode or rs.returncode:
        raise SystemExit(f"shadow: a run failed\npython: {py.stderr[-800:]}\nrust: {rs.stderr[-800:]}")
    return agents, json.loads(py.stdout), json.loads(rs.stdout)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--home", help="a fixture home folder (with agents.json) instead of the real one")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()
    if not RUST.exists():
        raise SystemExit(f"shadow: build it first (cargo build --release -p plane-cli): {RUST} is missing")
    agents, py, rs = run_both(args.home)
    found = diffs(py, rs)
    if found:  # maybe a process started or stopped between the two runs: look once more
        agents, py, rs = run_both(args.home)
        found = diffs(py, rs)
    n = len(py.get("sessions", []))
    if not found:
        if not args.quiet:
            print(f"same: {n} sessions, {len(py.get('handoffs', []))} handoffs, {len(py.get('lost', []))} cut off, "
                  f"{len(py.get('writes', []))} writes it would make")
        return 0
    OUT.mkdir(parents=True, exist_ok=True)
    report = OUT / f"{time.strftime('%Y%m%d-%H%M%S')}.json"
    report.write_text(json.dumps({"differences": [{"at": p, "python": a, "rust": b} for p, a, b in found],
                                  "python": py, "rust": rs, "agents": agents}, indent=2))
    print(f"different in {len(found)} place(s); saved {report}")
    for p, a, b in found[:15]:
        print(f"  {p}: python={json.dumps(a)[:120]} rust={json.dumps(b)[:120]}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
