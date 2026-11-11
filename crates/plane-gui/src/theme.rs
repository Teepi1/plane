//! Colors: from the Noctalia user template's output, `~/.config/plane/theme.css`, so a theme
//! switch reaches the app (it watches the file) and its terminals. Tidewater's values are the
//! fallback when the file isn't there yet.

use std::collections::HashMap;
use std::path::PathBuf;

use gtk::gdk;

/// Tidewater, as the template renders it.
const FALLBACK: &str = "
@define-color plane_bg #0c111a;
@define-color plane_bg_alt #151c28;
@define-color plane_fg #f2f5f9;
@define-color plane_dim #8c99ad;
@define-color plane_outline #263247;
@define-color plane_primary #3a9be8;
@define-color plane_on_primary #04121f;
@define-color plane_accent #5ccff0;
@define-color plane_ok #4ed6a0;
@define-color plane_warn #f5c86a;
@define-color plane_bad #ff5c6c;
@define-color term_bg #0a0e16;
@define-color term_fg #f2f5f9;
@define-color term_cursor #5ccff0;
@define-color term_cursor_text #0a0e16;
@define-color term_sel_bg #1f4a73;
@define-color term_sel_fg #f2f5f9;
@define-color term_0 #151c28;
@define-color term_1 #ff5c6c;
@define-color term_2 #4ed6a0;
@define-color term_3 #f5c86a;
@define-color term_4 #3a9be8;
@define-color term_5 #e58ab8;
@define-color term_6 #5ccff0;
@define-color term_7 #c9d2de;
@define-color term_8 #5a6880;
@define-color term_9 #ff7f8b;
@define-color term_10 #72e3b5;
@define-color term_11 #ffd88a;
@define-color term_12 #62b2f0;
@define-color term_13 #f0a6cb;
@define-color term_14 #86ddf5;
@define-color term_15 #f2f5f9;
";

/// The app's look, in terms of the colors above.
const APP_CSS: &str = "
window.plane { background: transparent; }
.sidebar { background: alpha(@term_bg, 0.8); color: @plane_fg; }
.sidebar .brand { color: @plane_accent; font-weight: bold; letter-spacing: 2px; }
.dim { color: @plane_dim; }
.ok { color: @plane_ok; }
.warn { color: @plane_warn; }
.bad { color: @plane_bad; }
.accent { color: @plane_accent; }
.strong { font-weight: bold; }
.head { padding: 10px 12px 2px 12px; }
.usage-row { padding: 1px 12px; }
.usage-row .label { font-weight: bold; min-width: 22px; }
progressbar.usage, progressbar.usage.horizontal > trough, progressbar.usage.horizontal > trough > progress { min-width: 30px; }
progressbar.usage trough { margin: 0; min-height: 6px; background: alpha(@plane_dim, 0.22); border-radius: 3px; border: none; }
progressbar.usage progress { margin: 0; min-width: 0; min-height: 6px; border-radius: 3px; background: @plane_ok; border: none; }
progressbar.usage.warn progress { background: @plane_warn; }
progressbar.usage.bad progress { background: @plane_bad; }
scrolledwindow.list, list.sessions { background: transparent; }
list.sessions row { padding: 3px 10px 3px 7px; min-height: 26px; border-left: 3px solid transparent; color: @plane_fg; outline: none; }
list.sessions row:selected { background: @term_0; border-left-color: @plane_accent; }
list.sessions row:selected .name { font-weight: bold; }
list.sessions row:hover:not(:selected) { background: alpha(@term_0, 0.55); }
list.sessions row.note { color: @plane_dim; min-height: 22px; }
list.sessions row .icon { font-weight: bold; min-width: 14px; }
list.sessions row .shown { color: @plane_accent; min-width: 10px; }
list.sessions row.dimmed .name, list.sessions row.dimmed .state { color: @plane_dim; }
list.sessions row.urgent .name { font-weight: bold; }
.section { color: @plane_fg; font-size: 0.85em; font-weight: bold; letter-spacing: 1px; padding: 12px 12px 3px 12px; }
.section.first { padding-top: 4px; }
.section.warn { color: @plane_warn; }
.more-above, .more-below { font-size: 0.85em; padding: 0 12px; }
.details { padding: 6px 12px 4px 12px; border-top: 1px solid @plane_outline; min-height: 118px; }
.details .name { font-weight: bold; }
.message { padding: 2px 12px; min-height: 22px; }
.confirm { padding: 4px 12px; }
.confirm .question { color: @plane_warn; font-weight: bold; }
.hints { padding: 2px 12px 8px 12px; color: @plane_dim; }
.page { padding: 8px 12px; }
.page .step { font-size: 0.85em; letter-spacing: 1px; color: @plane_dim; }
.page .ask { color: @plane_accent; font-weight: bold; font-size: 1.05em; }
.page entry { background: @term_0; color: @plane_fg; border: 1px solid @plane_outline; }
.page entry:focus-within { border-color: @plane_accent; }
.page .error { color: @plane_bad; }
.keys .key { font-weight: bold; }
.keys .heading { color: @plane_accent; font-weight: bold; font-size: 0.85em; letter-spacing: 1px; padding-top: 8px; }
paned > separator { background: @plane_outline; background-image: none; min-width: 1px; margin: 0; }
notebook > header { background: alpha(@term_bg, 0.8); border: none; box-shadow: none; }
notebook > header tabs > tab { background: transparent; color: @plane_dim; padding: 4px 6px 4px 12px; min-height: 22px; border: none; }
notebook > header tabs > tab:checked { background: alpha(@term_0, 0.9); color: @plane_fg; box-shadow: inset 0 -2px @plane_accent; }
notebook > header tabs > tab:hover:not(:checked) { background: alpha(@term_0, 0.5); }
notebook > header tabs > tab button { padding: 0; min-width: 18px; min-height: 18px; }
notebook > stack { background: transparent; }
vte-terminal { padding: 25px; }
.empty { background: alpha(@term_bg, 0.8); color: @plane_dim; }
.empty .big { font-size: 1.15em; color: @plane_fg; }
textview.prompt, textview.prompt text { background: alpha(@term_bg, 0.8); color: @plane_fg; }
textview.prompt { padding: 25px; font-family: monospace; }
popover.menu contents { background: @plane_bg_alt; color: @plane_fg; }
";

pub fn theme_path() -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| plane_core::Env::from_env().home.join(".config"));
    config.join("plane/theme.css")
}

/// The `@define-color` lines in use: the rendered template, or the fallback.
pub fn defines() -> String {
    match std::fs::read_to_string(theme_path()) {
        Ok(text) if text.contains("@define-color plane_bg") => text,
        _ => FALLBACK.to_owned(),
    }
}

pub fn css(defines: &str) -> String {
    format!("{defines}\n{APP_CSS}")
}

/// name -> color, from `@define-color name #rrggbb;` lines.
pub fn colors(defines: &str) -> HashMap<String, gdk::RGBA> {
    let mut out = HashMap::new();
    for text in [FALLBACK, defines] {
        for line in text.lines() {
            let Some(rest) = line.trim().strip_prefix("@define-color ") else { continue };
            let mut parts = rest.trim_end_matches(';').split_whitespace();
            if let (Some(name), Some(value)) = (parts.next(), parts.next()) {
                if let Ok(c) = gdk::RGBA::parse(value) {
                    out.insert(name.to_owned(), c);
                }
            }
        }
    }
    out
}
