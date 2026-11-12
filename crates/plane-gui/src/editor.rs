//! A file in a tab: GtkSourceView with syntax colors, undo, find and save, the lines that
//! differ from the last commit marked beside the line numbers, and edits made on disk (by an
//! agent, say) taken in as they land.
//!
//! Your text is never replaced while it has unsaved changes. If the file changes on disk
//! then, a bar says so; Compare shows both versions side by side, and you choose which one
//! to keep. A save first checks the disk still holds what the editor last read or wrote, and
//! writes through a temporary file renamed into place (`plane_core::files::save`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib, pango};
use plane_core::files::{self, Base, Content, ReadError, SaveError, Stamp};
use sourceview5 as sv;
use sv::prelude::*;

const MARK_ADDED: &str = "plane-added";
const MARK_CHANGED: &str = "plane-changed";
const MARK_REMOVED: &str = "plane-removed";

/// What the editor tells the app.
pub struct Hooks {
    /// Unsaved changes appeared or went (the tab title shows a ●).
    pub dirty: Box<dyn Fn(bool)>,
    /// Ctrl+W.
    pub close: Box<dyn Fn()>,
    /// The file changed on disk (the app looks up who changed it).
    pub changed_on_disk: Box<dyn Fn()>,
    /// Ctrl+P: find a file to open.
    pub find_file: Box<dyn Fn()>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Trouble {
    None,
    /// Changed on disk while you had unsaved text (or a save found it changed).
    Conflict,
    /// Deleted or moved away on disk.
    Gone,
    /// The disk now holds something that isn't text.
    NotText,
    /// A save failed.
    SaveFailed,
}

pub struct Editor {
    pub path: PathBuf,
    pub page: gtk::Box,
    pub view: sv::View,
    buffer: sv::Buffer,
    stack: gtk::Stack,
    note: gtk::Label,
    show_btn: gtk::Button,
    who: gtk::Label,
    changes_btn: gtk::ToggleButton,
    find_btn: gtk::Button,
    save_btn: gtk::Button,
    // the bar for trouble: a conflict, a deleted file, a failed save
    banner: gtk::Box,
    banner_text: gtk::Label,
    compare_btn: gtk::Button,
    mine_btn: gtk::Button,
    theirs_btn: gtk::Button,
    retry_btn: gtk::Button,
    // find
    find_bar: gtk::Box,
    find_entry: gtk::SearchEntry,
    find_count: gtk::Label,
    search: sv::SearchContext,
    settings: sv::SearchSettings,
    // changes
    diff_head: gtk::Label,
    diff_view: sv::View,
    diff_lines: RefCell<Vec<Option<u32>>>,
    // compare
    disk_buffer: sv::Buffer,
    mine_view: sv::View,
    // state
    stamp: RefCell<Option<Stamp>>,
    base: RefCell<Base>,
    theirs: RefCell<Option<(String, Stamp)>>,
    trouble: Cell<Trouble>,
    landed_at: Cell<Option<i32>>,
    check_queued: Cell<bool>,
    marks_queued: Cell<bool>,
    monitor: RefCell<Option<gio::FileMonitor>>,
    hooks: Hooks,
}

/// What the status line says when nothing else is going on: true, and what the tab is for.
const LIVE_NOTE: &str = "Live: edits by sessions appear here";

fn button(text: &str) -> gtk::Button {
    let b = gtk::Button::with_label(text);
    b.set_focusable(false);
    b.set_cursor_from_name(Some("pointer"));
    b
}

fn text_of(b: &impl IsA<gtk::TextBuffer>) -> String {
    let (s, e) = b.bounds();
    b.text(&s, &e, true).to_string()
}

fn line_iter(b: &impl IsA<gtk::TextBuffer>, line: usize) -> gtk::TextIter {
    b.iter_at_line(line as i32).unwrap_or_else(|| b.end_iter())
}

/// A small picture for the line-mark gutter: a bar (added, changed) or a wedge (removed).
fn mark_picture(color: &gdk::RGBA, wedge: bool) -> gtk::gdk_pixbuf::Pixbuf {
    let (w, h) = (16usize, 16usize);
    let mut px = vec![0u8; w * h * 4];
    let (r, g, b) = ((color.red() * 255.0) as u8, (color.green() * 255.0) as u8, (color.blue() * 255.0) as u8);
    for y in 0..h {
        for x in 0..w {
            let on = if wedge { y < 7 && x < 7 - y } else { x < 4 };
            if on {
                let i = (y * w + x) * 4;
                px[i..i + 4].copy_from_slice(&[r, g, b, 255]);
            }
        }
    }
    gtk::gdk_pixbuf::Pixbuf::from_bytes(&glib::Bytes::from_owned(px), gtk::gdk_pixbuf::Colorspace::Rgb, true, 8, w as i32, h as i32, (w * 4) as i32)
}

fn new_view(buffer: &sv::Buffer) -> sv::View {
    let v = sv::View::with_buffer(buffer);
    v.add_css_class("editor");
    v.set_monospace(true);
    v.set_show_line_numbers(true);
    v.set_highlight_current_line(true);
    v.set_tab_width(4);
    v.set_indent_width(-1);
    v.set_auto_indent(true);
    v.set_smart_backspace(true);
    v.set_smart_home_end(sv::SmartHomeEndType::Before);
    v.set_left_margin(6);
    v.set_bottom_margin(40);
    v.set_hexpand(true);
    v.set_vexpand(true);
    v
}

fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let s = gtk::ScrolledWindow::new();
    s.set_child(Some(child));
    s.set_hexpand(true);
    s.set_vexpand(true);
    s.set_kinetic_scrolling(false);
    s
}

