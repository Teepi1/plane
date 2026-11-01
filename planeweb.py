#!/usr/bin/env python3
"""planeweb: Plane on your phone.

A small web server for Plane's Home Screen app. It listens on 127.0.0.1 only;
`tailscale serve` puts it on your tailnet with HTTPS and tells it who is asking.
Sessions keep running on this computer: the phone only watches and types.
The phone can also open a plain login shell here (the Terminal), unless web.json
says "terminal": false.

  plane web [--port 8377]

Needs python-pyte (reading Claude's prompts) and python-cryptography (push).
"""
import base64
import fcntl
import hashlib
import json
import os
import pty
import queue
import re
import secrets
import select
import signal
import struct
import subprocess
import sys
import termios
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, unquote, urlparse

try:
    import pyte
except ImportError:  # the server still runs; prompts and the terminal need it
    pyte = None

P = None  # the plane module (see load_plane)
HERE = Path(__file__).resolve().parent
WEB = HERE / "web"
PORT = 8377
CHAT_LIMIT = 400  # newest chat items sent to the phone


def load_plane():
    """Import the `plane` script (it has no .py suffix) as a module."""
    import importlib.machinery
    import importlib.util
    loader = importlib.machinery.SourceFileLoader("plane", str(HERE / "plane"))
    spec = importlib.util.spec_from_loader("plane", loader)
    mod = importlib.util.module_from_spec(spec)
    sys.modules["plane"] = mod
    loader.exec_module(mod)
    return mod


def state_dir():
    return P.DATA / "web"


def config_path():
    return Path(os.environ.get("XDG_CONFIG_HOME", P.HOME / ".config")) / "plane" / "web.json"


def log(text):
    print(f"{time.strftime('%H:%M:%S')} {text}", flush=True)


# ---------------------------------------------------------------- who may connect

def tailscale_status():
    try:
        out = subprocess.run(["tailscale", "status", "--json"], capture_output=True, text=True, timeout=10)
        return json.loads(out.stdout or "{}")
    except (OSError, ValueError, subprocess.SubprocessError):
        return {}


class Access:
    """Requests come through `tailscale serve`, which adds Tailscale-User-Login and
    X-Forwarded-For. Allowed: your own login, from the devices named (by tailnet name) in
    ~/.config/plane/web.json, e.g. {"devices": ["my-iphone"]}; none listed = any of yours."""

    def __init__(self):
        self.logins, self.device_names, self.ips = set(), [], set()
        self.loaded = 0.0
        self.dev = os.environ.get("PLANE_WEB_DEV") == "1"
        self.reload()

    def reload(self):
        cfg = P.read_json(config_path(), {}) or {}
        st = tailscale_status()
        logins = set(cfg.get("allow") or [])
        if not logins:  # default: whoever owns this computer on the tailnet
            me = (st.get("User") or {}).get(str((st.get("Self") or {}).get("UserID")), {})
            if me.get("LoginName"):
                logins.add(me["LoginName"])
        self.logins = {x.lower() for x in logins}
        self.device_names = [d.lower() for d in cfg.get("devices") or []]
        ips = set()
        if self.device_names:
            peers = list((st.get("Peer") or {}).values()) + [st.get("Self") or {}]
            for p in peers:  # by tailnet name: iOS devices all report HostName "localhost"
                dns = (p.get("DNSName") or "").split(".")[0].lower()
                if dns and dns in self.device_names:
                    ips.update(p.get("TailscaleIPs") or [])
        self.ips = ips
        self.loaded = time.time()

    def check(self, headers):
        if self.dev:
            return True
        if time.time() - self.loaded > 300:
            self.reload()
        login = (headers.get("Tailscale-User-Login") or "").lower()
        if not login or login not in self.logins:
            return False
        if self.device_names:
            ip = (headers.get("X-Forwarded-For") or "").split(",")[0].strip()
            if ip not in self.ips:
                self.reload()
                return ip in self.ips
        return True


# ---------------------------------------------------------------- reading Claude's screen

HINT = re.compile(r"Enter to (select|confirm|submit|continue)|Esc to (cancel|go back|exit)|↑/↓ to (navigate|select)",
                  re.I)
OPT_NUM = re.compile(r"^(\s*)(?:❯\s*)?(\d{1,2})\.\s+(.*\S)\s*$")
SOLID = re.compile(r"^\s*[─━]{12,}")
DASHED = re.compile(r"^\s*[╌┄┈]{12,}")
SELECTOR = re.compile(r"^(\s*)❯\s+(\S.*?)\s*$")


def _indent(line):
    return len(line) - len(line.lstrip())


def parse_prompt(lines):
    """Find a menu Claude is showing: the options above an "Enter to select"/"Esc to cancel"
    line, and what it is asking above them. Returns None when there's no menu."""
    lines = [ln.rstrip() for ln in lines]
    while lines and not lines[-1].strip():
        lines.pop()
    hint = next((i for i in range(len(lines) - 1, max(-1, len(lines) - 14), -1) if HINT.search(lines[i])), None)
    if hint is None:
        return None
    opts, pending = [], []
    i, num_col, skipped = hint - 1, None, 0
    # numbered options, read upward: "❯ 1. Yes", descriptions indented under them
    while i >= 0:
        ln = lines[i]
        if not ln.strip() or SOLID.match(ln):
            skipped += 1
            if skipped > 3 or (not opts and skipped > 2):
                break
            i -= 1
            continue
        m = OPT_NUM.match(ln)
        if m:
            col = ln.index(m.group(2), len(m.group(1)))
            if num_col is not None and abs(col - num_col) > 1:
                break
            num_col = col
            opts.append({"n": int(m.group(2)), "label": m.group(3), "desc": " ".join(reversed(pending)),
                         "selected": "❯" in ln[:col]})
            pending, skipped = [], 0
            if int(m.group(2)) == 1:
                i -= 1
                break
        elif num_col is not None and _indent(ln) > num_col + 1 and not HINT.search(ln):
            pending.append(ln.strip())
            skipped = 0
        elif not opts and not pending:
            break  # not a numbered menu; try the plain kind below
        else:
            break
        i -= 1
    if opts:
        opts.reverse()
        top = i
        # numbers must run 1..N
        if [o["n"] for o in opts] != list(range(1, len(opts) + 1)):
            return None
    else:
        # plain menu (the folder-trust prompt): "❯ No, exit" with its siblings at the same column
        sel = next((j for j in range(hint - 1, max(-1, hint - 12), -1) if SELECTOR.match(lines[j])), None)
        if sel is None:
            return None
        m = SELECTOR.match(lines[sel])
        text_col = lines[sel].index(m.group(2))
        j = sel
        while j - 1 >= 0 and lines[j - 1].strip() and _indent(lines[j - 1]) == text_col and not SOLID.match(lines[j - 1]):
            j -= 1
        top = j - 1
        k = j
        while k < hint and lines[k].strip() and (k == sel or _indent(lines[k]) == text_col):
            label = SELECTOR.match(lines[k]).group(2) if k == sel else lines[k].strip()
            opts.append({"n": None, "label": label, "desc": "", "selected": k == sel})
            k += 1
        if len(opts) < 2:
            return None
    # what it's asking: the lines above the options, up to the last solid rule
    ctx, in_code = [], False
    j = top
    while j >= 0 and len(ctx) < 40:
        ln = lines[j]
        if SOLID.match(ln):
            break
        if DASHED.match(ln):
            in_code = not in_code
        elif ln.strip():
            text = re.sub(r"^\s*│\s?", "", ln).strip()
            ctx.append({"t": "code" if in_code else "text", "s": text})
        j -= 1
    ctx.reverse()
    merged = []  # join code lines, and text the terminal wrapped mid-sentence
    for c in ctx:
        if merged and merged[-1]["t"] == c["t"] == "code":
            merged[-1]["s"] += "\n" + c["s"]
        elif merged and merged[-1]["t"] == c["t"] == "text" and c["s"][:1].islower():
            merged[-1]["s"] += " " + c["s"]
        else:
            merged.append(dict(c))
    if len(merged) > 14:
        merged = [{"t": "text", "s": "…"}] + merged[-13:]
    selected = next((k for k, o in enumerate(opts) if o["selected"]), 0)
    body = json.dumps([merged, [o["label"] for o in opts]], ensure_ascii=False)
    return {"context": merged, "options": opts, "selected": selected, "hint": lines[hint].strip(),
            "sig": hashlib.sha1(body.encode()).hexdigest()[:12]}


ANSI = re.compile(r"\x1b\[[0-9;?<>=]*[A-Za-z~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[()][A-Z0-9]|\x1b[=>78]")


