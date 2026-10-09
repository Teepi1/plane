#!/bin/sh
# Install Plane for the current user.
#
#   ./install.sh                  the `plane` command, its icon and an app-menu launcher
#   ./install.sh --claude         also add Plane's statusLine and hooks to ~/.claude/settings.json
#   ./install.sh --autostart      also run the recovery check at login
#
# `plane` is symlinked, so `git pull` in this folder updates it.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
bin=${XDG_BIN_HOME:-$HOME/.local/bin}
data=${XDG_DATA_HOME:-$HOME/.local/share}
config=${XDG_CONFIG_HOME:-$HOME/.config}
claude=0 autostart=0
for arg in "$@"; do
    case $arg in
        --claude) claude=1 ;;
        --autostart) autostart=1 ;;
        -h|--help) sed -n '2,8s/^# \{0,1\}//p' "$0"; exit 0 ;;
        *) echo "install.sh: unknown option $arg" >&2; exit 2 ;;
    esac
done

for cmd in python3 kitty claude; do
    command -v "$cmd" >/dev/null || echo "note: '$cmd' isn't on your PATH yet; Plane needs it" >&2
done

mkdir -p "$bin" "$data/applications" "$data/icons/hicolor/scalable/apps"
ln -sf "$here/plane" "$bin/plane"
cp "$here/icons/plane.svg" "$data/icons/hicolor/scalable/apps/plane.svg"
for size in 32 48 64 128 256; do
    mkdir -p "$data/icons/hicolor/${size}x${size}/apps"
    cp "$here/icons/plane-$size.png" "$data/icons/hicolor/${size}x${size}/apps/plane.png"
done
sed "s|@PLANE@|$bin/plane|" "$here/desktop/plane.desktop" > "$data/applications/plane.desktop"
echo "installed: $bin/plane, icon, app launcher"

if [ $autostart = 1 ]; then
    mkdir -p "$config/autostart"
    sed "s|@PLANE@|$bin/plane|" "$here/desktop/plane-recover.desktop" > "$config/autostart/plane-recover.desktop"
    echo "installed: login recovery check"
fi

if [ $claude = 1 ]; then
    PLANE="$bin/plane" python3 - <<'EOF'
import json, os, shutil, time
from pathlib import Path

path = Path.home() / ".claude" / "settings.json"
plane = os.environ["PLANE"]
settings = json.loads(path.read_text()) if path.exists() else {}
if path.exists():
    backup = path.with_name(f"settings.json.bak-{time.strftime('%Y%m%d-%H%M%S')}")
    shutil.copy(path, backup)
    print(f"backed up settings to {backup}")

if settings.get("statusLine"):
    print("statusLine: you already have one, left alone (the README shows how to add usage tracking)")
else:
    settings["statusLine"] = {"type": "command", "command": f"{plane} statusline"}
    print("statusLine: added")

hooks = settings.setdefault("hooks", {})
for event, arg in (("SessionStart", "start"), ("UserPromptSubmit", "prompt"), ("SessionEnd", "end")):
    command = f"{plane} track {arg}"
    groups = hooks.setdefault(event, [])
    if any(command == h.get("command") for g in groups for h in g.get("hooks", [])):
        continue
    groups.append({"hooks": [{"type": "command", "command": command}]})
    print(f"hook {event}: added")

path.parent.mkdir(parents=True, exist_ok=True)
path.write_text(json.dumps(settings, indent=2) + "\n")
EOF
fi

case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "note: add $bin to your PATH" ;;
esac
echo "start it with: plane open"
