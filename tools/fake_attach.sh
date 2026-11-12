#!/bin/sh
# Stands in for `claude attach` in tools/click_check.sh: alternate screen and mouse
# reporting on (as attach does), then a plain path and an OSC 8 file:// link to FILE.
#   tools/fake_attach.sh FILE
f=$1
printf '\033[?1049h\033[?1000h\033[?1002h\033[?1006h\033[H\033[2J'
printf '● Update(src/main.rs)\r\n'
printf '  ⎿  Updated src/main.rs with 1 addition\r\n\r\n'
printf '● Read \033]8;;file://%s\033\\LINKED.rs\033]8;;\033\\ (link)\r\n' "$f"
sleep 60