if pyte is not None:
    class DimScreen(pyte.Screen):
        """pyte drops "dim" (SGR 2), but Claude draws its suggested next message dim, which is
        how a suggestion is told apart from text you typed. Kept in the blink flag (unused)."""

        def select_graphic_rendition(self, *attrs, **kw):
            super().select_graphic_rendition(*attrs, **kw)
            dim, i, seq = getattr(self, "_dim", False), 0, list(attrs) or [0]
            while i < len(seq):
                a = seq[i]
                if a in (38, 48):  # extended color: skip its arguments
                    i += 2 if i + 1 < len(seq) and seq[i + 1] == 5 else 4
                    continue
                if a == 2:
                    dim = True
                elif a in (0, 22):
                    dim = False
                i += 1
            self._dim = dim
            self.cursor.attrs = self.cursor.attrs._replace(blink=dim)


class PtyScreen:
    """A Claude TUI running in a pty, mirrored onto a pyte screen."""

    def __init__(self, cmd, cwd, cols=100, rows=50):
        self.cols, self.rows = cols, rows
        self.lock = threading.RLock()
        self.listeners = set()  # queues of the phones watching
        self.prompt = None
        self.raw = bytearray()  # everything printed (Starter reads its result from it)
        self.dead = False
        self.last_output = time.time()
        self.screen = DimScreen(cols, rows)
        self.stream = pyte.ByteStream(self.screen)
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            try:
                os.chdir(cwd)
                env = dict(os.environ, TERM="xterm-256color", COLORTERM="truecolor")
                env.pop("KITTY_WINDOW_ID", None)
                os.execvpe(cmd[0], cmd, env)
            finally:
                os._exit(127)
        self._set_size(cols, rows)
        self._dirty = threading.Event()
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._parser, daemon=True).start()

    def _set_size(self, cols, rows):
        try:
            fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        except OSError:
            pass

    def resize(self, cols, rows):
        cols, rows = max(30, min(int(cols), 240)), max(10, min(int(rows), 120))
        with self.lock:
            same = (cols, rows) == (self.cols, self.rows)
            self.cols, self.rows = cols, rows
            if same:  # nudge it, so a viewer that just arrived gets a full redraw
                self._set_size(cols, rows - 1)
                time.sleep(0.05)
            self.screen.resize(rows, cols)
            self._set_size(cols, rows)

    def _answer_queries(self, data):
        # Claude asks the terminal a few things; answer like a plain xterm would
        if b"\x1b[c" in data or b"\x1b[0c" in data:
            self.write(b"\x1b[?62;22c")
        if b"\x1b[>c" in data or b"\x1b[>0c" in data:
            self.write(b"\x1b[>0;10;1c")
        if b"\x1b[6n" in data:
            self.write(f"\x1b[{self.screen.cursor.y + 1};{self.screen.cursor.x + 1}R".encode())
        if b"\x1b]11;?" in data:
            self.write(b"\x1b]11;rgb:0a0a/0e0e/1616\x1b\\")
        if b"\x1b]10;?" in data:
            self.write(b"\x1b]10;rgb:f2f2/f5f5/f9f9\x1b\\")

    def _read(self):
        while True:
            try:
                r, _, _ = select.select([self.fd], [], [], 1.0)
            except (OSError, ValueError):
                break
            if not r:
                continue
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                break
            if not data:
                break
            with self.lock:
                self.last_output = time.time()
                if len(self.raw) < 4_000_000:
                    self.raw += data
                try:
                    self.stream.feed(data)
                except Exception:  # a sequence pyte can't take shouldn't kill the session view
                    pass
                self._answer_queries(data)
                listeners = list(self.listeners)
            b64 = base64.b64encode(data).decode()
            for q in listeners:
                if getattr(q, "term", False):
                    q.put(("term", {"b64": b64}))
            self._dirty.set()
        self.dead = True
        try:
            os.waitpid(self.pid, 0)
        except ChildProcessError:
            pass
        try:
            os.close(self.fd)
        except OSError:
            pass
        self._dirty.set()
        self.on_exit()

    def _parser(self):
        while not self.dead:
            self._dirty.wait()
            if self.dead:
                break
            time.sleep(0.15)  # let a redraw finish before reading it
            self._dirty.clear()
            with self.lock:
                p = parse_prompt(self.screen.display)
            if (p or {}).get("sig") != (self.prompt or {}).get("sig"):
                self.prompt = p
                self.broadcast("screen", self.screen_state())
        self.broadcast("screen", self.screen_state())

    def screen_state(self):
        return {"prompt": self.prompt, "attached": not self.dead}

    def broadcast(self, kind, data):
        with self.lock:
            listeners = list(self.listeners)
        for q in listeners:
            q.put((kind, data))

    def write(self, data):
        if self.dead:
            raise RuntimeError("the session view closed")
        if isinstance(data, str):
            data = data.encode()
        os.write(self.fd, data)

    def send_text(self, text, enter=True):
        text = text.replace("\r\n", "\n")
        self.write(b"\x1b[200~" + text.encode() + b"\x1b[201~")
        if enter:
            time.sleep(0.15)
            self.write(b"\r")

    def choose(self, index, sig):
        """Pick option `index` of the menu on screen, if it's still the menu the phone saw."""
        with self.lock:
            p = self.prompt
            if not p or p["sig"] != sig:
                raise LookupError("That question changed. The current one is showing now.")
            opt = p["options"][index]
        if opt["n"] and opt["n"] <= 9:
            self.write(str(opt["n"]))
            return
        delta = index - p["selected"]
        for _ in range(abs(delta)):
            self.write(b"\x1b[B" if delta > 0 else b"\x1b[A")
            time.sleep(0.06)
        time.sleep(0.1)
        self.write(b"\r")

    def close(self):
        if self.dead:
            return
        try:
            os.kill(self.pid, signal.SIGHUP)  # the terminal went away: claude attach just detaches
        except ProcessLookupError:
            return
        for _ in range(20):
            if self.dead:
                return
            time.sleep(0.1)
        try:
            os.kill(self.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass

    def on_exit(self):
        pass

    def text(self):
        with self.lock:
            return "\n".join(ln.rstrip() for ln in self.screen.display).strip()


class Attach(PtyScreen):
    """`claude attach <id>` for the phones looking at one session."""

    def __init__(self, hub, job):
        self.hub, self.job = hub, job
        self.idle_since = time.time()
        self.started = time.time()
        super().__init__([P.claude_bin(), "attach", job], str(P.HOME))

    def input_line(self):
        """The text in Claude's input box (the last "❯" line near the bottom), or None."""
        with self.lock:
            lines = [ln.rstrip() for ln in self.screen.display]
        while lines and not lines[-1].strip():
            lines.pop()
        for ln in reversed(lines[-14:]):
            m = re.match(r"^\s*❯(.*)$", ln)
            if m:
                return m.group(1).strip()
        return None

    def draft(self):
        """Text someone typed into Claude's message box and hasn't sent (on the laptop: the
        box is shared by every view of the session). A dim suggestion doesn't count."""
        with self.lock:
            buf, cols = self.screen.buffer, self.screen.columns
            for y in range(self.screen.lines - 1, max(-1, self.screen.lines - 15), -1):
                row = [buf[y][x] for x in range(cols)]
                text = "".join(c.data for c in row)
                if text.lstrip().startswith("❯"):
                    start = text.index("❯") + 1
                    return "".join(c.data for c in row[start:] if not c.blink).strip()
        return ""

    def wait_ready(self, limit=30):
        """A new attach (or a paused session coming back) needs a moment before it takes
        keys: wait until Claude's input box (or a menu) is drawn and the screen is quiet."""
        end = time.time() + limit
        while time.time() < end and not self.dead:
            if self.input_line() is not None and time.time() - self.last_output > 0.4:
                return
            time.sleep(0.1)
        if self.dead:
            raise RuntimeError("The session closed before it could take your message.")

    def send_text(self, text, enter=True):
        super().send_text(text, enter)
        if not enter:
            return
        # A session coming back from pause draws its input box a few seconds before it takes a
        # submit, and an Enter mid-redraw can get lost: keep pressing while the text sits there.
        probe = text.strip().splitlines()[0][:30] if text.strip() else ""
        if probe:
            threading.Thread(target=self._make_sure_sent, args=(probe,), daemon=True).start()

    def _make_sure_sent(self, probe):
        end = time.time() + 20
        while time.time() < end and not self.dead:
            time.sleep(1.5)
            now = self.input_line()
            if now is None:  # mid-redraw, or still coming back: look again
                continue
            if probe not in now:
                return
            log(f"{self.job}: message still in the input box, pressing Enter again")
            self.write(b"\r")

    def on_exit(self):
        self.hub.attach_gone(self)


class Starter(PtyScreen):
    """`claude --bg …` for a new session, a handoff or a resume. It shows the folder-trust
    prompt here when there is one, then prints the new session's id and exits."""

    def __init__(self, hub, cwd, name="", prompt="", resume=None, handoff=None):
        self.hub, self.id = hub, secrets.token_hex(4)
        self.cwd, self.name, self.resume, self.handoff = cwd, name, resume, handoff
        self.result = None  # {"jobId": …} or {"error": …}
        self.created = time.time()
        cmd = ([P.claude_bin(), "--bg"] + (["--resume", resume] if resume else [])
               + (["--name", name[:60]] if name else []) + ([prompt] if prompt else []))
        super().__init__(cmd, cwd, cols=100, rows=40)

    def screen_state(self):
        st = super().screen_state()
        st.update(status="done" if self.result and self.result.get("jobId") else
                  "failed" if self.result else "prompt" if self.prompt else "starting",
                  result=self.result, folder=Path(self.cwd).name or "~", cwd=self.cwd)
        return st

    def on_exit(self):
        text = ANSI.sub("", self.raw.decode(errors="replace"))
        m = re.search(r"backgrounded\s*·\s*([0-9a-f]{6,})", text)
        job = m and m.group(1)
        if not job and self.resume:  # find the job by the conversation it continued
            for f in (P.CLAUDE_DIR / "sessions").glob("*.json"):
                info = P.read_json(f, {}) or {}
                if info.get("sessionId") == self.resume and info.get("jobId") and P.proc_alive(info.get("pid")):
                    job = info["jobId"]
        h = P.read_json(self.handoff) if self.handoff else None
        if job:
            if h is not None:
                h["launchedSessionId"] = job
                P.write_json(self.handoff, h)
            self.result = {"jobId": job}
        else:
            if h is not None:  # back into the queue so it can be started again
                h.pop("launchedAt", None)
                P.write_json(P.HANDOFFS / Path(self.handoff).name, h)
                Path(self.handoff).unlink(missing_ok=True)
            tail = [ln for ln in self.text().splitlines() if ln.strip()][-6:]
            self.result = {"error": "No session started.", "detail": "\n".join(tail)}
        self.hub.store.poke()
        self._dirty.set()
        self.broadcast("screen", self.screen_state())


class Shell:
    """Your login shell on this computer, for the phone's Terminal. It keeps running while
    no phone is looking (a command you started finishes), until you end it or it exits.
    The phone draws it with xterm.js, so the raw bytes go straight through; the last
    SHELL_KEEP bytes are kept to redraw the screen when a phone comes back."""

    SHELL_KEEP = 400_000

    def __init__(self, hub, cols=80, rows=24):
        self.hub = hub
        self.lock = threading.Lock()
        self.listeners = set()
        self.buf = bytearray()
        self.total = 0  # bytes printed since the start
        self.seen = 0  # bytes some phone has been sent live (older ones were answered already)
        self.dead, self.code = False, None
        self.started = time.time()
        self.cols, self.rows = self._clamp(cols, rows)
        shell = os.environ.get("SHELL") or ""
        try:
            import pwd
            shell = pwd.getpwuid(os.getuid()).pw_shell or shell
        except (KeyError, ImportError):
            pass
        shell = shell if shell and os.access(shell, os.X_OK) else "/bin/sh"
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            try:
                os.chdir(str(P.HOME))
                env = {k: v for k, v in os.environ.items()
                       if not k.startswith(("KITTY_", "INVOCATION_ID", "JOURNAL_STREAM", "PLANE_"))}
                env.update(TERM="xterm-256color", COLORTERM="truecolor", PLANE_PHONE_TERMINAL="1")
                os.execvpe(shell, [shell, "-l"], env)
            finally:
                os._exit(127)
        self.resize(self.cols, self.rows)
        threading.Thread(target=self._read, daemon=True).start()
        log(f"terminal: started {shell} (pid {self.pid})")

    @staticmethod
    def _clamp(cols, rows):
        return max(20, min(int(cols), 300)), max(5, min(int(rows), 150))

    def resize(self, cols, rows):
        self.cols, self.rows = self._clamp(cols, rows)
        try:
            fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", self.rows, self.cols, 0, 0))
        except OSError:
            pass

    def _read(self):
        while True:
            try:
                r, _, _ = select.select([self.fd], [], [], 1.0)
                if not r:
                    continue
                data = os.read(self.fd, 65536)
            except (OSError, ValueError):
                break
            if not data:
                break
            b64 = base64.b64encode(data).decode()
            with self.lock:
                self.buf += data
                if len(self.buf) > self.SHELL_KEEP:
                    del self.buf[:len(self.buf) - self.SHELL_KEEP]
                self.total += len(data)
                if self.listeners:
                    self.seen = self.total
                listeners = list(self.listeners)
            for q in listeners:
                q.put(("out", {"b64": b64}))
        try:
            _, status = os.waitpid(self.pid, 0)
            self.code = os.waitstatus_to_exitcode(status)
        except ChildProcessError:
            pass
        try:
            os.close(self.fd)
        except OSError:
            pass
        with self.lock:
            self.dead = True
            listeners = list(self.listeners)
        log(f"terminal: ended (code {self.code})")
        for q in listeners:
            q.put(("ended", {"code": self.code}))
        self.hub.shell_gone(self)

    def join(self, q):
        """Add a phone. Returns what to redraw: bytes it has seen before (replayed quietly,
        so old questions to the terminal aren't answered twice) and bytes nobody has seen."""
        with self.lock:
            self.listeners.add(q)
            start = self.total - len(self.buf)
            data = bytes(self.buf)
            cut = max(0, self.seen - start)
            old, new = data[:cut], data[cut:]
            if start > 0 and old:  # the kept bytes begin mid-stream: start on a fresh line
                nl = old.find(b"\n")
                old = old[nl + 1:] if nl >= 0 else b""
            self.seen = self.total
            return old, new

    def leave(self, q):
        with self.lock:
            self.listeners.discard(q)

    def write(self, data):
        if self.dead:
            raise LookupError("The terminal has ended.")
        if isinstance(data, str):
            data = data.encode()
        while data:
            n = os.write(self.fd, data)
            data = data[n:]

    def end(self):
        for sig in (signal.SIGHUP, signal.SIGKILL):
            try:
                os.killpg(os.getpgid(self.pid), sig)
            except (ProcessLookupError, PermissionError):
                try:
                    os.kill(self.pid, sig)
                except ProcessLookupError:
                    return
            for _ in range(20):
                if self.dead:
                    return
                time.sleep(0.1)

    def info(self):
        return {"open": not self.dead, "startedAt": self.started, "pid": self.pid}


def terminal_allowed():
    """On unless ~/.config/plane/web.json says "terminal": false."""
    return (P.read_json(config_path(), {}) or {}).get("terminal", True) is not False


# ---------------------------------------------------------------- chat from the transcript

def transcript_path(session_id):
    """Sessions move folders (a worktree changes the project dir), so look it up by id."""
    hits = list((P.CLAUDE_DIR / "projects").glob(f"*/{session_id}.jsonl"))
    if not hits:
        return None
    return max(hits, key=lambda p: p.stat().st_mtime)


def short_path(p, cwd=""):
    p = str(p or "")
    if cwd and p.startswith(cwd.rstrip("/") + "/"):
        return p[len(cwd.rstrip("/")) + 1:]
    return p.replace(str(P.HOME), "~", 1)


TOOL_KIND = {"Bash": "cmd", "Read": "read", "Edit": "edit", "MultiEdit": "edit", "Write": "edit",
             "NotebookEdit": "edit", "Grep": "search", "Glob": "search", "WebFetch": "web", "WebSearch": "web",
             "AskUserQuestion": "ask"}
KIND_WORDS = {"cmd": ("Ran {} command", "Ran {} commands"), "read": ("read {} file", "read {} files"),
              "edit": ("edited {} file", "edited {} files"), "search": ("searched {} time", "searched {} times"),
              "web": ("looked up {} page", "looked up {} pages"), "ask": ("asked {} question", "asked {} questions"),
              "other": ("used {} tool", "used {} tools")}


def tool_label(name, inp, cwd):
    inp = inp or {}
    if name == "Bash":
        return "$ " + str(inp.get("command", "")).strip().splitlines()[0][:300] if inp.get("command") else "$"
    if name == "Read":
        return "Read " + short_path(inp.get("file_path"), cwd)
    if name in ("Edit", "MultiEdit"):
        return "Edited " + short_path(inp.get("file_path"), cwd)
    if name == "Write":
        return "Wrote " + short_path(inp.get("file_path"), cwd)
    if name in ("Grep", "Glob"):
        return f"Searched for {inp.get('pattern', '')}"[:200]
    if name == "WebFetch":
        return "Fetched " + str(inp.get("url", ""))[:200]
    if name == "WebSearch":
        return "Searched the web: " + str(inp.get("query", ""))[:200]
    if name in ("Agent", "Task"):
        return "Agent: " + str(inp.get("description") or inp.get("prompt", ""))[:160]
    if name == "AskUserQuestion":
        qs = inp.get("questions") or []
        return "Asked: " + " / ".join(str(q.get("question", "")) for q in qs)[:300]
    if name.startswith("mcp__"):
        return name.split("__", 2)[-1].replace("_", " ")
    return name


def summarize(tools):
    counts = {}
    for t in tools:
        counts[t["kind"]] = counts.get(t["kind"], 0) + 1
    parts = [KIND_WORDS[k][n != 1].format(n) for k, n in counts.items()]
    text = " · ".join(parts)
    return text[:1].upper() + text[1:]


def result_text(content):
    if isinstance(content, list):
        content = "\n".join(c.get("text", "") for c in content if isinstance(c, dict) and c.get("type") == "text")
    text = str(content or "").strip()
    return text[:800] + ("…" if len(text) > 800 else "")


SKIP_USER = re.compile(r"^\s*<(system-reminder|command-|local-command|bash-|user-memory|task-notification)|^Caveat:")


class Transcript:
    """Reads a session's .jsonl as it grows and keeps a short chat: what you said, what
    Claude said, and tool runs folded into one line each."""

    def __init__(self, session_id, cwd=""):
        self.session_id, self.cwd = session_id, cwd
        self.path, self.offset, self.items, self.tail = None, 0, [], b""
        self.by_tool = {}
        self.lock = threading.Lock()
        self.changed_from = None

    def refresh(self):
        """Read what's new; returns the first index that changed, or None."""
        with self.lock:
            path = transcript_path(self.session_id)
            if path is None:
                return None
            if path != self.path:
                self.path, self.offset, self.items, self.tail, self.by_tool = path, 0, [], b"", {}
                self.changed_from = 0
            try:
                size = path.stat().st_size
                if size < self.offset:  # rewritten: start over
                    self.offset, self.items, self.tail, self.by_tool, self.changed_from = 0, [], b"", {}, 0
                if size == self.offset:
                    return self._take()
                with open(path, "rb") as f:
                    f.seek(self.offset)
                    data = f.read(min(size - self.offset, 64_000_000))
                self.offset += len(data)
            except OSError:
                return None
            data = self.tail + data
            lines = data.split(b"\n")
            self.tail = lines.pop()
            for line in lines:
                try:
                    self._add(json.loads(line))
                except (ValueError, TypeError, AttributeError):
                    continue
            return self._take()

    def _take(self):
        c, self.changed_from = self.changed_from, None
        return c

    def _mark(self, idx):
        self.changed_from = idx if self.changed_from is None else min(self.changed_from, idx)

    def _push(self, item):
        self.items.append(item)
        self._mark(len(self.items) - 1)

    def _add(self, rec):
        kind = rec.get("type")
        if kind not in ("user", "assistant") or rec.get("isSidechain") or rec.get("isMeta"):
            return
        msg = rec.get("message") or {}
        content = msg.get("content")
        ts = rec.get("timestamp")
        if kind == "user":
            if isinstance(content, str):
                content = [{"type": "text", "text": content}]
            for c in content or []:
                if not isinstance(c, dict):
                    continue
                if c.get("type") == "tool_result":
                    hit = self.by_tool.get(c.get("tool_use_id"))
                    if hit:
                        idx, tool = hit
                        tool["out"] = result_text(c.get("content"))
                        tool["err"] = bool(c.get("is_error"))
                        self._mark(idx)
                elif c.get("type") == "text":
                    text = c.get("text", "").strip()
                    if not text or SKIP_USER.match(text):
                        continue
                    if text.startswith("[Request interrupted"):
                        self._push({"k": "note", "text": "Interrupted"})
                        continue
                    self._push({"k": "you", "text": text[:20000], "ts": ts})
            return
        for c in content or []:
            if not isinstance(c, dict):
                continue
            if c.get("type") == "text" and c.get("text", "").strip():
                self._push({"k": "claude", "text": c["text"].strip()[:40000], "ts": ts})
            elif c.get("type") == "tool_use":
                name, inp = c.get("name", ""), c.get("input") or {}
                if name == "ExitPlanMode":
                    self._push({"k": "plan", "text": str(inp.get("plan") or "")[:60000], "ts": ts})
                    self.by_tool[c.get("id")] = (len(self.items) - 1, {})
                    continue
                tool = {"label": tool_label(name, inp, self.cwd), "kind": TOOL_KIND.get(name, "other"),
                        "out": "", "err": False}
                last = self.items[-1] if self.items else None
                if last and last["k"] == "tools":
                    last["tools"].append(tool)
                    idx = len(self.items) - 1
                    self._mark(idx)
                else:
                    self._push({"k": "tools", "tools": [tool], "ts": ts})
                    idx = len(self.items) - 1
                self.by_tool[c.get("id")] = (idx, tool)
                self.items[idx]["summary"] = summarize(self.items[idx]["tools"])

    def view(self, start=None):
        with self.lock:
            first = max(0, len(self.items) - CHAT_LIMIT)
            start = first if start is None else max(start, first)
            return {"start": start, "total": len(self.items), "items": self.items[start:]}


# ---------------------------------------------------------------- push notifications (Web Push)

def b64u(data):
    return base64.urlsafe_b64encode(data).rstrip(b"=").decode()


def unb64u(text):
    return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))


