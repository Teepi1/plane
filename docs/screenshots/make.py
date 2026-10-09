#!/usr/bin/env python3
"""Regenerate the README screenshots from made-up demo data.

Runs the real `plane sidebar` against a throwaway HOME (fake sessions, handoffs and
usage, plus stand-in `claude` and `kitty` commands), captures it with the pyte
terminal emulator, and renders the frames to PNG with headless Chrome. Nothing on
your machine is read or touched.

    python3 -m venv /tmp/shotenv && /tmp/shotenv/bin/pip install pyte
    /tmp/shotenv/bin/python docs/screenshots/make.py
"""
import html
import json
import os
import pty
import select
import shutil
import struct
import subprocess
import sys
import tempfile
import termios
import fcntl
import time
from pathlib import Path

import pyte
import pyte.graphics

HERE = Path(__file__).resolve().parent
PLANE = HERE.parents[1] / "plane"
CHROME = next(filter(None, map(shutil.which, ("google-chrome-stable", "google-chrome",
                                               "chromium", "chromium-browser"))), None)
pyte.graphics.TEXT[2] = "+strikethrough"  # pyte has no "dim": borrow a flag plane never uses


class Screen(pyte.Screen):
    """pyte plus SU/SD (CSI n S / CSI n T), which ncurses uses to scroll part of the screen."""

    def _scroll(self, count, down):
        top = self.margins.top if self.margins else 0
        saved = self.cursor.y, self.cursor.x
        self.cursor.y = top
        (self.insert_lines if down else self.delete_lines)(count or 1)
        self.cursor.y, self.cursor.x = saved

    def scroll_up(self, count=1, *_):
        self._scroll(count, down=False)

    def scroll_down(self, count=1, *_):
        self._scroll(count, down=True)


pyte.Stream.csi = {**pyte.Stream.csi, "S": "scroll_up", "T": "scroll_down"}

# Colors of the kitty theme in the screenshots
THEME = {
    "bg": "#0a0e16", "fg": "#f2f5f9", "border": "#3a9be8",
    "black": "#151c28", "red": "#ff5c6c", "green": "#4ed6a0", "brown": "#f5c86a",
    "blue": "#3a9be8", "magenta": "#e58ab8", "cyan": "#5ccff0", "white": "#c9d2de",
}
ROWS, SIDEBAR, PANE = 34, 38, 88
NOW = time.time()


# ---------------------------------------------------------------- demo data

