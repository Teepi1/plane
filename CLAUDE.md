# Plane

Notes for Claude Code. Only things the code doesn't show. See README.md for what Plane does.

## Commands

- Tests (each file runs on its own, no pytest):
  `for t in tests/test_*.py; do python3 "$t" || echo "FAIL $t"; done`
- Syntax check after editing: `python3 -m py_compile plane planeweb.py`
- `test_push.py` needs python-cryptography. The phone app needs python-pyte too. `plane` itself is standard library only; keep it that way.

## Gotchas

- `~/.local/bin/plane` is a symlink into the main checkout, and the phone service runs from there too. Merging to main is shipping.
- A running sidebar doesn't pick up edits. Reload it before asking the user to test:
  `kitty @ --to unix:$XDG_RUNTIME_DIR/plane-kitty.sock send-text --match cmdline:sidebar R`
- Hook commands (`plane track ...`, `plane statusline`) must print nothing to stdout. Anything they print lands in the user's conversation.
- To check the sidebar without touching the user's window, render it with pyte against real data and set `KITTY_LISTEN_ON` to a socket that doesn't exist.
- Menu parsing reads real Claude Code screens. When it breaks after a Claude Code update, capture a real screen with pyte first (phone width is about 44 columns), add it to `tests/screens/`, then fix.
- The phone poller thread must survive any one bad pass. A thread that dies leaves the server answering with stale data and looking healthy (see `tests/test_poller.py`).
- This repo is public. Don't commit personal hostnames, device names, emails or tailnet details. Tests use fakes like `my-iphone.tail.ts.net`.
