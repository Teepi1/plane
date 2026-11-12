//! The app: the sidebar (a port of the Python `Sidebar`) beside tabs of built-in terminals.
//!
//! Sections, rows, states, keys and words follow the kitty sidebar. Rows are real list rows
//! that keep their identity across refreshes, so the selection follows the item, and their
//! order is held while you're in the list (keyboard focus or the pointer over it): a click
//! can't land on a row that just moved. States still update in place, and anything new goes
//! to the end of its section until you leave the list.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use plane_core::actions::{self, ClaimError};
use plane_core::state::session_state;
use plane_core::{hypr, lost_label, num, pget, py_str, truthy, Env, Fs, Mode};
use serde_json::Value;
use vte::prelude::*;

use crate::editor::{self, Editor};
use crate::store::{Snap, Store};
use crate::text::{ago, esc, fmt_reset, now, short_needs, tilde};
use crate::{term, theme};
use plane_core::files;

/// Newest handoffs listed before "N more handoffs" (the list folds above 5).
const HANDOFFS_SHOWN: usize = 4;
/// Sessions kept attached in tabs at once. Each `claude attach` holds about 150 MB, so
/// opening one more reuses the tab you looked at longest ago.
const MAX_ATTACHED: usize = 4;
/// A click on a row that moved this recently is refused (seconds).
const MOVED_GUARD: f64 = 0.7;
/// Messages stay this long (seconds).
const MESSAGE_SECS: f64 = 6.0;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Section {
    Lost,
    Sessions,
    Paused,
    Windows,
    Handoffs,
}

impl Section {
    const ALL: [Section; 5] = [Section::Lost, Section::Sessions, Section::Paused, Section::Windows, Section::Handoffs];

    fn title(self) -> &'static str {
        match self {
            Section::Lost => "Cut off by a crash",
            Section::Sessions => "Sessions",
            Section::Paused => "Paused",
            Section::Windows => "In other windows",
            Section::Handoffs => "Handoffs",
        }
    }
    /// The short dim words beside the title: what the rows in it are.
    fn caption(self) -> &'static str {
        match self {
            Section::Lost => "resume or close",
            Section::Sessions => "running",
            Section::Paused => "open to continue",
            Section::Windows => "click to go there",
            Section::Handoffs => "waiting to start",
        }
    }
    /// Its name in `app.json`'s "collapsed" list.
    fn key(self) -> String {
        format!("{self:?}")
    }
    fn order(self) -> u8 {
        self as u8
    }
}

/// What Files follows: the session you last picked, or a tab's folder.
#[derive(Clone, Debug)]
enum Follow {
    Session(String),
    Folder(Origin),
}

/// `~/.config/plane/app.json`: the app's own remembered choices (folded sections).
fn app_config_path() -> PathBuf {
    theme::theme_path().with_file_name("app.json")
}