def demo_home(root, lost=False):
    home = root / "home" / "demo"
    code = home / "code"
    for repo, branch in (("shop-api", "fix-flaky-checkout"), ("web-app", "dark-mode"), ("infra", "pg17"),
                         ("docs-site", "docs-audit-2"), ("dotfiles", "main"),
                         ("storefront", "react-router-7")):
        (code / repo / ".git").mkdir(parents=True, exist_ok=True)
        (code / repo / ".git" / "HEAD").write_text(f"ref: refs/heads/{branch}\n")
    wt = lambda repo, _name: str(code / repo)

    def bg(id_, name, cwd, mins_ago, status="idle", state="idle", alive=True):
        return {"id": id_, "sessionId": id_ + "-0000", "name": name, "kind": "background",
                "cwd": cwd, "status": status, "state": state, "pid": 1 if alive else None,
                "startedAt": (NOW - mins_ago * 60) * 1000}

    agents = [
        bg("a1f3c9", "Fix flaky checkout test", wt("shop-api", "fix-flaky-checkout"), 95, "busy", "running"),
        bg("b72e10", "Add dark mode toggle", wt("web-app", "dark-mode"), 70),
        bg("c40d5a", "Postgres 17 upgrade", wt("infra", "pg17"), 52, state="blocked"),
        bg("d9a2e4", "Docs audit 2/5: guides", wt("docs-site", "docs-audit-2"), 18, "busy", "running"),
        bg("e5b711", "Refactor auth middleware", wt("shop-api", "auth-mw"), 600, alive=False),
        {"id": "f1c2d3", "sessionId": "f1c2d3-0000", "name": "dotfiles", "kind": "interactive",
         "cwd": str(code / "dotfiles"), "status": "idle", "pid": 1, "startedAt": (NOW - 30 * 60) * 1000},
    ]
    claude = home / ".claude"
    (claude / "jobs" / "c40d5a").mkdir(parents=True)
    (claude / "jobs" / "c40d5a" / "state.json").write_text(json.dumps(
        {"needs": "ok to run the migration on staging?"}))
    (claude / "sessions").mkdir(parents=True)

    data = home / ".local/share/plane"
    handoffs, done = data / "handoffs", data / "handoffs" / "done"
    done.mkdir(parents=True)
    (data / "live").mkdir()

    def handoff(file, title, cwd, src_name, src_id, mins_ago, prompt):
        (handoffs / f"{file}.json").write_text(json.dumps({
            "id": file, "title": title, "cwd": str(cwd), "prompt": prompt,
            "createdAt": NOW - mins_ago * 60, "sourceName": src_name,
            "sourceSessionId": src_id and src_id + "-0000"}))

    handoff("1-checkout-metrics", "Checkout: retry metrics", code / "shop-api",
            "Fix flaky checkout test", "a1f3c9", 6,
            "Add a counter for cart-save retries so we can see\nhow often the debounce race happens in production.")
    handoff("2-release-notes", "Release notes v2.4", code / "web-app", "Ship v2.4", None, 40,
            "Write v2.4 notes from merged PRs.\n"
            "Group: Features, Fixes, Internal.\nLink every PR.\n"
            "Open a draft PR with the notes.")
    (done / "0-docs-audit-2.json").write_text(json.dumps({
        "id": "0-docs-audit-2", "title": "Docs audit 2/5: guides", "cwd": str(code / "docs-site"),
        "prompt": "…", "createdAt": NOW - 3600, "launchedAt": NOW - 18 * 60, "launchedSessionId": "d9a2e4"}))

    if lost:
        (data / "live" / "lost-1.json").write_text(json.dumps({
            "sessionId": "9e8d7c6b-lost", "pid": 999999, "procStart": "1", "bootId": "an-earlier-boot",
            "name": "Upgrade React Router", "cwd": str(code / "storefront"),
            "lastCwd": str(code / "storefront"), "lastActive": NOW - 2.2 * 3600,
            "lastPrompt": "fix the loaders, run e2e"}))

    cache = home / ".cache/plane"
    cache.mkdir(parents=True)
    (cache / "usage.json").write_text(json.dumps({
        "five_hour": {"pct": 42, "resetsAt": NOW + 2 * 3600 + 600},
        "seven_day": {"pct": 67, "resetsAt": NOW + 3 * 86400 + 5 * 3600},
        "updatedAt": NOW - 40}))

    bin_ = root / "bin"
    bin_.mkdir(exist_ok=True)
    (root / "agents.json").write_text(json.dumps(agents))
    (bin_ / "claude").write_text(f"#!/bin/sh\n[ \"$1\" = agents ] && cat '{root}/agents.json'\nexit 0\n")
    windows = [{"tabs": [{"windows": [{"id": 2, "user_vars": {"plane_role": "session", "plane_id": "a1f3c9"}}]}]}]
    (bin_ / "kitty").write_text("#!/bin/sh\ncase \"$*\" in *' ls'*) echo '%s';; esac\nexit 0\n" % json.dumps(windows))
    for f in bin_.iterdir():
        f.chmod(0o755)
    return home, bin_


# ---------------------------------------------------------------- capture