pub fn scheme() -> Option<sv::StyleScheme> {
    sv::StyleSchemeManager::default().scheme("plane")
}

/// Writes the theme's style scheme where GtkSourceView finds it (the app's runtime folder;
/// nothing under your home changes).
pub fn install_scheme(colors: &HashMap<String, gdk::RGBA>) {
    let dir = plane_core::control::socket_path().parent().map(|p| p.join("plane-next-styles")).unwrap_or_else(|| std::env::temp_dir().join("plane-next-styles"));
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("plane.xml"), crate::theme::source_scheme(colors));
    let mgr = sv::StyleSchemeManager::default();
    let dir_s = dir.to_string_lossy().into_owned();
    if !mgr.search_path().iter().any(|p| p.as_str() == dir_s) {
        mgr.append_search_path(&dir_s);
    }
    mgr.force_rescan();
}

/// Where a path sits, for the bar: relative to `root` when inside it.
pub fn display_path(path: &Path, root: &Path, home: &Path) -> String {
    if let Ok(rel) = path.strip_prefix(root) {
        if !rel.as_os_str().is_empty() {
            return rel.display().to_string();
        }
    }
    match path.strip_prefix(home) {
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => path.display().to_string(),
    }
}

impl Editor {
    pub fn new(path: PathBuf, shown: &str, text: String, stamp: Stamp, base: Base, colors: &HashMap<String, gdk::RGBA>, hooks: Hooks) -> Rc<Editor> {
        let lang = {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let head = &text.as_bytes()[..text.len().min(4096)];
            let (ctype, _) = gio::content_type_guess(Some(&name), head);
            sv::LanguageManager::default().guess_language(Some(&name), Some(&ctype))
        };
        let buffer = sv::Buffer::new(None);
        buffer.set_implicit_trailing_newline(false); // the buffer is exactly the file
        buffer.set_highlight_matching_brackets(true);
        buffer.set_language(lang.as_ref());
        buffer.set_style_scheme(scheme().as_ref());
        buffer.begin_irreversible_action();
        buffer.set_text(&text);
        buffer.end_irreversible_action();
        buffer.set_modified(false);
        buffer.place_cursor(&buffer.start_iter());
        let landed = gtk::TextTag::builder().name("landed").paragraph_background_rgba(&crate::theme::tint(colors, "plane_accent", 0.16)).build();
        buffer.tag_table().add(&landed);
        let differs = gtk::TextTag::builder().name("differs").paragraph_background_rgba(&crate::theme::tint(colors, "plane_warn", 0.16)).build();
        buffer.tag_table().add(&differs);

        let view = new_view(&buffer);
        // indent the way the file already does
        let tabs = text.lines().filter(|l| l.starts_with('\t')).count();
        let spaces = text.lines().filter(|l| l.starts_with("  ")).count();
        view.set_insert_spaces_instead_of_tabs(tabs <= spaces);
        let marks = [(MARK_ADDED, "plane_ok", false, "added since the last commit"), (MARK_CHANGED, "plane_warn", false, "changed since the last commit"), (MARK_REMOVED, "plane_bad", true, "lines removed here")];
        for (cat, key, wedge, tip) in marks {
            let a = sv::MarkAttributes::new();
            a.set_pixbuf(&mark_picture(colors.get(key).unwrap_or(&gdk::RGBA::WHITE), wedge));
            let tip = tip.to_owned();
            a.connect_query_tooltip_text(move |_, _| tip.clone());
            view.set_mark_attributes(cat, &a, 10);
        }
        view.set_show_line_marks(true);

        // ---- the bar: where it is, what just happened, who's on it, Changes, Save
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        bar.add_css_class("bar");
        let where_ = gtk::Label::new(Some(shown));
        where_.add_css_class("path");
        where_.set_ellipsize(pango::EllipsizeMode::Start);
        where_.set_xalign(0.0);
        where_.set_tooltip_text(Some(&path.to_string_lossy()));
        let note = gtk::Label::new(None);
        note.set_xalign(0.0);
        note.set_ellipsize(pango::EllipsizeMode::End);
        let show_btn = button("Show");
        show_btn.add_css_class("flat");
        show_btn.set_visible(false);
        let who = gtk::Label::new(None);
        who.add_css_class("dim");
        who.set_hexpand(true);
        who.set_xalign(1.0);
        who.set_ellipsize(pango::EllipsizeMode::Start);
        let find_btn = gtk::Button::new();
        let fb = gtk::Box::new(gtk::Orientation::Horizontal, 5);
        fb.append(&gtk::Image::from_icon_name("edit-find-symbolic"));
        fb.append(&gtk::Label::new(Some("Find")));
        find_btn.set_child(Some(&fb));
        find_btn.set_focusable(false);
        find_btn.set_tooltip_text(Some("Find in this file (Ctrl+F)"));
        let changes_btn = gtk::ToggleButton::with_label("Changes");
        changes_btn.set_focusable(false);
        changes_btn.set_tooltip_text(Some("What's different from the last commit: lines added, changed and removed"));
        let save_btn = button("Saved");
        save_btn.set_sensitive(false);
        save_btn.set_tooltip_text(Some("Ctrl+S"));
        for b in [&find_btn, changes_btn.upcast_ref(), &save_btn] {
            b.set_cursor_from_name(Some("pointer"));
        }
        note.add_css_class("dim");
        note.set_text(LIVE_NOTE);
        bar.append(&where_);
        bar.append(&note);
        bar.append(&show_btn);
        bar.append(&who);
        bar.append(&find_btn);
        bar.append(&changes_btn);
        bar.append(&save_btn);

        // ---- trouble
        let banner = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        banner.add_css_class("banner");
        let banner_text = gtk::Label::new(None);
        banner_text.add_css_class("text");
        banner_text.set_wrap(true);
        banner_text.set_xalign(0.0);
        banner_text.set_hexpand(true);
        let compare_btn = button("Compare");
        let mine_btn = button("Save mine over it");
        let theirs_btn = button("Use the version on disk");
        let retry_btn = button("Try again");
        compare_btn.add_css_class("suggested-action");
        for b in [&compare_btn, &mine_btn, &theirs_btn, &retry_btn] {
            b.set_valign(gtk::Align::Center);
        }
        banner.append(&banner_text);
        banner.append(&compare_btn);
        banner.append(&theirs_btn);
        banner.append(&mine_btn);
        banner.append(&retry_btn);
        banner.set_visible(false);

        // ---- find
        let find_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        find_bar.add_css_class("findbar");
        let find_entry = gtk::SearchEntry::new();
        find_entry.set_placeholder_text(Some("Find in this file"));
        let find_count = gtk::Label::new(None);
        find_count.add_css_class("dim");
        let prev = gtk::Button::from_icon_name("go-up-symbolic");
        prev.set_tooltip_text(Some("Previous (Shift+Enter)"));
        prev.set_focusable(false);
        let next = gtk::Button::from_icon_name("go-down-symbolic");
        next.set_tooltip_text(Some("Next (Enter)"));
        next.set_focusable(false);
        let find_close = button("Close");
        find_close.add_css_class("flat");
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        find_bar.append(&find_entry);
        find_bar.append(&prev);
        find_bar.append(&next);
        find_bar.append(&find_count);
        find_bar.append(&spacer);
        find_bar.append(&find_close);
        find_bar.set_visible(false);
        let settings = sv::SearchSettings::new();
        settings.set_wrap_around(true);
        let search = sv::SearchContext::new(&buffer, Some(&settings));
        search.set_highlight(false);

        // ---- the three views: editing, changes, compare
        let stack = gtk::Stack::new();
        stack.set_transition_type(gtk::StackTransitionType::None);
        stack.add_named(&scrolled(&view), Some("edit"));

        let diff_buffer = sv::Buffer::new(None);
        diff_buffer.set_language(sv::LanguageManager::default().language("diff").as_ref());
        diff_buffer.set_style_scheme(scheme().as_ref());
        let diff_view = new_view(&diff_buffer);
        diff_view.set_editable(false);
        diff_view.set_show_line_numbers(false);
        diff_view.set_highlight_current_line(false);
        let diff_head = gtk::Label::new(None);
        diff_head.add_css_class("changes-head");
        diff_head.set_xalign(0.0);
        diff_head.set_wrap(true);
        let changes = gtk::Box::new(gtk::Orientation::Vertical, 0);
        changes.append(&diff_head);
        changes.append(&scrolled(&diff_view));
        stack.add_named(&changes, Some("changes"));

        let disk_buffer = sv::Buffer::new(None);
        disk_buffer.set_language(lang.as_ref());
        disk_buffer.set_style_scheme(scheme().as_ref());
        disk_buffer.set_implicit_trailing_newline(false);
        let d = gtk::TextTag::builder().name("differs").paragraph_background_rgba(&crate::theme::tint(colors, "plane_warn", 0.16)).build();
        disk_buffer.tag_table().add(&d);
        let disk_view = new_view(&disk_buffer);
        disk_view.set_editable(false);
        let mine_view = new_view(&buffer);
        let side = |title: &str, v: &sv::View| {
            let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let l = gtk::Label::new(Some(title));
            l.add_css_class("side-head");
            l.set_xalign(0.0);
            b.append(&l);
            b.append(&scrolled(v));
            b
        };
        let compare = gtk::Paned::new(gtk::Orientation::Horizontal);
        compare.set_start_child(Some(&side("ON DISK NOW · read-only", &disk_view)));
        compare.set_end_child(Some(&side("YOURS · not saved yet · you can edit here", &mine_view)));
        compare.set_resize_start_child(true);
        compare.set_resize_end_child(true);
        stack.add_named(&compare, Some("compare"));

        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.add_css_class("file");
        page.append(&bar);
        page.append(&banner);
        page.append(&find_bar);
        page.append(&stack);

        let ed = Rc::new(Editor {
            path,
            page,
            view,
            buffer,
            stack,
            note,
            show_btn,
            who,
            changes_btn,
            find_btn,
            save_btn,
            banner,
            banner_text,
            compare_btn,
            mine_btn,
            theirs_btn,
            retry_btn,
            find_bar,
            find_entry,
            find_count,
            search,
            settings,
            diff_head,
            diff_view,
            diff_lines: RefCell::new(vec![]),
            disk_buffer,
            mine_view,
            stamp: RefCell::new(Some(stamp)),
            base: RefCell::new(base),
            theirs: RefCell::new(None),
            trouble: Cell::new(Trouble::None),
            landed_at: Cell::new(None),
            check_queued: Cell::new(false),
            marks_queued: Cell::new(false),
            monitor: RefCell::new(None),
            hooks,
        });
        ed.wire(&prev, &next, &find_close);
        ed.update_marks();
        ed
    }