class Push:
    def __init__(self, contact):
        self.ok, self.why = False, ""
        self.contact = contact
        self.subs_path = state_dir() / "subscriptions.json"
        self.subs = P.read_json(self.subs_path, []) or []
        self.lock = threading.Lock()
        try:
            from cryptography.hazmat.primitives import serialization
            from cryptography.hazmat.primitives.asymmetric import ec
        except ImportError:
            self.why = "python-cryptography isn't installed"
            return
        key_path = state_dir() / "vapid.pem"
        if key_path.exists():
            self.key = serialization.load_pem_private_key(key_path.read_bytes(), None)
        else:
            self.key = ec.generate_private_key(ec.SECP256R1())
            key_path.parent.mkdir(parents=True, exist_ok=True)
            key_path.write_bytes(self.key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                                                        serialization.NoEncryption()))
            os.chmod(key_path, 0o600)
        self.public = b64u(self.key.public_key().public_bytes(serialization.Encoding.X962,
                                                              serialization.PublicFormat.UncompressedPoint))
        self.ok = True

    def subscribe(self, sub):
        if not isinstance(sub, dict) or not str(sub.get("endpoint", "")).startswith("https://"):
            raise ValueError("not a push subscription")
        with self.lock:
            self.subs = [s for s in self.subs if s.get("endpoint") != sub["endpoint"]] + [sub]
            P.write_json(self.subs_path, self.subs)

    def unsubscribe(self, endpoint):
        with self.lock:
            self.subs = [s for s in self.subs if s.get("endpoint") != endpoint]
            P.write_json(self.subs_path, self.subs)

    def _jwt(self, aud):
        from cryptography.hazmat.primitives import hashes
        from cryptography.hazmat.primitives.asymmetric import ec, utils
        head = b64u(json.dumps({"typ": "JWT", "alg": "ES256"}).encode())
        body = b64u(json.dumps({"aud": aud, "exp": int(time.time()) + 12 * 3600,
                                "sub": f"mailto:{self.contact}"}).encode())
        der = self.key.sign(f"{head}.{body}".encode(), ec.ECDSA(hashes.SHA256()))
        r, s = utils.decode_dss_signature(der)
        return f"{head}.{body}.{b64u(r.to_bytes(32, 'big') + s.to_bytes(32, 'big'))}"

    @staticmethod
    def encrypt(payload, p256dh, auth):
        """RFC 8291 (aes128gcm) message encryption."""
        from cryptography.hazmat.primitives import hashes, serialization
        from cryptography.hazmat.primitives.asymmetric import ec
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
        from cryptography.hazmat.primitives.kdf.hkdf import HKDF
        ua_pub = unb64u(p256dh)
        secret = unb64u(auth)
        eph = ec.generate_private_key(ec.SECP256R1())
        as_pub = eph.public_key().public_bytes(serialization.Encoding.X962, serialization.PublicFormat.UncompressedPoint)
        shared = eph.exchange(ec.ECDH(), ec.EllipticCurvePublicKey.from_encoded_point(ec.SECP256R1(), ua_pub))
        ikm = HKDF(hashes.SHA256(), 32, secret, b"WebPush: info\x00" + ua_pub + as_pub).derive(shared)
        salt = secrets.token_bytes(16)
        cek = HKDF(hashes.SHA256(), 16, salt, b"Content-Encoding: aes128gcm\x00").derive(ikm)
        nonce = HKDF(hashes.SHA256(), 12, salt, b"Content-Encoding: nonce\x00").derive(ikm)
        body = AESGCM(cek).encrypt(nonce, payload + b"\x02", None)
        return salt + struct.pack(">I", 4096) + bytes([len(as_pub)]) + as_pub + body

    def send(self, payload, topic=""):
        if not self.ok:
            return
        data = json.dumps(payload).encode()
        with self.lock:
            subs = list(self.subs)
        for sub in subs:
            threading.Thread(target=self._send_one, args=(sub, data, topic), daemon=True).start()

    def _send_one(self, sub, data, topic):
        try:
            ep = sub["endpoint"]
            u = urlparse(ep)
            body = self.encrypt(data, sub["keys"]["p256dh"], sub["keys"]["auth"])
            headers = {"TTL": "3600", "Content-Encoding": "aes128gcm", "Content-Type": "application/octet-stream",
                       "Urgency": "high", "Authorization": f"vapid t={self._jwt(f'{u.scheme}://{u.netloc}')}, k={self.public}"}
            if topic:
                headers["Topic"] = re.sub(r"[^A-Za-z0-9_-]", "", topic)[:32]
            req = urllib.request.Request(ep, data=body, headers=headers, method="POST")
            urllib.request.urlopen(req, timeout=15).close()
        except urllib.error.HTTPError as e:
            if e.code in (404, 410):  # the phone dropped this subscription
                self.unsubscribe(sub.get("endpoint"))
            log(f"push: {e.code} {e.reason}")
        except Exception as e:  # never let a push take the server down
            log(f"push failed: {e}")


