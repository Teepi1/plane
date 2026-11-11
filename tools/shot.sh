#!/bin/sh
# Picture the app without putting it on the desktop: runs it under GTK's broadway backend
# with its own runtime folder, saves a PNG, quits. Its refreshes are read-only, as always.
#   tools/shot.sh OUT.png [KEYS...]     e.g. tools/shot.sh /tmp/help.png question
set -eu
out=$1
shift
here=$(cd "$(dirname "$0")/.." && pwd)
rt=$(mktemp -d)
XDG_RUNTIME_DIR=$rt gtk4-broadwayd :7 >/dev/null 2>&1 &
bpid=$!
sleep 0.5
XDG_RUNTIME_DIR=$rt GDK_BACKEND=broadway BROADWAY_DISPLAY=:7 PLANE_SHOT=$out PLANE_SHOT_AFTER=${PLANE_SHOT_AFTER:-5} PLANE_SHOT_KEYS="$*" \
    timeout 30 "$here/target/release/plane-gui" >/dev/null 2>&1 || true
kill $bpid 2>/dev/null || true
rm -rf "$rt"
ls -la "$out"
