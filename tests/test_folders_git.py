"""Handoff folders (project › folder › handoff) and the Source Control section's git work.

  python3 tests/test_folders_git.py

The git tests build a throwaway repo, a clone and a bare "GitHub" in a temp folder, so they
need git but no network.
"""
import importlib.machinery
import importlib.util
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
TMP = Path(tempfile.mkdtemp(prefix="plane-test-"))
os.environ["HOME"] = str(TMP / "home")  # Plane's data files land here, not in the real ~
(TMP / "home").mkdir()
loader = importlib.machinery.SourceFileLoader("plane", str(HERE.parent / "plane"))
P = importlib.util.module_from_spec(importlib.util.spec_from_loader("plane", loader))
loader.exec_module(P)

FARM, SITE = "/code/farm app", "/code/site"


def h(hid, cwd, created, folder=None, ready=True):
    return {"id": hid, "cwd": cwd, "createdAt": created, "ready": ready, **({"folder": folder} if folder else {})}


def test_tree_lists_projects_first_and_only_folders_with_handoffs():
    hs = [h("a", FARM, 5, "Orders"), h("b", FARM, 4, "Printers"), h("c", FARM, 3), h("d", SITE, 1),
          h("e", FARM, 2, "Orders", ready=False)]
    folders = {FARM: ["Dashboard", "Orders", "Printers"]}  # Dashboard is empty: not listed
    rows = P.handoff_tree(hs, folders, lambda k: False)
    assert [(k, o["name"], d) for k, o, d in rows] == [("project", "farm app", 0), ("project", "site", 0)], rows
    farm = rows[0][1]
    assert farm["count"] == 4 and farm["folders"] == 2 and farm["waiting"] == 1, farm

    opened = {("project", FARM), ("folder", f"{FARM}\nOrders")}
    rows = P.handoff_tree(hs, folders, lambda k: k in opened)
    got = [(k, o.get("name") or o["id"], d) for k, o, d in rows]
    assert got == [("project", "farm app", 0),
                   ("folder", "Orders", 1), ("handoff", "a", 2), ("handoff", "e", 2),
                   ("folder", "Printers", 1),          # closed: its handoff isn't listed
                   ("handoff", "c", 1),                # no folder: under the project, after folders
                   ("project", "site", 0)], got


def test_tree_keeps_folders_in_project_order_and_unknown_ones_after():
    hs = [h("a", FARM, 1, "Zeta"), h("b", FARM, 2, "Printers"), h("c", FARM, 3, "alpha"), h("d", FARM, 4, "Orders")]
    rows = P.handoff_tree(hs, {FARM: ["Orders", "Printers"]}, lambda k: k == ("project", FARM))
    assert [o["name"] for k, o, _ in rows if k == "folder"] == ["Orders", "Printers", "alpha", "Zeta"]


def test_tree_rank_holds_project_order():
    hs = [h("a", FARM, 1), h("b", SITE, 9)]
    assert [o["id"] for _, o, _ in P.handoff_tree(hs, {}, lambda k: False)] == [SITE, FARM]
    held = {FARM: 0, SITE: 1}
    rows = P.handoff_tree(hs, {}, lambda k: False, rank=lambda p, _hs: held[p])
    assert [o["id"] for _, o, _ in rows] == [FARM, SITE]


def test_folder_files_add_rename():
    P.add_project_folder(FARM, "Orders")
    P.add_project_folder(FARM, "Printers")
    P.add_project_folder(FARM, "Orders")  # no duplicate
    assert P.project_folders(FARM) == ["Orders", "Printers"]
    assert P.folder_name(FARM, "  orders ") == "Orders"
    assert P.folder_name(FARM, "New  one") == "New one"
    P.HANDOFFS.mkdir(parents=True, exist_ok=True)
    P.write_json(P.HANDOFFS / "x.json", h("x", FARM, 1, "Printers"))
    P.write_json(P.HANDOFFS / "y.json", h("y", SITE, 1, "Printers"))  # another project: untouched
    P.rename_project_folder(FARM, "Printers", "Orders")  # onto an existing one: they merge
    assert P.project_folders(FARM) == ["Orders"]
    assert P.read_json(P.HANDOFFS / "x.json")["folder"] == "Orders"
    assert P.read_json(P.HANDOFFS / "y.json")["folder"] == "Printers"


def test_handoff_add_with_folder():
    env = dict(os.environ, HOME=str(TMP / "home"))
    r = subprocess.run([sys.executable, str(HERE.parent / "plane"), "handoff", "add", "--title", "Fix it",
                        "--cwd", FARM, "--folder", "orders"], input="do the thing", text=True,
                       capture_output=True, env=env)
    assert r.returncode == 0, r.stderr
    assert "farm app › Orders" in r.stdout, r.stdout
    made = [P.read_json(f) for f in P.HANDOFFS.glob("*fix-it.json")]
    assert made and made[0]["folder"] == "Orders", made
    r = subprocess.run([sys.executable, str(HERE.parent / "plane"), "handoff", "folders", "--cwd", FARM],
                       text=True, capture_output=True, env=env)
    assert r.stdout.split("\n")[0] == "Orders", r.stdout