# ---------------------------------------------------------------- the hub

def job_state(job):
    return P.read_json(P.CLAUDE_DIR / "jobs" / str(job) / "state.json", {}) or {}


def project_name(cwd):
    """The project a folder belongs to: a worktree is named after its repo (its branch shows apart)."""
    m = re.match(r"^(.*?)/\.(?:claude/)?worktrees/", cwd or "")
    path = Path(m.group(1) if m else cwd or "")
    return "~" if not cwd or path == P.HOME else path.name


def session_locked():
    """The lock screen is up (logind's LockedHint, which Noctalia sets)."""
    try:
        sid = subprocess.run(["loginctl", "show-user", str(os.getuid()), "-p", "Display", "--value"],
                             capture_output=True, text=True, timeout=5).stdout.strip()
        if not sid:
            return False
        out = subprocess.run(["loginctl", "show-session", sid, "-p", "LockedHint", "--value"],
                             capture_output=True, text=True, timeout=5).stdout.strip()
        return out == "yes"
    except (OSError, subprocess.SubprocessError):
        return False


def cheap_signature():
    """Changes when a session changes state, starts/ends, or a handoff comes or goes, without
    running `claude agents` (which costs half a second of CPU)."""
    parts = []
    for f in sorted((P.CLAUDE_DIR / "jobs").glob("*/state.json")):
        st = P.read_json(f, {}) or {}
        parts.append((f.parent.name, st.get("state"), st.get("tempo")))
    for f in sorted((P.CLAUDE_DIR / "sessions").glob("*.json")):  # status: busy / shell / waiting / idle
        parts.append((f.name, (P.read_json(f, {}) or {}).get("status")))
    parts.append(tuple(sorted(f.name for f in P.HANDOFFS.glob("*.json"))))
    try:
        parts.append(P.USAGE.stat().st_mtime_ns)
    except OSError:
        pass
    return hash(tuple(parts))


