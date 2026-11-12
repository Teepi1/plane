#!/bin/sh
# Checks `plane-next edit PATH:LINE` off-screen, with its own runtime folder (so it never
# reaches the app on your screen):
#   1. with the app running: the file opens at the line
#   2. with no app running: it starts the app, waits for it, and the file opens
#   tools/edit_check.sh OUTDIR
set -u
out=$1
here=$(cd "$(dirname "$0")/.." && pwd)
bin=$here/target/release
mkdir -p "$out"
"$here/tools/files_fixture.sh" "$out/repo" >/dev/null
f=$out/repo/src/main.rs
fail=0
check() { if grep -q "$2" "$3"; then echo "ok $1"; else echo "FAIL $1"; fail=1; fi; }

# 1. running
rt=$(mktemp -d)
XDG_RUNTIME_DIR=$rt gtk4-broadwayd :6 >/dev/null 2>&1 &
b=$!
sleep 0.5
XDG_RUNTIME_DIR=$rt GDK_BACKEND=broadway BROADWAY_DISPLAY=:6 PLANE_SHOT=$out/edit-1.png PLANE_SHOT_AFTER=8 \
    PLANE_SHOT_KEYS="dump:$out/edit-1.txt" timeout 30 "$bin/plane-gui" >/dev/null 2>&1 &
g=$!
sleep 3
XDG_RUNTIME_DIR=$rt "$bin/plane-next" edit "$f:7"
wait $g
kill $b 2>/dev/null
rm -rf "$rt"
check "1: opened in the running app at line 7" "tab	main.rs	line 7" "$out/edit-1.txt"

# 2. not running: plane-next starts it (with this environment, so off-screen too)
rt=$(mktemp -d)
XDG_RUNTIME_DIR=$rt gtk4-broadwayd :6 >/dev/null 2>&1 &
b=$!
sleep 0.5
(cd "$out/repo" && XDG_RUNTIME_DIR=$rt GDK_BACKEND=broadway BROADWAY_DISPLAY=:6 PLANE_SHOT=$out/edit-2.png PLANE_SHOT_AFTER=7 \
    PLANE_SHOT_KEYS="dump:$out/edit-2.txt" "$bin/plane-next" edit src/main.rs:12)
echo "  (plane-next edit returned $?)"
sleep 8
kill $b 2>/dev/null
rm -rf "$rt"
check "2: started the app and opened a relative path at line 12" "tab	main.rs	line 12" "$out/edit-2.txt"
exit $fail
