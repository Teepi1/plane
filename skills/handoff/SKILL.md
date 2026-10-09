---
name: handoff
description: |
  Write a prompt that lets a fresh Claude session pick up the current work, and
  queue it in Plane. Use when the user types /handoff or says "hand this off",
  "write a continuation prompt", "start a new session for this", or when a long
  session's output is getting worse and a clean start would help.
---

# Handoff

A long session fills up with dead ends and stale tool output. A handoff distills
it to what a cold session can't recover on its own, so the next one starts sharp.

## 1. Read the repo state

```bash
git branch --show-current
git status --short
git log --oneline -8
```

Uncommitted changes are the thing a handoff most often loses. If the tree is
dirty, say so and suggest committing first.

## 2. Write the prompt

One fenced block the next session can act on with no questions:

- **Goal:** what we're doing and why, in two or three lines.
- **Done so far:** what already landed (commits, files), briefly.
- **Decisions:** choices made this session that aren't written down anywhere
  else, each with its one-line reason.
- **Uncommitted work:** each dirty file and what the change is mid-way through.
- **Next step:** the single concrete thing to do first.

Don't restate CLAUDE.md, memory or docs the new session loads anyway: point to
them by path.

## 3. Queue it in Plane

Give it a 2–5 word title and pass the block's contents verbatim:

```bash
command -v plane >/dev/null && plane handoff add --title "<short title>" --cwd "$(git rev-parse --show-toplevel)" <<'HANDOFF'
<the prompt, exactly as in the fenced block>
HANDOFF
```

If `plane` isn't installed, skip this silently. If it ran, tell the user:
"Queued in Plane as '<title>'." Plane records which session it came from and
marks it ready once this session stops working.