class Hub:
    def __init__(self, access):
        self.access = access
        self.event_queues = set()  # phones on the session list
        self.lock = threading.Lock()
        self.attaches = {}  # job id -> Attach
        self.viewers = {}  # job id -> set of queues
        self.transcripts = {}  # session id -> Transcript
        self.starters = {}  # starter id -> Starter
        self.shell = None  # the phone's Terminal (a Shell), while it runs
        self.snap_json = ""
        self.snap = {}
        self.launched = {}  # handoff files in done/, by session id, cached by dir mtime
        self._done_mtime = None
        contact = next(iter(access.logins), "plane@localhost")
        self.push = Push(contact)
        self.store = self._make_store()
        threading.Thread(target=self._janitor, daemon=True).start()

    # -- polling
    def _make_store(self):
        hub = self

        class WebStore(P.Store):
            def _loop(self):
                purged, last, sig = 0.0, 0.0, None
                while True:
                    now = time.time()
                    hub.mark_phone()
                    if now - purged > 3600:
                        if hasattr(P, "purge_discarded"):
                            P.purge_discarded()
                        purge_uploads()
                        purged = now
                    try:
                        s = cheap_signature()
                    except Exception:
                        s = None
                    watching = bool(hub.event_queues or hub.viewers)
                    if s != sig or (watching and now - last >= 2.5) or now - last >= 60:
                        sig = s
                        try:
                            self._refresh()
                        except Exception as exc:
                            self.error = f"refresh: {exc}"
                        last = time.time()
                        hub.changed()
                    if self._wake.wait(1.0):
                        sig = None
                    self._wake.clear()

        return WebStore(self.on_ready, self.on_waiting, lambda sid: None)

    def phone_open(self):
        return bool(self.event_queues or self.viewers)

    def mark_phone(self):
        """Tell the sidebar the app is open on a phone (it then skips its own notifications):
        a file touched every few seconds while a phone is connected, removed when none is."""
        flag = P.PHONE_FLAG
        try:
            if self.phone_open():
                if time.time() - getattr(self, "_flagged", 0) > 5:
                    flag.touch()
                    self._flagged = time.time()
            elif getattr(self, "_flagged", 0):
                flag.unlink(missing_ok=True)
                self._flagged = 0
        except OSError:
            pass

    def laptop_watching(self):
        """You're at the laptop looking at Plane: it's the focused window, the screen isn't
        locked, and there's been input lately. Plane is focused most of the time it's open,
        including after you walk away, so focus alone says nothing."""
        if (P.RUNTIME / "plane-away").exists():  # Noctalia's idle "plane-away": 2 min without input
            return False
        if session_locked():
            return False
        try:
            return json.loads(P.hypr("activewindow", "-j").stdout or "{}").get("class") == P.CLASS
        except (ValueError, AttributeError):
            return False

    def quiet_reason(self):
        if self.phone_open():
            return "the app is open on the phone"
        if self.laptop_watching():
            return "you're in Plane on the laptop"
        return ""

    def on_waiting(self, s, state):
        name = s.get("name") or s["id"]
        title = f"{name} needs you" if state == "needs you" else f"Your turn: {name}"
        why = self.quiet_reason()
        log(f"push skipped ({why}): {title}" if why else f"push: {title}")
        if why:
            return
        body = (s.get("needs") or "") if state == "needs you" else ""
        self.push.send({"title": title, "body": body[:240], "url": f"#s/{s['id']}", "tag": s["id"]}, topic=s["id"])

    def on_ready(self, h):
        why = self.quiet_reason()
        log(f"push skipped ({why}): handoff ready" if why else f"push: handoff ready: {h.get('title', '')}")
        if why:
            return
        self.push.send({"title": "Handoff ready", "body": h.get("title", ""), "url": f"#h/{Path(h['path']).stem}",
                        "tag": "h-" + Path(h["path"]).stem}, topic="h" + Path(h["path"]).stem[-30:])

    # -- the session list
    def launched_handoffs(self):
        try:
            mt = P.DONE.stat().st_mtime_ns
        except OSError:
            return {}
        if mt != self._done_mtime:
            out = {}
            for f in P.DONE.glob("*.json"):
                h = P.read_json(f)
                if h and h.get("launchedSessionId"):
                    out[h["launchedSessionId"]] = f
            self.launched, self._done_mtime = out, mt
        return self.launched

    def snapshot(self):
        st = getattr(self, "store", None)
        if st is None:  # the poller's first pass can finish before __init__ does
            return {}
        with st.lock:
            sessions, handoffs, lost = list(st.sessions), list(st.handoffs), list(st.lost)
            usage, error = dict(st.usage or {}), st.error
        launched = self.launched_handoffs()
        out = {"sessions": [], "handoffs": [], "lost": [], "error": error, "updatedAt": time.time(),
               "uploadsDir": str(uploads_dir()),
               "terminal": {"allowed": terminal_allowed(), "open": bool(self.shell and not self.shell.dead)},
               "push": {"ok": self.push.ok, "why": self.push.why}}
        for s in sessions:
            cwd = s.get("cwd", "")
            job = job_state(s["id"]) if s.get("bg") else {}
            out["sessions"].append({
                "id": s["id"], "name": s.get("name") or Path(cwd).name or s["id"], "state": P.session_state(s),
                "needs": s.get("needs", ""), "detail": job.get("detail", ""), "cwd": cwd.replace(str(P.HOME), "~", 1),
                "folder": project_name(cwd), "branch": s.get("branch", ""), "bg": bool(s.get("bg")),
                "stopped": bool(s.get("stopped")), "startedAt": s.get("startedAt"), "sessionId": s.get("sessionId"),
                "fromHandoff": s["id"] in launched})
        for h in handoffs:
            src = h.get("source") or {}
            out["handoffs"].append({
                "id": Path(h["path"]).stem, "title": h.get("title") or "handoff", "cwd": (h.get("cwd") or "").replace(
                    str(P.HOME), "~", 1), "folder": project_name(h.get("cwd") or ""), "ready": h.get("ready"),
                "from": src.get("name") or h.get("sourceName") or "", "prompt": h.get("prompt", ""),
                "createdAt": h.get("createdAt")})
        for r in lost:
            out["lost"].append({
                "id": r["sessionId"], "name": P.lost_label(r)[:80], "cause": r.get("cause"),
                "cwd": (r.get("cwd") or "").replace(str(P.HOME), "~", 1), "folder": project_name(r.get("cwd") or ""),
                "lastActive": r.get("lastActive") or r.get("seenAt"), "kind": r.get("kind"), "jobId": r.get("jobId"),
                "needs": r.get("needs", "")})
        fh, sd = usage.get("five_hour") or {}, usage.get("seven_day") or {}
        out["usage"] = {"five_hour": {"pct": fh.get("pct"), "resetsAt": fh.get("resetsAt")},
                        "seven_day": {"pct": sd.get("pct"), "resetsAt": sd.get("resetsAt")}}
        return out

    def changed(self):
        snap = self.snapshot()
        if not snap:
            return
        body = json.dumps({k: v for k, v in snap.items() if k != "updatedAt"}, sort_keys=True)
        with self.lock:
            self.snap = snap
            fresh = body != self.snap_json
            self.snap_json = body
            queues = list(self.event_queues)
        for q in queues:
            q.put(("state", snap) if fresh else ("tick", {"updatedAt": snap["updatedAt"]}))

    def find_session(self, job):
        with self.store.lock:
            return next((s for s in self.store.sessions if s.get("id") == job), None)

    def find_lost(self, session_id):
        with self.store.lock:
            return next((r for r in self.store.lost if r.get("sessionId") == session_id), None)

    # -- watching one session
    def transcript(self, session_id, cwd):
        with self.lock:
            t = self.transcripts.get(session_id)
            if t is None:
                t = self.transcripts[session_id] = Transcript(session_id, cwd)
            return t

    def attach(self, job, create=True):
        with self.lock:
            a = self.attaches.get(job)
            if a and not a.dead:
                return a
            if not create:
                return None
            if pyte is None:
                raise RuntimeError("python-pyte isn't installed, so Plane can't open the session")
            a = self.attaches[job] = Attach(self, job)
            for q in self.viewers.get(job, ()):
                a.listeners.add(q)
            log(f"attached {job}")
            return a

    def attach_gone(self, a):
        with self.lock:
            if self.attaches.get(a.job) is a:
                del self.attaches[a.job]
        log(f"detached {a.job}")

    def watch(self, job, q):
        with self.lock:
            self.viewers.setdefault(job, set()).add(q)
            a = self.attaches.get(job)
        if a:
            with a.lock:
                a.listeners.add(q)
        self.store.poke()

    def unwatch(self, job, q):
        with self.lock:
            vs = self.viewers.get(job, set())
            vs.discard(q)
            if not vs:
                self.viewers.pop(job, None)
            a = self.attaches.get(job)
        if a:
            with a.lock:
                a.listeners.discard(q)
                a.idle_since = time.time()
            if getattr(q, "term", False):
                threading.Thread(target=self._unterm, args=(a,), daemon=True).start()

    @staticmethod
    def _unterm(a):
        """Back to the size the prompt reader likes, unless a terminal view came right back
        (it reconnects on every rotation)."""
        time.sleep(2)
        with a.lock:
            if any(getattr(x, "term", False) for x in a.listeners):
                return
        if not a.dead:
            a.resize(100, 50)

    def _janitor(self):
        """Detach from sessions nobody has looked at for two minutes; forget old starters."""
        while True:
            time.sleep(15)
            now = time.time()
            with self.lock:
                idle = [a for job, a in self.attaches.items() if not self.viewers.get(job)
                        and now - max(a.idle_since, a.last_output) > 120]
                for k, s in list(self.starters.items()):
                    if s.dead and now - s.created > 900:
                        del self.starters[k]
            for a in idle:
                a.close()

    # -- the Terminal
    def open_shell(self, cols, rows):
        if not terminal_allowed():
            raise PermissionError("The terminal is turned off in ~/.config/plane/web.json.")
        with self.lock:
            if self.shell is None or self.shell.dead:
                self.shell = Shell(self, cols, rows)
            sh = self.shell
        self.changed()
        return sh

    def shell_gone(self, sh):
        with self.lock:
            if self.shell is sh:
                self.shell = None
        self.changed()

    def start(self, cwd, name="", prompt="", resume=None, handoff=None):
        if pyte is None:
            raise RuntimeError("python-pyte isn't installed")
        s = Starter(self, cwd, name, prompt, resume, handoff)
        with self.lock:
            self.starters[s.id] = s
        return s


