//! The built-in terminal, as tested in step 1: kitty's look (the Noctalia palette, 0.8
//! opacity, 25 px padding, monospace 11), kitty's copy, paste and zoom keys, Shift+Enter
//! as a new line, and file paths and links that open when clicked.

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

/// Lines per mouse-wheel notch in a terminal's own scrollback (kitty's default).
const WHEEL_LINES: f64 = 5.0;
/// Touchpad, in a session: finger travel (pixels) per wheel click sent to Claude.
const TOUCHPAD_PX_PER_CLICK: f64 = 30.0;
/// The terminal's padding (`vte-terminal { padding: 25px }`), to find the cell under the pointer.
const PADDING: f64 = 25.0;

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

/// A terminal for one tab. `open` gets the text of a clicked file path or hyperlink.
/// `wheel_to_app` is set once the terminal runs `claude attach`, which takes mouse events.
pub fn new_terminal(theme: &HashMap<String, gdk::RGBA>, scale: Rc<Cell<f64>>, open: Rc<dyn Fn(String)>, wheel_to_app: Rc<Cell<bool>>) -> vte::Terminal {
    let term = vte::Terminal::new();
    term.set_hexpand(true);
    term.set_vexpand(true);
    apply_colors(&term, theme);
    term.set_font(Some(&pango::FontDescription::from_string("monospace 11")));
    term.set_font_scale(scale.get());
    term.set_scrollback_lines(10_000);
    // Scrolling is done here, not by VTE. `claude attach` runs full screen and asks for
    // mouse events, so the terminal has nothing to scroll itself: it passes the wheel to
    // Claude, which scrolls several lines per wheel click. VTE turned every bit of touchpad
    // movement into clicks, which flew through the conversation. Here a mouse wheel sends
    // one click per notch, as kitty does, and a touchpad one click per TOUCHPAD_PX_PER_CLICK
    // of finger travel, with nothing carrying on after you stop.
    term.set_scroll_unit_is_pixels(true);
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
    let pointer = Rc::new(Cell::new((0.0f64, 0.0f64)));
    let motion = gtk::EventControllerMotion::new();
    let p = pointer.clone();
    motion.connect_motion(move |_, x, y| p.set((x, y)));
    term.add_controller(motion);
    let owed = Rc::new(Cell::new(0.0f64)); // part of a click not sent yet
    let o = owed.clone();
    scroll.connect_scroll_end(move |_| o.set(0.0));
    let t = term.clone();
    scroll.connect_scroll(move |c, _, dy| {
        let Some(adj) = t.vadjustment() else { return glib::Propagation::Proceed };
        let top = adj.upper() - adj.page_size();
        let wheel = c.unit() == gdk::ScrollUnit::Wheel;
        if top > adj.lower() {
            // a plain terminal with scrollback: scroll it
            let step = if wheel { dy * WHEEL_LINES * t.char_height() as f64 } else { dy };
            adj.set_value((adj.value() + step).clamp(adj.lower(), top));
            return glib::Propagation::Stop;
        }
        if !wheel_to_app.get() {
            return glib::Propagation::Proceed; // a program that didn't ask for the mouse
        }
        let mut n = owed.get() + if wheel { dy } else { dy / TOUCHPAD_PX_PER_CLICK };
        if n.signum() != owed.get().signum() && owed.get() != 0.0 {
            n = if wheel { dy } else { dy / TOUCHPAD_PX_PER_CLICK }; // turned round: start afresh
        }
        let clicks = n.trunc();
        owed.set(n - clicks);
        let (x, y) = pointer.get();
        let (cw, ch) = (t.char_width().max(1) as f64, t.char_height().max(1) as f64);
        let col = (((x - PADDING) / cw).floor() as i64 + 1).clamp(1, t.column_count().max(1));
        let row = (((y - PADDING) / ch).floor() as i64 + 1).clamp(1, t.row_count().max(1));
        let button = if clicks < 0.0 { 64 } else { 65 }; // SGR mouse: wheel up, wheel down
        for _ in 0..(clicks.abs() as i64).min(20) {
            t.feed_child(format!("\x1b[<{button};{col};{row}M").as_bytes());
        }
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
    // a path or link under the pointer says what a click does
    term.set_has_tooltip(true);
    term.connect_query_tooltip(|t, x, y, _, tip| {
        let (x, y) = (x as f64, y as f64);
        let text = match t.check_hyperlink_at(x, y) {
            Some(uri) if uri.starts_with("http://") || uri.starts_with("https://") => "Click to open in the browser",
            Some(_) => "Click to open in Plane",
            None if t.check_match_at(x, y).0.is_some() => "Click to open in Plane",
            None => return false,
        };
        tip.set_text(Some(text));
        true
    });

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

    // a click (not a drag) on a file path or a link opens it. The raw press and release are
    // watched, not a click gesture: VTE takes over the press for its own selection and for
    // the session's mouse use (`claude attach` turns mouse reporting on), so a gesture never
    // hears the release. Both still reach the terminal untouched.
    let raw = gtk::EventControllerLegacy::new();
    raw.set_propagation_phase(gtk::PropagationPhase::Capture);
    let down: Rc<Cell<Option<(f64, f64)>>> = Rc::default();
    let t = term.clone();
    raw.connect_event(move |_, ev| {
        let left = ev.downcast_ref::<gdk::ButtonEvent>().is_some_and(|b| b.button() == 1);
        let pressed = ev.event_type() == gdk::EventType::ButtonPress;
        if !left || !(pressed || ev.event_type() == gdk::EventType::ButtonRelease) {
            return glib::Propagation::Proceed;
        }
        let Some((x, y)) = event_point(&t, ev) else { return glib::Propagation::Proceed };
        if pressed {
            down.set(Some((x, y)));
            return glib::Propagation::Proceed;
        }
        let Some((x0, y0)) = down.take() else { return glib::Propagation::Proceed };
        if (x - x0).hypot(y - y0) > 4.0 {
            return glib::Propagation::Proceed; // a drag: selecting text
        }
        if let Some(uri) = t.check_hyperlink_at(x, y) {
            open(uri.to_string());
        } else if let (Some(text), _) = t.check_match_at(x, y) {
            open(text.to_string());
        }
        glib::Propagation::Proceed
    });
    term.add_controller(raw);
    term
}

/// Where an event happened, in `w`'s own coordinates (raw events come in the window's).
fn event_point(w: &impl IsA<gtk::Widget>, ev: &gdk::Event) -> Option<(f64, f64)> {
    let (ex, ey) = ev.position()?;
    let native = w.native()?;
    let (nx, ny) = native.surface_transform();
    let p = native.compute_point(w, &gtk::graphene::Point::new((ex - nx) as f32, (ey - ny) as f32))?;
    Some((p.x() as f64, p.y() as f64))
}

/// Runs `argv` in the terminal, in `cwd`.
pub fn spawn(term: &vte::Terminal, argv: &[String], cwd: &str, done: impl FnOnce(Result<glib::Pid, glib::Error>) + 'static) {
    let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
    let env = child_env();
    let env: Vec<&str> = env.iter().map(String::as_str).collect();
    let cwd = if Path::new(cwd).is_dir() { cwd.to_owned() } else { home().to_string_lossy().into_owned() };
    term.spawn_async(vte::PtyFlags::DEFAULT, Some(&cwd), &argv, &env, glib::SpawnFlags::SEARCH_PATH, || {}, -1, None::<&gio::Cancellable>, done);
}
