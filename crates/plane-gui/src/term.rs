//! The built-in terminal, as tested in step 1: kitty's look (the Noctalia palette, 0.8
//! opacity, 25 px padding, monospace 11), kitty's copy, paste and zoom keys, Shift+Enter
//! as a new line, and file paths that can be clicked (reported for now; opening them is
//! step 6).

use std::cell::Cell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use vte::prelude::*;

/// PCRE2_MULTILINE: VTE requires it for match regexes.
const PCRE2_MULTILINE: u32 = 0x0000_0400;
/// A file path with an extension, optionally followed by :line or :line:column.
const PATH_RE: &str = r"(?:~|\.{1,2})?/?[\w.\-]+(?:/[\w.\-]+)*\.[A-Za-z0-9]{1,8}(?::\d+(?::\d+)?)?";

/// Lines per mouse-wheel notch. kitty's default is 5; this is a touch slower.
const WHEEL_LINES: f64 = 4.0;
/// Touchpad: pixels scrolled per pixel your fingers move.
const TOUCHPAD_SPEED: f64 = 0.8;

fn home() -> PathBuf {
    plane_core::Env::from_env().home
}

/// The environment for programs in the terminal: without kitty's variables or those of a
/// Claude session that may have launched the app, so Claude sees a plain VTE terminal.
pub fn child_env() -> Vec<String> {
    let mut env: Vec<String> = std::env::vars()
        .filter(|(k, _)| {
            !(k.starts_with("KITTY_") || k.starts_with("TERM_PROGRAM") || k == "TERM" || k == "COLORTERM" || k == "CLAUDECODE"
                || k.starts_with("CLAUDE_CODE_") || k == "CLAUDE_JOB_DIR")
        })
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    env.push("TERM=xterm-256color".into());
    env.push("COLORTERM=truecolor".into());
    env
}

/// Turns `src/app.ts:12:4` into (absolute path, line) against the session's folder.
fn resolve(text: &str, cwd: &str) -> (PathBuf, Option<u32>) {
    let mut parts = text.splitn(3, ':');
    let file = parts.next().unwrap_or(text);
    let line = parts.next().and_then(|l| l.parse().ok());
    let path = if let Some(rest) = file.strip_prefix("~/") {
        home().join(rest)
    } else if Path::new(file).is_absolute() {
        PathBuf::from(file)
    } else {
        Path::new(cwd).join(file)
    };
    (path, line)
}

fn color(theme: &HashMap<String, gdk::RGBA>, key: &str) -> gdk::RGBA {
    theme.get(key).copied().unwrap_or(gdk::RGBA::WHITE)
}

pub fn apply_colors(term: &vte::Terminal, theme: &HashMap<String, gdk::RGBA>) {
    let palette: Vec<gdk::RGBA> = (0..16).map(|i| color(theme, &format!("term_{i}"))).collect();
    let refs: Vec<&gdk::RGBA> = palette.iter().collect();
    let bg = color(theme, "term_bg");
    let bg = gdk::RGBA::new(bg.red(), bg.green(), bg.blue(), 0.8); // kitty: background_opacity 0.8
    term.set_colors(Some(&color(theme, "term_fg")), Some(&bg), &refs);
    term.set_color_cursor(Some(&color(theme, "term_cursor")));
    term.set_color_cursor_foreground(Some(&color(theme, "term_cursor_text")));
    term.set_color_highlight(Some(&color(theme, "term_sel_bg")));
    term.set_color_highlight_foreground(Some(&color(theme, "term_sel_fg")));
}