# ---------------------------------------------------------------- actions (the sidebar's, minus the curses)

def run_claude(*args):
    r = subprocess.run([P.claude_bin(), *args], cwd=str(P.HOME), capture_output=True, text=True, timeout=60)
    return r.returncode, (r.stdout + r.stderr).strip()


def first_line(text, fallback):
    return (text.splitlines()[0][:160] if text else fallback)


def op_close(hub, job):
    a = hub.attach(job, create=False)
    if a:
        a.close()
    code, out = run_claude("rm", job)
    if code != 0:
        raise RuntimeError("Not closed: " + first_line(out, "claude rm failed"))
    P.ledger_end(job, "closed")
    hub.store.poke()


def op_pause(hub, job):
    a = hub.attach(job, create=False)
    if a:
        a.close()
    code, out = run_claude("stop", job)
    if code != 0:
        raise RuntimeError("Not paused: " + first_line(out, "claude stop failed"))
    hub.store.poke()


def op_requeue(hub, job):
    f = hub.launched_handoffs().get(job)
    if not f or not f.exists():
        raise RuntimeError("This session didn't come from a handoff.")
    op_close(hub, job)  # still running: requeuing first could start it twice
    h = P.read_json(f) or {}
    for k in ("launchedAt", "launchedSessionId"):
        h.pop(k, None)
    P.write_json(P.HANDOFFS / f.name, h)
    f.unlink(missing_ok=True)
    hub._done_mtime = None
    hub.store.poke()


def handoff_file(hid):
    if not re.fullmatch(r"[\w.-]+", hid or ""):
        raise LookupError("No such handoff.")
    return P.HANDOFFS / f"{hid}.json"


def op_discard(hub, hid):
    src = handoff_file(hid)
    dest_dir = getattr(P, "DISCARDED", None)
    try:
        if dest_dir:
            dest_dir.mkdir(parents=True, exist_ok=True)
            os.replace(src, dest_dir / src.name)
            os.utime(dest_dir / src.name)  # the 14 days count from now
        else:
            src.unlink()
    except FileNotFoundError:
        raise LookupError("That handoff is already gone.")
    hub.store.poke()


def op_start_handoff(hub, hid):
    src = handoff_file(hid)
    done = P.DONE / src.name
    done.parent.mkdir(parents=True, exist_ok=True)
    try:  # claim it first: a second tap finds nothing and can't start it twice
        os.replace(src, done)
    except FileNotFoundError:
        raise LookupError("That handoff was already started.")
    h = P.read_json(done) or {}
    h["launchedAt"] = time.time()
    P.write_json(done, h)
    cwd = os.path.expanduser(h.get("cwd") or str(P.HOME))
    if not Path(cwd).is_dir():
        h.pop("launchedAt", None)
        P.write_json(src, h)
        done.unlink(missing_ok=True)
        raise RuntimeError(f"Its folder is gone: {cwd}")
    hub.store.poke()
    return hub.start(cwd, h.get("title", "")[:60], h.get("prompt", ""), handoff=str(done))


def op_resume_lost(hub, session_id):
    r = hub.find_lost(session_id)
    if not r:
        raise LookupError("That session isn't waiting to be resumed any more.")
    cwd = r.get("cwd") or str(P.HOME)
    if not Path(cwd).is_dir():
        raise RuntimeError(f"Its folder is gone: {cwd}")
    job = r.get("jobId")
    if r.get("kind") == "bg" and job and (P.CLAUDE_DIR / "jobs" / job).is_dir():
        hub.attach(job)  # attaching brings its background job back
        hub.store.poke()
        return {"open": job}
    name = r.get("name") or ""
    s = hub.start(cwd, name, "", resume=r["sessionId"])
    return {"starter": s.id}


def op_close_lost(hub, session_id):
    r = hub.find_lost(session_id)
    if not r:
        raise LookupError("That session is already gone.")
    job = r.get("jobId")
    if r.get("kind") == "bg" and job and (P.CLAUDE_DIR / "jobs" / job).is_dir():
        run_claude("rm", job)  # or it would come back under paused
    Path(r["path"]).unlink(missing_ok=True)
    hub.store.poke()


