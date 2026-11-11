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
    glib::timeout_add_local_once(std::time::Duration::from_secs_f64(after), move || {
        let (width, height) = (w.width(), w.height());
        let paintable = gtk::WidgetPaintable::new(Some(&w));
        let snap = gtk::Snapshot::new();
        paintable.snapshot(&snap, width as f64, height as f64);
        if let Some(node) = snap.to_node() {
            let renderer = gsk::CairoRenderer::new();
            if renderer.realize_for_display(&gdk::Display::default().unwrap()).is_ok() {
                let tex = renderer.render_texture(&node, Some(&gtk::graphene::Rect::new(0.0, 0.0, width as f32, height as f32)));
                let _ = tex.save_to_png(&path);
                renderer.unrealize();
            }
        }
        w.close();
    });
}