fn load_collapsed() -> HashSet<String> {
    plane_core::read_json(&app_config_path())
        .and_then(|v| v.get("collapsed").and_then(Value::as_array).cloned())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

/// The pointer becomes a hand over anything you can click.
fn clickable(w: &impl IsA<gtk::Widget>) {
    w.set_cursor_from_name(Some("pointer"));
}

/// A short press look on a row (the list takes the click itself, so `:active` never shows).
fn flash_pressed(w: &impl IsA<gtk::Widget>) {
    let w = w.clone().upcast::<gtk::Widget>();
    w.add_css_class("pressed");
    glib::timeout_add_local_once(std::time::Duration::from_millis(140), move || w.remove_css_class("pressed"));
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// A section's heading row: click to fold or show it.
    Head,
    Session,
    Lost,
    Handoff,
    More,
    Note,
}

#[derive(Clone)]
struct Item {
    key: String,
    kind: Kind,
    section: Section,
    sub: u8,
    obj: Value,
}

struct Row {
    row: gtk::ListBoxRow,
    shown: gtk::Label,
    icon: gtk::Label,
    name: gtk::Label,
    state: gtk::Label,
    /// Heading rows only: the dim words after the count.
    #[allow(dead_code)]
    caption: gtk::Label,
}

#[derive(Clone, PartialEq, Debug)]
enum TabKind {
    /// Attached to a background session (its job id).
    Session(String),
    /// Running `claude --bg`, waiting to learn the job id.
    Starting(Option<PathBuf>),
    /// A handoff's prompt, read-only (the handoff's id).
    Prompt(String),
    /// A file, in an editor or as a "can't show this" page (its full path).
    File(PathBuf),
}

/// Where a file was opened from: the folder its Files button shows, and the session.
#[derive(Clone, Debug)]
struct Origin {
    root: PathBuf,
    /// The session's job id, when it came from one.
    session: Option<String>,
    name: String,
}

struct Tab {
    kind: RefCell<TabKind>,
    page: gtk::Widget,
    title: gtk::Label,
    name: String,
    editor: RefCell<Option<Rc<Editor>>>,
    origin: RefCell<Option<Origin>>,
    term: Option<vte::Terminal>,
    shown_at: Cell<f64>,
    /// The tab is being taken away: its terminal's exit isn't news.
    closing: Cell<bool>,
    /// Its terminal runs `claude attach`, which takes the mouse wheel (see term.rs).
    wheel_to_app: Rc<Cell<bool>>,
}

enum Step {
    Folder,
    Path,
    Name(String),
    Message(String, String),
}

/// What a start does: a new session, a handoff, or a cut-off conversation picked up again.
#[derive(Default, Clone)]
struct StartOpts {
    name: String,
    prompt: String,
    resume: Option<String>,
    handoff: Option<PathBuf>,
}

type Confirm = Box<dyn FnOnce(&Rc<App>)>;

pub struct App {
    env: Env,
    home: String,
    window: gtk::ApplicationWindow,
    store: Store,
    sidebar: gtk::Box,
    needs_btn: gtk::Button,
    turn_btn: gtk::Button,
    switch_sessions: gtk::ToggleButton,
    switch_files: gtk::ToggleButton,
    new_btn: gtk::Button,
    /// The switcher is being set from code: its toggles aren't clicks.
    switching: Cell<bool>,
    /// The selection fell on a heading only because nothing else was listed yet.
    auto_head: Cell<bool>,
    help: gtk::Widget,
    /// Sections and Files groups folded by hand (remembered in app.json).
    collapsed: RefCell<HashSet<String>>,
    usage_rows: Vec<(gtk::ProgressBar, gtk::Label, gtk::Label)>,
    usage_note: gtk::Label,
    stack: gtk::Stack,
    scroller: gtk::ScrolledWindow,
    list: gtk::ListBox,
    details: gtk::Box,
    message: gtk::Label,
    confirm_bar: gtk::Box,
    confirm_q: gtk::Label,
    confirm_yes: gtk::Button,
    hints: gtk::Label,
    right: gtk::Stack,
    notebook: gtk::Notebook,
    // the new-session page
    ns_step: gtk::Label,
    ns_ask: gtk::Label,
    ns_folders: gtk::ListBox,
    ns_entry: gtk::Entry,
    ns_error: gtk::Label,
    ns_hint: gtk::Label,
    ns_state: RefCell<Step>,
    // the files page
    fp_title: gtk::Label,
    fp_where: gtk::Label,
    fp_store: gio::ListStore,
    fp_tree: gtk::TreeListModel,
    fp_view: gtk::ListView,
    fp_origin: RefCell<Option<Origin>>,
    /// Items in the store before the folder tree (the changed lists).
    fp_split: Cell<u32>,
    fp_shown: RefCell<Vec<String>>,
    fp_ignored: Rc<RefCell<HashSet<PathBuf>>>,
    fp_timer: RefCell<Option<glib::SourceId>>,
    /// What Files shows next time it opens (or now, when it's open).
    fp_follow: RefCell<Option<Follow>>,
    /// A folder picked by hand: it stays until the next click on a session.
    fp_pinned: Cell<bool>,
    /// What each session's transcripts say about edits, read a little more each time.
    scans: Arc<Mutex<HashMap<PathBuf, files::TranscriptScan>>>,
    /// Unsaved files were dealt with: the window may close.
    allow_close: Cell<bool>,
    // state
    snap: RefCell<Snap>,
    items: RefCell<Vec<Item>>,
    rows: RefCell<HashMap<String, Row>>,
    order: Rc<RefCell<HashMap<String, (u8, u8, usize)>>>,
    placed: RefCell<HashMap<String, (usize, f64)>>,
    tabs: RefCell<Vec<Rc<Tab>>>,
    handoffs_open: Cell<bool>,
    last_discard: RefCell<Option<PathBuf>>,
    confirm: RefCell<Option<Confirm>>,
    flash: RefCell<Option<(String, &'static str, f64)>>,
    focus_in: Cell<bool>,
    hover: Cell<bool>,
    syncing: Cell<bool>,
    pending_key: RefCell<Option<String>>,
    press: RefCell<(String, f64)>,
    menu_item: RefCell<Option<Item>>,
    /// What the list and the details panel last showed: a refresh that changes neither
    /// (most of them) costs no layout.
    shown_print: RefCell<String>,
    shown_details: RefCell<String>,
    scale: Rc<Cell<f64>>,
    colors: RefCell<HashMap<String, gdk::RGBA>>,
    provider: gtk::CssProvider,
    monitor: RefCell<Option<gio::FileMonitor>>,
    commands: Arc<Mutex<Vec<(String, UnixStream)>>>,
}

fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    for c in classes {
        l.add_css_class(c);
    }
    l
}

const TONES: [&str; 5] = ["ok", "warn", "bad", "accent", "dim"];

fn set_tone(w: &impl IsA<gtk::Widget>, tone: &str) {
    for t in TONES {
        if t != tone {
            w.remove_css_class(t);
        }
    }
    if !tone.is_empty() {
        w.add_css_class(tone);
    }
}

fn sval<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

/// The job id `claude --bg` prints: "… backgrounded · 655d7b29".
fn backgrounded_id(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(i) = rest.find("backgrounded") {
        rest = &rest[i + "backgrounded".len()..];
        let after = rest.trim_start();
        if let Some(after) = after.strip_prefix('·') {
            let id: String = after.trim_start().chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            if id.len() >= 6 {
                return Some(id);
            }
        }
    }
    None
}

impl App {
    pub fn build(app: &gtk::Application, commands: Arc<Mutex<Vec<(String, UnixStream)>>>) -> Rc<App> {
        let env = Env::from_env();
        let home = env.home.to_string_lossy().into_owned();
        let provider = gtk::CssProvider::new();
        let defines = theme::defines();
        provider.load_from_string(&theme::css(&defines));
        gtk::style_context_add_provider_for_display(&gdk::Display::default().expect("no display"), &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        editor::install_scheme(&theme::colors(&defines));

        let window = gtk::ApplicationWindow::builder().application(app).title("Plane").default_width(1346).default_height(713).build();
        window.add_css_class("plane");
        window.set_decorated(false); // like the kitty window: Hyprland draws the frame

        // ---- sidebar
        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 0);
        sidebar.add_css_class("sidebar");
        sidebar.set_size_request(220, -1);
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        head.add_css_class("head");
        let brand = label("PLANE", &["brand"]);
        brand.set_hexpand(true);
        // "2 need you" and "1 your turn": each click selects the next session in that state
        let needs_btn = gtk::Button::with_label("");
        needs_btn.add_css_class("count-btn");
        needs_btn.add_css_class("needs");
        needs_btn.set_tooltip_text(Some("Select the next session that needs you"));
        let turn_btn = gtk::Button::with_label("");
        turn_btn.add_css_class("count-btn");
        turn_btn.add_css_class("turn");
        turn_btn.set_tooltip_text(Some("Select the next session waiting for your message"));
        for b in [&needs_btn, &turn_btn] {
            b.set_focus_on_click(false);
            b.set_valign(gtk::Align::Center);
            clickable(b);
        }
        head.append(&brand);
        head.append(&needs_btn);
        head.append(&turn_btn);
        sidebar.append(&head);
        // one grid, so both bars share a width, as in the kitty sidebar
        let usage = gtk::Grid::new();
        usage.add_css_class("usage-row");
        usage.set_column_spacing(8);
        usage.set_row_spacing(2);
        let mut usage_rows = vec![];
        for (y, name) in ["5h", "wk"].into_iter().enumerate() {
            let l = label(name, &["label"]);
            let bar = gtk::ProgressBar::new();
            bar.add_css_class("usage");
            bar.set_hexpand(true);
            bar.set_valign(gtk::Align::Center);
            let pct = label("", &["strong"]);
            pct.set_width_chars(4);
            pct.set_xalign(1.0);
            let reset = label("", &["dim"]);
            reset.set_ellipsize(pango::EllipsizeMode::End);
            usage.attach(&l, 0, y as i32, 1, 1);
            usage.attach(&bar, 1, y as i32, 1, 1);
            usage.attach(&pct, 2, y as i32, 1, 1);
            usage.attach(&reset, 3, y as i32, 1, 1);
            usage_rows.push((bar, pct, reset));
        }
        sidebar.append(&usage);
        let usage_note = label("", &["dim", "usage-row"]);
        usage_note.set_visible(false);
        sidebar.append(&usage_note);

        // Sessions | Files, side by side, and a new session
        let switch_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        switch_row.add_css_class("switch-row");
        let views = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        views.add_css_class("linked");
        views.add_css_class("switcher");
        views.set_homogeneous(true);
        views.set_hexpand(true);
        let switch_sessions = gtk::ToggleButton::with_label("Sessions");
        switch_sessions.set_tooltip_text(Some("Your sessions and handoffs (Esc)"));
        let switch_files = gtk::ToggleButton::with_label("Files");
        switch_files.set_tooltip_text(Some("The files of the session you picked (f)"));
        switch_files.set_group(Some(&switch_sessions));
        switch_sessions.set_active(true);
        let new_btn = gtk::Button::with_label("+ New");
        new_btn.add_css_class("new");
        new_btn.set_tooltip_text(Some("Start a new session (n)"));
        for b in [switch_sessions.upcast_ref::<gtk::Button>(), switch_files.upcast_ref(), &new_btn] {
            b.set_focus_on_click(false);
            clickable(b);
        }
        views.append(&switch_sessions);
        views.append(&switch_files);
        switch_row.append(&views);
        switch_row.append(&new_btn);
        sidebar.append(&switch_row);

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.set_transition_type(gtk::StackTransitionType::None);
        let list = gtk::ListBox::new();
        list.add_css_class("sessions");
        list.set_selection_mode(gtk::SelectionMode::Single);
        list.set_activate_on_single_click(false);
        clickable(&list);
        let scroller = gtk::ScrolledWindow::new();
        scroller.add_css_class("list");
        scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        scroller.set_kinetic_scrolling(false); // a flick doesn't keep the list gliding
        scroller.set_child(Some(&list));
        scroller.set_vexpand(true);
        stack.add_named(&scroller, Some("list"));

        // new session
        let page = gtk::Box::new(gtk::Orientation::Vertical, 6);
        page.add_css_class("page");
        let ns_step = label("", &["step"]);
        let ns_ask = label("", &["ask"]);
        ns_ask.set_wrap(true);
        let ns_folders = gtk::ListBox::new();
        ns_folders.add_css_class("sessions");
        ns_folders.set_activate_on_single_click(true);
        let folder_scroll = gtk::ScrolledWindow::new();
        folder_scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
        folder_scroll.set_kinetic_scrolling(false);
        folder_scroll.set_child(Some(&ns_folders));
        folder_scroll.set_vexpand(true);
        let ns_entry = gtk::Entry::new();
        let ns_error = label("", &["error"]);
        ns_error.set_wrap(true);
        let ns_hint = label("", &["dim"]);
        ns_hint.set_wrap(true);
        page.append(&ns_step);
        page.append(&ns_ask);
        page.append(&folder_scroll);
        page.append(&ns_entry);
        page.append(&ns_error);
        page.append(&ns_hint);
        clickable(&ns_folders);
        stack.add_named(&page, Some("new"));
        let (help_scroll, help) = help_page();
        stack.add_named(&help_scroll, Some("help"));

        // files: a session's changed files, then its folder as a tree
        let fpage = gtk::Box::new(gtk::Orientation::Vertical, 2);
        fpage.add_css_class("page");
        fpage.add_css_class("files-page");
        let fp_title = label("", &["step"]);
        let fp_where = label("", &["where"]);
        fp_where.set_ellipsize(pango::EllipsizeMode::Start);
        let fp_store = gio::ListStore::new::<gtk::StringObject>();
        let fp_ignored: Rc<RefCell<HashSet<PathBuf>>> = Rc::default();
        let ig = fp_ignored.clone();
        let fp_tree = gtk::TreeListModel::new(fp_store.clone(), false, false, move |item| {
            let s = item.downcast_ref::<gtk::StringObject>()?.string();
            let (kind, path, _) = fp_parse(&s);
            (kind == "dir").then(|| folder_model(Path::new(path), &ig.borrow()).upcast())
        });
        let fp_sel = gtk::SingleSelection::new(Some(fp_tree.clone()));
        fp_sel.set_autoselect(false);
        let fp_view = gtk::ListView::new(Some(fp_sel), None::<gtk::ListItemFactory>);
        let fscroll = gtk::ScrolledWindow::new();
        fscroll.set_hscrollbar_policy(gtk::PolicyType::Never);
        fscroll.set_kinetic_scrolling(false);
        fscroll.set_child(Some(&fp_view));
        fscroll.set_vexpand(true);
        fpage.append(&fp_title);
        fpage.append(&fp_where);
        fpage.append(&fscroll);
        fpage.append(&label("esc back", &["dim"]));
        stack.add_named(&fpage, Some("files"));
        sidebar.append(&stack);

        let details = gtk::Box::new(gtk::Orientation::Vertical, 1);
        details.add_css_class("details");
        sidebar.append(&details);
        let message = label("", &["message"]);
        message.set_wrap(true);
        message.set_wrap_mode(pango::WrapMode::WordChar);
        sidebar.append(&message);
        let confirm_bar = gtk::Box::new(gtk::Orientation::Vertical, 4);
        confirm_bar.add_css_class("confirm");
        let confirm_q = label("", &["question"]);
        confirm_q.set_wrap(true);
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let confirm_yes = gtk::Button::with_label("yes");
        confirm_yes.add_css_class("destructive-action");
        let confirm_no = gtk::Button::with_label("cancel");
        confirm_no.add_css_class("flat");
        let yes_hint = label("y", &["dim"]);
        let no_hint = label("n", &["dim"]);
        buttons.append(&yes_hint);
        buttons.append(&confirm_yes);
        buttons.append(&no_hint);
        buttons.append(&confirm_no);
        confirm_bar.append(&confirm_q);
        confirm_bar.append(&buttons);
        confirm_bar.set_visible(false);
        sidebar.append(&confirm_bar);
        let hints = label("", &["hints"]);
        hints.set_ellipsize(pango::EllipsizeMode::End);
        sidebar.append(&hints);

        // ---- tabs
        let notebook = gtk::Notebook::new();
        notebook.set_scrollable(true);
        notebook.set_show_border(false);
        notebook.set_hexpand(true);
        notebook.set_vexpand(true);
        let empty = gtk::Box::new(gtk::Orientation::Vertical, 8);
        empty.add_css_class("empty");
        empty.set_valign(gtk::Align::Fill);
        let inner = gtk::Box::new(gtk::Orientation::Vertical, 6);
        inner.set_valign(gtk::Align::Center);
        inner.set_vexpand(true);
        let big = gtk::Label::new(Some("No session open"));
        big.add_css_class("big");
        let small = gtk::Label::new(Some("Click a session on the left to open it, or + New to start one."));
        let small2 = gtk::Label::new(Some("Click a file path in a session, or open Files, to edit a file."));
        inner.append(&big);
        inner.append(&small);
        inner.append(&small2);
        empty.append(&inner);
        let right = gtk::Stack::new();
        right.add_named(&empty, Some("empty"));
        right.add_named(&notebook, Some("tabs"));
        right.set_visible_child_name("empty");

        let paned = gtk::Paned::new(gtk::Orientation::Horizontal);
        paned.set_start_child(Some(&sidebar));
        paned.set_end_child(Some(&right));
        paned.set_resize_start_child(false);
        paned.set_shrink_start_child(false);
        paned.set_shrink_end_child(false);
        paned.set_position(300);
        window.set_child(Some(&paned));

        let app = Rc::new(App {
            env,
            home,
            window,
            store: Store::start(Env::from_env()),
            sidebar,
            needs_btn,
            turn_btn,
            switch_sessions,
            switch_files,
            new_btn,
            switching: Cell::new(false),
            auto_head: Cell::new(false),
            help,
            collapsed: RefCell::new(load_collapsed()),
            usage_rows,
            usage_note,
            stack,
            scroller,
            list,
            details,
            message,
            confirm_bar,
            confirm_q,
            confirm_yes,
            hints,
            right,
            notebook,
            ns_step,
            ns_ask,
            ns_folders,
            ns_entry,
            ns_error,
            ns_hint,
            ns_state: RefCell::new(Step::Folder),
            fp_title,
            fp_where,
            fp_store,
            fp_tree,
            fp_view,
            fp_origin: RefCell::new(None),
            fp_split: Cell::new(0),
            fp_shown: RefCell::new(vec![]),
            fp_ignored,
            fp_timer: RefCell::new(None),
            fp_follow: RefCell::new(None),
            fp_pinned: Cell::new(false),
            scans: Arc::default(),
            allow_close: Cell::new(false),
            snap: RefCell::new(Snap::default()),
            items: RefCell::new(vec![]),
            rows: RefCell::new(HashMap::new()),
            order: Rc::new(RefCell::new(HashMap::new())),
            placed: RefCell::new(HashMap::new()),
            tabs: RefCell::new(vec![]),
            handoffs_open: Cell::new(false),
            last_discard: RefCell::new(None),
            confirm: RefCell::new(None),
            flash: RefCell::new(None),
            focus_in: Cell::new(false),
            hover: Cell::new(false),
            syncing: Cell::new(false),
            pending_key: RefCell::new(None),
            press: RefCell::new((String::new(), 0.0)),
            menu_item: RefCell::new(None),
            shown_print: RefCell::new(String::new()),
            shown_details: RefCell::new(String::new()),
            scale: Rc::new(Cell::new(1.0)),
            colors: RefCell::new(theme::colors(&defines)),
            provider,
            monitor: RefCell::new(None),
            commands,
        });
        app.wire(&confirm_no);
        app.sync();
        app.window.present();
        app.list.grab_focus();
        let weak = Rc::downgrade(&app);
        crate::shot::arm(&app.window, Rc::new(move |name: &str| {
            let Some(app) = weak.upgrade() else { return };
            if name == "menu" {
                // right-click the selected row
                if let Some(r) = app.list.selected_row() {
                    let y = r.compute_point(&app.list, &gtk::graphene::Point::new(10.0, 10.0)).map(|p| p.y()).unwrap_or(0.0);
                    app.context_menu(40.0, y as f64);
                }
            } else if let Some(text) = name.strip_prefix("open:") {
                // a file, as a click on its path in a terminal would open it
                let bases = vec![PathBuf::from(&app.home)];
                if let Some(l) = files::parse_link(text) {
                    if let Some(p) = files::resolve(&l.file, &bases, &app.env.home) {
                        app.open_file(&p, l.line, l.col, None);
                    }
                }
            } else if let Some(text) = name.strip_prefix("type:") {
                // typing into the focused editor
                if let Some(ed) = app.current_tab().and_then(|t| t.editor.borrow().clone()) {
                    let b = ed.view.buffer();
                    b.insert_at_cursor(&text.replace('_', " "));
                }
            } else if let Some(cmd) = name.strip_prefix("ed:") {
                if let Some(ed) = app.current_tab().and_then(|t| t.editor.borrow().clone()) {
                    ed.shot_command(cmd);
                }
            } else if name == "files" {
                app.switch_files.set_active(true);
            } else if name == "sessions" {
                app.switch_sessions.set_active(true);
            } else if let Some(sec) = name.strip_prefix("toggle:") {
                if let Some(s) = Section::ALL.into_iter().find(|s| s.key().eq_ignore_ascii_case(sec)) {
                    app.toggle_section(s);
                }
            } else if let Some(what) = name.strip_prefix("hover:") {
                // the hover look, for pictures (broadway has no pointer): hover:row:N, hover:files, hover:new
                let w: Option<gtk::Widget> = match what {
                    "files" => Some(app.switch_files.clone().upcast()),
                    "new" => Some(app.new_btn.clone().upcast()),
                    _ => what.strip_prefix("row:").and_then(|n| n.parse().ok()).and_then(|n| app.list.row_at_index(n)).map(|r| r.upcast()),
                };
                if let Some(w) = w {
                    w.add_css_class("shot-hover"); // theme.rs copies the :hover rules to it
                }
            } else if let Some(key) = gdk::Key::from_name(name) {
                app.on_key(key, gdk::ModifierType::empty());
            }
        }));
        app
    }

    // ------------------------------------------------------------------ wiring

    fn wire(self: &Rc<Self>, confirm_no: &gtk::Button) {
        let order = self.order.clone();
        self.list.set_sort_func(move |a, b| {
            let o = order.borrow();
            let ka = o.get(a.widget_name().as_str()).copied().unwrap_or((9, 9, usize::MAX));
            let kb = o.get(b.widget_name().as_str()).copied().unwrap_or((9, 9, usize::MAX));
            ka.cmp(&kb).into()
        });
        let weak = Rc::downgrade(self);
        self.list.connect_selected_rows_changed(move |_| {
            if let Some(app) = weak.upgrade() {
                if !app.syncing.get() {
                    app.auto_head.set(false);
                    app.show_details();
                    // Files follows the session you pick (a folder picked by hand stays)
                    if let Some(i) = app.current().filter(|i| i.kind == Kind::Session && truthy(pget(&i.obj, "bg"))) {
                        app.follow(Follow::Session(py_str(pget(&i.obj, "id"))), false);
                    }
                }
            }
        });

        // Sessions | Files
        let weak = Rc::downgrade(self);
        self.switch_sessions.connect_toggled(move |b| {
            if let Some(app) = weak.upgrade() {
                if b.is_active() && !app.switching.get() {
                    app.back_to_list();
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.switch_files.connect_toggled(move |b| {
            if let Some(app) = weak.upgrade() {
                if b.is_active() && !app.switching.get() {
                    app.open_files(true);
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.new_btn.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.new_session();
            }
        });
        for (b, state) in [(&self.needs_btn, "needs you"), (&self.turn_btn, "your turn")] {
            let weak = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(app) = weak.upgrade() {
                    app.select_next_in_state(state);
                }
            });
        }

        // a click anywhere in the sidebar gives it the keyboard (Esc, the letter keys), unless
        // the click itself sent focus somewhere (a session opened on the right, say)
        let any_click = gtk::GestureClick::new();
        any_click.set_button(0);
        any_click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        any_click.connect_pressed(move |_, _, _, _| {
            let weak = weak.clone();
            glib::idle_add_local_once(move || {
                let Some(app) = weak.upgrade() else { return };
                let focus = gtk::prelude::GtkWindowExt::focus(&app.window);
                let inside = focus.as_ref().is_some_and(|w| w.is_ancestor(&app.sidebar));
                let in_popover = focus.as_ref().is_some_and(|w| w.ancestor(gtk::Popover::static_type()).is_some());
                if !inside && !in_popover {
                    app.focus_list();
                }
            });
        });
        self.sidebar.add_controller(any_click);

        // left click: one click opens, a handoff needs two (it starts a whole session)
        let click = gtk::GestureClick::new();
        click.set_button(1);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |g, n, _x, y| {
            if let Some(app) = weak.upgrade() {
                g.set_state(gtk::EventSequenceState::Claimed); // the list's own click would pull focus back
                app.click(n, y);
            }
        });
        self.list.add_controller(click);

        // right click: the row's menu
        let menu_click = gtk::GestureClick::new();
        menu_click.set_button(3);
        let weak = Rc::downgrade(self);
        menu_click.connect_pressed(move |g, _, x, y| {
            if let Some(app) = weak.upgrade() {
                g.set_state(gtk::EventSequenceState::Claimed);
                app.context_menu(x, y);
            }
        });
        self.list.add_controller(menu_click);
        self.install_actions();

        // the order holds while you're in the list: keyboard focus or the pointer over it
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(self);
        focus.connect_enter(move |_| {
            if let Some(app) = weak.upgrade() {
                app.focus_in.set(true);
            }
        });
        let weak = Rc::downgrade(self);
        focus.connect_leave(move |_| {
            if let Some(app) = weak.upgrade() {
                app.focus_in.set(false);
                app.cancel_confirm();
                app.sync();
            }
        });
        self.sidebar.add_controller(focus);
        let motion = gtk::EventControllerMotion::new();
        let weak = Rc::downgrade(self);
        motion.connect_enter(move |_, _, _| {
            if let Some(app) = weak.upgrade() {
                app.hover.set(true);
            }
        });
        let weak = Rc::downgrade(self);
        motion.connect_leave(move |_| {
            if let Some(app) = weak.upgrade() {
                app.hover.set(false);
                app.sync();
            }
        });
        self.scroller.add_controller(motion);

        // keys: only while the sidebar has focus, so a terminal never loses a key to the app
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, mods| match weak.upgrade() {
            Some(app) => app.on_key(key, mods),
            None => glib::Propagation::Proceed,
        });
        self.sidebar.add_controller(keys);

        let weak = Rc::downgrade(self);
        self.confirm_yes.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.run_confirm();
            }
        });
        let weak = Rc::downgrade(self);
        confirm_no.connect_clicked(move |_| {
            if let Some(app) = weak.upgrade() {
                app.cancel_confirm();
                app.list.grab_focus();
            }
        });

        // tabs
        let weak = Rc::downgrade(self);
        self.notebook.connect_switch_page(move |_, page, _| {
            if let Some(app) = weak.upgrade() {
                if let Some(tab) = app.tabs.borrow().iter().find(|t| &t.page == page) {
                    tab.shown_at.set(now());
                }
                let a = app.clone();
                glib::idle_add_local_once(move || {
                    a.mark_shown();
                    a.follow_current_tab(); // after the tab's folder is set
                });
            }
        });

        // files
        self.wire_files_page();
        // unsaved files are asked about before the window goes
        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |_| {
            let Some(app) = weak.upgrade() else { return glib::Propagation::Proceed };
            if app.allow_close.get() || std::env::var_os("PLANE_SHOT").is_some() {
                return glib::Propagation::Proceed; // (an off-screen picture quits without asking)
            }
            let dirty = app.dirty_editors();
            if dirty.is_empty() {
                return glib::Propagation::Proceed;
            }
            app.ask_quit(dirty);
            glib::Propagation::Stop
        });

        // new-session page
        let weak = Rc::downgrade(self);
        self.ns_folders.connect_row_activated(move |_, row| {
            if let Some(app) = weak.upgrade() {
                app.folder_chosen(row.widget_name().to_string());
            }
        });
        let weak = Rc::downgrade(self);
        self.ns_entry.connect_activate(move |_| {
            if let Some(app) = weak.upgrade() {
                app.entry_done();
            }
        });
        let tab_keys = gtk::EventControllerKey::new();
        tab_keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        tab_keys.connect_key_pressed(move |_, key, _, _| {
            let Some(app) = weak.upgrade() else { return glib::Propagation::Proceed };
            if key == gdk::Key::Tab && matches!(*app.ns_state.borrow(), Step::Path) {
                let done = actions::complete_dir(&app.env, &app.ns_entry.text());
                app.ns_entry.set_text(&done);
                app.ns_entry.set_position(-1);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.ns_entry.add_controller(tab_keys);
        let help_click = gtk::GestureClick::new();
        let weak = Rc::downgrade(self);
        help_click.connect_pressed(move |_, _, _, _| {
            if let Some(app) = weak.upgrade() {
                app.back_to_list();
            }
        });
        if let Some(help) = self.stack.child_by_name("help") {
            help.add_controller(help_click);
        }

        // the poll: fresh data, control commands, messages running out
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(std::time::Duration::from_millis(120), move || {
            let Some(app) = weak.upgrade() else { return glib::ControlFlow::Break };
            app.tick();
            glib::ControlFlow::Continue
        });
        let weak = Rc::downgrade(self);
        glib::timeout_add_seconds_local(15, move || {
            let Some(app) = weak.upgrade() else { return glib::ControlFlow::Break };
            app.show_usage(); // "resets in", "started 5m ago" move on
            app.show_details();
            app.refresh_who(None); // "edited this 5m ago" too
            glib::ControlFlow::Continue
        });

        // colors follow Noctalia: the template's output is watched
        let file = gio::File::for_path(theme::theme_path());
        if let Ok(mon) = file.monitor_file(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>) {
            let weak = Rc::downgrade(self);
            mon.connect_changed(move |_, _, _, event| {
                if matches!(event, gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::Created | gio::FileMonitorEvent::Renamed | gio::FileMonitorEvent::MovedIn) {
                    if let Some(app) = weak.upgrade() {
                        app.reload_theme();
                    }
                }
            });
            *self.monitor.borrow_mut() = Some(mon);
        }
    }

    fn reload_theme(&self) {
        let defines = theme::defines();
        self.provider.load_from_string(&theme::css(&defines));
        *self.colors.borrow_mut() = theme::colors(&defines);
        let colors = self.colors.borrow();
        editor::install_scheme(&colors);
        for tab in self.tabs.borrow().iter() {
            if let Some(t) = &tab.term {
                term::apply_colors(t, &colors);
            }
            if let Some(ed) = tab.editor.borrow().as_ref() {
                ed.set_scheme(&colors);
            }
        }
    }

    fn tick(self: &Rc<Self>) {
        if let Some(fresh) = self.store.take() {
            {
                let mut snap = self.snap.borrow_mut();
                if fresh.loaded {
                    *snap = fresh;
                } else {
                    snap.error = fresh.error; // keep the last good list, say what failed
                }
            }
            self.sync();
        }
        let cmds: Vec<(String, UnixStream)> = std::mem::take(&mut *self.commands.lock().unwrap());
        for (cmd, mut stream) in cmds {
            let reply = self.command(cmd.trim());
            use std::io::Write;
            let _ = stream.write_all(format!("{reply}\n").as_bytes());
        }
        let expired = self.flash.borrow().as_ref().is_some_and(|(_, _, at)| now() - at > MESSAGE_SECS);
        if expired {
            *self.flash.borrow_mut() = None;
            self.show_message();
        }
    }

    /// A command from `plane-next open` / `plane-next focus` (Super+arrows).
    fn command(self: &Rc<Self>, cmd: &str) -> &'static str {
        let focus = gtk::prelude::GtkWindowExt::focus(&self.window);
        let in_sidebar = focus.as_ref().is_some_and(|w| w.is_ancestor(&self.sidebar) || w == self.sidebar.upcast_ref::<gtk::Widget>());
        match cmd {
            "raise" => {
                self.window.present();
                std::thread::spawn(|| hypr::focus("class:^(plane-next)$"));
                "ok"
            }
            "focus left" if !in_sidebar => {
                self.focus_list();
                "ok"
            }
            "focus right" if in_sidebar => match self.current_tab() {
                Some(tab) => {
                    self.focus_tab(&tab);
                    "ok"
                }
                None => "edge",
            },
            _ => "edge",
        }
    }

    // ------------------------------------------------------------------ the list

    fn snap_lists(&self) -> (Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>, Vec<Value>) {
        let snap = self.snap.borrow();
        let lost: Vec<Value> = snap.lost.iter().filter(|r| pget(r, "cause") != "standby").cloned().collect();
        let standby: Vec<Value> = snap.lost.iter().filter(|r| pget(r, "cause") == "standby").cloned().collect();
        let mine: Vec<Value> = snap.sessions.iter().filter(|s| truthy(pget(s, "bg")) && !truthy(pget(s, "stopped"))).cloned().collect();
        let paused: Vec<Value> = snap.sessions.iter().filter(|s| truthy(pget(s, "bg")) && truthy(pget(s, "stopped"))).cloned().collect();
        let windows: Vec<Value> = snap.sessions.iter().filter(|s| !truthy(pget(s, "bg"))).cloned().collect();
        let mut handoffs = snap.handoffs.clone();
        handoffs.sort_by(|a, b| num(pget(b, "createdAt")).partial_cmp(&num(pget(a, "createdAt"))).unwrap_or(std::cmp::Ordering::Equal));
        (lost, mine, standby, paused, windows, handoffs)
    }

    fn build_items(&self) -> Vec<Item> {
        let (lost, mine, standby, paused, windows, handoffs) = self.snap_lists();
        let loaded = self.snap.borrow().loaded;
        let mut items = vec![];
        // `sub` orders rows inside a section; 0 is the heading's
        let mut push = |kind, section, sub, obj: Value| {
            let id = match kind {
                Kind::Lost => py_str(pget(&obj, "sessionId")),
                Kind::More => "handoffs".into(),
                Kind::Note => format!("{section:?}"),
                _ => py_str(pget(&obj, "id")),
            };
            let key = format!("{kind:?}:{id}");
            items.push(Item { key, kind, section, sub, obj });
        };
        for r in lost {
            push(Kind::Lost, Section::Lost, 1, r);
        }
        let none_running = mine.is_empty() && standby.is_empty();
        for s in mine {
            push(Kind::Session, Section::Sessions, 1, s);
        }
        for r in standby {
            push(Kind::Lost, Section::Sessions, 2, r);
        }
        if none_running {
            let text = if loaded { "None running. Click + New to start one." } else { "loading sessions…" };
            push(Kind::Note, Section::Sessions, 10, Value::String(text.into()));
        }
        for s in paused {
            push(Kind::Session, Section::Paused, 1, s);
        }
        for s in windows {
            push(Kind::Session, Section::Windows, 1, s);
        }
        let total = handoffs.len();
        let fold = total > HANDOFFS_SHOWN + 1;
        let hidden = if fold && !self.handoffs_open.get() { total - HANDOFFS_SHOWN } else { 0 };
        for h in handoffs.into_iter().take(total - hidden) {
            push(Kind::Handoff, Section::Handoffs, 1, h);
        }
        if fold {
            push(Kind::More, Section::Handoffs, 2, serde_json::json!({ "hidden": hidden }));
        }
        if total == 0 && loaded {
            push(Kind::Note, Section::Handoffs, 10, Value::String("None queued.".into()));
        }
        // each section with rows gets a heading row; a folded section shows only its heading
        let collapsed = self.collapsed.borrow();
        let mut out = vec![];
        for sec in Section::ALL {
            let rows: Vec<Item> = items.iter().filter(|i| i.section == sec).cloned().collect();
            if rows.is_empty() {
                continue;
            }
            let count = if sec == Section::Handoffs { total } else { rows.iter().filter(|i| !matches!(i.kind, Kind::Note | Kind::More)).count() };
            let folded = collapsed.contains(&sec.key());
            out.push(Item { key: format!("Head:{sec:?}"), kind: Kind::Head, section: sec, sub: 0, obj: serde_json::json!({ "count": count, "folded": folded }) });
            if !folded {
                out.extend(rows);
            }
        }
        out
    }

    /// (icon, icon tone, name, state word, state tone, emphasis) for one row.
    fn row_look(&self, item: &Item) -> (String, &'static str, String, String, &'static str, &'static str) {
        let o = &item.obj;
        match item.kind {
            Kind::Head => {
                let chevron = if truthy(pget(o, "folded")) { "▸" } else { "▾" };
                let tone = if item.section == Section::Lost { "warn" } else { "" };
                (chevron.into(), "dim", item.section.title().into(), py_str(pget(o, "count")), tone, "")
            }
            Kind::Session => {
                let state = session_state(o);
                let (icon, tone) = match state {
                    "working" => ("●", "ok"),
                    "needs you" => ("!", "warn"),
                    "your turn" => ("○", "accent"),
                    "paused" => ("‖", "dim"),
                    "failed" => ("✕", "bad"),
                    _ => ("○", "dim"),
                };
                let name = py_str(plane_core::or(&[pget(o, "name"), pget(o, "id"), &Value::String("?".into())]));
                let emph = if truthy(pget(o, "stopped")) { "dim" } else if state == "needs you" { "bold" } else { "" };
                (icon.into(), tone, name, state.into(), tone, emph)
            }
            Kind::Lost if pget(o, "cause") == "standby" => {
                if pget(o, "jobState") == "blocked" {
                    ("!".into(), "warn", lost_label(o), "needs you".into(), "warn", "bold")
                } else {
                    ("○".into(), "accent", lost_label(o), "your turn".into(), "accent", "")
                }
            }
            Kind::Lost => {
                let cause = sval(o, "cause");
                ("↯".into(), "warn", lost_label(o), if cause == "crashed" { "crash".into() } else { cause.into() }, "warn", "")
            }
            Kind::More => {
                let hidden = num(pget(o, "hidden")) as usize;
                if hidden > 0 {
                    ("+".into(), "dim", format!("{hidden} more handoffs"), String::new(), "dim", "dim")
                } else {
                    ("−".into(), "dim", "show fewer".into(), String::new(), "dim", "dim")
                }
            }
            Kind::Handoff if truthy(pget(o, "ready")) => ("→".into(), "accent", sval(o, "title").to_owned(), String::new(), "accent", ""),
            Kind::Handoff => ("…".into(), "warn", sval(o, "title").to_owned(), "not ready".into(), "warn", "dim"),
            Kind::Note => (String::new(), "dim", o.as_str().unwrap_or("").to_owned(), String::new(), "dim", "dim"),
        }
    }

    fn new_row(&self, item: &Item) -> Row {
        let row = gtk::ListBoxRow::new();
        row.set_widget_name(&item.key);
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let shown = label("", &["shown"]);
        let icon = label("", &["icon"]);
        icon.set_xalign(0.5);
        let name = label("", &["name"]);
        name.set_hexpand(true);
        name.set_ellipsize(pango::EllipsizeMode::End);
        let state = label("", &["state"]);
        let caption = label("", &["caption"]);
        caption.set_visible(false);
        b.append(&shown);
        b.append(&icon);
        b.append(&name);
        b.append(&state);
        b.append(&caption);
        row.set_child(Some(&b));
        if item.kind == Kind::Head {
            row.add_css_class("head-row");
            if item.section == Section::Lost {
                row.add_css_class("warn");
            }
            shown.set_visible(false);
            icon.add_css_class("chevron");
            name.add_css_class("title");
            name.set_hexpand(false);
            state.add_css_class("count");
            state.set_valign(gtk::Align::Center);
            caption.set_text(item.section.caption());
            caption.set_hexpand(true);
            caption.set_ellipsize(pango::EllipsizeMode::End);
            caption.set_visible(true);
        }
        if item.kind == Kind::Note {
            row.add_css_class("note");
            row.set_selectable(false);
            row.set_activatable(false);
            row.set_focusable(false);
            shown.set_visible(false);
            icon.set_visible(false);
        }
        Row { row, shown, icon, name, state, caption }
    }

    fn update_row(&self, r: &Row, item: &Item, shown_id: Option<&str>) {
        let (icon, itone, name, state, stone, emph) = self.row_look(item);
        r.icon.set_text(&icon);
        set_tone(&r.icon, itone);
        r.name.set_text(&name);
        r.name.set_tooltip_text(Some(&name));
        r.state.set_text(&state);
        r.state.set_visible(!state.is_empty());
        set_tone(&r.state, stone);
        if item.kind == Kind::Head {
            r.name.set_tooltip_text(Some(if truthy(pget(&item.obj, "folded")) { "Click to show this section" } else { "Click to fold this section" }));
            return;
        }
        // "needs you" is the one filled pill; "your turn" is outlined
        let (needs, turn) = (state == "needs you", state == "your turn");
        for (class, on) in [("pill", needs || turn), ("pill-needs", needs), ("pill-turn", turn)] {
            if on { r.state.add_css_class(class) } else { r.state.remove_css_class(class) }
        }
        let shown = item.kind == Kind::Session && shown_id.is_some_and(|id| py_str(pget(&item.obj, "id")) == id);
        r.shown.set_text(if shown { "▶" } else { "" });
        for (class, on) in [("dimmed", emph == "dim"), ("urgent", emph == "bold")] {
            if on { r.row.add_css_class(class) } else { r.row.remove_css_class(class) }
        }
    }

    fn selected_key(&self) -> Option<String> {
        self.list.selected_row().map(|r| r.widget_name().to_string())
    }

    fn item(&self, key: &str) -> Option<Item> {
        self.items.borrow().iter().find(|i| i.key == key).cloned()
    }

    fn current(&self) -> Option<Item> {
        self.selected_key().and_then(|k| self.item(&k))
    }

    /// Rebuild the list from the latest data, keeping each row's widget (and so the
    /// selection) and holding the order while you're in the list.
    fn sync(self: &Rc<Self>) {
        let items = self.build_items();
        let selected = self.selected_key();
        let sel_index = self.list.selected_row().map(|r| r.index()).unwrap_or(0);
        let frozen = self.focus_in.get() || self.hover.get();
        // the rows as they'd look: when that's unchanged, only the small things need updating
        let shown_id = self.shown_session();
        let mut print = format!("{frozen}|{shown_id:?}|{}|{}\n", self.snap.borrow().handoffs.len(), self.pending_key.borrow().is_some());
        for it in &items {
            print.push_str(&format!("{}|{:?}|{}|{:?}\n", it.key, it.section, it.sub, self.row_look(it)));
        }
        if *self.shown_print.borrow() == print {
            *self.items.borrow_mut() = items; // fresh data behind the same rows, for actions
            self.show_attention();
            self.show_usage();
            self.show_details();
            self.show_message();
            return;
        }
        *self.shown_print.borrow_mut() = print;
        {
            let placed = self.placed.borrow();
            let mut order = self.order.borrow_mut();
            order.clear();
            for (i, it) in items.iter().enumerate() {
                let rank = if frozen && !placed.is_empty() { placed.get(&it.key).map(|p| p.0).unwrap_or(1_000_000 + i) } else { i };
                order.insert(it.key.clone(), (it.section.order(), it.sub, rank));
            }
        }
        *self.items.borrow_mut() = items.clone();
        self.syncing.set(true);
        let shown_id = self.shown_session();
        let keys: HashSet<&str> = items.iter().map(|i| i.key.as_str()).collect();
        let gone: Vec<String> = self.rows.borrow().keys().filter(|k| !keys.contains(k.as_str())).cloned().collect();
        for k in gone {
            if let Some(r) = self.rows.borrow_mut().remove(&k) {
                self.list.remove(&r.row);
            }
            self.placed.borrow_mut().remove(&k);
        }
        for it in &items {
            let exists = self.rows.borrow().contains_key(&it.key);
            if !exists {
                let r = self.new_row(it);
                self.update_row(&r, it, shown_id.as_deref());
                self.list.append(&r.row);
                self.rows.borrow_mut().insert(it.key.clone(), r);
            } else {
                let rows = self.rows.borrow();
                self.update_row(&rows[&it.key], it, shown_id.as_deref());
            }
        }
        self.list.invalidate_sort();
        // when each row last moved, for the "list just changed" guard
        let t = now();
        let mut i = 0;
        while let Some(row) = self.list.row_at_index(i) {
            let key = row.widget_name().to_string();
            let mut placed = self.placed.borrow_mut();
            match placed.get(&key) {
                Some(&(at, _)) if at == i as usize => {}
                _ => {
                    placed.insert(key, (i as usize, t));
                }
            }
            i += 1;
        }
        // the selection follows its item; if that's gone, it stays at the same place
        let pending = self.pending_key.borrow_mut().take();
        // a heading the app fell back to (nothing else was there yet) gives way to a row
        let selected = selected.filter(|k| !(self.auto_head.get() && k.starts_with("Head:")));
        let want = pending.or(selected).filter(|k| self.rows.borrow().contains_key(k));
        match want {
            Some(k) => {
                let row = self.rows.borrow()[&k].row.clone();
                if self.list.selected_row().as_ref() != Some(&row) {
                    self.list.select_row(Some(&row));
                }
            }
            None => self.select_near(sel_index),
        }
        self.syncing.set(false);
        self.show_attention();
        self.show_usage();
        self.show_details();
        self.show_message();
    }

    /// Select the selectable row at or nearest after `index` (or before, at the end).
    fn select_near(&self, index: i32) {
        let mut rows = vec![];
        let mut i = 0;
        while let Some(r) = self.list.row_at_index(i) {
            rows.push(r);
            i += 1;
        }
        // a session or handoff before a heading: the details then say something useful
        let item_row = |r: &&gtk::ListBoxRow| r.is_selectable() && !r.widget_name().starts_with("Head:");
        let pick = rows
            .iter()
            .skip(index.max(0) as usize)
            .find(item_row)
            .or_else(|| rows.iter().rev().find(item_row))
            .or_else(|| rows.iter().find(|r| r.is_selectable()));
        self.auto_head.set(pick.is_some_and(|r| r.widget_name().starts_with("Head:")));
        if let Some(r) = pick {
            self.list.select_row(Some(r));
        }
    }

    fn selectable_rows(&self) -> Vec<gtk::ListBoxRow> {
        let mut out = vec![];
        let mut i = 0;
        while let Some(r) = self.list.row_at_index(i) {
            if r.is_selectable() {
                out.push(r);
            }
            i += 1;
        }
        out
    }

    fn move_selection(&self, to: impl FnOnce(usize, usize) -> usize) {
        let rows = self.selectable_rows();
        if rows.is_empty() {
            return;
        }
        let cur = self.list.selected_row().and_then(|s| rows.iter().position(|r| *r == s)).unwrap_or(0);
        let next = to(cur, rows.len()).min(rows.len() - 1);
        self.list.select_row(Some(&rows[next]));
        rows[next].grab_focus();
    }

    /// The keyboard to the sidebar's page: the list, or the main widget of the page showing.
    fn focus_list(&self) {
        match self.stack.visible_child_name().as_deref() {
            Some("files") => {
                self.fp_view.grab_focus();
                return;
            }
            Some("help") => {
                self.help.grab_focus();
                return;
            }
            Some("new") => {
                if WidgetExt::is_visible(&self.ns_entry) {
                    self.ns_entry.grab_focus();
                } else if let Some(r) = self.ns_folders.selected_row() {
                    r.grab_focus();
                } else {
                    self.ns_folders.grab_focus();
                }
                return;
            }
            _ => {}
        }
        match self.list.selected_row() {
            Some(r) => {
                r.grab_focus();
            }
            None => {
                self.list.grab_focus();
            }
        }
    }

    fn show_attention(&self) {
        let (_, mine, standby, _, _, _) = self.snap_lists();
        let mut states: Vec<&str> = mine.iter().map(session_state).collect();
        states.extend(standby.iter().map(|r| if pget(r, "jobState") == "blocked" { "needs you" } else { "your turn" }));
        let needs = states.iter().filter(|s| **s == "needs you").count();
        let turn = states.iter().filter(|s| **s == "your turn").count();
        self.needs_btn.set_label(&format!("{needs} need{} you", if needs == 1 { "s" } else { "" }));
        self.needs_btn.set_visible(needs > 0);
        self.turn_btn.set_label(&format!("{turn} your turn"));
        self.turn_btn.set_visible(turn > 0);
    }

    /// The header count buttons: select the next running session in `state` (after the
    /// selected one, round to the first), opening the Sessions section if it's folded.
    fn select_next_in_state(self: &Rc<Self>, state: &str) {
        self.cancel_confirm();
        if self.stack.visible_child_name().as_deref() != Some("list") {
            self.back_to_list();
        }
        if self.collapsed.borrow().contains(&Section::Sessions.key()) {
            self.toggle_collapsed(&Section::Sessions.key());
            self.sync();
        }
        let in_state: Vec<gtk::ListBoxRow> = self
            .selectable_rows()
            .into_iter()
            .filter(|r| {
                self.item(&r.widget_name())
                    .is_some_and(|i| i.section == Section::Sessions && matches!(i.kind, Kind::Session | Kind::Lost) && self.row_look(&i).3 == state)
            })
            .collect();
        if in_state.is_empty() {
            return;
        }
        let cur = self.list.selected_row().and_then(|s| in_state.iter().position(|r| *r == s));
        let next = &in_state[cur.map(|c| (c + 1) % in_state.len()).unwrap_or(0)];
        self.list.select_row(Some(next));
        next.grab_focus();
    }

    /// Folds or opens a section by hand (remembered).
    fn toggle_section(self: &Rc<Self>, sec: Section) {
        self.toggle_collapsed(&sec.key());
        self.auto_head.set(false);
        *self.pending_key.borrow_mut() = Some(format!("Head:{sec:?}"));
        self.sync();
        self.focus_list();
    }

    /// ← and → on the list: fold (`true`) or open a section, and select its heading.
    fn fold_section(self: &Rc<Self>, sec: Section, fold: bool) {
        if self.collapsed.borrow().contains(&sec.key()) != fold {
            self.toggle_collapsed(&sec.key());
        }
        self.auto_head.set(false);
        *self.pending_key.borrow_mut() = Some(format!("Head:{sec:?}"));
        self.sync();
        self.focus_list();
    }

    /// Folds or opens one section or Files group, and remembers it in app.json. This is a
    /// user action, so it writes; nothing else in the app's refreshes does.
    fn toggle_collapsed(&self, key: &str) -> bool {
        let folded = {
            let mut c = self.collapsed.borrow_mut();
            if c.remove(key) {
                false
            } else {
                c.insert(key.to_owned());
                true
            }
        };
        if std::env::var_os("PLANE_SHOT").is_none() {
            // (off-screen pictures don't change your saved choices)
            let path = app_config_path();
            let mut v = plane_core::read_json(&path).filter(Value::is_object).unwrap_or_else(|| serde_json::json!({}));
            let mut list: Vec<String> = self.collapsed.borrow().iter().cloned().collect();
            list.sort();
            v["collapsed"] = serde_json::json!(list);
            if let Err(e) = plane_core::write_json_atomic(&path, &v) {
                self.say(&format!("couldn't remember that: {e}"), "warn");
            }
        }
        folded
    }

    fn show_usage(&self) {
        let usage = self.snap.borrow().usage.clone();
        for ((bar, pct, reset), key) in self.usage_rows.iter().zip(["five_hour", "seven_day"]) {
            let w = pget(&usage, key);
            if !w.is_object() {
                bar.set_visible(false);
                pct.set_text("no data yet");
                set_tone(pct, "dim");
                reset.set_text("");
                continue;
            }
            let p = num(pget(w, "pct"));
            let tone = if p < 50.0 { "ok" } else if p < 80.0 { "warn" } else { "bad" };
            bar.set_visible(true);
            bar.set_fraction((p / 100.0).clamp(0.0, 1.0));
            set_tone(bar, tone);
            pct.set_text(&format!("{p:.0}%"));
            set_tone(pct, tone);
            let r = fmt_reset(pget(w, "resetsAt"));
            reset.set_text(&if r.is_empty() { String::new() } else { format!("resets {r}") });
        }
        let updated = num(pget(&usage, "updatedAt"));
        let stale = updated > 0.0 && now() - updated > 900.0;
        self.usage_note.set_text(&if stale { format!("usage as of {}", ago(updated)) } else { String::new() });
        self.usage_note.set_visible(stale);
    }

    fn detail_lines(&self, item: &Item) -> Vec<(String, &'static str, i32)> {
        let o = &item.obj;
        let mut out: Vec<(String, &'static str, i32)> = vec![];
        let (_, _, name, _, _, _) = self.row_look(item);
        if item.kind != Kind::Note {
            out.push((name, "name", 2));
        }
        let home = self.home.as_str();
        match item.kind {
            Kind::Head => {
                let n = py_str(pget(o, "count"));
                if truthy(pget(o, "folded")) {
                    out.push((format!("Folded: {n} hidden. Click the heading, or press →, to show them."), "", 3));
                } else {
                    out.push((format!("{n} {}. Click the heading, or press ←, to fold it.", item.section.caption()), "", 3));
                }
            }
            Kind::Session => {
                out.push((tilde(sval(o, "cwd"), home), "", 2));
                let mut bits = vec![];
                if !sval(o, "branch").is_empty() {
                    bits.push(format!("branch {}", sval(o, "branch")));
                }
                if truthy(pget(o, "startedAt")) {
                    bits.push(format!("started {}", ago(num(pget(o, "startedAt")) / 1000.0)));
                }
                if !bits.is_empty() {
                    out.push((bits.join(" · "), "", 2));
                }
                if truthy(pget(o, "needs")) && session_state(o) == "needs you" {
                    out.push((format!("needs you: {}", short_needs(&py_str(pget(o, "needs")), 300)), "warn", 4));
                }
                if !truthy(pget(o, "bg")) {
                    out.push(("in its own window · /exit there to close".into(), "", 2));
                }
            }
            Kind::Lost => {
                let where_ = py_str(plane_core::or(&[pget(o, "lastCwd"), pget(o, "cwd"), &Value::String(String::new())]));
                out.push((tilde(&where_, home), "", 2));
                let branch = plane_core::git_branch(&where_);
                if !branch.is_empty() {
                    out.push((format!("branch {branch}"), "", 1));
                }
                let when = num(plane_core::or(&[pget(o, "lastActive"), pget(o, "seenAt"), &Value::from(0)]));
                if pget(o, "cause") == "standby" {
                    out.push((
                        if when > 0.0 { format!("on standby since {} · opens where it left off", ago(when)) } else { "on standby · opens where it left off".into() },
                        "",
                        2,
                    ));
                    if truthy(pget(o, "needs")) && pget(o, "jobState") == "blocked" {
                        out.push((format!("needs you: {}", short_needs(&py_str(pget(o, "needs")), 300)), "warn", 4));
                    }
                } else {
                    let cause = if pget(o, "cause") == "reboot" { "a reboot" } else { "a crash" };
                    out.push((format!("cut off by {cause}{}", if when > 0.0 { format!(" {}", ago(when)) } else { String::new() }), "warn", 2));
                }
                if truthy(pget(o, "lastPrompt")) {
                    out.push((format!("last ask: {}", py_str(pget(o, "lastPrompt"))), "", 2));
                }
            }
            Kind::More => {
                let hidden = num(pget(o, "hidden")) > 0.0;
                out.push((if hidden { "open it to list every handoff" } else { "open it to show only the newest" }.into(), "", 2));
            }
            Kind::Handoff => {
                let src = pget(o, "source");
                let state = if src.is_null() { "has ended" } else if pget(src, "status") == "busy" { "is still writing it" } else { "is done" };
                let from = if truthy(pget(o, "sourceName")) { py_str(pget(o, "sourceName")) } else { "?".into() };
                out.push((format!("from {from}, which {state}"), "", 2));
                let created = if truthy(pget(o, "createdAt")) { num(pget(o, "createdAt")) } else { now() };
                out.push((format!("{} · queued {}", tilde(sval(o, "cwd"), home), ago(created)), "", 2));
                let prompt = sval(o, "prompt").split_whitespace().collect::<Vec<_>>().join(" ");
                if !prompt.is_empty() {
                    out.push((prompt, "", 5));
                }
            }
            Kind::Note => {}
        }
        out
    }

    fn show_details(&self) {
        let item = self.current();
        let lines = item.as_ref().map(|i| self.detail_lines(i)).unwrap_or_default();
        let print = format!("{:?}|{lines:?}", item.as_ref().map(|i| &i.key));
        if *self.shown_details.borrow() == print {
            return;
        }
        *self.shown_details.borrow_mut() = print;
        while let Some(c) = self.details.first_child() {
            self.details.remove(&c);
        }
        for (text, style, max) in lines {
            let l = label(&text, &[if style.is_empty() { "dim" } else { style }]);
            l.set_wrap(true);
            l.set_wrap_mode(pango::WrapMode::WordChar);
            l.set_lines(max);
            l.set_ellipsize(pango::EllipsizeMode::End);
            self.details.append(&l);
        }
        self.show_hints(item.as_ref());
    }

    fn show_hints(&self, item: Option<&Item>) {
        let hint = match item.map(|i| (i.kind, &i.obj)) {
            Some((Kind::Head, _)) => "←→ fold  n new  ? keys",
            Some((Kind::Handoff, _)) => "v read  x discard  n new  ? keys",
            Some((Kind::Lost, _)) => "x close  n new  ? keys",
            Some((Kind::Session, o)) if truthy(pget(o, "bg")) && truthy(pget(o, "stopped")) => "x close  f files  n new  ? keys",
            Some((Kind::Session, o)) if truthy(pget(o, "bg")) => "x close  p pause  f files  ? keys",
            Some((Kind::Session, _)) => "f files  n new  ? keys",
            _ => "n new session  ? keys",
        };
        let markup: Vec<String> = hint
            .split("  ")
            .map(|pair| {
                let (k, w) = pair.split_once(' ').unwrap_or((pair, ""));
                format!("<b>{}</b> {}", esc(k), esc(w))
            })
            .collect();
        self.hints.set_markup(&markup.join("   "));
    }

    fn say(&self, text: &str, tone: &'static str) {
        *self.flash.borrow_mut() = Some((text.to_owned(), tone, now()));
        self.show_message();
    }

    fn show_message(&self) {
        let flash = self.flash.borrow().clone();
        let err = self.snap.borrow().error.clone();
        match flash {
            Some((text, tone, _)) => {
                self.message.set_text(&text);
                set_tone(&self.message, if tone == "info" { "accent" } else { tone });
                self.message.set_visible(true);
            }
            None if !err.is_empty() => {
                self.message.set_text(&err);
                set_tone(&self.message, "bad");
                self.message.set_visible(true);
            }
            None => {
                self.message.set_text("");
                self.message.set_visible(true);
            }
        }
    }

    // ------------------------------------------------------------------ input

    fn click(self: &Rc<Self>, n: i32, y: f64) {
        if self.confirm.borrow().is_some() {
            self.cancel_confirm(); // a click backs out of a question
            return;
        }
        let Some(row) = self.list.row_at_y(y as i32) else { return };
        let key = row.widget_name().to_string();
        let Some(item) = self.item(&key) else { return };
        if item.kind == Kind::Note {
            return;
        }
        let t = now();
        let moved = self.placed.borrow().get(&key).map(|p| p.1).unwrap_or(0.0);
        if t - moved < MOVED_GUARD && n == 1 {
            // this row just moved here: you were aiming at what used to be in its place
            self.say("the list just changed: click again", "warn");
            return;
        }
        if n == 1 {
            *self.press.borrow_mut() = (key.clone(), t);
            flash_pressed(&row);
            if item.kind == Kind::Head {
                self.toggle_section(item.section);
                return;
            }
            self.list.select_row(Some(&row));
            if item.kind == Kind::Session && truthy(pget(&item.obj, "bg")) {
                // a click on a session points Files at it, even over a folder picked by hand
                self.follow(Follow::Session(py_str(pget(&item.obj, "id"))), true);
            }
            if item.kind == Kind::Handoff {
                row.grab_focus();
                self.say("double-click or press Enter to start", "info");
            } else {
                if item.kind == Kind::More {
                    row.grab_focus();
                }
                self.activate(&item);
            }
        } else if n == 2 && item.kind == Kind::Handoff && self.press.borrow().0 == key {
            // both clicks on the same handoff, wherever the list put it in between
            *self.press.borrow_mut() = (String::new(), 0.0);
            self.activate(&item);
        }
    }

    fn on_key(self: &Rc<Self>, key: gdk::Key, mods: gdk::ModifierType) -> glib::Propagation {
        use gdk::Key as K;
        let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
        let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
        let other = mods.intersects(gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK);
        let modifier_only = matches!(key, K::Shift_L | K::Shift_R | K::Control_L | K::Control_R | K::Alt_L | K::Alt_R | K::Super_L | K::Super_R | K::Caps_Lock);
        if modifier_only {
            return glib::Propagation::Proceed;
        }
        if self.confirm.borrow().is_some() {
            if !ctrl && !other && matches!(key.to_lower(), K::y) {
                self.run_confirm();
            } else {
                self.cancel_confirm();
                self.say("cancelled", "info");
            }
            return glib::Propagation::Stop;
        }
        let page = self.stack.visible_child_name().map(|s| s.to_string()).unwrap_or_default();
        if page == "help" {
            self.back_to_list();
            return glib::Propagation::Stop;
        }
        if page == "files" {
            return self.files_key(key, mods);
        }
        if page == "new" {
            if key == K::Escape {
                self.back_to_list();
                return glib::Propagation::Stop;
            }
            return glib::Propagation::Proceed;
        }
        if ctrl && !shift && !other && key.to_lower() == K::p {
            self.find_a_file();
            return glib::Propagation::Stop;
        }
        // a paste in the sidebar was meant for the session
        if (ctrl && key.to_lower() == K::v) || (shift && key == K::Insert) {
            self.forward_paste();
            return glib::Propagation::Stop;
        }
        if ctrl || other {
            return glib::Propagation::Proceed;
        }
        let item = self.current();
        let kind = item.as_ref().map(|i| i.kind);
        match key {
            K::Up | K::k | K::KP_Up => self.move_selection(|c, _| c.saturating_sub(1)),
            K::Down | K::j | K::KP_Down => self.move_selection(|c, _| c + 1),
            K::Home | K::g => self.move_selection(|_, _| 0),
            K::End | K::G => self.move_selection(|_, n| n - 1),
            K::Right | K::l if kind == Some(Kind::Head) => {
                if let Some(i) = item {
                    self.fold_section(i.section, false);
                }
            }
            K::Left | K::h => {
                if let Some(i) = item {
                    self.fold_section(i.section, true);
                }
            }
            K::space if kind == Some(Kind::Head) => {
                if let Some(i) = item {
                    self.toggle_section(i.section);
                }
            }
            K::Return | K::KP_Enter | K::Right | K::l => {
                if let Some(item) = item {
                    self.activate(&item);
                }
            }
            K::slash => self.find_a_file(),
            K::n => self.new_session(),
            K::x | K::X | K::d => match (kind, item) {
                (Some(Kind::Session), Some(i)) => self.close(&i.obj),
                (Some(Kind::Lost), Some(i)) => self.close_lost(&i.obj, false),
                (Some(Kind::Handoff), Some(i)) => self.discard(&i.obj),
                _ => {}
            },
            K::p => {
                if let (Some(Kind::Session), Some(i)) = (kind, item) {
                    self.pause(&i.obj);
                }
            }
            K::f => match (kind, item) {
                (Some(Kind::Session), Some(i)) => self.files_for_session(&i.obj),
                _ => self.open_files(true),
            },
            K::u => self.undo_discard(),
            K::v => {
                if let (Some(Kind::Handoff), Some(i)) = (kind, item) {
                    self.view_handoff(&i.obj);
                }
            }
            K::r => self.store.poke(),
            K::question => self.show_help(),
            K::q => self.window.close(),
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    }

    fn forward_paste(self: &Rc<Self>) {
        let term = self.current_tab().filter(|t| matches!(*t.kind.borrow(), TabKind::Session(_) | TabKind::Starting(_))).and_then(|t| t.term.clone());
        match term {
            Some(t) => {
                t.paste_clipboard();
                t.grab_focus();
            }
            None => self.say("open a session first; pastes go into the session", "warn"),
        }
    }

    fn activate(self: &Rc<Self>, item: &Item) {
        match item.kind {
            Kind::Head => self.toggle_section(item.section),
            Kind::Session => self.attach(&item.obj),
            Kind::Handoff => self.launch_handoff(&item.obj),
            Kind::Lost => self.resume_lost(&item.obj),
            Kind::More => self.toggle_handoffs(),
            Kind::Note => {}
        }
    }

    fn toggle_handoffs(self: &Rc<Self>) {
        let open = !self.handoffs_open.get();
        self.handoffs_open.set(open);
        if open {
            // select the first handoff that was hidden
            let (_, _, _, _, _, handoffs) = self.snap_lists();
            if let Some(h) = handoffs.get(HANDOFFS_SHOWN) {
                *self.pending_key.borrow_mut() = Some(format!("Handoff:{}", py_str(pget(h, "id"))));
            }
        }
        self.sync();
        self.focus_list();
    }

    // ------------------------------------------------------------------ confirm

    fn ask(self: &Rc<Self>, question: &str, yes: &str, then: impl FnOnce(&Rc<App>) + 'static) {
        self.confirm_q.set_text(question);
        self.confirm_yes.set_label(yes);
        *self.confirm.borrow_mut() = Some(Box::new(then));
        *self.flash.borrow_mut() = None;
        self.message.set_visible(false);
        self.confirm_bar.set_visible(true);
        self.focus_list();
    }

    fn run_confirm(self: &Rc<Self>) {
        let f = self.confirm.borrow_mut().take();
        self.confirm_bar.set_visible(false);
        self.show_message();
        if let Some(f) = f {
            f(self);
        }
    }

    fn cancel_confirm(&self) {
        if self.confirm.borrow_mut().take().is_some() {
            self.confirm_bar.set_visible(false);
            self.show_message();
        }
    }

    // ------------------------------------------------------------------ menus

    fn context_menu(self: &Rc<Self>, x: f64, y: f64) {
        self.cancel_confirm();
        let item = self.list.row_at_y(y as i32).and_then(|row| {
            let it = self.item(&row.widget_name());
            if it.as_ref().is_some_and(|i| i.kind != Kind::Note) {
                self.list.select_row(Some(&row));
            }
            it
        });
        let item = item.filter(|i| i.kind != Kind::Note);
        let menu = gio::Menu::new();
        let acts = gio::Menu::new();
        if let Some(i) = &item {
            let o = &i.obj;
            match i.kind {
                Kind::Session if !truthy(pget(o, "bg")) => {
                    acts.append(Some("Go to its window"), Some("row.open"));
                    acts.append(Some("Files"), Some("row.files"));
                }
                Kind::Session => {
                    acts.append(Some(if truthy(pget(o, "stopped")) { "Continue" } else { "Open" }), Some("row.open"));
                    acts.append(Some("Files"), Some("row.files"));
                    if !truthy(pget(o, "stopped")) {
                        acts.append(Some("Pause"), Some("row.pause"));
                    }
                    acts.append(Some("Close"), Some("row.close"));
                    if actions::handoff_of(&self.env, &py_str(pget(o, "id"))).is_some() {
                        acts.append(Some("Put back in queue"), Some("row.requeue"));
                    }
                }
                Kind::Lost => {
                    let standby = pget(o, "cause") == "standby";
                    acts.append(Some(if standby { "Continue" } else { "Resume" }), Some("row.open"));
                    acts.append(Some("Close"), Some("row.close"));
                    acts.append(Some(if standby { "Close all standby sessions" } else { "Close all cut-off sessions" }), Some("row.close-all"));
                }
                Kind::Handoff => {
                    acts.append(Some("Start"), Some("row.open"));
                    acts.append(Some("Read prompt"), Some("row.read"));
                    acts.append(Some("Discard"), Some("row.close"));
                }
                Kind::More => acts.append(Some(if num(pget(o, "hidden")) > 0.0 { "Show all handoffs" } else { "Show fewer" }), Some("row.open")),
                Kind::Head => acts.append(Some(if truthy(pget(o, "folded")) { "Show this section" } else { "Fold this section" }), Some("row.open")),
                Kind::Note => {}
            }
            menu.append_section(None, &acts);
        }
        let general = gio::Menu::new();
        general.append(Some("New session…"), Some("row.new"));
        menu.append_section(None, &general);
        *self.menu_item.borrow_mut() = item;
        // parented to the sidebar box, not the list: a list can only remove rows, and
        // tearing it down with a menu inside never finishes
        let at = self.list.compute_point(&self.sidebar, &gtk::graphene::Point::new(x as f32, y as f32)).unwrap_or(gtk::graphene::Point::new(x as f32, y as f32));
        let pop = gtk::PopoverMenu::from_model(Some(&menu));
        pop.set_parent(&self.sidebar);
        pop.set_has_arrow(false);
        pop.set_halign(gtk::Align::Start);
        pop.set_pointing_to(Some(&gdk::Rectangle::new(at.x() as i32, at.y() as i32, 1, 1)));
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent()); // after its action has run
        });
        pop.popup();
    }

    fn install_actions(self: &Rc<Self>) {
        let group = gio::SimpleActionGroup::new();
        let add = |name: &str, f: fn(&Rc<App>, &Item)| {
            let a = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                let Some(app) = weak.upgrade() else { return };
                let item = app.menu_item.borrow().clone();
                if let Some(item) = item {
                    f(&app, &item);
                }
            });
            group.add_action(&a);
        };
        add("open", |app, i| app.activate(i));
        add("pause", |app, i| app.pause(&i.obj));
        add("close", |app, i| match i.kind {
            Kind::Session => app.close(&i.obj),
            Kind::Lost => app.close_lost(&i.obj, false),
            Kind::Handoff => app.discard(&i.obj),
            _ => {}
        });
        add("close-all", |app, i| app.close_lost(&i.obj, true));
        add("requeue", |app, i| app.requeue(&i.obj));
        add("read", |app, i| app.view_handoff(&i.obj));
        add("files", |app, i| app.files_for_session(&i.obj));
        let a = gio::SimpleAction::new("new", None);
        let weak = Rc::downgrade(self);
        a.connect_activate(move |_, _| {
            if let Some(app) = weak.upgrade() {
                app.new_session();
            }
        });
        group.add_action(&a);
        self.sidebar.insert_action_group("row", Some(&group));
    }

    // ------------------------------------------------------------------ actions

    /// Runs a blocking action (a `claude` call) off the main thread, then `done` on it.
    fn background<T: Send + 'static>(self: &Rc<Self>, work: impl FnOnce(&Env) -> T + Send + 'static, done: impl FnOnce(&Rc<App>, T) + 'static) {
        let env = self.env.clone();
        let handle = gio::spawn_blocking(move || work(&env));
        let app = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(v) = handle.await {
                done(&app, v);
            }
            app.store.poke();
        });
    }

    fn attach(self: &Rc<Self>, s: &Value) {
        if !truthy(pget(s, "bg")) {
            let pid = pget(s, "pid").as_i64().unwrap_or(0);
            if !hypr::available() {
                self.say("it's in another window (switching to it needs Hyprland)", "info");
            } else if !hypr::focus_process_window(pid) {
                self.say("couldn't find that session's window", "bad");
            }
            return;
        }
        let id = py_str(pget(s, "id"));
        let name = py_str(plane_core::or(&[pget(s, "name"), pget(s, "id")]));
        let cwd = if sval(s, "cwd").is_empty() { self.home.clone() } else { sval(s, "cwd").to_owned() };
        self.open_session(&id, &name, &cwd);
    }

    fn open_session(self: &Rc<Self>, id: &str, name: &str, cwd: &str) {
        let existing = self.tabs.borrow().iter().find(|t| *t.kind.borrow() == TabKind::Session(id.to_owned())).cloned();
        if let Some(tab) = existing {
            self.show_tab(&tab);
            return;
        }
        if kitty_plane_shows().as_deref() == Some(id) {
            self.say("the kitty Plane window shows this session too: two views at different sizes garble each other", "warn");
        }
        self.make_room();
        let tab = self.add_term_tab(TabKind::Session(id.to_owned()), name, cwd);
        if let Some(t) = &tab.term {
            let argv = vec![actions::claude_bin(&self.env).to_string_lossy().into_owned(), "attach".into(), id.to_owned()];
            let weak = Rc::downgrade(self);
            term::spawn(t, &argv, cwd, move |r| {
                if let (Err(e), Some(app)) = (r, weak.upgrade()) {
                    app.say(&format!("couldn't open it: {e}"), "bad");
                }
            });
        }
        self.store.poke();
    }

    /// Keep at most MAX_ATTACHED sessions attached: take away the one looked at longest ago.
    fn make_room(self: &Rc<Self>) {
        let current = self.current_tab();
        let attached: Vec<Rc<Tab>> = self.tabs.borrow().iter().filter(|t| matches!(*t.kind.borrow(), TabKind::Session(_))).cloned().collect();
        if attached.len() < MAX_ATTACHED {
            return;
        }
        let oldest = attached.into_iter().filter(|t| current.as_ref().is_none_or(|c| !Rc::ptr_eq(c, t))).min_by(|a, b| a.shown_at.get().total_cmp(&b.shown_at.get()));
        if let Some(t) = oldest {
            self.close_tab(&t);
        }
    }

    fn add_term_tab(self: &Rc<Self>, kind: TabKind, title: &str, cwd: &str) -> Rc<Tab> {
        let weak = Rc::downgrade(self);
        let slot: Rc<RefCell<std::rc::Weak<Tab>>> = Rc::default();
        let s = slot.clone();
        let open: Rc<dyn Fn(String)> = Rc::new(move |text| {
            if let (Some(app), Some(tab)) = (weak.upgrade(), s.borrow().upgrade()) {
                app.open_link(&tab, &text);
            }
        });
        let wheel = Rc::new(Cell::new(matches!(kind, TabKind::Session(_))));
        let t = term::new_terminal(&self.colors.borrow(), self.scale.clone(), open, wheel.clone());
        let tab = self.add_tab(kind, title, t.clone().upcast(), Some(t.clone()), wheel);
        *slot.borrow_mut() = Rc::downgrade(&tab);
        *tab.origin.borrow_mut() = Some(Origin { root: PathBuf::from(cwd), session: None, name: title.to_owned() });
        let weak = Rc::downgrade(self);
        let w_tab = Rc::downgrade(&tab);
        t.connect_child_exited(move |term, _| {
            let (Some(app), Some(tab)) = (weak.upgrade(), w_tab.upgrade()) else { return };
            if tab.closing.get() {
                return;
            }
            let kind = tab.kind.borrow().clone();
            match kind {
                TabKind::Starting(_) => {} // handled by the start itself
                _ => {
                    term.feed(b"\r\n\x1b[2mThis view ended. The session keeps running unless you closed it; open it again from the list.\x1b[0m\r\n");
                    app.store.poke();
                }
            }
        });
        tab
    }

    fn add_tab(self: &Rc<Self>, kind: TabKind, title: &str, page: gtk::Widget, term: Option<vte::Terminal>, wheel_to_app: Rc<Cell<bool>>) -> Rc<Tab> {
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        let title_label = gtk::Label::new(Some(title));
        title_label.set_ellipsize(pango::EllipsizeMode::End);
        title_label.set_max_width_chars(24);
        title_label.set_width_chars(title.chars().count().min(24) as i32);
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat");
        close.set_focusable(false);
        close.set_tooltip_text(Some(match kind {
            TabKind::Prompt(_) => "Hide this tab",
            TabKind::File(_) => "Close this file (Ctrl+W)",
            _ => "Hide this view · the session keeps running",
        }));
        if let TabKind::File(p) = &kind {
            head.set_tooltip_text(Some(&tilde(&p.to_string_lossy(), &self.home)));
        }
        head.append(&title_label);
        head.append(&close);
        clickable(&head);
        clickable(&close);
        let tab = Rc::new(Tab {
            kind: RefCell::new(kind),
            page: page.clone(),
            title: title_label,
            name: title.to_owned(),
            editor: RefCell::new(None),
            origin: RefCell::new(None),
            term,
            shown_at: Cell::new(now()),
            closing: Cell::new(false),
            wheel_to_app,
        });
        let weak = Rc::downgrade(self);
        let w_tab = Rc::downgrade(&tab);
        close.connect_clicked(move |_| {
            if let (Some(app), Some(tab)) = (weak.upgrade(), w_tab.upgrade()) {
                app.close_tab(&tab);
            }
        });
        // middle-click on a tab hides it too, as in browsers
        let middle = gtk::GestureClick::new();
        middle.set_button(2);
        let weak = Rc::downgrade(self);
        let w_tab = Rc::downgrade(&tab);
        middle.connect_pressed(move |_, _, _, _| {
            if let (Some(app), Some(tab)) = (weak.upgrade(), w_tab.upgrade()) {
                app.close_tab(&tab);
            }
        });
        head.add_controller(middle);
        self.tabs.borrow_mut().push(tab.clone());
        let n = self.notebook.append_page(&page, Some(&head));
        self.notebook.set_tab_reorderable(&page, true);
        self.right.set_visible_child_name("tabs");
        self.notebook.set_current_page(Some(n));
        self.focus_tab(&tab);
        self.mark_shown();
        tab
    }

    fn show_tab(self: &Rc<Self>, tab: &Rc<Tab>) {
        if let Some(n) = self.notebook.page_num(&tab.page) {
            self.notebook.set_current_page(Some(n));
        }
        self.focus_tab(tab);
    }

    fn focus_tab(&self, tab: &Tab) {
        tab.shown_at.set(now());
        let page = tab.page.clone();
        let ed = tab.editor.borrow().clone();
        // after GTK has shown the page, or the focus lands nowhere
        glib::idle_add_local_once(move || match ed {
            Some(ed) => ed.focus(),
            None => {
                page.grab_focus();
            }
        });
    }

    /// Takes a tab away; a file with unsaved changes asks first.
    fn close_tab(self: &Rc<Self>, tab: &Rc<Tab>) {
        let ed = tab.editor.borrow().clone();
        match ed {
            Some(ed) if ed.is_dirty() => {
                self.show_tab(tab);
                let dialog = gtk::AlertDialog::builder()
                    .modal(true)
                    .message(format!("Save your changes to {}?", ed.file_name()))
                    .detail("If you close it without saving, your changes are lost.")
                    .buttons(["Cancel", "Close without saving", "Save"])
                    .cancel_button(0)
                    .default_button(2)
                    .build();
                let app = self.clone();
                let tab = tab.clone();
                dialog.choose(Some(&self.window), None::<&gio::Cancellable>, move |r| match r {
                    Ok(2) => {
                        if ed.save(false) {
                            app.remove_tab(&tab);
                        }
                    }
                    Ok(1) => app.remove_tab(&tab),
                    _ => ed.focus(),
                });
            }
            _ => self.remove_tab(tab),
        }
    }

    fn remove_tab(self: &Rc<Self>, tab: &Rc<Tab>) {
        tab.closing.set(true);
        // a handoff still starting (at a trust prompt, say) stops with its tab: back in the queue
        if let TabKind::Starting(Some(done)) = tab.kind.borrow().clone() {
            let started = plane_core::read_json(&done).is_some_and(|h| truthy(pget(&h, "launchedSessionId")));
            if !started && done.exists() {
                actions::handoff_not_started(&self.env, &mut Fs::new(Mode::Real), &done);
                self.say("not started · the handoff is back in the queue", "info");
                self.store.poke();
            }
        }
        if let Some(n) = self.notebook.page_num(&tab.page) {
            self.notebook.remove_page(Some(n));
        }
        self.tabs.borrow_mut().retain(|t| !Rc::ptr_eq(t, tab));
        if self.tabs.borrow().is_empty() {
            self.right.set_visible_child_name("empty");
            self.focus_list();
        }
        self.mark_shown();
    }

    fn close_session_tab(self: &Rc<Self>, id: &str) {
        let tab = self.tabs.borrow().iter().find(|t| *t.kind.borrow() == TabKind::Session(id.to_owned())).cloned();
        if let Some(t) = tab {
            self.close_tab(&t);
        }
    }

    fn current_tab(&self) -> Option<Rc<Tab>> {
        let page = self.notebook.current_page().and_then(|n| self.notebook.nth_page(Some(n)))?;
        self.tabs.borrow().iter().find(|t| t.page == page).cloned()
    }

    /// The session showing in the current tab.
    fn shown_session(&self) -> Option<String> {
        match self.current_tab()?.kind.borrow().clone() {
            TabKind::Session(id) => Some(id),
            _ => None,
        }
    }

    /// The ▶ marker on the session that's showing.
    fn mark_shown(&self) {
        let shown = self.shown_session();
        let items = self.items.borrow();
        let rows = self.rows.borrow();
        for it in items.iter() {
            if let Some(r) = rows.get(&it.key) {
                let on = it.kind == Kind::Session && shown.as_deref().is_some_and(|id| py_str(pget(&it.obj, "id")) == id);
                r.shown.set_text(if on { "▶" } else { "" });
            }
        }
    }

    /// Starts `claude --bg` in a new tab, where a folder-trust prompt can be answered, then
    /// attaches there once it says which job it became. A port of `plane start`.
    fn start_bg(self: &Rc<Self>, cwd: &str, title: &str, opts: StartOpts) {
        let claude = actions::claude_bin(&self.env).to_string_lossy().into_owned();
        let mut argv = vec![claude.clone(), "--bg".into()];
        if let Some(r) = &opts.resume {
            argv.extend(["--resume".into(), r.clone()]);
        }
        if !opts.name.is_empty() {
            argv.extend(["--name".into(), opts.name.clone()]);
        }
        if !opts.prompt.is_empty() {
            argv.push(opts.prompt.clone());
        }
        let jobs_before: HashSet<String> = std::fs::read_dir(self.env.jobs())
            .map(|d| d.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        self.make_room();
        let tab = self.add_term_tab(TabKind::Starting(opts.handoff.clone()), title, cwd);
        let Some(t) = tab.term.clone() else { return };
        let weak = Rc::downgrade(self);
        let w_tab = Rc::downgrade(&tab);
        let cwd_s = cwd.to_owned();
        t.connect_child_exited(move |term, _| {
            let (Some(app), Some(tab)) = (weak.upgrade(), w_tab.upgrade()) else { return };
            if !matches!(*tab.kind.borrow(), TabKind::Starting(_)) || tab.closing.get() {
                return;
            }
            let text = term.text_format(vte::Format::Text).map(|s| s.to_string()).unwrap_or_default();
            let id = backgrounded_id(&text).or_else(|| app.find_started_job(&opts, &jobs_before));
            let mut fs = Fs::new(Mode::Real);
            match id {
                Some(id) => {
                    if let Some(done) = &opts.handoff {
                        actions::handoff_started(&app.env, &mut fs, done, &id);
                    }
                    *tab.kind.borrow_mut() = TabKind::Session(id.clone());
                    let argv = vec![claude.clone(), "attach".into(), id];
                    term::spawn(term, &argv, &cwd_s, |_| {});
                    tab.wheel_to_app.set(true);
                    app.mark_shown();
                }
                None => {
                    if let Some(done) = &opts.handoff {
                        actions::handoff_not_started(&app.env, &mut fs, done); // back in the queue
                    }
                    term.feed(b"\r\n\x1b[2mNo session started. Hide this tab when you're done with it.\x1b[0m\r\n");
                    app.say("no session started", "warn");
                }
            }
            app.store.poke();
        });
        let weak = Rc::downgrade(self);
        term::spawn(&t, &argv, cwd, move |r| {
            if let (Err(e), Some(app)) = (r, weak.upgrade()) {
                app.say(&format!("couldn't start claude: {e}"), "bad");
            }
        });
        self.say("starting… answer any prompt on the right", "info");
        self.store.poke();
    }

    /// When `claude --bg` didn't print its job id: a resumed conversation's job, or the one
    /// new job folder that appeared while it ran.
    fn find_started_job(&self, opts: &StartOpts, before: &HashSet<String>) -> Option<String> {
        if let Some(resume) = &opts.resume {
            for f in plane_core::glob_json(&self.env.sessions()) {
                let info = plane_core::read_obj(&f);
                if sval(&info, "sessionId") == resume && truthy(pget(&info, "jobId")) && plane_core::proc_alive(pget(&info, "pid"), &Value::Null) {
                    return Some(py_str(pget(&info, "jobId")));
                }
            }
        }
        let fresh: Vec<String> = std::fs::read_dir(self.env.jobs())
            .map(|d| d.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| !before.contains(n)).collect())
            .unwrap_or_default();
        if fresh.len() == 1 { fresh.into_iter().next() } else { None }
    }

    fn launch_handoff(self: &Rc<Self>, h: &Value) {
        let h = h.clone();
        if !truthy(pget(&h, "ready")) {
            self.ask("Its session is still writing it. Start anyway?", "start", move |app| app.do_launch(&h));
            return;
        }
        self.do_launch(&h);
    }

    fn do_launch(self: &Rc<Self>, h: &Value) {
        let path = PathBuf::from(sval(h, "path"));
        let mut fs = Fs::new(Mode::Real);
        match actions::claim_handoff(&self.env, &mut fs, now(), &path) {
            Err(ClaimError::AlreadyStarted) => {
                self.say("that handoff was already started", "warn");
                self.store.poke();
            }
            Err(ClaimError::FolderGone(cwd)) => self.say(&format!("its folder is gone: {cwd} · the handoff stays queued"), "bad"),
            Ok((done, claimed)) => {
                self.snap.borrow_mut().handoffs.retain(|x| pget(x, "id") != pget(h, "id"));
                self.sync();
                let cwd = actions::handoff_cwd(&self.env, &claimed);
                let title: String = sval(&claimed, "title").chars().take(60).collect();
                let opts = StartOpts { name: title.clone(), prompt: sval(&claimed, "prompt").to_owned(), resume: None, handoff: Some(done) };
                self.start_bg(&cwd, if title.is_empty() { "handoff" } else { &title }, opts);
                let src = pget(h, "source");
                if src.is_object() && !truthy(pget(src, "bg")) {
                    self.say(&format!("'{}' is in its own window: /exit there to close it", py_str(pget(src, "name"))), "info");
                }
            }
        }
    }

    fn resume_lost(self: &Rc<Self>, r: &Value) {
        let name = sval(r, "name").to_owned();
        let cwd = if sval(r, "cwd").is_empty() { self.home.clone() } else { sval(r, "cwd").to_owned() };
        if !Path::new(&cwd).is_dir() {
            self.say(&format!("its folder is gone: {cwd}"), "bad");
            return;
        }
        let job = sval(r, "jobId").to_owned();
        if pget(r, "kind") == "bg" && !job.is_empty() && self.env.jobs().join(&job).is_dir() {
            self.open_session(&job, if name.is_empty() { &job } else { &name }, &cwd);
            return;
        }
        let title: String = if name.is_empty() { lost_label(r).chars().take(40).collect() } else { name.clone() };
        self.start_bg(&cwd, &title, StartOpts { name, resume: Some(py_str(pget(r, "sessionId"))), ..StartOpts::default() });
    }

    fn close_lost(self: &Rc<Self>, r: &Value, everything: bool) {
        let standby = pget(r, "cause") == "standby";
        let group: Vec<Value> = if everything {
            self.snap.borrow().lost.iter().filter(|x| (pget(x, "cause") == "standby") == standby).cloned().collect()
        } else {
            vec![r.clone()]
        };
        let what = if everything {
            format!("all {} {} sessions", group.len(), if standby { "standby" } else { "cut-off" })
        } else {
            format!("'{}'", lost_label(r).chars().take(24).collect::<String>())
        };
        self.ask(&format!("Close {what}?"), "close", move |app| {
            app.say("closing…", "info");
            app.background(
                move |env| {
                    let mut fs = Fs::new(Mode::Real);
                    let e = env.clone();
                    let mut claude = move |a: &[&str], cwd: &Path| actions::run_claude(&e, a, cwd);
                    actions::close_lost(env, &mut fs, &mut claude, &group);
                },
                |app, _| app.say("closed · conversation kept (claude --resume)", "info"),
            );
        });
    }

    fn close(self: &Rc<Self>, s: &Value) {
        if !truthy(pget(s, "bg")) {
            self.say("it's in its own window: type /exit there", "info");
            return;
        }
        let id = py_str(pget(s, "id"));
        let name: String = py_str(plane_core::or(&[pget(s, "name"), pget(s, "id")])).chars().take(26).collect();
        self.ask(&format!("Close '{name}'?"), "close", move |app| {
            app.say("closing…", "info");
            let job = id.clone();
            app.background(
                move |env| {
                    let mut fs = Fs::new(Mode::Real);
                    let e = env.clone();
                    let mut claude = move |a: &[&str], cwd: &Path| actions::run_claude(&e, a, cwd);
                    actions::close_session(env, &mut fs, now(), &mut claude, &job)
                },
                move |app, r| match r {
                    Ok(()) => {
                        app.close_session_tab(&id);
                        app.say("closed · conversation kept (claude --resume)", "info");
                    }
                    Err(e) => app.say(&e, "bad"),
                },
            );
        });
    }

    fn pause(self: &Rc<Self>, s: &Value) {
        if !truthy(pget(s, "bg")) {
            self.say("it's in its own window: type /exit there", "info");
            return;
        }
        if truthy(pget(s, "stopped")) {
            self.say("already paused · x closes it", "info");
            return;
        }
        let id = py_str(pget(s, "id"));
        self.say("pausing…", "info");
        let job = id.clone();
        self.background(
            move |env| {
                let e = env.clone();
                let mut claude = move |a: &[&str], cwd: &Path| actions::run_claude(&e, a, cwd);
                actions::pause_session(env, &mut claude, &job)
            },
            move |app, r| match r {
                Ok(()) => {
                    app.close_session_tab(&id);
                    app.say("paused · open it to continue", "info");
                }
                Err(e) => app.say(&e, "bad"),
            },
        );
    }

    fn requeue(self: &Rc<Self>, s: &Value) {
        let id = py_str(pget(s, "id"));
        if actions::handoff_of(&self.env, &id).is_none() {
            self.say("no handoff to put back", "warn");
            return;
        }
        self.ask("Close it and put the handoff back?", "put back", move |app| {
            app.say("closing…", "info");
            let job = id.clone();
            app.background(
                move |env| {
                    let mut fs = Fs::new(Mode::Real);
                    let e = env.clone();
                    let mut claude = move |a: &[&str], cwd: &Path| actions::run_claude(&e, a, cwd);
                    actions::requeue(env, &mut fs, now(), &mut claude, &job)
                },
                move |app, r| match r {
                    Ok(()) => {
                        app.close_session_tab(&id);
                        app.say("closed · handoff back in the queue", "info");
                    }
                    Err(e) => app.say(&e, "bad"),
                },
            );
        });
    }

    fn discard(self: &Rc<Self>, h: &Value) {
        let path = PathBuf::from(sval(h, "path"));
        let title: String = sval(h, "title").chars().take(26).collect();
        self.ask(&format!("Discard '{title}'?"), "discard", move |app| {
            let mut fs = Fs::new(Mode::Real);
            if let Some(dest) = actions::discard_handoff(&app.env, &mut fs, &path) {
                *app.last_discard.borrow_mut() = Some(dest);
                app.say("discarded · u brings it back", "info");
            }
            app.store.poke();
        });
    }

    fn undo_discard(self: &Rc<Self>) {
        let f = self.last_discard.borrow_mut().take();
        let mut fs = Fs::new(Mode::Real);
        match f {
            Some(f) if actions::undo_discard(&self.env, &mut fs, &f) => {
                *self.pending_key.borrow_mut() = Some(format!("Handoff:{}", f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()));
                self.say("handoff back in the queue", "info");
                self.store.poke();
            }
            _ => self.say("nothing to bring back", "warn"),
        }
    }

    fn view_handoff(self: &Rc<Self>, h: &Value) {
        let id = py_str(pget(h, "id"));
        let existing = self.tabs.borrow().iter().find(|t| *t.kind.borrow() == TabKind::Prompt(id.clone())).cloned();
        if let Some(t) = existing {
            self.show_tab(&t);
            return;
        }
        let from = if truthy(pget(h, "sourceName")) { py_str(pget(h, "sourceName")) } else { "?".into() };
        let text = format!("{}\nfrom {} · {}\n\n{}\n", sval(h, "title"), from, sval(h, "cwd"), sval(h, "prompt"));
        let view = gtk::TextView::new();
        view.add_css_class("prompt");
        view.set_editable(false);
        view.set_cursor_visible(false);
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.buffer().set_text(&text);
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_child(Some(&view));
        scroll.set_hexpand(true);
        scroll.set_vexpand(true);
        let title = format!("handoff: {}", sval(h, "title"));
        self.add_tab(TabKind::Prompt(id), &title, scroll.upcast(), None, Rc::new(Cell::new(false)));
    }

    // ------------------------------------------------------------------ files

    /// Runs `work` off the main thread, then `done` on it (no refresh afterwards).
    fn off_thread<T: Send + 'static>(self: &Rc<Self>, work: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(&Rc<App>, T) + 'static) {
        let handle = gio::spawn_blocking(work);
        let app = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(v) = handle.await {
                done(&app, v);
            }
        });
    }

    fn session_obj(&self, id: &str) -> Option<Value> {
        self.snap.borrow().sessions.iter().find(|s| py_str(pget(s, "id")) == id).cloned()
    }

    /// Where a session's relative paths start: the folder it works in now (a worktree it
    /// entered, from the end of its transcript), the one in its process file, where it began.
    fn session_bases(&self, s: &Value) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = vec![];
        let sid = sval(s, "sessionId");
        if !sid.is_empty() {
            let main = format!("{sid}.jsonl");
            if let Some(t) = files::transcripts(&self.env.claude, sid).into_iter().find(|p| p.file_name().is_some_and(|n| n.to_string_lossy() == main)) {
                if let Some(c) = files::last_cwd(&t) {
                    out.push(c.into());
                }
            }
        }
        if truthy(pget(s, "pid")) {
            let f = plane_core::read_obj(&self.env.sessions().join(format!("{}.json", py_str(pget(s, "pid")))));
            if !sval(&f, "cwd").is_empty() {
                out.push(sval(&f, "cwd").into());
            }
        }
        if !sval(s, "cwd").is_empty() {
            out.push(sval(s, "cwd").into());
        }
        let mut seen = HashSet::new();
        out.retain(|p| p.is_dir() && seen.insert(p.clone()));
        if out.is_empty() {
            out.push(self.env.home.clone());
        }
        out
    }

    fn session_origin(&self, s: &Value) -> (Vec<PathBuf>, Origin) {
        let bases = self.session_bases(s);
        let name = py_str(plane_core::or(&[pget(s, "name"), pget(s, "id")]));
        let origin = Origin { root: bases[0].clone(), session: Some(py_str(pget(s, "id"))), name };
        (bases, origin)
    }

    /// A click on a path or a link in a terminal.
    fn open_link(self: &Rc<Self>, tab: &Rc<Tab>, text: &str) {
        let Some(link) = files::parse_link(text) else {
            if text.starts_with("http://") || text.starts_with("https://") {
                gtk::UriLauncher::new(text).launch(Some(&self.window), None::<&gio::Cancellable>, |_| {}); // as kitty does
            }
            return;
        };
        let kind = tab.kind.borrow().clone();
        let session = match &kind {
            TabKind::Session(id) => self.session_obj(id),
            _ => None,
        };
        let (bases, origin) = match session {
            Some(s) => self.session_origin(&s),
            None => {
                let o = tab.origin.borrow().clone().unwrap_or(Origin { root: self.env.home.clone(), session: None, name: tab.name.clone() });
                (vec![o.root.clone()], o)
            }
        };
        match files::resolve(&link.file, &bases, &self.env.home) {
            Some(p) => self.open_file(&p, link.line, link.col, Some(origin)),
            None => self.say(&format!("no file called {} in this session's folder", link.file), "warn"),
        }
    }

    fn file_tab(&self, path: &Path) -> Option<Rc<Tab>> {
        self.tabs.borrow().iter().find(|t| matches!(&*t.kind.borrow(), TabKind::File(p) if p == path)).cloned()
    }

    /// Opens a file in a tab (or shows its tab), at `line` if given.
    fn open_file(self: &Rc<Self>, path: &Path, line: Option<u32>, col: Option<u32>, origin: Option<Origin>) {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Some(tab) = self.file_tab(&path) {
            self.show_tab(&tab);
            if let (Some(ed), Some(l)) = (tab.editor.borrow().clone(), line) {
                ed.goto(l, col);
            }
            return;
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
        let origin = origin.unwrap_or_else(|| {
            let root = path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.env.home.clone());
            let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "~".into());
            Origin { root, session: None, name }
        });
        let shown = editor::display_path(&path, &origin.root, &self.env.home);
        match files::load(&path) {
            Ok((files::Content::Text(text), stamp)) => {
                let base = files::head_text(&path).map(files::Base::Head).unwrap_or_else(|| files::Base::Opened(text.clone()));
                let hooks = {
                    let (w1, w2, w3) = (Rc::downgrade(self), Rc::downgrade(self), Rc::downgrade(self));
                    let (p1, p2, p3) = (path.clone(), path.clone(), path.clone());
                    editor::Hooks {
                        dirty: Box::new(move |d| {
                            if let Some(app) = w1.upgrade() {
                                app.file_dirty(&p1, d);
                            }
                        }),
                        close: Box::new(move || {
                            if let Some(app) = w2.upgrade() {
                                if let Some(t) = app.file_tab(&p2) {
                                    app.close_tab(&t);
                                }
                            }
                        }),
                        changed_on_disk: Box::new(move || {
                            if let Some(app) = w3.upgrade() {
                                app.refresh_who(Some(&p3));
                            }
                        }),
                    }
                };
                let ed = Editor::new(path.clone(), &shown, text, stamp, base, &self.colors.borrow(), hooks);
                let tab = self.add_tab(TabKind::File(path.clone()), &name, ed.page.clone().upcast(), None, Rc::new(Cell::new(false)));
                *tab.editor.borrow_mut() = Some(ed.clone());
                *tab.origin.borrow_mut() = Some(origin);
                self.focus_tab(&tab);
                if let Some(l) = line {
                    ed.goto(l, col);
                }
                self.refresh_who(Some(&path));
            }
            Ok((c, _)) => {
                let page = editor::message_page(&path, &shown, &editor::why_not_text(&c));
                let tab = self.add_tab(TabKind::File(path.clone()), &name, page, None, Rc::new(Cell::new(false)));
                *tab.origin.borrow_mut() = Some(origin);
            }
            Err(files::ReadError::Missing) => self.say(&format!("no such file: {}", tilde(&path.to_string_lossy(), &self.home)), "warn"),
            Err(files::ReadError::Other(e)) => self.say(&format!("couldn't open {name}: {e}"), "bad"),
        }
    }

    /// The ● in a file tab's title while it has unsaved changes.
    fn file_dirty(&self, path: &Path, dirty: bool) {
        if let Some(t) = self.file_tab(path) {
            let text = if dirty { format!("● {}", t.name) } else { t.name.clone() };
            t.title.set_width_chars(text.chars().count().min(26) as i32);
            t.title.set_max_width_chars(26);
            t.title.set_text(&text);
        }
    }

    fn dirty_editors(&self) -> Vec<(Rc<Tab>, Rc<Editor>)> {
        self.tabs.borrow().iter().filter_map(|t| t.editor.borrow().clone().filter(|e| e.is_dirty()).map(|e| (t.clone(), e))).collect()
    }

    /// The window is closing with unsaved files: save them, leave them, or stay.
    fn ask_quit(self: &Rc<Self>, dirty: Vec<(Rc<Tab>, Rc<Editor>)>) {
        let names: Vec<String> = dirty.iter().map(|(_, e)| e.file_name()).collect();
        let (message, save) = if names.len() == 1 {
            (format!("Save your changes to {} before quitting?", names[0]), "Save and quit")
        } else {
            (format!("{} files have unsaved changes: {}", names.len(), names.join(", ")), "Save all and quit")
        };
        let dialog = gtk::AlertDialog::builder()
            .modal(true)
            .message(message)
            .detail("If you quit without saving, the changes are lost. Sessions keep running either way.")
            .buttons(["Cancel", "Quit without saving", save])
            .cancel_button(0)
            .default_button(2)
            .build();
        let app = self.clone();
        dialog.choose(Some(&self.window), None::<&gio::Cancellable>, move |r| match r {
            Ok(2) => {
                for (tab, ed) in &dirty {
                    if !ed.save(false) {
                        app.show_tab(tab); // it says why, and nothing is lost
                        return;
                    }
                }
                app.allow_close.set(true);
                app.window.close();
            }
            Ok(1) => {
                app.allow_close.set(true);
                app.window.close();
            }
            _ => {}
        });
    }

    /// Who last edited each open file, from the transcripts of the sessions listed: "node is
    /// working on this" for an edit in the last two minutes by a working session.
    fn refresh_who(self: &Rc<Self>, only: Option<&Path>) {
        let paths: Vec<PathBuf> = self
            .tabs
            .borrow()
            .iter()
            .filter(|t| t.editor.borrow().is_some())
            .filter_map(|t| match &*t.kind.borrow() {
                TabKind::File(p) if only.is_none_or(|o| o == p) => Some(p.clone()),
                _ => None,
            })
            .collect();
        if paths.is_empty() {
            return;
        }
        let sessions: Vec<(String, String, bool)> = self
            .snap
            .borrow()
            .sessions
            .iter()
            .filter(|s| !sval(s, "sessionId").is_empty())
            .map(|s| (sval(s, "sessionId").to_owned(), py_str(plane_core::or(&[pget(s, "name"), pget(s, "id")])), session_state(s) == "working"))
            .collect();
        let (scans, claude) = (self.scans.clone(), self.env.claude.clone());
        self.off_thread(
            move || {
                let mut best: HashMap<PathBuf, (String, f64, bool)> = HashMap::new();
                let mut scans = scans.lock().unwrap();
                for (sid, name, working) in &sessions {
                    for t in files::transcripts(&claude, sid) {
                        let scan = scans.entry(t.clone()).or_default();
                        scan.update(&t);
                        for p in &paths {
                            if let Some(&ts) = scan.edits.get(p) {
                                if best.get(p).is_none_or(|b| ts > b.1) {
                                    best.insert(p.clone(), (name.clone(), ts, *working));
                                }
                            }
                        }
                    }
                }
                (paths, best)
            },
            |app, (paths, best)| {
                for p in paths {
                    let text = match best.get(&p) {
                        Some((name, ts, true)) if now() - ts < 120.0 => format!("{name} is working on this"),
                        Some((name, ts, _)) => format!("{name} edited this {}", ago(*ts)),
                        None => String::new(),
                    };
                    if let Some(ed) = app.file_tab(&p).and_then(|t| t.editor.borrow().clone()) {
                        ed.set_who(&text);
                    }
                }
            },
        );
    }

    /// `f` or the menu's Files on a session: that session's files, now.
    fn files_for_session(self: &Rc<Self>, s: &Value) {
        if truthy(pget(s, "bg")) {
            self.follow(Follow::Session(py_str(pget(s, "id"))), true);
            self.open_files(true);
        } else {
            let (_, origin) = self.session_origin(s);
            self.fp_pinned.set(false);
            *self.fp_follow.borrow_mut() = Some(Follow::Folder(origin.clone()));
            self.show_files(origin, true);
        }
    }

    /// Points Files at a session or folder: shown now if Files is open, else next time.
    /// `by_click` (a click on a session) overrides a folder picked by hand; tab switches
    /// and selection moves don't.
    fn follow(self: &Rc<Self>, f: Follow, by_click: bool) {
        if by_click {
            self.fp_pinned.set(false);
        } else if self.fp_pinned.get() {
            return;
        }
        *self.fp_follow.borrow_mut() = Some(f);
        if self.stack.visible_child_name().as_deref() == Some("files") {
            self.open_files(false);
        }
    }

    /// A tab came to the front: Files follows its session, or the folder its file came from.
    fn follow_current_tab(self: &Rc<Self>) {
        let Some(tab) = self.current_tab() else { return };
        let kind = tab.kind.borrow().clone();
        let f = match kind {
            TabKind::Session(id) => Some(Follow::Session(id)),
            _ => tab.origin.borrow().clone().map(|o| match &o.session {
                Some(id) if self.session_obj(id).is_some() => Follow::Session(id.clone()),
                _ => Follow::Folder(o),
            }),
        };
        if let Some(f) = f {
            self.follow(f, false);
        }
    }

    fn follow_origin(&self) -> Option<Origin> {
        match self.fp_follow.borrow().clone()? {
            Follow::Session(id) => self.session_obj(&id).map(|s| self.session_origin(&s).1),
            Follow::Folder(o) => Some(o),
        }
    }

    /// Shows the Files view: a folder picked by hand, else what it follows (the session you
    /// last picked or the tab in front), else the folder it last showed, else home.
    fn open_files(self: &Rc<Self>, focus: bool) {
        let pinned = if self.fp_pinned.get() { self.fp_origin.borrow().clone() } else { None };
        let origin = pinned.or_else(|| self.follow_origin()).or_else(|| self.fp_origin.borrow().clone()).unwrap_or_else(|| Origin {
            root: self.env.home.clone(),
            session: None,
            name: "~".into(),
        });
        self.show_files(origin, focus);
    }

    /// `/` and Ctrl+P: the Files view with the cursor in its find box.
    fn find_a_file(self: &Rc<Self>) {
        if self.stack.visible_child_name().as_deref() != Some("files") {
            self.open_files(true);
        }
        self.fp_view.grab_focus();
    }

    /// Keys in the Files view.
    fn files_key(self: &Rc<Self>, key: gdk::Key, _mods: gdk::ModifierType) -> glib::Propagation {
        if key == gdk::Key::Escape {
            self.back_to_list();
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    }

    fn show_files(self: &Rc<Self>, origin: Origin, focus: bool) {
        self.cancel_confirm();
        let same = self.fp_origin.borrow().as_ref().is_some_and(|o| o.root == origin.root);
        self.fp_title.set_text(&format!("FILES · {}", origin.name));
        self.fp_where.set_text(&tilde(&origin.root.to_string_lossy(), &self.home));
        self.fp_where.set_tooltip_text(Some(&origin.root.to_string_lossy()));
        *self.fp_origin.borrow_mut() = Some(origin);
        if !same {
            self.fp_store.remove_all();
            self.fp_store.append(&fp_item("note", "", "reading the folder…"));
            self.fp_split.set(0);
            self.fp_shown.borrow_mut().clear();
        }
        self.show_page("files");
        self.refresh_files_page(!same);
        if self.fp_timer.borrow().is_none() {
            let weak = Rc::downgrade(self);
            let id = glib::timeout_add_seconds_local(4, move || {
                let Some(app) = weak.upgrade() else { return glib::ControlFlow::Break };
                if app.stack.visible_child_name().as_deref() != Some("files") {
                    *app.fp_timer.borrow_mut() = None;
                    return glib::ControlFlow::Break;
                }
                app.refresh_files_page(false);
                glib::ControlFlow::Continue
            });
            *self.fp_timer.borrow_mut() = Some(id);
        }
        if focus {
            self.fp_view.grab_focus();
        }
    }

    /// The changed lists at the top (git status, and what the session's transcripts say it
    /// edited), and with `full` the folder tree under them.
    fn refresh_files_page(self: &Rc<Self>, full: bool) {
        let Some(origin) = self.fp_origin.borrow().clone() else { return };
        let session = origin.session.as_ref().and_then(|id| self.session_obj(id)).map(|s| sval(&s, "sessionId").to_owned()).filter(|s| !s.is_empty());
        let (scans, claude, root) = (self.scans.clone(), self.env.claude.clone(), origin.root.clone());
        self.off_thread(
            move || {
                let changes = files::git_changes(&root);
                let mut edits: Vec<(PathBuf, f64)> = vec![];
                if let Some(sid) = &session {
                    let mut scans = scans.lock().unwrap();
                    let mut all: HashMap<PathBuf, f64> = HashMap::new();
                    for t in files::transcripts(&claude, sid) {
                        let scan = scans.entry(t.clone()).or_default();
                        scan.update(&t);
                        for (p, ts) in &scan.edits {
                            let e = all.entry(p.clone()).or_insert(0.0);
                            *e = e.max(*ts);
                        }
                    }
                    let listed: HashSet<&PathBuf> = changes.iter().flatten().map(|c| &c.path).collect();
                    edits = all.into_iter().filter(|(p, _)| !listed.contains(p) && p.is_file()).collect();
                    edits.sort_by(|a, b| b.1.total_cmp(&a.1));
                    edits.truncate(12);
                }
                let ignored = if full { files::git_ignored(&files::repo_root(&root).unwrap_or_else(|| root.clone())) } else { vec![] };
                (root, changes, edits, ignored)
            },
            move |app, (root, changes, edits, ignored)| {
                if app.fp_origin.borrow().as_ref().is_none_or(|o| o.root != root) {
                    return; // the page moved on to another folder meanwhile
                }
                let mut top: Vec<String> = vec![];
                if let Some(mut ch) = changes {
                    ch.sort_by(|a, b| a.path.cmp(&b.path));
                    top.push(format!("head\t\tNOT COMMITTED · {}", ch.len()));
                    if ch.is_empty() {
                        top.push("note\t\tnothing uncommitted".into());
                    }
                    for c in ch {
                        top.push(format!("chg\t{}\t{}", c.path.display(), c.word));
                    }
                }
                if !edits.is_empty() {
                    top.push("head\t\tEDITED BY THIS SESSION".into());
                    for (p, ts) in edits {
                        top.push(format!("edit\t{}\t{}", p.display(), ago(ts)));
                    }
                }
                top.push("head\t\tALL FILES".into());
                if full {
                    *app.fp_ignored.borrow_mut() = ignored.into_iter().collect();
                    let tree = folder_model(&root, &app.fp_ignored.borrow());
                    let mut all: Vec<glib::Object> = top.iter().map(|s| gtk::StringObject::new(s).upcast()).collect();
                    for i in 0..tree.n_items() {
                        if let Some(o) = tree.item(i) {
                            all.push(o);
                        }
                    }
                    let n = app.fp_store.n_items();
                    app.fp_store.splice(0, n, &all);
                } else if *app.fp_shown.borrow() != top {
                    let objs: Vec<gtk::StringObject> = top.iter().map(|s| gtk::StringObject::new(s)).collect();
                    app.fp_store.splice(0, app.fp_split.get(), &objs);
                } else {
                    return;
                }
                app.fp_split.set(top.len() as u32);
                *app.fp_shown.borrow_mut() = top;
            },
        );
    }

    fn wire_files_page(self: &Rc<Self>) {
        let factory = gtk::SignalListItemFactory::new();
        let weak = Rc::downgrade(self);
        factory.connect_setup(move |_, obj| {
            let Some(li) = obj.downcast_ref::<gtk::ListItem>() else { return };
            let exp = gtk::TreeExpander::new();
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let name = label("", &["name"]);
            name.set_ellipsize(pango::EllipsizeMode::Middle);
            let dir = label("", &["note"]);
            dir.set_hexpand(true);
            dir.set_ellipsize(pango::EllipsizeMode::Start);
            dir.set_max_width_chars(4); // the name gets the room first; the folder takes what's left
            let note = label("", &["note"]);
            b.append(&name);
            b.append(&dir);
            b.append(&note);
            exp.set_child(Some(&b));
            // one click opens a file or opens and folds a folder, as in other file lists
            let click = gtk::GestureClick::new();
            click.set_button(1);
            let (w, l) = (weak.clone(), li.downgrade());
            click.connect_released(move |g, n, _, _| {
                let (Some(app), Some(li)) = (w.upgrade(), l.upgrade()) else { return };
                if n == 1 && g.current_event_state().is_empty() {
                    app.files_activate(li.position());
                }
            });
            exp.add_controller(click);
            li.set_child(Some(&exp));
        });
        let home = self.home.clone();
        let weak = Rc::downgrade(self);
        factory.connect_bind(move |_, obj| {
            let Some(li) = obj.downcast_ref::<gtk::ListItem>() else { return };
            let Some(row) = li.item().and_downcast::<gtk::TreeListRow>() else { return };
            let Some(exp) = li.child().and_downcast::<gtk::TreeExpander>() else { return };
            exp.set_list_row(Some(&row));
            let Some(s) = row.item().and_downcast::<gtk::StringObject>().map(|o| o.string()) else { return };
            let (kind, path, note) = fp_parse(&s);
            let Some(b) = exp.child().and_downcast::<gtk::Box>() else { return };
            let name = b.first_child().and_downcast::<gtk::Label>().unwrap();
            let dir = name.next_sibling().and_downcast::<gtk::Label>().unwrap();
            let note_l = b.last_child().and_downcast::<gtk::Label>().unwrap();
            let p = Path::new(path);
            let file_name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let root = weak.upgrade().and_then(|a| a.fp_origin.borrow().as_ref().map(|o| o.root.clone())).unwrap_or_default();
            let rel_dir = p.parent().and_then(|d| d.strip_prefix(&root).ok()).map(|d| d.display().to_string()).unwrap_or_else(|| p.parent().map(|d| tilde(&d.to_string_lossy(), &home)).unwrap_or_default());
            let heading = kind == "head" || kind == "note";
            li.set_selectable(!heading);
            li.set_activatable(!heading);
            exp.set_hide_expander(kind != "dir");
            exp.set_indent_for_icon(kind == "dir" || kind == "file");
            for (w, c, on) in [(&name, "head", kind == "head"), (&name, "dim", kind == "note"), (&name, "ignored", note == "ignored"), (&name, "strike", note == "deleted")] {
                if on { w.add_css_class(c) } else { w.remove_css_class(c) }
            }
            dir.set_visible(!heading);
            note_l.set_visible(!heading);
            name.set_hexpand(heading);
            match kind {
                "head" | "note" => {
                    name.set_text(note);
                    dir.set_text("");
                    note_l.set_text("");
                }
                "chg" | "edit" => {
                    name.set_text(&file_name);
                    dir.set_text(&rel_dir);
                    note_l.set_text(note);
                    exp.set_tooltip_text(Some(&tilde(path, &home)));
                }
                _ => {
                    name.set_text(&file_name);
                    dir.set_text("");
                    note_l.set_text(if note == "ignored" { "" } else { note });
                    exp.set_tooltip_text(None);
                }
            }
        });
        self.fp_view.set_factory(Some(&factory));
        let weak = Rc::downgrade(self);
        self.fp_view.connect_activate(move |_, pos| {
            if let Some(app) = weak.upgrade() {
                app.files_activate(pos);
            }
        });
        // → opens a folder, ← folds it (or goes up to the folder it's in)
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(app) = weak.upgrade() else { return glib::Propagation::Proceed };
            if !mods.is_empty() || !matches!(key, gdk::Key::Left | gdk::Key::Right) {
                return glib::Propagation::Proceed;
            }
            let Some(sel) = app.fp_view.model().and_downcast::<gtk::SingleSelection>() else { return glib::Propagation::Proceed };
            let Some(row) = sel.selected_item().and_downcast::<gtk::TreeListRow>() else { return glib::Propagation::Proceed };
            if key == gdk::Key::Right {
                if row.is_expandable() {
                    row.set_expanded(true);
                }
            } else if row.is_expanded() {
                row.set_expanded(false);
            } else if let Some(parent) = row.parent() {
                let pos = parent.position();
                sel.set_selected(pos);
                app.fp_view.scroll_to(pos, gtk::ListScrollFlags::FOCUS, None);
            }
            glib::Propagation::Stop
        });
        self.fp_view.add_controller(keys);
    }

    fn files_activate(self: &Rc<Self>, pos: u32) {
        let Some(row) = self.fp_tree.item(pos).and_downcast::<gtk::TreeListRow>() else { return };
        let Some(s) = row.item().and_downcast::<gtk::StringObject>().map(|o| o.string()) else { return };
        let (kind, path, note) = fp_parse(&s);
        if let Some(sel) = self.fp_view.model().and_downcast::<gtk::SingleSelection>() {
            if kind != "head" && kind != "note" {
                sel.set_selected(pos);
            }
        }
        match kind {
            "dir" => row.set_expanded(!row.is_expanded()),
            "file" | "chg" | "edit" if note == "deleted" => self.say("deleted: it's only in the last commit now", "info"),
            "file" | "chg" | "edit" => {
                let origin = self.fp_origin.borrow().clone();
                self.open_file(Path::new(path), None, None, origin);
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------------ pages

    /// The help and new-session pages take the sidebar; the details and key hints are for
    /// the list.
    fn show_page(&self, name: &str) {
        self.stack.set_visible_child_name(name);
        self.details.set_visible(name == "list");
        self.hints.set_visible(name == "list");
        // the switcher shows which view this is (New and Keys belong to Sessions)
        self.switching.set(true);
        if name == "files" {
            self.switch_files.set_active(true);
        } else {
            self.switch_sessions.set_active(true);
        }
        self.switching.set(false);
        self.new_btn.set_visible(name != "files");
    }

    fn back_to_list(&self) {
        self.show_page("list");
        *self.ns_state.borrow_mut() = Step::Folder;
        self.focus_list();
    }

    fn show_help(&self) {
        self.cancel_confirm();
        self.show_page("help");
        self.help.grab_focus();
    }

    fn new_session(self: &Rc<Self>) {
        self.cancel_confirm();
        while let Some(c) = self.ns_folders.first_child() {
            self.ns_folders.remove(&c);
        }
        let mut opts: Vec<(String, String, String)> = actions::recent_projects(&self.env, 10)
            .into_iter()
            .map(|p| {
                let path = Path::new(&p);
                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.clone());
                let parent = path.parent().map(|d| tilde(&d.to_string_lossy(), &self.home)).unwrap_or_default();
                (name, p, parent)
            })
            .collect();
        opts.push(("~".into(), self.home.clone(), "home folder".into()));
        opts.push(("Type a path…".into(), String::new(), String::new()));
        for (name, path, note) in opts {
            let row = gtk::ListBoxRow::new();
            row.set_widget_name(&path);
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let n = label(&name, &["name"]);
            let d = label(&note, &["dim"]);
            d.set_ellipsize(pango::EllipsizeMode::Start);
            d.set_hexpand(true);
            b.append(&n);
            b.append(&d);
            row.set_child(Some(&b));
            self.ns_folders.append(&row);
        }
        self.set_step(Step::Folder);
        self.show_page("new");
        if let Some(first) = self.ns_folders.row_at_index(0) {
            self.ns_folders.select_row(Some(&first));
            first.grab_focus();
        }
    }

    fn set_step(&self, step: Step) {
        self.ns_error.set_text("");
        self.ns_error.set_visible(false);
        let folder_view = self.ns_folders.parent().and_then(|p| p.parent());
        let (n, ask, hint, entry) = match &step {
            Step::Folder => ("1 of 3", "Which folder?".to_owned(), "esc cancels", false),
            Step::Path => ("1 of 3", "Folder path".to_owned(), "tab completes · esc cancels", true),
            Step::Name(cwd) => ("2 of 3", format!("Name it ({})", tilde(cwd, &self.home)), "leave empty to skip · esc cancels", true),
            Step::Message(cwd, _) => ("3 of 3", format!("First message ({})", tilde(cwd, &self.home)), "leave empty to skip · esc cancels", true),
        };
        self.ns_step.set_text(&format!("NEW SESSION · {n}"));
        self.ns_ask.set_text(&ask);
        self.ns_hint.set_text(hint);
        if let Some(v) = folder_view {
            v.set_visible(!entry);
        }
        self.ns_entry.set_visible(entry);
        if entry {
            self.ns_entry.set_text(if matches!(step, Step::Path) { "~/" } else { "" });
            self.ns_entry.grab_focus();
            self.ns_entry.set_position(-1);
        }
        *self.ns_state.borrow_mut() = step;
    }

    fn folder_chosen(self: &Rc<Self>, path: String) {
        if path.is_empty() {
            self.set_step(Step::Path);
        } else {
            self.set_step(Step::Name(path));
        }
    }

    fn entry_done(self: &Rc<Self>) {
        let text = self.ns_entry.text().trim().to_owned();
        let step = std::mem::replace(&mut *self.ns_state.borrow_mut(), Step::Folder);
        match step {
            Step::Path => {
                if text.is_empty() {
                    self.back_to_list();
                    return;
                }
                let full = actions::expand_user(&self.env, &text);
                let full = if Path::new(&full).is_absolute() { full } else { self.env.home.join(&full).to_string_lossy().into_owned() };
                let full = full.trim_end_matches('/').to_owned();
                let full = if full.is_empty() { "/".to_owned() } else { full };
                if !Path::new(&full).is_dir() {
                    *self.ns_state.borrow_mut() = Step::Path;
                    self.ns_error.set_text(&format!("no such folder: {full}"));
                    self.ns_error.set_visible(true);
                    return;
                }
                self.set_step(Step::Name(full));
            }
            Step::Name(cwd) => self.set_step(Step::Message(cwd, text)),
            Step::Message(cwd, name) => {
                self.back_to_list();
                let title = if !name.is_empty() {
                    name.clone()
                } else {
                    Path::new(&cwd).file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.is_empty()).unwrap_or_else(|| "~".into())
                };
                self.start_bg(&cwd, &title, StartOpts { name, prompt: text, ..StartOpts::default() });
            }
            Step::Folder => {}
        }
    }
}

