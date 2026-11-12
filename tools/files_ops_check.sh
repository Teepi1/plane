#!/bin/sh
# Checks the Files view's new / rename / trash and its live tree, off-screen, on a throwaway
# repo (tools/files_fixture.sh). Each run types its steps into the app as picture keys,
# then the files on disk (and what the list showed, through the `dump:` key) are checked.
# The trash goes to a throwaway data folder, not yours.
#   tools/files_ops_check.sh OUTDIR
set -u
out=$1
here=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$out"
repo=$out/repo
fail=0

run() { # N KEYS...
    n=$1; shift
    "$here/tools/files_fixture.sh" "$repo"
    XDG_DATA_HOME=$out/data PLANE_SHOT_AFTER=$(( 6 + $# )) "$here/tools/shot.sh" "$out/ops-$n.png" folder:"$repo" "$@" >/dev/null
}
ok() { echo "ok $1"; }
bad() { echo "FAIL $1"; fail=1; }
check() { # description test-args...
    what=$1; shift
    if "$@"; then ok "$what"; else bad "$what"; fi
}
has() { grep -q "$2" "$1"; }

run 1 fsel:src fop:new name:added.rs
check "1: new file made in the selected folder" test -f "$repo/src/added.rs"

run 2 fop:newdir name:docs
check "2: new folder made at the top" test -d "$repo/docs"

run 3 fsel:src/main.rs fop:rename name:logo.bin dump:"$out/dump-3.txt"
check "3: rename onto an existing name refused" test -f "$repo/src/main.rs"
check "3: the error sits beside the field" has "$out/dump-3.txt" "namebox	logo.bin	a file called logo.bin already exists"

run 4 fsel:src/main.rs fop:rename name:app.rs
check "4: renamed" test -f "$repo/src/app.rs" -a ! -e "$repo/src/main.rs"

run 5 open:"$repo/src/main.rs" type:MINE_ fsel:src/main.rs fop:rename name:app.rs dump:"$out/dump-5.txt"
check "5: rename refused while the tab has unsaved changes" test -f "$repo/src/main.rs" -a ! -e "$repo/src/app.rs"
check "5: it says what to do" has "$out/dump-5.txt" "save or close main.rs first"

run 6 fsel:notes.txt fop:trash y dump:"$out/dump-6.txt"
check "6: moved to the trash" test ! -e "$repo/notes.txt"
check "6: it's in the trash" test -n "$(find "$out/data/Trash/files" -name 'notes.txt*' 2>/dev/null)"
check "6: it says it can come back" has "$out/dump-6.txt" "restore it from there"

run 7 fsel:src/main.rs touch:"$repo/src/outside.rs" dump:"$out/dump-7.txt"
check "7: a file made outside shows up in the open folder" has "$out/dump-7.txt" "file	$repo/src/outside.rs"

run 8 touch:"$repo/top-outside.md" find:outsi dump:"$out/dump-8.txt"
check "8: find a file sees a file made outside" has "$out/dump-8.txt" "hit	$repo/top-outside.md"

exit $fail