UPLOAD_MAX = 60_000_000
UPLOAD_KEEP_DAYS = 30
FILE_TYPES = {".jpg": "image/jpeg", ".jpeg": "image/jpeg", ".png": "image/png", ".gif": "image/gif",
              ".webp": "image/webp", ".pdf": "application/pdf", ".txt": "text/plain; charset=utf-8"}


def uploads_dir():
    return P.DATA / "uploads"


def purge_uploads():
    """Uploads are for the conversation they were sent in: drop them after a month."""
    cutoff = time.time() - UPLOAD_KEEP_DAYS * 86400
    for day in uploads_dir().glob("*"):
        for f in day.glob("*"):
            try:
                if f.stat().st_mtime < cutoff:
                    f.unlink()
            except OSError:
                pass
        try:
            day.rmdir()  # only when empty
        except OSError:
            pass


def folders(q):
    if not q:
        hidden = str(P.CLAUDE_DIR) + "/"  # job scratch folders aren't projects
        return [{"path": p.replace(str(P.HOME), "~", 1), "name": Path(p).name}
                for p in P.recent_projects(12) if not p.startswith(hidden)][:8]
    base = os.path.expanduser(q)
    parent, stem = os.path.split(base)
    try:
        names = sorted(e.name for e in os.scandir(parent or ".") if e.is_dir() and e.name.startswith(stem)
                       and (stem.startswith(".") or not e.name.startswith(".")))[:8]
    except OSError:
        return []
    return [{"path": os.path.join(parent, n).replace(str(P.HOME), "~", 1) + "/", "name": n} for n in names]


# ---------------------------------------------------------------- HTTP

TYPES = {".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8", ".css": "text/css; charset=utf-8",
         ".webmanifest": "application/manifest+json", ".png": "image/png", ".svg": "image/svg+xml",
         ".json": "application/json"}
KEYS = {"esc": b"\x1b", "tab": b"\t", "stab": b"\x1b[Z", "up": b"\x1b[A", "down": b"\x1b[B", "left": b"\x1b[D",
        "right": b"\x1b[C", "enter": b"\r", "backspace": b"\x7f"}


