"""Web Push encryption round trip: encrypt as the server does, decrypt as a phone would
(RFC 8291, aes128gcm). Needs python-cryptography.

  python3 tests/test_push.py
"""
import secrets
import struct
import sys
import types
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.modules.setdefault("pyte", None)
import planeweb  # noqa: E402

try:
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    from cryptography.hazmat.primitives.kdf.hkdf import HKDF
except ImportError:
    print("skip: python-cryptography isn't installed")
    sys.exit(0)


def decrypt(body, ua_key, auth):
    salt, rs, idlen = body[:16], struct.unpack(">I", body[16:20])[0], body[20]
    as_pub = body[21:21 + idlen]
    ct = body[21 + idlen:]
    assert rs == 4096
    ua_pub = ua_key.public_key().public_bytes(serialization.Encoding.X962, serialization.PublicFormat.UncompressedPoint)
    shared = ua_key.exchange(ec.ECDH(), ec.EllipticCurvePublicKey.from_encoded_point(ec.SECP256R1(), as_pub))
    ikm = HKDF(hashes.SHA256(), 32, auth, b"WebPush: info\x00" + ua_pub + as_pub).derive(shared)
    cek = HKDF(hashes.SHA256(), 16, salt, b"Content-Encoding: aes128gcm\x00").derive(ikm)
    nonce = HKDF(hashes.SHA256(), 12, salt, b"Content-Encoding: nonce\x00").derive(ikm)
    plain = AESGCM(cek).decrypt(nonce, ct, None)
    assert plain.endswith(b"\x02"), "last-record delimiter missing"
    return plain[:-1]


def test_round_trip():
    ua = ec.generate_private_key(ec.SECP256R1())
    auth = secrets.token_bytes(16)
    p256dh = planeweb.b64u(ua.public_key().public_bytes(serialization.Encoding.X962,
                                                        serialization.PublicFormat.UncompressedPoint))
    msg = b'{"title": "Pi Zero 2 W setup needs you", "body": "Tell me which version it shows"}'
    body = planeweb.Push.encrypt(msg, p256dh, planeweb.b64u(auth))
    assert decrypt(body, ua, auth) == msg


def test_jwt_shape():
    p = planeweb.Push.__new__(planeweb.Push)
    p.key = ec.generate_private_key(ec.SECP256R1())
    p.contact = "me@example.com"
    tok = p._jwt("https://web.push.apple.com")
    head, body, sig = tok.split(".")
    import json
    assert json.loads(planeweb.unb64u(head)) == {"typ": "JWT", "alg": "ES256"}
    claims = json.loads(planeweb.unb64u(body))
    assert claims["aud"] == "https://web.push.apple.com" and claims["sub"] == "mailto:me@example.com"
    raw = planeweb.unb64u(sig)
    assert len(raw) == 64
    from cryptography.hazmat.primitives.asymmetric import utils
    der = utils.encode_dss_signature(int.from_bytes(raw[:32], "big"), int.from_bytes(raw[32:], "big"))
    p.key.public_key().verify(der, f"{head}.{body}".encode(), ec.ECDSA(hashes.SHA256()))


def bare_push(tmp):
    """A Push with no key and no network: subscriptions in `tmp`, sends recorded."""
    import threading
    import json as _json
    p = planeweb.Push.__new__(planeweb.Push)
    p.ok, p.why, p.contact = True, "", "me@example.com"
    p.subs_path, p.subs, p.lock, p.results = Path(tmp) / "subs.json", [], threading.Lock(), {}
    p.sent = []

    def fake_send_one(sub, data, topic, pid=None):
        p.sent.append((sub["endpoint"], _json.loads(data)))
        p.results[pid]["apple"].append(201)
    p._send_one = fake_send_one
    return p


class FakeP:
    @staticmethod
    def write_json(path, data):
        import json as _json
        Path(path).write_text(_json.dumps(data))


def with_tmp(fn):
    import tempfile
    old_p, old_log = planeweb.P, planeweb.log
    planeweb.P, planeweb.log = FakeP, lambda text: None
    try:
        with tempfile.TemporaryDirectory() as tmp:
            fn(bare_push(tmp))
    finally:
        planeweb.P, planeweb.log = old_p, old_log


def test_one_subscription_per_device():
    def run(p):
        sub = lambda n: {"endpoint": f"https://web.push.apple.com/{n}", "keys": {"p256dh": "x", "auth": "y"}}
        p.subs = [dict(sub("legacy"))]  # saved before devices were recorded
        p.subscribe(sub("ipad"), "100.0.0.2")
        p.subscribe(sub("old-icon"), "100.0.0.1")
        p.subscribe(sub("new-icon"), "100.0.0.1")
        p.subscribe(sub("new-icon"), "100.0.0.1")  # every app load signs up again
        assert sorted(s["endpoint"].rsplit("/", 1)[1] for s in p.subs) == ["ipad", "new-icon"], p.subs
    with_tmp(run)


def test_send_only_and_seen():
    def run(p):
        for n, ip in (("a", "1"), ("b", "2")):
            p.subscribe({"endpoint": f"https://e/{n}", "keys": {}}, ip)
        pid = p.send({"title": "T"}, only="https://e/b", pid="abc")
        assert pid == "abc" and [e for e, _ in p.sent] == ["https://e/b"]
        assert p.sent[0][1]["id"] == "abc"  # the phone reports back with it
        assert p.results["abc"]["apple"] == [201] and not p.results["abc"].get("seen")
        p.seen("abc", True)
        assert p.results["abc"]["seen"] and p.results["abc"]["shown"]
        p.seen("nope", True)  # unknown ids are ignored
        everyone = p.send({"title": "U"})
        assert len([e for e, d in p.sent if d["id"] == everyone]) == 2
    with_tmp(run)


if __name__ == "__main__":
    fails = 0
    for name, fn in list(globals().items()):
        if name.startswith("test_"):
            try:
                fn()
                print("ok  ", name)
            except Exception as e:
                fails += 1
                print("FAIL", name, repr(e))
    sys.exit(1 if fails else 0)
