//! Plane, step 1: the terminal test window.
//!
//! A bare GTK window with a VTE terminal that attaches to a real background session, so the
//! built-in terminal can be compared with kitty before anything else is built. It copies
//! kitty's look (Noctalia palette, 0.8 opacity, 25 px padding, monospace 11) and its copy and
//! paste keys, remaps Shift+Enter to a new line, and reports what a clicked file path would open.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use vte::prelude::*;

/// PCRE2_MULTILINE: VTE requires it for match regexes.
const PCRE2_MULTILINE: u32 = 0x0000_0400;
/// A file path with an extension, optionally followed by :line or :line:column.
const PATH_RE: &str = r"(?:~|\.{1,2})?/?[\w.\-]+(?:/[\w.\-]+)*\.[A-Za-z0-9]{1,8}(?::\d+(?::\d+)?)?";
/// Window class while this runs beside the kitty version (it becomes `plane` at the switch).
const CLASS: &str = "plane-next";

#[derive(Clone)]
struct Choice {
    label: String,
    argv: Vec<String>,
    cwd: Option<String>,
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

fn claude_bin() -> String {
    let local = home().join(".local/bin/claude");
    if local.exists() { local.to_string_lossy().into_owned() } else { "claude".into() }
}

/// Your shell first (to compare plain terminal behaviour), then every live background session.
fn choices() -> Vec<Choice> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into());
    let mut out = vec![Choice { label: "Your shell (to compare)".into(), argv: vec![shell], cwd: Some(home().to_string_lossy().into_owned()) }];
    let Ok(res) = std::process::Command::new(claude_bin()).args(["agents", "--json", "--all"]).output() else { return out };
    let Ok(serde_json::Value::Array(list)) = serde_json::from_slice::<serde_json::Value>(&res.stdout) else { return out };
    let mut sessions: Vec<Choice> = list
        .iter()
        .filter(|s| s["kind"] == "background")
        .filter(|s| s["pid"].as_i64().is_some_and(|pid| Path::new(&format!("/proc/{pid}")).exists()))
        .filter_map(|s| {
            let id = s["id"].as_str()?;
            let name = s["name"].as_str().unwrap_or(id);
            Some(Choice {
                label: format!("{name}  ({})", s["state"].as_str().unwrap_or("?")),
                argv: vec![claude_bin(), "attach".into(), id.into()],
                cwd: s["cwd"].as_str().map(str::to_owned),
            })
        })
        .collect();
    sessions.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
    out.extend(sessions);
    out
}

/// kitty's Noctalia theme file: foreground, background, cursor, selection and color0-15.
fn kitty_theme() -> std::collections::HashMap<String, gdk::RGBA> {
    let path = home().join(".config/kitty/themes/noctalia.conf");
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let (k, v) = (parts.next()?, parts.next()?);
            (!k.starts_with('#')).then(|| gdk::RGBA::parse(v).ok().map(|c| (k.to_string(), c))).flatten()
        })
        .collect()
}

fn color(theme: &std::collections::HashMap<String, gdk::RGBA>, key: &str, fallback: &str) -> gdk::RGBA {
    theme.get(key).copied().unwrap_or_else(|| gdk::RGBA::parse(fallback).unwrap())
}

/// The environment for the attached program: drop kitty's and this session's own variables
/// so Claude sees a plain VTE terminal.
fn child_env() -> Vec<String> {
    let mut env: Vec<String> = std::env::vars()
        .filter(|(k, _)| !(k.starts_with("KITTY_") || k.starts_with("TERM_PROGRAM") || k == "TERM" || k == "COLORTERM"
            || k == "CLAUDECODE" || k.starts_with("CLAUDE_CODE_") || k == "CLAUDE_JOB_DIR"))
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    env.push("TERM=xterm-256color".into());
    env.push("COLORTERM=truecolor".into());
    env
}

/// Turns `src/app.ts:12:4` into (absolute path, line) against the session's folder.
fn resolve(text: &str, cwd: Option<&str>) -> (PathBuf, Option<u32>) {
    let mut parts = text.splitn(3, ':');
    let file = parts.next().unwrap_or(text);
    let line = parts.next().and_then(|l| l.parse().ok());
    let path = if let Some(rest) = file.strip_prefix("~/") {
        home().join(rest)
    } else if Path::new(file).is_absolute() {
        PathBuf::from(file)
    } else {
        Path::new(cwd.unwrap_or("/")).join(file)
    };
    (path, line)
}

struct Ui {
    holder: gtk::Box,
    status: gtk::Label,
    term: RefCell<Option<vte::Terminal>>,
    current: RefCell<Option<Choice>>,
    scale: Cell<f64>,
}