class Handler(BaseHTTPRequestHandler):
    hub = None
    access = None
    server_version = "plane"
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        if os.environ.get("PLANE_DEBUG"):
            log(fmt % args)

    # -- helpers
    def send_json(self, data, code=200):
        body = json.dumps(data).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def fail(self, code, text):
        self.send_json({"error": text}, code)

    def body(self):
        n = int(self.headers.get("Content-Length") or 0)
        if n > 1_000_000:
            raise ValueError("too big")
        return json.loads(self.rfile.read(n) or b"{}") if n else {}

    def allowed(self):
        if not self.access.check(self.headers):
            self.fail(403, "This app is only for its owner, over Tailscale.")
            return False
        return True

    def same_origin(self):
        origin = self.headers.get("Origin")
        host = self.headers.get("X-Forwarded-Host") or self.headers.get("Host") or ""
        if self.headers.get("X-Plane") != "1":
            return False
        return origin is None or urlparse(origin).netloc == host

    # -- GET
    def do_GET(self):
        url = urlparse(self.path)
        path = unquote(url.path)
        if not self.allowed():
            return
        q = {k: v[-1] for k, v in parse_qs(url.query).items()}
        try:
            if path == "/api/state":
                return self.send_json(self.hub.snap or self.hub.snapshot())
            if path == "/api/events":
                return self.events()
            if path.startswith("/api/file/"):
                return self.uploaded_file(path[len("/api/file/"):])
            if path == "/api/folders":
                return self.send_json({"folders": folders(q.get("q", ""))})
            if path == "/api/push/key":
                return self.send_json({"key": self.hub.push.public if self.hub.push.ok else None,
                                       "why": self.hub.push.why})
            m = re.fullmatch(r"/api/session/([\w-]+)/stream", path)
            if m:
                return self.session_stream(m.group(1), q)
            m = re.fullmatch(r"/api/start/([0-9a-f]+)/stream", path)
            if m:
                return self.starter_stream(m.group(1))
            if path == "/api/shell/stream":
                return self.shell_stream()
            if path.startswith("/api/"):
                return self.fail(404, "Not found")
            return self.static(path)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def static(self, path):
        if path in ("", "/"):
            path = "/index.html"
        f = (WEB / path.lstrip("/")).resolve()
        if WEB.resolve() not in f.parents or not f.is_file():
            return self.fail(404, "Not found")
        data = f.read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", TYPES.get(f.suffix, "application/octet-stream"))
        self.send_header("Cache-Control", "no-cache" if f.suffix in (".html", ".js", ".css", ".webmanifest")
                         else "max-age=86400")
        if f.name == "sw.js":
            self.send_header("Service-Worker-Allowed", "/")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def sse_start(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Accel-Buffering", "no")
        self.send_header("Connection", "keep-alive")
        self.end_headers()
        self.close_connection = True

    def sse(self, kind, data):
        self.wfile.write(f"event: {kind}\ndata: {json.dumps(data)}\n\n".encode())
        self.wfile.flush()

    def events(self):
        hub = self.hub
        q = queue.Queue()
        self.sse_start()
        with hub.lock:
            hub.event_queues.add(q)
        hub.store.poke()
        try:
            self.sse("state", hub.snap or hub.snapshot())
            while True:
                try:
                    kind, data = q.get(timeout=15)
                    self.sse(kind, data)
                except queue.Empty:
                    self.wfile.write(b": ping\n\n")
                    self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass
        finally:
            with hub.lock:
                hub.event_queues.discard(q)

    def session_stream(self, job, q):
        """Everything about one session the chat or terminal needs: the conversation as it
        grows, the menu on Claude's screen, and (terminal) the raw screen bytes."""
        hub = self.hub
        s = hub.find_session(job)
        lost = None if s else next((r for r in (hub.store.lost or []) if r.get("jobId") == job), None)
        session_id = (s or {}).get("sessionId") or (lost or {}).get("sessionId") or job_state(job).get("sessionId")
        if not session_id:
            return self.fail(404, "No such session.")
        cwd = (s or lost or {}).get("cwd") or ""
        live = bool(s and s.get("bg") and not s.get("stopped"))
        wake = q.get("wake") == "1"
        term = q.get("term") == "1"
        tq = queue.Queue()
        tq.term = term
        self.sse_start()
        t = hub.transcript(session_id, cwd)
        t.refresh()
        try:
            self.sse("chat", t.view())
            a = None
            hub.watch(job, tq)
            if (live or wake) and pyte is not None:
                try:
                    a = hub.attach(job)
                except Exception as e:
                    self.sse("info", {"error": str(e)})
            elif pyte is None and (live or wake):
                self.sse("info", {"error": "python-pyte isn't installed, so prompts and the terminal are off."})
            if a and term:
                a.resize(q.get("cols", 60), q.get("rows", 30))
            self.sse("screen", a.screen_state() if a else {"prompt": None, "attached": False})
            last_poll = last_ping = 0.0
            while True:
                try:
                    kind, data = tq.get(timeout=0.5)
                    self.sse(kind, data)
                except queue.Empty:
                    pass
                now = time.time()
                if now - last_ping >= 15:
                    last_ping = now
                    self.wfile.write(b": ping\n\n")
                    self.wfile.flush()
                if now - last_poll >= 0.7:
                    last_poll = now
                    start = t.refresh()
                    if start is not None:
                        self.sse("chat", t.view(start))
                    if not a:
                        a = hub.attach(job, create=False)  # a send from this phone may have attached
                        if a:
                            self.sse("screen", a.screen_state())
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass
        finally:
            hub.unwatch(job, tq)

    def shell_stream(self):
        """The Terminal's screen: what it printed lately (to redraw), then everything live."""
        sh = self.hub.shell
        q = queue.Queue()
        self.sse_start()
        try:
            if not sh or sh.dead:
                self.sse("ended", {"code": None, "none": True})
                return
            old, new = sh.join(q)
            self.sse("hello", {"old": base64.b64encode(old).decode(), "new": base64.b64encode(new).decode(),
                               "startedAt": sh.started})
            while True:
                try:
                    kind, data = q.get(timeout=15)
                    if kind == "out":  # send what's queued in one go: fewer, bigger writes
                        chunks = [base64.b64decode(data["b64"])]
                        ended = None
                        while len(chunks) < 256:
                            try:
                                k2, d2 = q.get_nowait()
                            except queue.Empty:
                                break
                            if k2 == "out":
                                chunks.append(base64.b64decode(d2["b64"]))
                            else:
                                ended = (k2, d2)
                                break
                        self.sse("out", {"b64": base64.b64encode(b"".join(chunks)).decode()})
                        if ended:
                            self.sse(*ended)
                            return
                    else:
                        self.sse(kind, data)
                        if kind == "ended":
                            return
                except queue.Empty:
                    self.wfile.write(b": ping\n\n")
                    self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass
        finally:
            if sh:
                sh.leave(q)

    def shell_action(self, action, b):
        hub = self.hub
        if not terminal_allowed():
            return self.fail(403, "The terminal is turned off in ~/.config/plane/web.json.")
        if action == "open":
            sh = hub.open_shell(b.get("cols", 80), b.get("rows", 24))
            return self.send_json(sh.info())
        sh = hub.shell
        if action == "end":
            if sh:
                sh.end()
            return self.send_json({"ok": True})
        if not sh or sh.dead:
            raise LookupError("The terminal has ended.")
        if action == "input":
            data = b.get("data")
            if not isinstance(data, str) or len(data) > 200_000:
                return self.fail(400, "Bad input.")
            sh.write(data)
        elif action == "resize":
            sh.resize(b.get("cols", 80), b.get("rows", 24))
        else:
            return self.fail(404, "Not found")
        return self.send_json({"ok": True})

    def starter_stream(self, sid):
        s = self.hub.starters.get(sid)
        if not s:
            return self.fail(404, "That start is over.")
        q = queue.Queue()
        self.sse_start()
        with s.lock:
            s.listeners.add(q)
        try:
            self.sse("screen", s.screen_state())
            while True:
                try:
                    kind, data = q.get(timeout=15)
                    self.sse(kind, data)
                except queue.Empty:
                    self.wfile.write(b": ping\n\n")
                    self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass
        finally:
            with s.lock:
                s.listeners.discard(q)

    # -- POST
    def do_POST(self):
        if not self.allowed():
            return
        if not self.same_origin():
            return self.fail(403, "Bad origin.")
        path = unquote(urlparse(self.path).path)
        hub = self.hub
        if path == "/api/upload":
            return self.upload()
        try:
            b = self.body()
            m = re.fullmatch(r"/api/shell/(\w+)", path)
            if m:
                return self.shell_action(m.group(1), b)
            m = re.fullmatch(r"/api/session/([\w-]+)/(\w+)", path)
            if m:
                return self.session_action(m.group(1), m.group(2), b)
            if path == "/api/start":
                cwd = os.path.abspath(os.path.expanduser(str(b.get("cwd") or "~")))
                if not Path(cwd).is_dir():
                    return self.fail(400, f"No such folder: {cwd.replace(str(P.HOME), '~', 1)}")
                s = hub.start(cwd, str(b.get("name") or "").strip(), str(b.get("prompt") or "").strip())
                return self.send_json({"starter": s.id})
            m = re.fullmatch(r"/api/start/([0-9a-f]+)/choose", path)
            if m:
                s = hub.starters.get(m.group(1))
                if not s or s.dead:
                    return self.fail(404, "That start is over.")
                s.choose(int(b["index"]), str(b.get("sig")))
                return self.send_json({"ok": True})
            m = re.fullmatch(r"/api/handoff/([\w.-]+)/(start|discard)", path)
            if m:
                if m.group(2) == "start":
                    return self.send_json({"starter": op_start_handoff(hub, m.group(1)).id})
                op_discard(hub, m.group(1))
                return self.send_json({"ok": True})
            m = re.fullmatch(r"/api/lost/([\w-]+)/(resume|close)", path)
            if m:
                if m.group(2) == "resume":
                    return self.send_json(op_resume_lost(hub, m.group(1)))
                op_close_lost(hub, m.group(1))
                return self.send_json({"ok": True})
            if path == "/api/push/subscribe":
                hub.push.subscribe(b.get("subscription"))
                return self.send_json({"ok": True})
            if path == "/api/push/unsubscribe":
                hub.push.unsubscribe(str(b.get("endpoint", "")))
                return self.send_json({"ok": True})
            if path == "/api/push/test":
                hub.push.send({"title": "Plane", "body": "Notifications work.", "url": "#", "tag": "test"})
                return self.send_json({"ok": hub.push.ok, "why": hub.push.why})
            return self.fail(404, "Not found")
        except LookupError as e:
            return self.fail(409, str(e).strip("'\""))
        except PermissionError as e:
            return self.fail(403, str(e))
        except (ValueError, KeyError, TypeError, IndexError) as e:
            return self.fail(400, f"Bad request: {e}")
        except Exception as e:
            return self.fail(500, str(e))

    def upload(self):
        """A photo or file from the phone, saved under ~/.local/share/plane/uploads/<day>/.
        The phone then sends its path in the message, for Claude to read."""
        try:
            n = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            n = 0
        if n <= 0:
            return self.fail(400, "That file is empty.")
        if n > UPLOAD_MAX:
            return self.fail(413, f"That file is too big ({UPLOAD_MAX // 1_000_000} MB at most).")
        name = parse_qs(urlparse(self.path).query).get("name", ["file"])[-1]
        safe = re.sub(r"[^\w.-]+", "_", Path(name).name).strip("._")[:80] or "file"
        day = uploads_dir() / time.strftime("%Y-%m-%d")
        day.mkdir(parents=True, exist_ok=True)
        f = day / f"{time.strftime('%H%M%S')}-{secrets.token_hex(2)}-{safe}"
        left = n
        try:
            with open(f, "wb") as out:
                while left > 0:
                    chunk = self.rfile.read(min(left, 1 << 20))
                    if not chunk:
                        raise OSError("the upload stopped part way")
                    out.write(chunk)
                    left -= len(chunk)
        except OSError as e:
            f.unlink(missing_ok=True)
            return self.fail(400, f"Upload failed: {e}")
        rel = f.relative_to(uploads_dir()).as_posix()
        return self.send_json({"path": str(f), "name": safe, "size": n, "url": f"/api/file/{rel}"})

    def uploaded_file(self, rel):
        f = (uploads_dir() / rel).resolve()
        if uploads_dir().resolve() not in f.parents or not f.is_file():
            return self.fail(404, "Not found")
        data = f.read_bytes()
        self.send_response(200)
        self.send_header("Content-Type", FILE_TYPES.get(f.suffix.lower(), "application/octet-stream"))
        self.send_header("Cache-Control", "private, max-age=86400")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def session_action(self, job, action, b):
        hub = self.hub
        if action == "pause":
            op_pause(hub, job)
        elif action == "close":
            op_close(hub, job)
        elif action == "requeue":
            op_requeue(hub, job)
        elif action == "send":
            text = str(b.get("text") or "")
            if not text.strip():
                return self.fail(400, "Type a message first.")
            a = hub.attach(job)
            a.wait_ready()
            typed = a.draft() if not a.prompt else ""
            if typed:  # the message box is shared: don't mix your laptop text into this one
                short = typed if len(typed) <= 40 else typed[:40] + "…"
                raise LookupError(f"Not sent: the laptop has unsent text in this session's message box "
                                  f"(“{short}”). Send or clear it there, then try again.")
            a.send_text(text)
            hub.store.poke()
        elif action == "choose":
            a = hub.attach(job, create=False)
            if not a:
                raise LookupError("The question isn't showing any more.")
            a.choose(int(b["index"]), str(b.get("sig")))
        elif action == "key":
            a = hub.attach(job)
            a.wait_ready()
            key = b.get("key")
            if key == "text":
                a.send_text(str(b.get("text") or ""), enter=bool(b.get("enter")))
            elif key in KEYS:
                a.write(KEYS[key])
            elif re.fullmatch(r"[0-9]", str(key)):
                a.write(str(key))
            else:
                return self.fail(400, "Unknown key.")
        elif action == "wake":
            hub.attach(job)
            hub.store.poke()
        else:
            return self.fail(404, "Not found")
        return self.send_json({"ok": True})


def serve(plane_module=None, port=PORT):
    global P
    P = plane_module or load_plane()
    access = Access()
    hub = Hub(access)
    Handler.hub, Handler.access = hub, access
    httpd = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    httpd.daemon_threads = True
    who = ", ".join(sorted(access.logins)) or "nobody (no Tailscale login found)"
    log(f"Plane web on http://127.0.0.1:{port} · allowed: {who}"
        + (f" · devices: {', '.join(access.device_names)}" if access.device_names else "")
        + (" · DEV: no login check" if access.dev else ""))
    if pyte is None:
        log("python-pyte is missing: the chat works, prompts and the terminal don't")
    if not hub.push.ok:
        log(f"push is off: {hub.push.why}")

    def stop(*_):
        with hub.lock:
            attaches = list(hub.attaches.values())
        for a in attaches:
            a.close()
        if hub.shell:
            hub.shell.end()
        try:
            P.PHONE_FLAG.unlink(missing_ok=True)
        except (OSError, AttributeError):
            pass
        os._exit(0)

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    httpd.serve_forever()


if __name__ == "__main__":
    import argparse
    ap = argparse.ArgumentParser(description="Plane on your phone")
    ap.add_argument("--port", type=int, default=PORT)
    serve(port=ap.parse_args().port)