    fn wire(self: &Rc<Self>, prev: &gtk::Button, next: &gtk::Button, find_close: &gtk::Button) {
        let weak = Rc::downgrade(self);
        self.buffer.connect_modified_changed(move |b| {
            if let Some(ed) = weak.upgrade() {
                let dirty = b.is_modified();
                ed.save_btn.set_label(if dirty { "Save" } else { "Saved" });
                ed.save_btn.set_sensitive(dirty);
                if dirty {
                    ed.save_btn.add_css_class("suggested-action");
                } else {
                    ed.save_btn.remove_css_class("suggested-action");
                }
                // the status line: what's true now (a save or a take-in says more after this)
                if dirty {
                    ed.set_note("Unsaved · Ctrl+S saves", "warn", None);
                } else {
                    ed.set_note("", "", None);
                }
                (ed.hooks.dirty)(dirty);
            }
        });
        let weak = Rc::downgrade(self);
        self.find_btn.connect_clicked(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.open_find();
            }
        });
        let weak = Rc::downgrade(self);
        self.buffer.connect_changed(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.queue_marks();
            }
        });
        let weak = Rc::downgrade(self);
        self.save_btn.connect_clicked(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.save(false);
            }
        });
        let weak = Rc::downgrade(self);
        self.changes_btn.connect_toggled(move |b| {
            if let Some(ed) = weak.upgrade() {
                if b.is_active() {
                    ed.show_changes();
                } else if ed.stack.visible_child_name().as_deref() == Some("changes") {
                    ed.back_to_editing();
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.show_btn.connect_clicked(move |_| {
            if let Some(ed) = weak.upgrade() {
                if let Some(line) = ed.landed_at.get() {
                    ed.back_to_editing();
                    ed.goto(line as u32 + 1, None);
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.compare_btn.connect_clicked(move |_| {
            if let Some(ed) = weak.upgrade() {
                if ed.stack.visible_child_name().as_deref() == Some("compare") {
                    ed.back_to_editing();
                } else {
                    ed.show_compare();
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.mine_btn.connect_clicked(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.save(true);
            }
        });
        let weak = Rc::downgrade(self);
        self.theirs_btn.connect_clicked(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.use_disk();
            }
        });
        let weak = Rc::downgrade(self);
        self.retry_btn.connect_clicked(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.save(false);
            }
        });

        // find
        let weak = Rc::downgrade(self);
        self.find_entry.connect_search_changed(move |e| {
            if let Some(ed) = weak.upgrade() {
                let t = e.text();
                ed.settings.set_search_text(if t.is_empty() { None } else { Some(t.as_str()) });
                ed.find(true, true);
            }
        });
        for (b, forward) in [(prev, false), (next, true)] {
            let weak = Rc::downgrade(self);
            b.connect_clicked(move |_| {
                if let Some(ed) = weak.upgrade() {
                    ed.find(forward, false);
                }
            });
        }
        let weak = Rc::downgrade(self);
        self.find_entry.connect_activate(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.find(true, false);
            }
        });
        let weak = Rc::downgrade(self);
        self.find_entry.connect_next_match(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.find(true, false);
            }
        });
        let weak = Rc::downgrade(self);
        self.find_entry.connect_previous_match(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.find(false, false);
            }
        });
        let weak = Rc::downgrade(self);
        self.find_entry.connect_stop_search(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.close_find();
            }
        });
        let weak = Rc::downgrade(self);
        find_close.connect_clicked(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.close_find();
            }
        });
        let weak = Rc::downgrade(self);
        self.search.connect_occurrences_count_notify(move |_| {
            if let Some(ed) = weak.upgrade() {
                ed.show_count();
            }
        });
        let entry_keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        entry_keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(ed) = weak.upgrade() else { return glib::Propagation::Proceed };
            if mods.contains(gdk::ModifierType::SHIFT_MASK) && (key == gdk::Key::Return || key == gdk::Key::KP_Enter) {
                ed.find(false, false);
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        self.find_entry.add_controller(entry_keys);

        // a double-click on a line of the changes goes to that line
        let dbl = gtk::GestureClick::new();
        dbl.set_button(1);
        let weak = Rc::downgrade(self);
        dbl.connect_pressed(move |_, n, x, y| {
            let Some(ed) = weak.upgrade() else { return };
            if n != 2 {
                return;
            }
            let (bx, by) = ed.diff_view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
            if let Some(it) = ed.diff_view.iter_at_location(bx, by) {
                let at = ed.diff_lines.borrow().get(it.line() as usize).copied().flatten();
                if let Some(line) = at {
                    ed.changes_btn.set_active(false);
                    ed.back_to_editing();
                    ed.goto(line, None);
                }
            }
        });
        self.diff_view.add_controller(dbl);

        // keys: only ones a text editor doesn't use itself
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let Some(ed) = weak.upgrade() else { return glib::Propagation::Proceed };
            let m = mods & (gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SUPER_MASK);
            let ctrl = m == gdk::ModifierType::CONTROL_MASK;
            let ctrl_shift = m == gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK;
            match key.to_lower() {
                gdk::Key::s if ctrl => {
                    ed.save(false);
                }
                gdk::Key::f if ctrl => ed.open_find(),
                gdk::Key::w if ctrl => (ed.hooks.close)(),
                gdk::Key::p if ctrl => (ed.hooks.find_file)(),
                gdk::Key::g if ctrl => ed.find(true, false),
                gdk::Key::g if ctrl_shift => ed.find(false, false),
                gdk::Key::F3 if m.is_empty() => ed.find(true, false),
                gdk::Key::F3 if m == gdk::ModifierType::SHIFT_MASK => ed.find(false, false),
                gdk::Key::Escape if m.is_empty() => {
                    if ed.find_bar.is_visible() {
                        ed.close_find();
                    } else if ed.stack.visible_child_name().as_deref() != Some("edit") {
                        ed.back_to_editing();
                    } else {
                        return glib::Propagation::Proceed;
                    }
                }
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        self.page.add_controller(keys);

        // the file on disk: watched, and checked a moment after each change settles
        let file = gio::File::for_path(&self.path);
        if let Ok(mon) = file.monitor_file(gio::FileMonitorFlags::WATCH_MOVES, None::<&gio::Cancellable>) {
            let weak = Rc::downgrade(self);
            mon.connect_changed(move |_, _, _, event| {
                if matches!(event, gio::FileMonitorEvent::AttributeChanged | gio::FileMonitorEvent::PreUnmount | gio::FileMonitorEvent::Unmounted) {
                    return;
                }
                if let Some(ed) = weak.upgrade() {
                    ed.queue_check();
                }
            });
            *self.monitor.borrow_mut() = Some(mon);
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.buffer.is_modified()
    }

    pub fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Where the keyboard goes when the tab is shown.
    pub fn focus(&self) {
        match self.stack.visible_child_name().as_deref() {
            Some("changes") => self.diff_view.grab_focus(),
            Some("compare") => self.mine_view.grab_focus(),
            _ => self.view.grab_focus(),
        };
    }

    /// Puts the cursor on `line` (1-based) and scrolls it into the upper part of the view.
    pub fn goto(self: &Rc<Self>, line: u32, col: Option<u32>) {
        let mut it = line_iter(&self.buffer, line.saturating_sub(1) as usize);
        if let Some(c) = col {
            let end = it.chars_in_line().max(1) - 1;
            it.set_line_offset((c.saturating_sub(1) as i32).min(end.max(0)));
        }
        self.buffer.place_cursor(&it);
        let mark = self.buffer.create_mark(None, &it, true);
        let ed = self.clone();
        // after the view has its size, or the scroll goes nowhere
        glib::idle_add_local_once(move || {
            ed.view.scroll_to_mark(&mark, 0.0, true, 0.0, 0.3);
            ed.buffer.delete_mark(&mark);
            ed.view.grab_focus();
        });
    }

    /// For the off-screen pictures (`PLANE_SHOT_KEYS="ed:save"` and so on).
    pub fn shot_command(self: &Rc<Self>, cmd: &str) {
        match cmd.split_once('=') {
            Some(("find", text)) => {
                self.open_find();
                self.find_entry.set_text(text);
            }
            Some(("line", n)) => self.goto(n.parse().unwrap_or(1), None),
            // an outside edit, as an agent would make it (a whole new file renamed into place)
            Some(("outside", text)) => {
                let mut now = std::fs::read_to_string(&self.path).unwrap_or_default();
                now.push_str(&text.replace('_', " "));
                now.push('\n');
                let tmp = self.path.with_extension("outside-tmp");
                let _ = std::fs::write(&tmp, now).and_then(|_| std::fs::rename(&tmp, &self.path));
            }
            _ => match cmd {
                "save" => {
                    self.save(false);
                }
                "changes" => self.changes_btn.set_active(true),
                "compare" => self.show_compare(),
                "check" => self.check_disk(),
                "mine" => self.mine_btn.emit_clicked(),
                "theirs" => self.theirs_btn.emit_clicked(),
                "undo" => self.buffer.undo(),
                _ => {}
            },
        }
    }

    pub fn set_who(&self, text: &str) {
        self.who.set_text(text);
    }

    pub fn set_scheme(&self, colors: &HashMap<String, gdk::RGBA>) {
        let s = scheme();
        for b in [&self.buffer, &self.disk_buffer] {
            b.set_style_scheme(s.as_ref());
            if let Some(t) = b.tag_table().lookup("differs") {
                t.set_paragraph_background_rgba(Some(&crate::theme::tint(colors, "plane_warn", 0.16)));
            }
        }
        if let Some(t) = self.buffer.tag_table().lookup("landed") {
            t.set_paragraph_background_rgba(Some(&crate::theme::tint(colors, "plane_accent", 0.16)));
        }
        if let Some(b) = self.diff_view.buffer().downcast_ref::<sv::Buffer>() {
            b.set_style_scheme(s.as_ref());
        }
    }

    // ------------------------------------------------------------------ saving

    /// Saves. Without `force`, refuses if the file changed on disk since it was read or
    /// last saved, and shows the conflict instead. Returns whether it saved.
    pub fn save(self: &Rc<Self>, force: bool) -> bool {
        let text = text_of(&self.buffer);
        let stamp = self.stamp.borrow().clone();
        let expect = if force { None } else { Some(stamp.as_ref()) };
        match files::save(&self.path, &text, expect) {
            Ok(st) => {
                *self.stamp.borrow_mut() = Some(st);
                *self.theirs.borrow_mut() = None;
                self.buffer.set_modified(false);
                self.set_trouble(Trouble::None, "");
                if self.stack.visible_child_name().as_deref() == Some("compare") {
                    self.back_to_editing();
                }
                let t = glib::DateTime::now_local().ok().and_then(|d| d.format("%H:%M").ok()).map(|s| s.to_string()).unwrap_or_default();
                self.set_note(&format!("saved {t}"), "ok", None);
                self.queue_marks();
                true
            }
            Err(SaveError::Changed) => {
                match files::load(&self.path) {
                    Ok((Content::Text(t), st)) => {
                        *self.theirs.borrow_mut() = Some((t, st));
                        self.set_trouble(Trouble::Conflict, "Not saved: the file changed on disk. Your text is kept.");
                    }
                    _ => self.set_trouble(Trouble::NotText, "Not saved: the file on disk changed into something that isn't text. Your text is kept."),
                }
                false
            }
            Err(SaveError::Io(e)) => {
                self.set_trouble(Trouble::SaveFailed, &format!("Couldn't save: {e}. Your text is kept."));
                false
            }
        }
    }

    fn set_trouble(&self, t: Trouble, text: &str) {
        self.trouble.set(t);
        self.banner.set_visible(t != Trouble::None);
        self.banner_text.set_text(text);
        if matches!(t, Trouble::SaveFailed | Trouble::NotText) {
            self.banner.add_css_class("bad");
        } else {
            self.banner.remove_css_class("bad");
        }
        self.compare_btn.set_visible(t == Trouble::Conflict);
        self.theirs_btn.set_visible(t == Trouble::Conflict);
        self.mine_btn.set_visible(matches!(t, Trouble::Conflict | Trouble::NotText));
        self.retry_btn.set_visible(t == Trouble::SaveFailed);
        self.compare_btn.set_label(if self.stack.visible_child_name().as_deref() == Some("compare") { "Hide comparison" } else { "Compare" });
    }

    /// The status line beside the path. Empty text puts back the standing note.
    fn set_note(&self, text: &str, tone: &str, show_line: Option<i32>) {
        let (text, tone) = if text.is_empty() { (LIVE_NOTE, "dim") } else { (text, tone) };
        self.note.set_text(text);
        for t in ["ok", "accent", "warn", "dim"] {
            self.note.remove_css_class(t);
        }
        if !tone.is_empty() {
            self.note.add_css_class(tone);
        }
        self.landed_at.set(show_line);
        self.show_btn.set_visible(show_line.is_some());
    }

    /// Replaces your text with what's on disk (Ctrl+Z brings yours back).
    fn use_disk(self: &Rc<Self>) {
        let Some((text, st)) = self.theirs.borrow_mut().take() else { return };
        self.buffer.begin_user_action();
        let (mut s, mut e) = self.buffer.bounds();
        self.buffer.delete(&mut s, &mut e);
        self.buffer.insert(&mut self.buffer.start_iter(), &text);
        self.buffer.end_user_action();
        self.buffer.place_cursor(&self.buffer.start_iter());
        *self.stamp.borrow_mut() = Some(st);
        self.buffer.set_modified(false);
        self.set_trouble(Trouble::None, "");
        self.back_to_editing();
        self.set_note("now showing the version on disk · Ctrl+Z brings your text back", "accent", None);
    }

    // ------------------------------------------------------------------ the disk

    fn queue_check(self: &Rc<Self>) {
        if self.check_queued.replace(true) {
            return;
        }
        let ed = self.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
            ed.check_queued.set(false);
            ed.check_disk();
        });
    }

    /// Compares the disk with what the editor last read or wrote, and takes in a change if
    /// that can't cost you any text.
    pub fn check_disk(self: &Rc<Self>) {
        let known = self.stamp.borrow().clone();
        match files::load(&self.path) {
            Err(ReadError::Missing) => {
                if self.trouble.get() != Trouble::Gone {
                    self.set_trouble(Trouble::Gone, "This file was deleted or moved on disk. Your text is still here; Save puts it back.");
                    self.buffer.set_modified(true);
                    (self.hooks.changed_on_disk)();
                }
            }
            Err(ReadError::Other(_)) => {}
            Ok((content, st)) => {
                if self.trouble.get() == Trouble::Gone {
                    self.set_trouble(Trouble::None, "");
                }
                if known.as_ref() == Some(&st) {
                    // back as it was (after a delete and re-create): only "changed" if it is
                    if let Content::Text(t) = &content {
                        if self.buffer.is_modified() && *t == text_of(&self.buffer) {
                            self.buffer.set_modified(false);
                        }
                    }
                    return;
                }
                if self.theirs.borrow().as_ref().is_some_and(|(_, s)| *s == st) {
                    return; // the change already showing
                }
                (self.hooks.changed_on_disk)();
                match content {
                    Content::Text(t) => {
                        let dirty = self.buffer.is_modified();
                        let mine = text_of(&self.buffer);
                        if t == mine {
                            *self.stamp.borrow_mut() = Some(st);
                            *self.theirs.borrow_mut() = None;
                            self.buffer.set_modified(false);
                            self.set_trouble(Trouble::None, "");
                        } else if !dirty {
                            self.take_in(&mine, &t, st);
                        } else {
                            *self.theirs.borrow_mut() = Some((t, st));
                            self.set_trouble(Trouble::Conflict, "Changed on disk while you were editing. Your text is kept.");
                            if self.stack.visible_child_name().as_deref() == Some("compare") {
                                self.show_compare();
                            }
                        }
                    }
                    _ => {
                        *self.theirs.borrow_mut() = None;
                        self.set_trouble(Trouble::NotText, "The file on disk changed into something that isn't text. Your text is kept.");
                    }
                }
            }
        }
    }

    /// Takes a change from disk into an unchanged buffer, editing only the lines that
    /// changed, so the cursor and the lines you're looking at stay where they are. The new
    /// lines are tinted until the next change from disk.
    fn take_in(self: &Rc<Self>, old: &str, new: &str, st: Stamp) {
        let b = &self.buffer;
        let (s, e) = b.bounds();
        b.remove_tag_by_name("landed", &s, &e);
        // the line at the top of the view, held in place across the edit
        let rect = self.view.visible_rect();
        let top = self.view.iter_at_location(rect.x(), rect.y()).unwrap_or_else(|| b.start_iter());
        let top_line_y = self.view.line_yrange(&top).0;
        let top_offset = rect.y() - top_line_y;
        let anchor = b.create_mark(None, &top, true);
        let hunks = files::hunks(old, new);
        let mut lines = 0;
        let mut first: Option<i32> = None;
        b.begin_irreversible_action();
        for h in &hunks {
            let mut from = line_iter(b, h.old_start);
            let mut to = line_iter(b, h.old_end);
            b.delete(&mut from, &mut to);
            let start = b.create_mark(None, &from, true);
            b.insert(&mut from, &h.text);
            let s = b.iter_at_mark(&start);
            b.apply_tag_by_name("landed", &s, &from);
            b.delete_mark(&start);
            lines += h.new_len;
            first = Some(h.old_start as i32);
        }
        b.end_irreversible_action();
        b.set_modified(false);
        *self.stamp.borrow_mut() = Some(st);
        *self.theirs.borrow_mut() = None;
        self.set_trouble(Trouble::None, "");
        let t = glib::DateTime::now_local().ok().and_then(|d| d.format("%H:%M").ok()).map(|s| s.to_string()).unwrap_or_default();
        let what = if lines == 0 { "lines removed".to_owned() } else if lines == 1 { "1 line".to_owned() } else { format!("{lines} lines") };
        // hunks go in last first, so `first` is the earliest, and nothing above it moved
        self.set_note(&format!("changed on disk at {t}: {what}"), "accent", first);
        let ed = self.clone();
        glib::idle_add_local_once(move || {
            let it = ed.buffer.iter_at_mark(&anchor);
            let y = ed.view.line_yrange(&it).0 + top_offset;
            if let Some(adj) = ed.view.vadjustment() {
                adj.set_value(y as f64);
            }
            ed.buffer.delete_mark(&anchor);
        });
        if self.stack.visible_child_name().as_deref() == Some("changes") {
            self.show_changes();
        }
    }

    // ------------------------------------------------------------------ marks and views

    fn queue_marks(self: &Rc<Self>) {
        if self.marks_queued.replace(true) {
            return;
        }
        let ed = self.clone();
        glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
            ed.marks_queued.set(false);
            ed.update_marks();
            if ed.stack.visible_child_name().as_deref() == Some("compare") {
                ed.tint_compare();
            }
        });
    }

    /// "Changes · N" on the button: lines added, changed and removed, kept up to date.
    fn show_change_count(&self, n: usize, since: &str) {
        self.changes_btn.set_label(&format!("Changes · {n}"));
        let tip = match n {
            0 => format!("No lines changed since {since}"),
            1 => format!("1 line changed since {since}: click to see it"),
            n => format!("{n} lines changed since {since}: click to see them"),
        };
        self.changes_btn.set_tooltip_text(Some(&tip));
    }

    /// The bars beside the line numbers: lines added or changed since the last commit, and
    /// where lines were removed.
    fn update_marks(&self) {
        let b = &self.buffer;
        let (s, e) = b.bounds();
        for cat in [MARK_ADDED, MARK_CHANGED, MARK_REMOVED] {
            b.remove_source_marks(&s, &e, Some(cat));
        }
        let base = self.base.borrow();
        let since = if matches!(*base, Base::Head(_)) { "the last commit" } else { "you opened it (it isn't in git)" };
        if matches!(*base, Base::Opened(_)) && !b.is_modified() && text_of(b) == base.text() {
            self.show_change_count(0, since);
            return;
        }
        let m = files::line_marks(base.text(), &text_of(b));
        let removed: usize = m.removed.iter().map(|(_, n)| *n).sum();
        self.show_change_count(m.added.len() + m.changed.len() + removed, since);
        let last = (b.line_count() - 1).max(0) as usize;
        for (cat, lines) in [(MARK_ADDED, &m.added), (MARK_CHANGED, &m.changed)] {
            for &l in lines.iter() {
                b.create_source_mark(None, cat, &line_iter(b, l));
            }
        }
        for &(l, _) in &m.removed {
            b.create_source_mark(None, MARK_REMOVED, &line_iter(b, l.min(last)));
        }
    }

    fn refresh_base(&self) {
        if let Some(head) = files::head_text(&self.path) {
            *self.base.borrow_mut() = Base::Head(head);
        }
    }

    fn show_changes(self: &Rc<Self>) {
        self.refresh_base();
        let mine = text_of(&self.buffer);
        let base = self.base.borrow().clone();
        let (since, old_name) = match &base {
            Base::Head(_) => ("the last commit", "last commit"),
            Base::Opened(_) => ("you opened it (it isn't in git)", "when opened"),
        };
        let (text, lines, added, removed) = files::unified(base.text(), &mine, old_name, "now");
        let unsaved = if self.buffer.is_modified() { ", including your unsaved edits" } else { "" };
        if added == 0 && removed == 0 {
            self.diff_head.set_text(&format!("No changes since {since}{unsaved}."));
            self.diff_view.buffer().set_text("");
        } else {
            self.diff_head.set_text(&format!("Changes since {since}{unsaved}: {added} lines added, {removed} removed · double-click a line to go there"));
            self.diff_view.buffer().set_text(&text);
        }
        *self.diff_lines.borrow_mut() = lines;
        self.stack.set_visible_child_name("changes");
        if !self.changes_btn.is_active() {
            self.changes_btn.set_active(true);
        }
        self.compare_btn.set_label("Compare");
        self.diff_view.grab_focus();
    }

    fn show_compare(self: &Rc<Self>) {
        let theirs = self.theirs.borrow().as_ref().map(|(t, _)| t.clone());
        let Some(theirs) = theirs else { return };
        self.disk_buffer.set_text(&theirs);
        self.tint_compare();
        self.changes_btn.set_active(false);
        self.stack.set_visible_child_name("compare");
        self.compare_btn.set_label("Hide comparison");
        self.mine_view.grab_focus();
    }

    /// The lines that differ, tinted on both sides.
    fn tint_compare(&self) {
        let Some(theirs) = self.theirs.borrow().as_ref().map(|(t, _)| t.clone()) else { return };
        let mine = text_of(&self.buffer);
        for (buf, from, to) in [(&self.buffer, &theirs, &mine), (&self.disk_buffer, &mine, &theirs)] {
            let (s, e) = buf.bounds();
            buf.remove_tag_by_name("differs", &s, &e);
            for l in files::changed_lines(from, to) {
                let a = line_iter(buf, l);
                let z = line_iter(buf, l + 1);
                let z = if z.line() == a.line() { buf.end_iter() } else { z };
                buf.apply_tag_by_name("differs", &a, &z);
            }
        }
    }

    fn back_to_editing(&self) {
        if self.stack.visible_child_name().as_deref() == Some("compare") {
            let (s, e) = self.buffer.bounds();
            self.buffer.remove_tag_by_name("differs", &s, &e);
        }
        self.stack.set_visible_child_name("edit");
        if self.changes_btn.is_active() {
            self.changes_btn.set_active(false);
        }
        if self.trouble.get() == Trouble::Conflict {
            self.compare_btn.set_label("Compare");
        }
        self.view.grab_focus();
    }

    // ------------------------------------------------------------------ find

    fn open_find(self: &Rc<Self>) {
        if self.stack.visible_child_name().as_deref() != Some("edit") {
            self.back_to_editing();
        }
        if let Some((s, e)) = self.buffer.selection_bounds() {
            if s.line() == e.line() {
                self.find_entry.set_text(&self.buffer.text(&s, &e, false));
            }
        }
        self.find_bar.set_visible(true);
        self.search.set_highlight(true);
        self.find_entry.grab_focus();
        self.find_entry.select_region(0, -1);
        self.show_count();
    }

    fn close_find(&self) {
        self.find_bar.set_visible(false);
        self.search.set_highlight(false);
        self.view.grab_focus();
    }

    /// Moves to the next (or previous) match. `from_start_of_selection` keeps the current
    /// match while you're still typing the search.
    fn find(&self, forward: bool, from_start_of_selection: bool) {
        if self.settings.search_text().is_none_or(|t| t.is_empty()) {
            self.show_count();
            return;
        }
        let (s, e) = self.buffer.selection_bounds().unwrap_or_else(|| {
            let c = self.buffer.iter_at_mark(&self.buffer.get_insert());
            (c, c)
        });
        let found = if forward { self.search.forward(if from_start_of_selection { &s } else { &e }) } else { self.search.backward(&s) };
        if let Some((a, z, _)) = found {
            self.buffer.select_range(&a, &z);
            self.view.scroll_to_iter(&mut a.clone(), 0.0, true, 0.0, 0.3);
        }
        self.show_count();
    }

    fn show_count(&self) {
        if self.settings.search_text().is_none_or(|t| t.is_empty()) {
            self.find_count.set_text("");
            return;
        }
        let n = self.search.occurrences_count();
        if n < 0 {
            self.find_count.set_text("…");
            return;
        }
        if n == 0 {
            self.find_count.set_text("no matches");
            return;
        }
        let pos = self.buffer.selection_bounds().map(|(a, z)| self.search.occurrence_position(&a, &z)).unwrap_or(-1);
        self.find_count.set_text(&if pos > 0 { format!("{pos} of {n}") } else { format!("{n} matches") });
    }
}