fn new_terminal(ui: &Rc<Ui>) -> vte::Terminal {
    let term = vte::Terminal::new();
    term.set_hexpand(true);
    term.set_vexpand(true);
    let theme = kitty_theme();
    let names = ["color0", "color1", "color2", "color3", "color4", "color5", "color6", "color7",
                 "color8", "color9", "color10", "color11", "color12", "color13", "color14", "color15"];
    let defaults = ["#151c28", "#ff5c6c", "#4ed6a0", "#f5c86a", "#3a9be8", "#e58ab8", "#5ccff0", "#c9d2de",
                    "#5a6880", "#ff7f8b", "#72e3b5", "#ffd88a", "#62b2f0", "#f0a6cb", "#86ddf5", "#f2f5f9"];
    let palette: Vec<gdk::RGBA> = names.iter().zip(defaults).map(|(n, d)| color(&theme, n, d)).collect();
    let palette_refs: Vec<&gdk::RGBA> = palette.iter().collect();
    let bg = color(&theme, "background", "#0a0e16");
    let bg = gdk::RGBA::new(bg.red(), bg.green(), bg.blue(), 0.8); // kitty: background_opacity 0.8
    term.set_colors(Some(&color(&theme, "foreground", "#f2f5f9")), Some(&bg), &palette_refs);
    term.set_color_cursor(Some(&color(&theme, "cursor", "#5ccff0")));
    term.set_color_cursor_foreground(Some(&color(&theme, "cursor_text_color", "#0a0e16")));
    term.set_color_highlight(Some(&color(&theme, "selection_background", "#1f4a73")));
    term.set_color_highlight_foreground(Some(&color(&theme, "selection_foreground", "#f2f5f9")));
    term.set_font(Some(&pango::FontDescription::from_string("monospace 11")));
    term.set_font_scale(ui.scale.get());
    term.set_scrollback_lines(10_000);
    term.set_mouse_autohide(true);
    term.set_allow_hyperlink(true);
    term.set_audible_bell(false);

    match vte::Regex::for_match(PATH_RE, PCRE2_MULTILINE) {
        Ok(re) => {
            let tag = term.match_add_regex(&re, 0);
            term.match_set_cursor_name(tag, "pointer");
        }
        Err(e) => ui.status.set_text(&format!("File links are off: {e}")),
    }

    // keys: kitty's copy, paste and zoom keys; Shift+Enter becomes a new line (Ctrl+J), since
    // VTE can't tell Shift+Enter from Enter the way kitty's keyboard protocol does
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(ui);
    let t = term.clone();
    keys.connect_key_pressed(move |_, key, _, mods| {
        let Some(ui) = weak.upgrade() else { return glib::Propagation::Proceed };
        let mods = mods & (gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK
            | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK);
        let ctrl_shift = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK;
        let zoom = |s: f64| { ui.scale.set(s); t.set_font_scale(s); };
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
            gdk::Key::plus | gdk::Key::equal => zoom((ui.scale.get() * 1.1).min(3.0)),
            gdk::Key::minus | gdk::Key::underscore => zoom((ui.scale.get() / 1.1).max(0.5)),
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
    let weak = Rc::downgrade(ui);
    let t = term.clone();
    click.connect_released(move |_, _, x, y| {
        let Some(ui) = weak.upgrade() else { return };
        let (x0, y0) = down.get();
        if (x - x0).hypot(y - y0) > 4.0 {
            return;
        }
        if let Some(uri) = t.check_hyperlink_at(x, y) {
            ui.status.set_text(&format!("Link: {uri}"));
            return;
        }
        let (found, _) = t.check_match_at(x, y);
        let Some(text) = found else { return };
        let cwd = ui.current.borrow().as_ref().and_then(|c| c.cwd.clone());
        let (path, line) = resolve(&text, cwd.as_deref());
        let at = line.map(|l| format!(" at line {l}")).unwrap_or_default();
        if path.is_file() {
            ui.status.set_text(&format!("Would open {}{at}", path.display()));
        } else {
            ui.status.set_text(&format!("No file called {} in this session's folder", text));
        }
    });
    term.add_controller(click);

    let weak = Rc::downgrade(ui);
    term.connect_child_exited(move |_, _| {
        if let Some(ui) = weak.upgrade() {
            ui.status.set_text("This view closed. The session keeps running in the background; pick it again to reopen it.");
        }
    });
    term
}

/// Shows a choice: a fresh terminal each time, so the old one detaches (its `claude attach`
/// gets SIGHUP when the widget goes, which only detaches, like closing a kitty pane).
fn show(ui: &Rc<Ui>, choice: &Choice) {
    if let Some(old) = ui.term.borrow_mut().take() {
        ui.holder.remove(&old);
    }
    let term = new_terminal(ui);
    ui.holder.append(&term);
    let argv: Vec<&str> = choice.argv.iter().map(String::as_str).collect();
    let env = child_env();
    let env: Vec<&str> = env.iter().map(String::as_str).collect();
    let weak = Rc::downgrade(ui);
    let label = choice.label.clone();
    term.spawn_async(
        vte::PtyFlags::DEFAULT,
        choice.cwd.as_deref(),
        &argv,
        &env,
        glib::SpawnFlags::SEARCH_PATH,
        || {},
        -1,
        None::<&gio::Cancellable>,
        move |res| {
            if let Some(ui) = weak.upgrade() {
                match res {
                    Ok(_) => ui.status.set_text(&format!("Showing {label}")),
                    Err(e) => ui.status.set_text(&format!("Couldn't open it: {e}")),
                }
            }
        },
    );
    term.grab_focus();
    *ui.current.borrow_mut() = Some(choice.clone());
    *ui.term.borrow_mut() = Some(term);
}

const CSS: &str = "
window.plane-test { background: transparent; }
vte-terminal { padding: 25px; }
.plane-bar { background: #151c28; padding: 4px 10px; }
.plane-bar label.dim, .plane-status { color: #8c99ad; }
.plane-status { background: #0c111a; padding: 3px 10px; }
";

fn build(app: &gtk::Application) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().expect("no display"), &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);

    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("Plane (terminal test)")
        .default_width(1346)
        .default_height(713)
        .build();
    window.add_css_class("plane-test");

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    bar.add_css_class("plane-bar");
    let list = Rc::new(RefCell::new(choices()));
    let labels: Vec<String> = list.borrow().iter().map(|c| c.label.clone()).collect();
    let model = gtk::StringList::new(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    let picker = gtk::DropDown::new(Some(model.clone()), None::<gtk::Expression>);
    let refresh = gtk::Button::with_label("Refresh list");
    let hint = gtk::Label::new(Some("Shift+Enter: new line · Ctrl+Shift+C / V: copy / paste · click a file path to test opening it"));
    hint.add_css_class("dim");
    hint.set_hexpand(true);
    hint.set_xalign(1.0);
    hint.set_ellipsize(pango::EllipsizeMode::End);
    bar.append(&gtk::Label::new(Some("Show")));
    bar.append(&picker);
    bar.append(&refresh);
    bar.append(&hint);

    let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
    holder.set_vexpand(true);
    let status = gtk::Label::new(Some(
        "Pick a session that isn't open in Plane's right-hand pane: two views of one session at different sizes garble each other."));
    status.add_css_class("plane-status");
    status.set_xalign(0.0);
    status.set_ellipsize(pango::EllipsizeMode::End);
    root.append(&bar);
    root.append(&holder);
    root.append(&status);
    window.set_child(Some(&root));

    let ui = Rc::new(Ui { holder, status, term: RefCell::new(None), current: RefCell::new(None), scale: Cell::new(1.0) });

    // `plane-gui <job id>` opens that session straight away
    let wanted = std::env::args().nth(1);
    let start = wanted
        .and_then(|id| list.borrow().iter().position(|c| c.argv.last() == Some(&id)))
        .unwrap_or(0);
    picker.set_selected(start as u32);
    show(&ui, &list.borrow()[start]);

    // a refresh swaps the list under the picker; that must not reopen the session being shown
    let refreshing = Rc::new(Cell::new(false));
    let (u, l, busy) = (ui.clone(), list.clone(), refreshing.clone());
    picker.connect_selected_notify(move |p| {
        if busy.get() {
            return;
        }
        let i = p.selected() as usize;
        let choice = l.borrow().get(i).cloned();
        if let Some(choice) = choice {
            show(&u, &choice);
        }
    });
    let (u, l, m, p, busy) = (ui.clone(), list.clone(), model, picker.clone(), refreshing);
    refresh.connect_clicked(move |_| {
        let current = u.current.borrow().as_ref().map(|c| c.argv.clone());
        let fresh = choices();
        let keep = current.and_then(|argv| fresh.iter().position(|c| c.argv == argv));
        *l.borrow_mut() = fresh;
        let labels: Vec<String> = l.borrow().iter().map(|c| c.label.clone()).collect();
        // swap the list without reopening the session being shown
        busy.set(true);
        let n = m.n_items();
        m.splice(0, n, &labels.iter().map(String::as_str).collect::<Vec<_>>());
        p.set_selected(keep.map(|i| i as u32).unwrap_or(gtk::INVALID_LIST_POSITION));
        busy.set(false);
        u.status.set_text(if keep.is_some() { "List refreshed." } else { "List refreshed. The session shown here has ended or been paused." });
    });

    window.present();
}

fn main() -> glib::ExitCode {
    glib::set_prgname(Some(CLASS));
    glib::set_application_name("Plane");
    let app = gtk::Application::builder().flags(gio::ApplicationFlags::NON_UNIQUE).build();
    app.connect_activate(build);
    // GTK must not see our own arguments (a session id), so give it none
    app.run_with_args(&["plane-gui"])
}
