"""Who may use the phone app: checked against a fake `tailscale status`.

  python3 tests/test_access.py
"""
import json
import os
import sys
import tempfile
import types
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.modules.setdefault("pyte", types.ModuleType("pyte"))
import planeweb  # noqa: E402

STATUS = {
    "Self": {"UserID": 1, "DNSName": "laptop.tail.ts.net.", "TailscaleIPs": ["100.1.1.1"]},
    "User": {"1": {"LoginName": "me@example.com"}},
    "Peer": {
        "a": {"UserID": 1, "HostName": "localhost", "DNSName": "my-iphone.tail.ts.net.", "TailscaleIPs": ["100.2.2.2"]},
        "b": {"UserID": 1, "HostName": "localhost", "DNSName": "old-ipad.tail.ts.net.", "TailscaleIPs": ["100.3.3.3"]},
        "c": {"UserID": 1, "HostName": "printfarm", "DNSName": "printfarm.tail.ts.net.", "TailscaleIPs": ["100.4.4.4"]},
    },
}


def access(cfg):
    tmp = Path(tempfile.mkdtemp())
    (tmp / "plane").mkdir()
    (tmp / "plane" / "web.json").write_text(json.dumps(cfg))
    os.environ["XDG_CONFIG_HOME"] = str(tmp)
    os.environ.pop("PLANE_WEB_DEV", None)
    planeweb.P = types.SimpleNamespace(HOME=Path.home(), read_json=lambda p, d=None: json.loads(Path(p).read_text())
                                       if Path(p).exists() else d)
    planeweb.tailscale_status = lambda: STATUS
    return planeweb.Access()


def h(login=None, ip=None):
    d = {}
    if login:
        d["Tailscale-User-Login"] = login
    if ip:
        d["X-Forwarded-For"] = ip
    return d


def test_owner_from_listed_phone():
    a = access({"devices": ["my-iphone"]})
    assert a.logins == {"me@example.com"}
    assert a.check(h("me@example.com", "100.2.2.2"))
    assert a.check(h("ME@example.com", "100.2.2.2"))


def test_other_devices_refused_when_listed():
    a = access({"devices": ["my-iphone"]})
    assert not a.check(h("me@example.com", "100.4.4.4"))  # the print farm, same owner
    assert not a.check(h("me@example.com", "100.3.3.3"))  # an iPad that isn't listed


def test_no_login_refused():
    a = access({})
    assert not a.check(h())  # straight to 127.0.0.1, not through tailscale serve
    assert not a.check(h("someone@else.com", "100.2.2.2"))


def test_any_own_device_when_none_listed():
    a = access({})
    assert a.check(h("me@example.com", "100.4.4.4"))


if __name__ == "__main__":
    fails = 0
    for name, fn in list(globals().items()):
        if name.startswith("test_"):
            try:
                fn()
                print("ok  ", name)
            except AssertionError as e:
                fails += 1
                print("FAIL", name, e)
    sys.exit(1 if fails else 0)
