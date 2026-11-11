#!/bin/sh
# Checks the editor's handling of outside edits, off-screen, on a throwaway repo. The
# outside edit is a step in the key sequence ("ed:outside=TEXT", written the way an agent
# writes: a new file renamed into place), so the order is exact.
#   1. clean file, changed on disk      -> taken in, new lines tinted
#   2. unsaved text, changed on disk    -> conflict; "Save mine over it" writes mine
#   3. unsaved text, changed on disk    -> "Use the version on disk": mine isn't written
#   4. unsaved text, changed on disk    -> Ctrl+S refuses, both kept, compare shown
#   5. as 4, with only the file monitor to notice the change
# Pictures go to OUTDIR/check-N.png; the file on disk is checked after each run.
#   tools/editor_check.sh OUTDIR
set -u
out=$1
here=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$out"
repo=$out/repo
f=$repo/src/main.rs
fail=0

# run N KEYS...: one key every half second, the picture after the last
run() {
    n=$1; shift
    "$here/tools/files_fixture.sh" "$repo"
    PLANE_SHOT_AFTER=$(( 6 + $# )) "$here/tools/shot.sh" "$out/check-$n.png" "$@" >/dev/null
}
expect() { # N description grep-args...
    n=$1; what=$2; shift 2
    if grep -q "$@" "$f"; then echo "ok $n: $what"; else echo "FAIL $n: $what"; fail=1; fi
}
reject() {
    n=$1; what=$2; shift 2
    if grep -q "$@" "$f"; then echo "FAIL $n: $what"; fail=1; else echo "ok $n: $what"; fi
}

run 1 "open:$f:3" "ed:outside=//_added_on_disk" "ed:line=3"
expect 1 "outside edit on disk" 'added on disk'

run 2 "open:$f" "type:MINE_" "ed:outside=//_added_on_disk" "ed:check" "ed:mine"
expect 2 "my text saved" 'MINE '
reject 2 "their line replaced, as chosen" 'added on disk'

run 3 "open:$f" "type:MINE_" "ed:outside=//_added_on_disk" "ed:check" "ed:theirs"
expect 3 "their text on disk" 'added on disk'
reject 3 "mine not written" 'MINE '

run 4 "open:$f" "type:MINE_" "ed:outside=//_added_on_disk" "ed:save" "ed:compare"
expect 4 "their text still on disk" 'added on disk'
reject 4 "mine not written over theirs" 'MINE '

run 5 "open:$f" "type:MINE_" "ed:outside=//_added_on_disk" "ed:save"
expect 5 "their text still on disk" 'added on disk'
reject 5 "save refused" 'MINE '

leftover=$(find "$repo" -name '*.plane-*.tmp' | wc -l)
if [ "$leftover" = 0 ]; then echo "ok: no temp files left"; else echo "FAIL: $leftover temp files left"; fail=1; fi
exit $fail
