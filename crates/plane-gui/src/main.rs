//! Plane as a native app: the sidebar beside tabs of built-in terminals.
//!
//! Runs beside the kitty version until the switch (step 7): its window class is
//! `plane-next`, it sends no notifications, and its refreshes write nothing. Only the
//! actions you take write, the same way the Python sidebar's do (`tools/actions_check.py`).
//!
//! One copy runs at a time: a second start asks the first to come forward, through the
//! control socket that `plane-next open` and `plane-next focus` (Super+arrows) also use.

mod app;
mod shot;
mod store;
mod term;
mod text;
mod theme;

use std::io::{BufRead, BufReader};
use std::os::unix::net::UnixListener;
use std::sync::{Arc, Mutex};

use gtk::prelude::*;
use gtk::{gio, glib};
use plane_core::control;

/// Window class while this runs beside the kitty version (it becomes `plane` at the switch).
const CLASS: &str = "plane-next";

fn main() -> glib::ExitCode {
    if control::send("raise").is_some() {
        return glib::ExitCode::SUCCESS; // already running: it came forward
    }
    let path = control::socket_path();
    let _ = std::fs::remove_file(&path); // left behind by a copy that crashed
    let commands: Arc<Mutex<Vec<(String, std::os::unix::net::UnixStream)>>> = Arc::default();
    if let Ok(listener) = UnixListener::bind(&path) {
        let queue = commands.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut line = String::new();
                let Ok(reader) = stream.try_clone() else { continue };
                if BufReader::new(reader).read_line(&mut line).is_ok() {
                    queue.lock().unwrap().push((line, stream)); // answered on the main thread
                }
            }
        });
    }

    glib::set_prgname(Some(CLASS));
    glib::set_application_name("Plane");
    let app = gtk::Application::builder().flags(gio::ApplicationFlags::NON_UNIQUE).build();
    let keep: std::rc::Rc<std::cell::RefCell<Option<std::rc::Rc<app::App>>>> = Default::default();
    let k = keep.clone();
    app.connect_activate(move |a| {
        *k.borrow_mut() = Some(app::App::build(a, commands.clone()));
    });
    let code = app.run_with_args(&["plane-gui"]);
    let _ = std::fs::remove_file(&path);
    code
}
