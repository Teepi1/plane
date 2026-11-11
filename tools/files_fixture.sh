#!/bin/sh
# A throwaway git repo for picturing and testing the editor tabs: one committed Rust file
# with uncommitted changes, an untracked file, a binary file and an ignored folder.
#   tools/files_fixture.sh DIR      (DIR is emptied first)
set -eu
d=$1
rm -rf "$d"
mkdir -p "$d/src" "$d/target/debug"
cd "$d"
git init -q
git config user.email t@example.com
git config user.name test
cat > src/main.rs <<'EOF'
//! A small program, to look at in the editor.

use std::collections::HashMap;

/// Counts the words in `text`.
fn count(text: &str) -> HashMap<&str, usize> {
    let mut seen = HashMap::new();
    for word in text.split_whitespace() {
        *seen.entry(word).or_insert(0) += 1;
    }
    seen
}

fn main() {
    let text = "the quick brown fox jumps over the lazy dog";
    let counts = count(text);
    println!("{} different words", counts.len());
    for (word, n) in &counts {
        println!("{word}: {n}");
    }
}
EOF
printf 'target/\n' > .gitignore
git add -A
git commit -qm first
# uncommitted: one line changed, one added, one removed
sed -i 's/different words/distinct words/; s/^    seen$/    seen \/\/ done/; /lazy dog/a\    let total: usize = counts.values().sum();' src/main.rs
sed -i '/^\/\/\/ Counts the words/d' src/main.rs
printf 'notes, not committed yet\n' > notes.txt
printf '\000\001\002binary' > src/logo.bin
printf 'build output\n' > target/debug/out.txt