/// A tab for a file that can't be shown as text: says why, and offers another app.
pub fn message_page(path: &Path, shown: &str, why: &str) -> gtk::Widget {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 8);
    b.add_css_class("file-message");
    b.set_valign(gtk::Align::Fill);
    let inner = gtk::Box::new(gtk::Orientation::Vertical, 8);
    inner.set_valign(gtk::Align::Center);
    inner.set_vexpand(true);
    inner.set_halign(gtk::Align::Center);
    let big = gtk::Label::new(Some(shown));
    big.add_css_class("big");
    big.set_wrap(true);
    let small = gtk::Label::new(Some(why));
    small.set_wrap(true);
    small.set_justify(gtk::Justification::Center);
    let open = gtk::Button::with_label("Open with another app");
    open.set_halign(gtk::Align::Center);
    let file = gio::File::for_path(path);
    open.connect_clicked(move |btn| {
        let win = btn.root().and_downcast::<gtk::Window>();
        gtk::FileLauncher::new(Some(&file)).launch(win.as_ref(), None::<&gio::Cancellable>, |_| {});
    });
    inner.append(&big);
    inner.append(&small);
    inner.append(&open);
    b.append(&inner);
    b.set_focusable(true);
    b.upcast()
}

/// Words for a file that won't open as text.
pub fn why_not_text(c: &Content) -> String {
    match c {
        Content::Binary => "This looks like a binary file (an image, an archive or a program), so it isn't shown as text.".into(),
        Content::TooBig(n) => format!("This file is too big to open here ({:.1} MB). Plane opens files up to {} MB.", *n as f64 / 1_048_576.0, files::MAX_BYTES / 1_048_576),
        Content::NotUtf8 => "This file isn't UTF-8 text, so it isn't shown here (editing it could garble it).".into(),
        Content::Text(_) => String::new(),
    }
}