def run_sidebar(root, steps, lost=False):
    """Run the sidebar in a pty; `steps` are (bytes to type, seconds to wait) pairs."""
    home, bin_ = demo_home(root, lost)
    env = {"HOME": str(home), "PATH": f"{bin_}:/usr/bin:/bin", "TERM": "xterm-256color",
           "LANG": "C.UTF-8", "ESCDELAY": "25", "NCURSES_NO_UTF8_ACS": "1", "XDG_RUNTIME_DIR": str(root)}
    screen = Screen(SIDEBAR, ROWS)
    stream = pyte.ByteStream(screen)
    pid, fd = pty.fork()
    if pid == 0:
        fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, SIDEBAR, 0, 0))
        os.execve(sys.executable, [sys.executable, str(PLANE), "sidebar"], env)

    def pump(secs):
        end = time.time() + secs
        while time.time() < end:
            if select.select([fd], [], [], 0.05)[0]:
                try:
                    stream.feed(os.read(fd, 65536))
                except OSError:
                    return

    pump(3.5)  # first poll of the fake `claude agents`
    for keys, wait in steps:
        os.write(fd, keys)
        pump(wait)
    os.kill(pid, 9)
    os.waitpid(pid, 0)
    return screen


def click(row, col=6, button=0):
    """An SGR mouse press + release at a 0-based screen cell."""
    return f"\x1b[<{button};{col + 1};{row + 1}M\x1b[<{button};{col + 1};{row + 1}m".encode()


def find_row(screen, text):
    return next(i for i, line in enumerate(screen.display) if text in line)


def session_pane():
    """A mock Claude Code session for the right-hand pane."""
    o, d, g, c, r, b, x = ("\x1b[38;2;215;119;87m", "\x1b[2m", "\x1b[32m", "\x1b[36m",
                           "\x1b[31m", "\x1b[1m", "\x1b[0m")
    rule = d + "─" * PANE + x
    lines = [
        "",
        f" {d}>{x} The checkout test fails about 1 in 5 runs on CI. Find out why and fix it.",
        "",
        f" {o}●{x} I'll run it in a loop first so the failure shows up locally.",
        "",
        f" {g}●{x} {b}Bash{x}(for i in $(seq 20); do npm test -- checkout.spec.ts || echo FAIL; done)",
        f"   {d}⎿{x}  {r}FAIL{x}  checkout.spec.ts › applies the discount once",
        f"      {r}FAIL{x}  checkout.spec.ts › applies the discount once",
        f"      {d}… +18 lines (ctrl+o to expand){x}",
        "",
        f" {g}●{x} {b}Read{x}(src/cart/save.ts)",
        f"   {d}⎿{x}  Read 142 lines",
        "",
        f" {o}●{x} Found it. The test checks the total before the cart's 250 ms debounced",
        "   save has run. On a slow CI runner that window is often missed, so the",
        "   assertion sees the old total.",
        "",
        f" {g}●{x} {b}Update{x}(src/cart/save.ts)",
        f"   {d}⎿{x}  Updated src/cart/save.ts with 4 additions and 1 removal",
        f"      {d}41{x}  {g}+  export function flushSave(): Promise<void> {{{x}",
        f"      {d}42{x}  {g}+    return pending ?? Promise.resolve(){x}",
        f"      {d}43{x}  {g}+  }}{x}",
        "",
        f" {g}●{x} {b}Update{x}(test/checkout.spec.ts)",
        f"   {d}⎿{x}  Updated test/checkout.spec.ts with 1 addition",
        "",
        f" {o}✻ Running the test 50 times to confirm…{x} {d}(esc to interrupt){x}",
        "",
        rule,
        f" {d}>{x} ",
        rule,
        f"  {d}⏵⏵ accept edits on{x}" + " " * (PANE - 39) + f"{d}5h 42% · wk 67%{x}",
    ]
    screen = Screen(PANE, ROWS)
    stream = pyte.Stream(screen)
    stream.feed("\r\n".join(lines))
    return screen


# ---------------------------------------------------------------- render

