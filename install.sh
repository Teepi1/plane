#!/bin/sh
# Install Plane for the current user.
#
#   ./install.sh                  the `plane` command, its icon and an app-menu launcher
#   ./install.sh --claude         also add Plane's statusLine and hooks to ~/.claude/settings.json
#   ./install.sh --autostart      also run the recovery check at login
#   ./install.sh --web            also run the phone app (a user service) and put it on your tailnet
#
# `plane` is symlinked, so `git pull` in this folder updates it.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
bin=${XDG_BIN_HOME:-$HOME/.local/bin}
data=${XDG_DATA_HOME:-$HOME/.local/share}
config=${XDG_CONFIG_HOME:-$HOME/.config}
claude=0 autostart=0 web=0
for arg in "$@"; do
    case $arg in
        --claude) claude=1 ;;
        --autostart) autostart=1 ;;
        --web) web=1 ;;
        -h|--help) sed -n '2,9s/^# \{0,1\}//p' "$0"; exit 0 ;;
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

if [ $web = 1 ]; then
    for mod in pyte cryptography; do
        python3 -c "import $mod" 2>/dev/null || echo "note: install python $mod (Arch: python-$mod); the phone app needs it" >&2
    done
    command -v tailscale >/dev/null || { echo "install.sh: --web needs Tailscale" >&2; exit 1; }
    # who may use it: your Tailscale login, from your phones and tablets (edit to change)
    if [ ! -f "$config/plane/web.json" ]; then
        mkdir -p "$config/plane"
        tailscale status --json | python3 -c '
import json, sys
st = json.load(sys.stdin)
me = st.get("Self", {}).get("UserID")
peers = [p for p in st.get("Peer", {}).values() if p.get("UserID") == me and p.get("OS") in ("iOS", "android")]
devices = sorted({p["DNSName"].split(".")[0] for p in peers if p.get("DNSName")})  # iOS HostName is just "localhost"
print(json.dumps({"devices": devices}, indent=2))' > "$config/plane/web.json"
        echo "wrote $config/plane/web.json (devices allowed: $(python3 -c "import json;print(', '.join(json.load(open('$config/plane/web.json'))['devices']) or 'all of yours')"))"
    fi
    mkdir -p "$config/systemd/user"
    sed "s|@PLANE@|$bin/plane|" "$here/desktop/plane-web.service" > "$config/systemd/user/plane-web.service"
    systemctl --user daemon-reload
    systemctl --user enable --now plane-web.service
    systemctl --user restart plane-web.service
    echo "installed: plane-web.service (127.0.0.1:8377)"
    if tailscale serve --bg --https=443 http://127.0.0.1:8377; then
        host=$(tailscale status --json | python3 -c 'import json,sys;print(json.load(sys.stdin)["Self"]["DNSName"].rstrip("."))')
        echo "on your phone, open https://$host in Safari, then Share > Add to Home Screen"
    else
        echo "tailscale serve failed: turn on HTTPS Certificates (Tailscale admin console > DNS), then run ./install.sh --web again" >&2
    fi
fi

case ":$PATH:" in
    *":$bin:"*) ;;
    *) echo "note: add $bin to your PATH" ;;
esac
echo "start it with: plane open"
