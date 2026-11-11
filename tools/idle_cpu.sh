#!/bin/sh
# Idle CPU of the app with nobody using it: an off-screen copy (GTK broadway, its own
# runtime folder, so the running app is untouched) is left alone for SECONDS after it has
# settled. Counts the app's own CPU and its children's (the `claude agents` calls).
#   tools/idle_cpu.sh [SECONDS] [KEYS...]      e.g. tools/idle_cpu.sh 60 "open:/etc/hostname"
set -eu
secs=${1:-60}
shift || true
here=$(cd "$(dirname "$0")/.." && pwd)
rt=$(mktemp -d)
XDG_RUNTIME_DIR=$rt gtk4-broadwayd :8 >/dev/null 2>&1 &
b=$!
sleep 0.5
XDG_RUNTIME_DIR=$rt GDK_BACKEND=broadway BROADWAY_DISPLAY=:8 PLANE_SHOT=$rt/x.png PLANE_SHOT_AFTER=$((secs + 20)) PLANE_SHOT_KEYS="$*" \
    "$here/target/release/plane-gui" >/dev/null 2>&1 &
app=$!
sleep 15
tick=$(getconf CLK_TCK)
ticks() { awk "{print \$14+\$15, \$16+\$17}" "/proc/$app/stat"; }
t0=$(ticks)
sleep "$secs"
t1=$(ticks)
kill $app $b 2>/dev/null || true
rm -rf "$rt"
echo "$t0 $t1" | awk -v hz="$tick" -v s="$secs" '{ o = ($3 - $1) / hz; c = ($4 - $2) / hz; printf "app %.2f s, claude agents %.2f s, in %d s = %.2f%% of one core\n", o, c, s, 100 * (o + c) / s }'
