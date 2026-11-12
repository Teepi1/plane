# Plane as a native app (in progress)

The `plane` script in the main README is the kitty version: a curses sidebar inside a kitty
window. This branch builds its successor, a native GTK 4 app written in Rust. It has the
same sidebar beside built-in terminal tabs, plus file tabs you can edit.

**Status:** it's built beside the kitty version, not in place of it. While the two exist
side by side:
- The app's window class is `plane-next`, so window rules and Super+arrows can tell the
  two apart.
- Its command is `plane-next`. The hooks and the status line still call the Python `plane`.
- It sends no notifications.
- Its background refresh writes nothing. Only actions you take write (the same files the
  kitty sidebar writes), plus the folded sections it remembers.

Switching over (`plane` becomes this app) is a later step; until then both can run at
once. Avoid showing the same session in both at the same time: two views of one session
at different sizes garble each other.

It's built for one machine (Arch, Hyprland, Noctalia), but nothing in it requires
Hyprland or Noctalia.

## Requirements

- Linux with **GTK 4.14+**, **VTE for GTK 4 (0.76+)** and **GtkSourceView 5**.
  On Arch: `pacman -S gtk4 vte4 gtksourceview5`.
- **Rust** (stable; developed on 1.99).
- **git**, used by the Files view and the editor's change marks.
- **[Claude Code](https://claude.com/claude-code)** with background sessions, as for the
  kitty version.
- Optional: **Hyprland**, to raise the window and move focus with Super+arrows.
- Optional, only for the off-screen checks in `tools/`: **gtk4-broadwayd** (comes with
  gtk4), **google-chrome-stable**, **node** 22+ and **ImageMagick**.

## Build and run

```sh
cargo build --release
target/release/plane-next open        # starts the app, or brings it forward
```

This produces two programs, which must stay side by side in `target/release`:

| Program | What it is |
|---|---|
| `plane-gui` | The app itself. One copy runs at a time; a second start just raises the first. |
| `plane-next` | The command line. |

`plane-next` commands:

| Command | What it does |
|---|---|
| `plane-next open` | Raises the app, or starts it. |
| `plane-next edit PATH[:LINE]` | Opens a file in the app, at LINE if given, starting the app if needed. |
| `plane-next focus left\|right\|up\|down` | For Super+arrows: moves between the sidebar and the tab, and past the app's edge falls back to Hyprland focus. |
| `plane-next snapshot` | Prints what the sidebar would show, as JSON. Read-only. |

They talk over a socket at `$XDG_RUNTIME_DIR/plane-next.sock`.

### Launchers (optional)

An app-menu entry for the app itself; change the path to where you built it:

```ini
# ~/.local/share/applications/plane-next.desktop
[Desktop Entry]
Type=Application
Name=Plane (new app)
Exec=/path/to/plane/target/release/plane-next open
Icon=plane
StartupWMClass=plane-next
```

[`desktop/plane-next-edit.desktop`](../desktop/plane-next-edit.desktop) ("Plane editor",
hidden from the menu) lets a file manager's *Open With* send files to the app. Its `Exec`
holds an absolute path, so fix that, then copy it to `~/.local/share/applications/`. It
isn't made the default for anything. To make it the default for a type:

```sh
xdg-mime default plane-next-edit.desktop text/plain
```

Under Hyprland, a window rule for class `plane-next` (maximized, for example) and a key bound
to `plane-next open` make it feel like the kitty version.

## Using it

The window has a **sidebar** on the left and **tabs** on the right.

### Sessions

The sidebar's **Sessions** view lists the same sections as the kitty sidebar: Cut off by a
crash, Sessions, Paused, In other windows and Handoffs.
- **Headings:** each section has a heading row with its count. Click it (or press ← / →)
  to fold or show the section. Folded sections are remembered.
- **Status icons:** each row's icon on the left gives its state.

| Icon | State |
|---|---|
| half-filled ring | working |
| solid disc with **!** | needs you: it asked something and is waiting |
| speech bubble | your turn: it finished and waits for your next message |
| two bars | paused |
| disc with **×** | failed |
| lightning bolt | cut off by a crash or reboot |
| arrow / hourglass | a handoff ready to start / still being written |

Hover an icon for the state in words; the details panel under the list says it too. The
counts at the top ("1 needs you", "2 your turn") are buttons: each click selects the next
session in that state.

To act on a row:
- Click a session to open it in a tab, or double-click a handoff to start it.
- Right-click any row for everything you can do with it.
- Use **+ New** to start a session.

### Files

Click **Files** (or press `f`). It shows the files of the session you last clicked, or of
the tab in front:
- **Not committed:** what `git status` lists.
- **Edited by this session:** from the session's transcript.
- **All files:** the folder as a tree. It's kept up to date as files appear or go.

Groups fold like the sidebar sections. Things you can do there:
- **The folder button** at the top switches to another session's folder, a recent folder,
  or any folder (*Choose a folder…*). A folder picked by hand stays until you click a
  session.
- **Find a file…** (`/`, or Ctrl+P in the sidebar or an editor) matches by name; Enter
  opens the best match.
- **+ File**, **+ Folder**, **Rename** and **Trash** act on the selected row (also `n`,
  Shift+N, F2 and Delete, and a right-click menu with Copy path).
  - A new file opens in a tab.
  - Names that are taken are refused beside the field, keeping what you typed.
  - A file with unsaved changes can't be renamed.
  - Trash asks first, and the file can be restored from the trash.

### Opening and editing files

**Click a file path** in a session's terminal (a hover says "Click to open in Plane") to
open it in a tab. `file://` links open too; web links go to the browser. The editor:
- **Find** (Ctrl+F) searches the file.
- **Changes · N** shows what differs from the last commit. Marks beside the line numbers
  show added, changed and removed lines.
- **Save** (Ctrl+S) writes the file. Ctrl+W closes the tab.

When a session edits the open file, the change is taken in as it lands and tinted, and the
status line says so ("changed on disk at 14:02: 3 lines · Show"). The editor never
overwrites your unsaved text:
- If the file changes on disk while you have unsaved edits, a bar offers **Compare**,
  **Save mine over it** or **Use the version on disk**.
- Ctrl+S refuses to overwrite a newer version on disk.

### Keys

Keys apply only while the sidebar has focus, so a terminal never loses a key to the app.
`?` shows them all in the app. The main ones:

| Key | Does |
|---|---|
| ↑ ↓ / j k, Enter | move, open |
| ← → | fold / show a section |
| n | new session (in Files: new file) |
| x, p | close, pause |
| f, Esc | Files, back to Sessions |
| / or Ctrl+P | find a file |
| F2, Delete | rename, trash (in Files) |
| Ctrl+V | paste into the open session |
| q | quit (sessions keep running) |

### Files it reads and writes

| Path | What it is |
|---|---|
| `~/.config/plane/app.json` | Folded sections and Files groups. Written only when you fold one. |
| `~/.config/plane/theme.css` | Colors as `@define-color` lines, watched for changes. Optional: built-in colors are used without it. The author's machine fills it from a Noctalia template. |
| `$XDG_RUNTIME_DIR/plane-next-*` | The control socket, the editor's color scheme and the status icons, written at start. |

Everything else (`~/.claude`, `~/.local/share/plane`, `~/.cache/plane`) is read the same way
the kitty version reads it, and written only by actions you take.

## Checking it

Unit tests: `cargo test`. The rest run the real app off-screen under GTK's broadway backend,
in a throwaway runtime folder, so the app on your screen is never touched:

| Script | Checks |
|---|---|
| `tools/shot.sh OUT.png [KEYS…]` | Saves a picture; keys drive it (see `shot.rs` and the `shot::arm` call in `app.rs`). |
| `tools/editor_check.sh DIR` | Outside edits, conflicts, refused saves. |
| `tools/files_ops_check.sh DIR` | New, rename, trash, the live tree, find a file. |
| `tools/click_check.sh DIR` | A real mouse click on a path in a session tab (headless Chrome). |
| `tools/edit_check.sh DIR` | `plane-next edit`, with the app running and without. |
| `tools/actions_check.py` | Each sidebar action against the Python version. |
| `tools/idle_cpu.sh [SECS]` | The app's idle CPU. |

To picture an animation part-way through, set `GTK_SLOWDOWN=8`.

## Not done yet

- Switching over from the kitty version (hooks, status line, `plane` command, notifications).
- The agents board, plain shell tabs and split panes.