/// Files-page rows are strings: "kind\tpath\tnote". Kinds: head, note, chg (git status),
/// edit (the session's transcript), dir and file (the folder tree).
fn fp_parse(s: &str) -> (&str, &str, &str) {
    let mut it = s.splitn(3, '\t');
    (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""))
}

fn fp_item(kind: &str, path: &str, note: &str) -> gtk::StringObject {
    gtk::StringObject::new(&format!("{kind}\t{path}\t{note}"))
}

/// One folder's entries for the tree, folders first; `.git` left out, ignored ones marked.
fn folder_model(dir: &Path, ignored: &HashSet<PathBuf>) -> gio::ListStore {
    let store = gio::ListStore::new::<gtk::StringObject>();
    let Ok(rd) = std::fs::read_dir(dir) else {
        store.append(&fp_item("note", "", "can't read this folder"));
        return store;
    };
    let mut entries: Vec<(bool, String, PathBuf)> = rd
        .filter_map(Result::ok)
        .map(|e| (e.path().is_dir(), e.file_name().to_string_lossy().into_owned(), e.path()))
        .filter(|(_, n, _)| n != ".git")
        .collect();
    entries.sort_by(|a, b| (!a.0, a.1.to_lowercase()).cmp(&(!b.0, b.1.to_lowercase())));
    if entries.is_empty() {
        store.append(&fp_item("note", "", "empty folder"));
    }
    for (is_dir, _, p) in entries {
        let ign = p.ancestors().any(|a| ignored.contains(a));
        store.append(&fp_item(if is_dir { "dir" } else { "file" }, &p.to_string_lossy(), if ign { "ignored" } else { "" }));
    }
    store
}

