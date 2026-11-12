//! For checking the look without a screen: with `PLANE_SHOT=file.png`, the app saves a
//! picture of its window after `PLANE_SHOT_AFTER` seconds (default 4) and quits. Run it
//! under GTK's broadway backend so nothing appears on the desktop:
//!
//!     gtk4-broadwayd :5 &
//!     GDK_BACKEND=broadway BROADWAY_DISPLAY=:5 PLANE_SHOT=/tmp/plane.png plane-gui
//!
//! `PLANE_SHOT_KEYS` types keys into the sidebar first (`j`, `?`, `n`, ...), one every
//! half second, to picture other states.

use gtk::prelude::*;
use gtk::{gdk, glib, gsk};

fn popovers(w: &gtk::Widget, out: &mut Vec<gtk::Popover>) {
    let mut c = w.first_child();
    while let Some(ch) = c {
        if let Some(p) = ch.downcast_ref::<gtk::Popover>() {
            if p.is_visible() {
                out.push(p.clone());
            }
        }
        popovers(&ch, out);
        c = ch.next_sibling();
    }
}

pub fn arm(window: &gtk::ApplicationWindow, press: std::rc::Rc<dyn Fn(&str)>) {
    let Some(path) = std::env::var_os("PLANE_SHOT") else { return };
    let after: f64 = std::env::var("PLANE_SHOT_AFTER").ok().and_then(|s| s.parse().ok()).unwrap_or(4.0);
    let keys: Vec<String> = std::env::var("PLANE_SHOT_KEYS").map(|s| s.split_whitespace().map(str::to_owned).collect()).unwrap_or_default();
    let n = keys.len();
    for (i, k) in keys.into_iter().enumerate() {
        let delay = std::time::Duration::from_secs_f64((after - 0.5 * (n - i) as f64).max(0.1));
        let p = press.clone();
        glib::timeout_add_local_once(delay, move || p(&k));
    }
    let w = window.clone();
    let late = std::env::var("PLANE_DUMP_LATE").ok(); // a `dump:` right before the picture
    glib::timeout_add_local_once(std::time::Duration::from_secs_f64(after), move || {
        if let Some(f) = &late {
            press(&format!("dump:{f}"));
        }
        let (width, height) = (w.width(), w.height());
        let content = w.child().unwrap_or_else(|| w.clone().upcast());

        let snap = gtk::Snapshot::new();
        // broadway with no browser watching stops laying out and drawing: do both here
        content.allocate(width, height, -1, None);
        w.snapshot_child(&content, &snap);
        // open popovers (menus, the folder list, the name box) are surfaces of their own:
        // drawn on top, under the place they point at
        let mut pops = vec![];
        popovers(content.upcast_ref(), &mut pops);
        for p in pops {
            let Some(parent) = p.parent() else { continue };
            let rect = p.pointing_to().1;
            let Some(at) = parent.compute_point(&content, &gtk::graphene::Point::new((rect.x() + rect.width() / 2) as f32, (rect.y() + rect.height()) as f32)) else { continue };
            let (pw, ph) = if p.width() > 0 { (p.width(), p.height()) } else { (p.measure(gtk::Orientation::Horizontal, -1).1, p.measure(gtk::Orientation::Vertical, -1).1) };
            let x = (at.x() - pw as f32 / 2.0).clamp(0.0, (width - pw).max(0) as f32);
            let paint = gtk::WidgetPaintable::new(Some(&p));
            snap.save();
            snap.translate(&gtk::graphene::Point::new(x, at.y()));
            paint.snapshot(&snap, pw as f64, ph as f64);
            snap.restore();
        }
        match snap.to_node() {
            None => eprintln!("plane shot: nothing drawn"),
            Some(node) => {
                let renderer = gsk::CairoRenderer::new();
                match renderer.realize_for_display(&gdk::Display::default().unwrap()) {
                    Err(e) => eprintln!("plane shot: no renderer: {e}"),
                    Ok(()) => {
                        let tex = renderer.render_texture(&node, Some(&gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32)));
                        if let Err(e) = tex.save_to_png(&path) {
                            eprintln!("plane shot: can't save: {e}");
                        }
                        renderer.unrealize();
                    }
                }
            }
        }
        w.close();
    });
}
