"""Reading Claude's menus off real screens (captured with pyte from `claude attach`).

  python3 tests/test_prompts.py
"""
import sys
import types
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent))
sys.modules.setdefault("pyte", None)  # parse_prompt doesn't need it
import planeweb  # noqa: E402


def screen(name):
    return (HERE / "screens" / f"{name}.txt").read_text().splitlines()


def labels(p):
    return [o["label"] for o in p["options"]]


def test_permission():
    p = planeweb.parse_prompt(screen("permission"))
    assert p, "no menu found"
    assert [o["n"] for o in p["options"]] == [1, 2, 3, 4]
    assert labels(p)[0] == "Yes" and labels(p)[3] == "No"
    assert p["selected"] == 0
    texts = [c["s"] for c in p["context"]]
    assert texts[-1] == "Do you want to proceed?", texts
    assert any(c["t"] == "code" and c["s"].startswith("mkdir -p") for c in p["context"]), p["context"]
    assert "Bash command" in texts[0]


def test_question():
    p = planeweb.parse_prompt(screen("question"))
    assert p
    assert labels(p) == ["Tea", "Coffee", "Type something.", "Chat about this"], labels(p)
    assert p["options"][0]["desc"] == "Go with tea."
    assert [c["s"] for c in p["context"]][-1] == "Tea or coffee?"


def test_trust():
    p = planeweb.parse_prompt(screen("trust"))
    assert p
    assert labels(p) == ["No, exit", "Yes, I trust this folder"], labels(p)
    assert all(o["n"] is None for o in p["options"])
    assert p["selected"] == 0
    assert any("Quick safety check" in c["s"] for c in p["context"])


def test_idle():
    assert planeweb.parse_prompt(screen("idle")) is None


def test_question_narrow_with_name_footer():
    # a phone-width attach: the hint wraps onto two lines, and the session's name rule sits
    # at the very bottom with empty rows above it
    p = planeweb.parse_prompt(screen("question-narrow"))
    assert p, "no menu found"
    assert labels(p) == ["Summer", "Winter", "Type something.", "Chat about this"], labels(p)
    assert p["selected"] == 0
    texts = [c["s"] for c in p["context"]]
    assert texts == ["Season · Pet · Submit", "Pick a season."], texts


def test_question_review_has_no_hint():
    p = planeweb.parse_prompt(screen("question-review"))
    assert p, "no menu found"
    assert labels(p) == ["Submit answers", "Cancel"], labels(p)
    texts = [c["s"] for c in p["context"]]
    assert texts[0] == "Colour ✓ · Fruit ✓ · Submit" and texts[-1] == "Ready to submit your answers?", texts
    assert "→ Pear" in texts


def test_idle_with_name_footer():
    assert planeweb.parse_prompt(screen("idle-footer")) is None


def test_typed_numbered_draft_is_not_a_menu():
    lines = screen("idle-footer")
    i = next(k for k, ln in enumerate(lines) if ln.startswith("❯"))
    lines[i] = "❯ 1. buy milk"
    assert planeweb.parse_prompt(lines) is None


def test_sig_changes_with_options():
    a = planeweb.parse_prompt(screen("permission"))
    b = planeweb.parse_prompt(screen("question"))
    assert a["sig"] != b["sig"]
    assert a["sig"] == planeweb.parse_prompt(screen("permission"))["sig"]


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