/// The session the kitty Plane window has in its pane, if it's running (read-only).
fn kitty_plane_shows() -> Option<String> {
    let runtime = plane_core::control::socket_path().parent()?.to_path_buf();
    let sock = runtime.join("plane-kitty.sock");
    if !sock.exists() {
        return None;
    }
    let to = format!("unix:{}", sock.display());
    let out = std::process::Command::new("kitty").args(["@", "--to", &to, "ls"]).output().ok()?;
    let data: Value = serde_json::from_slice(&out.stdout).ok()?;
    for os in data.as_array()? {
        for tab in os.get("tabs")?.as_array()? {
            for w in tab.get("windows")?.as_array()? {
                let vars = w.get("user_vars")?;
                if vars.get("plane_role").and_then(Value::as_str) == Some("session") {
                    return vars.get("plane_id").and_then(Value::as_str).map(str::to_owned);
                }
            }
        }
    }
    None
}

/// The keys page: (the scrolled page, the grid that takes the keyboard).
fn help_page() -> (gtk::Widget, gtk::Widget) {
    let rows: &[(&str, &str)] = &[
        ("#", "ANY ROW"),
        ("Enter / click", "open it"),
        ("right-click", "all its actions"),
        ("↑↓  j k", "move the selection"),
        ("Home End  g G", "first / last"),
        ("#", "SESSIONS"),
        ("x", "close: off the list, conversation kept"),
        ("p", "pause: stays listed"),
        ("#", "HANDOFFS"),
        ("Enter / 2× click", "start it"),
        ("v", "read the prompt"),
        ("x", "discard (u undoes)"),
        ("#", "PLANE"),
        ("n", "new session"),
        ("super ←→", "sidebar ⇄ session"),
        ("Ctrl+V", "paste into the session"),
        ("r", "refresh"),
        ("q", "quit (sessions go on)"),
        ("#", "TABS"),
        ("× on a tab", "hide it; the session keeps running"),
        ("#", "FILES"),
        ("click a path", "open it in a tab"),
        ("f / Files", "a session's changed files and folder"),
        ("Ctrl+S  Ctrl+F", "save · find"),
        ("Ctrl+W", "close the file"),
        ("#", "TIP"),
        ("", "In a session, ← on an empty prompt opens Claude's own list; Enter there comes back."),
    ];
    let b = gtk::Grid::new();
    b.set_column_spacing(12);
    b.set_row_spacing(2);
    b.add_css_class("page");
    b.add_css_class("keys");
    b.set_focusable(true);
    let mut y = 0;
    b.attach(&label("KEYS", &["heading"]), 0, y, 2, 1);
    for (k, d) in rows {
        y += 1;
        if *k == "#" {
            b.attach(&label(d, &["heading"]), 0, y, 2, 1);
            continue;
        }
        let dl = label(d, &["dim"]);
        dl.set_wrap(true);
        dl.set_hexpand(true);
        if k.is_empty() {
            b.attach(&dl, 0, y, 2, 1);
        } else {
            b.attach(&label(k, &["key"]), 0, y, 1, 1);
            b.attach(&dl, 1, y, 1, 1);
        }
    }
    let back = label("any key or a click goes back", &["dim"]);
    back.set_margin_top(10);
    b.attach(&back, 0, y + 1, 2, 1);
    let s = gtk::ScrolledWindow::new();
    s.set_hscrollbar_policy(gtk::PolicyType::Never);
    s.set_child(Some(&b));
    (s.upcast(), b.upcast())
}
