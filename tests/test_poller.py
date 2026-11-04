"""The thread that keeps the phone up to date has to survive anything one pass throws at it.

On 2026-11-03 `claude agents` listed a session with no id. snapshot() raised KeyError, which
ended the thread, while the server went on answering with what it had: every list on the phone
froze and pushes stopped, and the service still looked healthy.

  python3 tests/test_poller.py
"""
import sys
import threading
import time
import types
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.modules.setdefault("pyte", None)
import planeweb  # noqa: E402

planeweb.P = planeweb.load_plane()


def bare_hub():
    """A Hub with no network, threads or files of its own."""
    hub = planeweb.Hub.__new__(planeweb.Hub)
    hub.event_queues, hub.viewers = set(), {}
    hub.lock = threading.Lock()
    hub.shell, hub.snap, hub.snap_json = None, {}, ""
    hub.push = types.SimpleNamespace(ok=True, why="")
    hub.launched_handoffs = lambda: {}
    hub.mark_phone = lambda: None
    return hub


def test_snapshot_skips_a_session_with_no_id():
    hub = bare_hub()
    hub.store = types.SimpleNamespace(
        lock=threading.Lock(), handoffs=[], lost=[], usage={}, error="",
        sessions=[{"kind": "interactive", "bg": False, "cwd": "/tmp/a", "pid": 1},
                  {"id": "abc", "kind": "background", "bg": False, "cwd": "/tmp/b", "pid": 2}])
    snap = hub.snapshot()
    assert [s["id"] for s in snap["sessions"]] == ["abc"], snap["sessions"]


def test_a_failed_pass_does_not_end_the_poller():
    planeweb.purge_uploads = lambda: None
    planeweb.P.purge_discarded = lambda: None
    planeweb.cheap_signature = lambda: "same"
    planeweb.P.Store._refresh = lambda self: None
    planeweb.log = lambda text: None

    hub = bare_hub()
    calls = []

    def changed():
        calls.append(threading.current_thread())
        if len(calls) == 1:
            raise KeyError("id")

    hub.changed = changed
    store = hub._make_store()

    deadline = time.time() + 5
    while not calls and time.time() < deadline:
        time.sleep(0.05)
    assert calls, "the poller never ran"
    time.sleep(0.2)
    assert calls[0].is_alive(), "the first failure ended the poller"

    store.poke()  # the next pass should run, and this time succeed
    while len(calls) < 2 and time.time() < deadline:
        store.poke()
        time.sleep(0.05)
    assert len(calls) >= 2, f"no pass ran after the failure ({len(calls)} calls)"


if __name__ == "__main__":
    threading.excepthook = lambda args: None  # the old code's dying thread would print here
    fails = 0
    for name, fn in sorted(globals().copy().items()):
        if name.startswith("test_"):
            try:
                fn()
                print("ok  ", name)
            except (AssertionError, KeyError) as e:
                fails += 1
                print("FAIL", name, repr(e))
    sys.exit(1 if fails else 0)
