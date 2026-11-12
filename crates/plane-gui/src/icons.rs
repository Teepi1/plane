//! The status icons in the list: one small picture per state, so a row needs no state word
//! and the name gets the room. Each is a one-color "symbolic" SVG, so GTK paints it in the
//! row's CSS color (the theme's green, warm, accent or dim) and it follows a theme switch.
//! Each state has its own shape too, so color is never the only sign.
//!
//! They're written to the app's runtime folder at start (nothing under your home changes)
//! and found through the icon theme as `plane-state-NAME-symbolic`.

use gtk::gdk;

/// (name, 16×16 SVG body). Fill only, no strokes: GTK recolors fills.
const ICONS: &[(&str, &str)] = &[
    // working: a ring, half filled, as in progress
    ("working", r#"<path fill-rule="evenodd" d="M8 1a7 7 0 1 0 0 14A7 7 0 0 0 8 1zm0 1.6a5.4 5.4 0 1 1 0 10.8A5.4 5.4 0 0 1 8 2.6z"/><path d="M8 4.4a3.6 3.6 0 0 1 0 7.2z"/>"#),
    // needs you: a solid disc with an exclamation mark cut out (the boldest of them)
    ("needs", r#"<path fill-rule="evenodd" d="M8 .5a7.5 7.5 0 1 0 0 15A7.5 7.5 0 0 0 8 .5zM6.9 3.9a1.1 1.1 0 0 1 2.2 0v4.8a1.1 1.1 0 0 1-2.2 0zM8 10.5a1.25 1.25 0 1 1 0 2.5 1.25 1.25 0 0 1 0-2.5z"/>"#),
    // your turn: a speech bubble waiting for your words
    ("turn", r#"<path fill-rule="evenodd" d="M3 1.5h10A2 2 0 0 1 15 3.5v6.5a2 2 0 0 1-2 2H8l-3.6 2.9a.4.4 0 0 1-.65-.3V12H3a2 2 0 0 1-2-2V3.5a2 2 0 0 1 2-2zm0 1.5a.5.5 0 0 0-.5.5v6.5a.5.5 0 0 0 .5.5h2.25v1.6l2-1.6H13a.5.5 0 0 0 .5-.5V3.5A.5.5 0 0 0 13 3z"/><path d="M4.75 5.25h6.5v1.5h-6.5zM4.75 7.75h4v1.5h-4z" opacity=".75"/>"#),
    // paused: two bars
    ("paused", r#"<path d="M4 2.5h2.6v11H4zM9.4 2.5H12v11H9.4z"/>"#),
    // failed: a disc with a cross cut out
    ("failed", r#"<path fill-rule="evenodd" d="M8 .5a7.5 7.5 0 1 0 0 15A7.5 7.5 0 0 0 8 .5zM5.3 4.2 8 6.9l2.7-2.7 1.1 1.1L9.1 8l2.7 2.7-1.1 1.1L8 9.1l-2.7 2.7-1.1-1.1L6.9 8 4.2 5.3z"/>"#),
    // anything else: a plain ring
    ("idle", r#"<path fill-rule="evenodd" d="M8 1a7 7 0 1 0 0 14A7 7 0 0 0 8 1zm0 1.6a5.4 5.4 0 1 1 0 10.8A5.4 5.4 0 0 1 8 2.6z"/>"#),
    // cut off by a crash or a reboot: a lightning bolt
    ("crash", r#"<path d="M9.8.8 2.6 9.2h4.6l-1.3 6 7.5-8.9H8.6z"/>"#),
    // a handoff ready to start: an arrow
    ("ready", r#"<path d="M1.5 7.1h9.4L7.4 3.6l1.3-1.3L14.4 8l-5.7 5.7-1.3-1.3 3.5-3.5H1.5z"/>"#),
    // a handoff still being written: an hourglass
    ("waiting", r#"<path d="M3.5 1h9v1.6h-1v1.6c0 1.3-.7 2.5-1.8 3.2L8.9 8l.8.6c1.1.7 1.8 1.9 1.8 3.2v1.6h1V15h-9v-1.6h1v-1.6c0-1.3.7-2.5 1.8-3.2L7.1 8l-.8-.6C5.2 6.7 4.5 5.5 4.5 4.2V2.6h-1zm2.6 1.6v1.6c0 .8.4 1.5 1 1.9L8 6.7l.9-.6c.6-.4 1-1.1 1-1.9V2.6z"/>"#),
];

/// The icon theme name for a state icon.
pub fn name(state: &str) -> String {
    format!("plane-state-{state}-symbolic")
}

/// Writes the icons where the icon theme finds them. Call once, after the display opens.
pub fn install() {
    let dir = plane_core::control::socket_path().parent().map(|p| p.join("plane-next-icons")).unwrap_or_else(|| std::env::temp_dir().join("plane-next-icons"));
    let _ = std::fs::create_dir_all(&dir);
    for (n, body) in ICONS {
        let svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16">{body}</svg>"#);
        let _ = std::fs::write(dir.join(format!("{}.svg", name(n))), svg);
    }
    if let Some(display) = gdk::Display::default() {
        let theme = gtk::IconTheme::for_display(&display);
        theme.add_search_path(&dir);
    }
}