def color(value, default):
    if value == "default":
        return default
    if value in THEME:
        return THEME[value]
    return f"#{value}" if len(value) == 6 else default


def cells_html(screen, cw):
    rows = []
    for y in range(screen.lines):
        line = screen.buffer[y]
        spans = []
        for x in range(screen.columns):
            ch = line[x]
            fg, bg = color(ch.fg, THEME["fg"]), color(ch.bg, "transparent")
            if ch.reverse:
                fg, bg = (THEME["bg"] if bg == "transparent" else bg), fg
            style = [f"color:{fg}"]
            if bg != "transparent":
                style.append(f"background:{bg}")
            if ch.strikethrough:  # dim
                style.append("opacity:.5" if not ch.reverse else "")
            if ch.bold:
                style.append("font-weight:700")
            spans.append(f'<i style="{";".join(filter(None, style))}">{html.escape(ch.data or " ")}</i>')
        rows.append("<div>" + "".join(spans) + "</div>")
    return "\n".join(rows)


def page(panes, title):
    cw, lh, pad = 8.6, 19, 14
    blocks, width = [], 0
    for i, screen in enumerate(panes):
        if i:
            blocks.append('<div class="split"></div>')
            width += 2
        blocks.append(f'<div class="pane">{cells_html(screen, cw)}</div>')
        width += screen.columns * cw + 2 * pad
    height = ROWS * lh + 2 * pad
    margin = 40
    css = f"""
    html,body{{margin:0;background:transparent}}
    .win{{position:absolute;left:{margin}px;top:{margin}px;display:flex;background:{THEME['bg']};
          border:2px solid {THEME['border']};border-radius:12px;overflow:hidden;
          box-shadow:0 18px 50px rgba(0,0,0,.45)}}
    .pane{{padding:{pad}px;font:14.4px/{lh}px 'MesloLGL Nerd Font Mono','MesloLGLDZ Nerd Font Mono','JetBrains Mono','DejaVu Sans Mono',monospace}}
    .pane div{{height:{lh}px;white-space:pre}}
    .pane i{{font-style:normal;display:inline-block;width:{cw}px;height:{lh}px;text-align:center;overflow:visible}}
    .split{{width:2px;background:{THEME['black']}}}
    """
    doc = f"<!doctype html><meta charset=utf-8><title>{title}</title><style>{css}</style><div class=win>{''.join(blocks)}</div>"
    return doc, (int(width + 2 * margin + 4), int(height + 2 * margin + 4))


def shoot(name, panes):
    doc, (w, h) = page(panes, name)
    out = HERE / f"{name}.png"
    with tempfile.TemporaryDirectory() as tmp:
        src = Path(tmp) / "shot.html"
        src.write_text(doc)
        subprocess.run([CHROME, "--headless=new", "--disable-gpu", "--hide-scrollbars",
                        f"--user-data-dir={tmp}/profile", "--default-background-color=00000000",
                        "--force-device-scale-factor=2", f"--window-size={w},{h}",
                        f"--screenshot={out}", src.as_uri()],
                       check=True, capture_output=True, timeout=60)
    print("wrote", out.relative_to(HERE.parents[1]))


def main():
    if not CHROME:
        sys.exit("needs Google Chrome or Chromium for headless screenshots")
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)

        def fresh(name):
            d = tmp / name
            d.mkdir()
            return d

        main_view = run_sidebar(fresh("main"), [])
        shoot("main", [main_view, session_pane()])

        probe = run_sidebar(fresh("probe"), [])
        h_row = find_row(probe, "Release notes v2.4")
        s_row = find_row(probe, "Docs audit 2/5")
        shoot("handoff", [run_sidebar(fresh("handoff"), [(click(h_row), 1.0)])])
        shoot("menu", [run_sidebar(fresh("menu"), [(click(s_row, button=2), 1.0)])])
        shoot("recovery", [run_sidebar(fresh("recovery"), [], lost=True)])


if __name__ == "__main__":
    main()