/// A terminal for one tab. `say` reports what a clicked file path would open.
pub fn new_terminal(theme: &HashMap<String, gdk::RGBA>, scale: Rc<Cell<f64>>, cwd: String, say: Rc<dyn Fn(String)>) -> vte::Terminal {
    let term = vte::Terminal::new();
    term.set_hexpand(true);
    term.set_vexpand(true);
    apply_colors(&term, theme);
    term.set_font(Some(&pango::FontDescription::from_string("monospace 11")));
    term.set_font_scale(scale.get());
    term.set_scrollback_lines(10_000);
    // Scrolling is done here, not by VTE, which glides on after a touchpad flick and felt
    // too fast. Like kitty: a wheel notch is a few lines, a touchpad follows your fingers
    // (a little slower), and nothing keeps moving after you stop.
    term.set_scroll_unit_is_pixels(true);
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
    let t = term.clone();
    scroll.connect_scroll(move |c, _, dy| {
        let Some(adj) = t.vadjustment() else { return glib::Propagation::Proceed };
        let top = adj.upper() - adj.page_size();
        if top <= adj.lower() {
            // no scrollback (a full-screen program): VTE turns the wheel into arrow keys
            return glib::Propagation::Proceed;
        }
        let step = match c.unit() {
            gdk::ScrollUnit::Wheel => dy * WHEEL_LINES * t.char_height() as f64,
            _ => dy * TOUCHPAD_SPEED,
        };
        adj.set_value((adj.value() + step).clamp(adj.lower(), top));
        glib::Propagation::Stop
    });
    term.add_controller(scroll);
    term.set_mouse_autohide(true);
    term.set_allow_hyperlink(true);
    term.set_audible_bell(false);
    if let Ok(re) = vte::Regex::for_match(PATH_RE, PCRE2_MULTILINE) {
        let tag = term.match_add_regex(&re, 0);
        term.match_set_cursor_name(tag, "pointer");
    }

    // kitty's copy, paste and zoom keys; Shift+Enter becomes a new line (Ctrl+J), since VTE
    // can't tell Shift+Enter from Enter the way kitty's keyboard protocol does
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let t = term.clone();
    keys.connect_key_pressed(move |_, key, _, mods| {
        let mods = mods & (gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK);
        let ctrl_shift = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK;
        let zoom = |s: f64| {
            scale.set(s);
            t.set_font_scale(s);
        };
        if mods == gdk::ModifierType::SHIFT_MASK && (key == gdk::Key::Return || key == gdk::Key::KP_Enter) {
            t.feed_child(b"\n");
            return glib::Propagation::Stop;
        }
        if mods != ctrl_shift {
            return glib::Propagation::Proceed;
        }
        match key.to_lower() {
            gdk::Key::c => t.copy_clipboard_format(vte::Format::Text),
            gdk::Key::v => t.paste_clipboard(),
            gdk::Key::plus | gdk::Key::equal => zoom((scale.get() * 1.1).min(3.0)),
            gdk::Key::minus | gdk::Key::underscore => zoom((scale.get() / 1.1).max(0.5)),
            gdk::Key::BackSpace => zoom(1.0),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    term.add_controller(keys);

    // a click (not a drag) on a file path reports what Plane would open; the click still
    // reaches the terminal, so selecting text and the session's own mouse use are untouched
    let click = gtk::GestureClick::new();
    click.set_button(1);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    let down = Rc::new(Cell::new((0.0, 0.0)));
    let d = down.clone();
    click.connect_pressed(move |_, _, x, y| d.set((x, y)));
    let t = term.clone();
    click.connect_released(move |_, _, x, y| {
        let (x0, y0) = down.get();
        if (x - x0).hypot(y - y0) > 4.0 {
            return;
        }
        if let Some(uri) = t.check_hyperlink_at(x, y) {
            say(format!("link: {uri}"));
            return;
        }
        let (found, _) = t.check_match_at(x, y);
        let Some(text) = found else { return };
        let (path, line) = resolve(&text, &cwd);
        let at = line.map(|l| format!(" at line {l}")).unwrap_or_default();
        say(if path.is_file() {
            format!("opening files comes later: {}{at}", path.display())
        } else {
            format!("no file called {text} in this session's folder")
        });
    });
    term.add_controller(click);
    term
}

/// Runs `argv` in the terminal, in `cwd`.
pub fn spawn(term: &vte::Terminal, argv: &[String], cwd: &str, done: impl FnOnce(Result<glib::Pid, glib::Error>) + 'static) {
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let env = child_env();
    let env: Vec<&str> = env.iter().map(String::as_str).collect();
    let cwd = if Path::new(cwd).is_dir() { cwd.to_owned() } else { home().to_string_lossy().into_owned() };
    term.spawn_async(vte::PtyFlags::DEFAULT, Some(&cwd), &argv, &env, glib::SpawnFlags::SEARCH_PATH, || {}, -1, None::<&gio::Cancellable>, done);
}
