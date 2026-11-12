#!/bin/sh
# A real mouse click on a path in a session tab, off-screen: the app runs under broadway with
# a headless Chrome watching, a stand-in for `claude attach` (mouse reporting on) prints a
# plain path and an OSC 8 link, and Chrome's DevTools protocol clicks on each. The file must
# open in a tab. (The `open:` picture key skips the click itself; this doesn't.)
#   tools/click_check.sh OUTDIR
set -u
out=$1
here=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$out"
"$here/tools/files_fixture.sh" "$out/repo" >/dev/null
fail=0

run() { # N TEXT
    n=$1; text=$2
    rm -f "$out/pos-$n" "$out/dump-$n.txt"
    rt=$(mktemp -d)
    XDG_RUNTIME_DIR=$rt gtk4-broadwayd :5 >/dev/null 2>&1 &
    b=$!
    sleep 0.5
    google-chrome-stable --headless=new --disable-gpu --no-first-run --user-data-dir="$rt/chrome" --remote-debugging-port=9333 \
        --window-size=1366,768 http://127.0.0.1:8085/ >/dev/null 2>&1 &
    c=$!
    sleep 1.5
    XDG_RUNTIME_DIR=$rt GDK_BACKEND=broadway BROADWAY_DISPLAY=:5 PLANE_SHOT=$out/click-$n.png PLANE_SHOT_AFTER=10 \
        PLANE_SHOT_KEYS="folder:$out/repo term:sh,$here/tools/fake_attach.sh,$out/repo/src/main.rs locate:$text=$out/pos-$n" \
        PLANE_DUMP_LATE=$out/dump-$n.txt timeout 40 "$here/target/release/plane-gui" >/dev/null 2>&1 &
    g=$!
    node "$here/tools/click.mjs" 9333 "$out/pos-$n"
    wait $g
    kill $c; wait $c 2>/dev/null
    kill $b
    rm -rf "$rt" 2>/dev/null
    if grep -q "^tab	main.rs" "$out/dump-$n.txt" 2>/dev/null; then echo "ok $n: a click on $text opens it"; else echo "FAIL $n: a click on $text opened nothing"; fail=1; fi
}

run 1 src/main.rs
run 2 LINKED.rs
exit $fail
