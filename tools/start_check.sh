#!/bin/sh
# Start a handoff in the app, off-screen, against a throwaway home and a stand-in claude:
# checks the claim, the job id read from `claude --bg`, the attach, and what lands in done/.
#   tools/start_check.sh OUT.png
set -eu
out=$1
here=$(cd "$(dirname "$0")/.." && pwd)
t=$(mktemp -d)
mkdir -p "$t/home/proj" "$t/home/.local/share/plane/handoffs/done" "$t/home/.claude/jobs" "$t/bin" "$t/rt"
cat > "$t/bin/claude" <<'EOF'
#!/bin/sh
echo "$*" >> "$HOME/claude.log"
case "$1" in
  agents) echo "[]" ;;
  --bg) echo "Trust this folder? (stand-in)"; echo "Session backgrounded · abc12345"; mkdir -p "$HOME/.claude/jobs/abc12345" ;;
  attach) echo "attached to $2 (stand-in)"; sleep 30 ;;
esac
EOF
chmod +x "$t/bin/claude"
now=$(date +%s)
cat > "$t/home/.local/share/plane/handoffs/h1.json" <<EOF
{"id": "h1", "title": "Check the start flow", "cwd": "$t/home/proj", "prompt": "hello", "createdAt": $now}
EOF
XDG_RUNTIME_DIR=$t/rt gtk4-broadwayd :4 >/dev/null 2>&1 &
b=$!
sleep 0.5
HOME=$t/home PATH=$t/bin:/usr/bin:/bin XDG_RUNTIME_DIR=$t/rt GDK_BACKEND=broadway BROADWAY_DISPLAY=:4 \
    PLANE_SHOT=$out PLANE_SHOT_AFTER=6 PLANE_SHOT_KEYS="Return" timeout 30 "$here/target/release/plane-gui" 2>&1 | grep -v "frame_timings\|^$" | sort | uniq -c | head -8 || true
kill $b 2>/dev/null || true
echo "claude calls:"; cat "$t/home/claude.log" | grep -v agents
echo "queued:"; ls "$t/home/.local/share/plane/handoffs/" | grep json || true
echo "done/h1.json:"; cat "$t/home/.local/share/plane/handoffs/done/h1.json" 2>/dev/null || echo missing
rm -rf "$t"