def test_parse_git_status():
    out = ("# branch.oid abc\n# branch.head main\n# branch.upstream origin/main\n# branch.ab +2 -3\n"
           "1 .M N... 100644 100644 100644 a b plane\n? new.txt\n")
    assert P.parse_git_status(out) == {"branch": "main", "upstream": "origin/main", "ahead": 2, "behind": 3,
                                       "changes": 2}
    assert P.parse_git_status("# branch.head (detached)\n")["branch"] == ""
    assert P.parse_git_status("# branch.head topic\n")["upstream"] == ""


def test_git_error_skips_hints():
    out = ("To github.com:me/repo.git\n ! [rejected]        main -> main (fetch first)\n"
           "error: failed to push some refs to 'github.com:me/repo.git'\nhint: Updates were rejected\n")
    assert P.git_error(out).startswith("! [rejected]"), P.git_error(out)
    assert P.git_error("") == "git failed"


def sh(cwd, *args):
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True,
                   env=dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.com",
                            GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.com"))


def wait_idle(repos, root, secs=20):
    end = time.time() + secs
    while time.time() < end:
        with repos.lock:
            r = repos.repos.get(root)
            if r and not r.get("busy"):
                return dict(r)
        time.sleep(0.05)
    raise AssertionError("git action didn't finish")


def test_repos_pull_push_sync_publish():
    hub = TMP / "hub.git"
    sh(TMP, "init", "--bare", "-b", "main", str(hub))
    mine, other = TMP / "mine", TMP / "other"
    for d in (mine, other):
        sh(TMP, "clone", str(hub), str(d))
        sh(d, "config", "pull.rebase", "false")
    (mine / "a.txt").write_text("1")
    sh(mine, "add", ".")
    sh(mine, "commit", "-m", "one")
    sh(mine, "push", "-u", "origin", "main")
    sh(other, "pull")
    wt = TMP / "mine-wt"  # a session in a worktree still syncs the main checkout
    sh(mine, "worktree", "add", "-b", "side", str(wt))

    said = []
    repos = P.Repos.__new__(P.Repos)  # no polling thread: drive it by hand
    import threading
    repos.lock, repos.repos, repos.roots = threading.Lock(), {}, {}
    repos.candidates, repos.on_done = (lambda: [str(wt), str(TMP / "nope")]), (lambda t, tone: said.append((t, tone)))
    repos._refresh()
    root = str(mine)
    assert list(repos.repos) == [root], repos.repos
    assert repos.repos[root]["upstream"] == "origin/main"

    # someone else pushed: ↓1 after a fetch, then pull brings it down
    (other / "b.txt").write_text("2")
    sh(other, "add", ".")
    sh(other, "commit", "-m", "two")
    sh(other, "push")
    assert repos.run(root, "fetch")
    assert not repos.run(root, "fetch")  # busy: a second one isn't started
    r = wait_idle(repos, root)
    assert r["behind"] == 1 and r["ahead"] == 0, r
    repos.run(root, "pull")
    r = wait_idle(repos, root)
    assert r["behind"] == 0 and (mine / "b.txt").exists(), r
    assert said[-1] == ("mine: pulled 1", "info"), said

    # a local commit: ↑1, and sync pushes it
    (mine / "c.txt").write_text("3")
    sh(mine, "add", ".")
    sh(mine, "commit", "-m", "three")
    repos._status(root)
    assert repos.repos[root]["ahead"] == 1
    (mine / "dirty.txt").write_text("x")
    repos._status(root)
    assert repos.repos[root]["changes"] == 1
    repos.run(root, "sync")
    r = wait_idle(repos, root)
    assert (r["ahead"], r["behind"]) == (0, 0), r
    assert said[-1] == ("mine: in sync", "info"), said

    # a push GitHub rejects shows git's reason, not a hang
    (other / "d.txt").write_text("4")
    sh(other, "add", ".")
    sh(other, "commit", "-m", "four")
    sh(other, "pull", "--no-edit")
    sh(other, "push")
    (mine / "e.txt").write_text("5")
    sh(mine, "add", "e.txt")
    sh(mine, "commit", "-m", "five")
    repos.run(root, "push")
    r = wait_idle(repos, root)
    assert r["error"].startswith("! [rejected]"), r
    assert said[-1][1] == "bad" and "push failed" in said[-1][0], said

    # a branch with no upstream: push publishes it
    sh(mine, "checkout", "-q", "-b", "topic")
    repos._status(root)
    assert repos.repos[root]["upstream"] == ""
    repos.run(root, "pull")
    wait_idle(repos, root)
    assert said[-1][1] == "warn", said
    repos.run(root, "push")
    r = wait_idle(repos, root)
    assert r["upstream"] == "origin/topic" and said[-1] == ("mine: published", "info"), (r, said)


def test_repos_loop_survives_a_bad_pass():
    import threading
    repos = P.Repos.__new__(P.Repos)
    repos.lock, repos.repos, repos.roots, repos._wake = threading.Lock(), {}, {}, threading.Event()
    calls = []

    def bad():
        calls.append(1)
        raise RuntimeError("boom")
    repos.candidates, repos.on_done = bad, (lambda *a: None)
    t = threading.Thread(target=repos._loop, daemon=True)
    t.start()
    repos.poke()
    time.sleep(0.2)
    repos.poke()
    time.sleep(0.2)
    assert t.is_alive() and len(calls) >= 2, calls


if __name__ == "__main__":
    for name, fn in list(globals().items()):
        if name.startswith("test_"):
            fn()
            print("ok", name)
