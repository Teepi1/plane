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
list.sessions row { padding: 3px 10px 3px 7px; min-height: 30px; border-left: 3px solid transparent; color: @plane_fg; font-size: 1em; }
list.sessions row:selected { background: @term_0; border-left-color: @plane_accent; }
list.sessions row:selected .name { font-weight: bold; }
list.sessions row.note { color: @plane_dim; min-height: 26px; }
list.sessions row .icon { font-weight: bold; min-width: 14px; }
list.sessions row .shown { color: @plane_accent; min-width: 10px; }
list.sessions row.dimmed .name, list.sessions row.dimmed .state { color: @plane_dim; }
list.sessions row.urgent .name { font-weight: bold; }
list.sessions row .state { font-size: 0.9em; }
/* section headings: real rows with a chevron, a count and a caption */
list.sessions row.head-row { min-height: 32px; margin-top: 8px; padding-left: 9px; }
list.sessions row.head-row:first-child { margin-top: 2px; }
list.sessions row.head-row .title { font-weight: bold; font-size: 1em; }
list.sessions row.head-row .chevron { color: @plane_dim; min-width: 12px; }
list.sessions row.head-row.warn .title, list.sessions row.head-row.warn .chevron { color: @plane_warn; }
list.sessions row.head-row .count { font-size: 0.85em; font-weight: bold; color: @plane_fg; background: alpha(@plane_dim, 0.22); border-radius: 9px; padding: 0 7px; min-width: 8px; }
list.sessions row.head-row .caption { color: @plane_dim; font-size: 0.9em; }
/* states: \"needs you\" is the one filled pill */
.pill { font-size: 0.85em; font-weight: bold; border-radius: 10px; padding: 1px 8px; }
.pill.pill-needs { background: @plane_warn; color: @plane_bg; }
.pill.pill-turn { border: 1px solid @plane_accent; color: @plane_accent; padding: 0 7px; }
/* the header counts, as buttons */
button.count-btn { padding: 1px 8px; min-height: 20px; border-radius: 10px; font-weight: bold; font-size: 0.85em; background: transparent; border: 1px solid transparent; box-shadow: none; }
button.count-btn.needs { background: @plane_warn; color: @plane_bg; }
button.count-btn.turn { color: @plane_accent; border-color: @plane_accent; }
/* Sessions | Files, and + New */
.switch-row { padding: 8px 10px 6px 10px; }
.switcher button { min-height: 32px; padding: 0 10px; font-weight: bold; color: @plane_dim; background: @term_0; border: 1px solid @plane_outline; box-shadow: none; }
.switcher button:first-child { border-radius: 7px 0 0 7px; }
.switcher button:last-child { border-radius: 0 7px 7px 0; border-left-width: 0; }
.switcher button:checked { color: @plane_fg; background: alpha(@plane_accent, 0.22); border-color: @plane_accent; box-shadow: inset 0 -3px @plane_accent; }
.switch-row button.new { min-height: 32px; padding: 0 12px; border-radius: 7px; font-weight: bold; color: @plane_accent; background: @term_0; border: 1px solid @plane_outline; box-shadow: none; }

/* hover and press, everywhere you can click (\"Bigger\"): a tint, a lift with a shadow and
   an accent outline on hover; a dip on press. Dense list rows don't move up, so the list
   doesn't jitter. */
.sidebar button, .files-page button, .file button, .file-message button, .empty button, notebook > header tabs > tab, popover.files-pop button, popover.name-pop button {
  transition: background-color 120ms ease-out, box-shadow 120ms ease-out, transform 120ms ease-out, outline-color 120ms ease-out, color 120ms ease-out;
  outline: 1px solid transparent; outline-offset: -1px;
}
.sidebar button:hover, .files-page button:hover, .file button:hover, .file-message button:hover, .empty button:hover, notebook > header tabs > tab:hover, popover.files-pop button:hover, popover.name-pop button:hover {
  outline-color: @plane_accent; box-shadow: 0 3px 8px alpha(black, 0.45); transform: translateY(-1px);
}
.sidebar button:hover:not(.count-btn):not(:checked), .files-page button:hover, notebook > header tabs > tab:hover:not(:checked) { background-color: mix(@term_0, @plane_fg, 0.1); }
.sidebar button:active, .files-page button:active, .file button:active, .file-message button:active, .empty button:active, notebook > header tabs > tab:active, popover.files-pop button:active, popover.name-pop button:active {
  transform: scale(0.97); box-shadow: none; background-color: mix(@term_0, black, 0.3);
}
list.sessions row, .files-page listview > row {
  transition: background-color 120ms ease-out, box-shadow 120ms ease-out, outline-color 120ms ease-out;
  outline: 1px solid transparent; outline-offset: -1px;
}
list.sessions row:hover:not(.note), .files-page listview > row:hover {
  background-color: mix(@term_0, @plane_fg, 0.06); outline-color: alpha(@plane_accent, 0.75); box-shadow: 0 2px 6px alpha(black, 0.35);
}
list.sessions row:selected:hover { background-color: mix(@term_0, @plane_fg, 0.1); }
list.sessions row.pressed, .files-page listview > row.pressed, .files-page row .pressed { background-color: mix(@term_0, black, 0.3); box-shadow: none; }
.more-above, .more-below { font-size: 0.85em; padding: 0 12px; }
.details { padding: 6px 12px 4px 12px; border-top: 1px solid @plane_outline; min-height: 118px; }
.details .name { font-weight: bold; font-size: 1.05em; }
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
.file { background: alpha(@term_bg, 0.8); color: @plane_fg; }
.file .bar { padding: 3px 8px 3px 14px; border-bottom: 1px solid @plane_outline; min-height: 28px; }
.file .bar button { padding: 2px 10px; min-height: 22px; }
.file .bar .path { color: @plane_dim; }
.file .banner { padding: 6px 14px; background: alpha(@plane_warn, 0.14); border-bottom: 1px solid alpha(@plane_warn, 0.5); }
.file .banner.bad { background: alpha(@plane_bad, 0.14); border-bottom-color: alpha(@plane_bad, 0.5); }
.file .banner label.text { color: @plane_fg; font-weight: bold; }
.file .banner button { padding: 2px 10px; min-height: 22px; }
.file .findbar { padding: 4px 14px; border-bottom: 1px solid @plane_outline; }
.file .findbar entry { min-width: 260px; background: @term_0; color: @plane_fg; }
.file .side-head { padding: 4px 14px; color: @plane_dim; font-weight: bold; font-size: 0.9em; border-bottom: 1px solid @plane_outline; }
.file .changes-head { padding: 6px 14px; border-bottom: 1px solid @plane_outline; }
textview.editor, textview.editor text { background: transparent; font-family: monospace; font-size: 11pt; }
textview.editor { padding: 0; }
textview.editor gutter, textview.editor border { background: transparent; background-color: transparent; }
.file button, .file-message button, notebook > header button.files {
  background: @term_0; color: @plane_fg; border: 1px solid @plane_outline; box-shadow: none; background-image: none; text-shadow: none;
}
.file button:hover, .file-message button:hover, notebook > header button.files:hover { background: mix(@term_0, @plane_fg, 0.08); }
.file button:checked { background: alpha(@plane_accent, 0.25); border-color: @plane_accent; }
.file button.suggested-action { background: @plane_primary; color: @plane_on_primary; border-color: @plane_primary; font-weight: bold; }
.file button:disabled { background: transparent; color: @plane_dim; border-color: transparent; }
.file button.flat { background: transparent; border-color: transparent; color: @plane_accent; }
notebook > header button.files { margin: 3px 8px; padding: 1px 12px; min-height: 20px; }
.file .findbar button { padding: 2px 6px; }
.files-page .strike { text-decoration: line-through; color: @plane_dim; }
textview.editor border.left gutter { padding-right: 6px; }
.file-message { background: alpha(@term_bg, 0.8); color: @plane_dim; }
.file-message .big { font-size: 1.15em; color: @plane_fg; }
.files-page .where { color: @plane_dim; }
.files-page listview, .files-page row { background: transparent; color: @plane_fg; }
.files-page row { padding: 2px 6px; min-height: 22px; }
.files-page row:selected { background: @term_0; }
.files-page .head { color: @plane_fg; font-size: 0.85em; font-weight: bold; letter-spacing: 1px; padding: 10px 0 3px 0; }
.files-page .note { color: @plane_dim; font-size: 0.9em; }
.files-page .ignored { color: @plane_dim; }
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
    // off-screen pictures: broadway with no browser never runs a transition to its end
    // and has no pointer, so the hover look is pictured through a class (`hover:` keys)
    let shot = if std::env::var_os("PLANE_SHOT").is_some() { format!("* {{ transition: none; }}\n{}", APP_CSS.replace(":hover", ".shot-hover")) } else { String::new() };
    format!("{defines}\n{APP_CSS}\n{shot}")
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

/// `a` moved toward `b` by `t` (0..1), as #rrggbb: source-view styles want solid colors.
fn mix(a: &gdk::RGBA, b: &gdk::RGBA, t: f32) -> String {
    let m = |x: f32, y: f32| ((x + (y - x) * t).clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", m(a.red(), b.red()), m(a.green(), b.green()), m(a.blue(), b.blue()))
}

fn hex(c: &gdk::RGBA) -> String {
    mix(c, c, 0.0)
}

/// A GtkSourceView style scheme ("plane") in the theme's colors. It sets no text background,
/// so the editor keeps the app's translucent one from CSS.
pub fn source_scheme(colors: &HashMap<String, gdk::RGBA>) -> String {
    let c = |k: &str| colors.get(k).copied().unwrap_or(gdk::RGBA::WHITE);
    let (bg, fg, dim) = (c("term_bg"), c("term_fg"), c("plane_dim"));
    let styles = [
        ("text", format!("foreground=\"{}\"", hex(&fg))),
        ("selection", format!("foreground=\"{}\" background=\"{}\"", hex(&c("term_sel_fg")), hex(&c("term_sel_bg")))),
        ("selection-unfocused", format!("background=\"{}\"", mix(&bg, &c("term_sel_bg"), 0.6))),
        ("cursor", format!("foreground=\"{}\"", hex(&c("term_cursor")))),
        ("current-line", format!("background=\"{}\"", mix(&bg, &c("term_0"), 0.9))),
        ("current-line-number", format!("foreground=\"{}\" bold=\"true\"", hex(&fg))),
        ("line-numbers", format!("foreground=\"{}\"", mix(&bg, &dim, 0.75))),
        ("bracket-match", format!("background=\"{}\" bold=\"true\"", mix(&bg, &c("plane_accent"), 0.3))),
        ("search-match", format!("foreground=\"{}\" background=\"{}\"", hex(&fg), mix(&bg, &c("plane_warn"), 0.35))),
        ("def:comment", format!("foreground=\"{}\" italic=\"true\"", hex(&dim))),
        ("def:doc-comment-element", format!("foreground=\"{}\" italic=\"true\" bold=\"true\"", hex(&dim))),
        ("def:string", format!("foreground=\"{}\"", hex(&c("term_2")))),
        ("def:special-char", format!("foreground=\"{}\"", hex(&c("term_10")))),
        ("def:keyword", format!("foreground=\"{}\"", hex(&c("term_5")))),
        ("def:statement", format!("foreground=\"{}\"", hex(&c("term_5")))),
        ("def:type", format!("foreground=\"{}\"", hex(&c("term_6")))),
        ("def:function", format!("foreground=\"{}\"", hex(&c("term_4")))),
        ("def:constant", format!("foreground=\"{}\"", hex(&c("term_3")))),
        ("def:number", format!("foreground=\"{}\"", hex(&c("term_3")))),
        ("def:boolean", format!("foreground=\"{}\"", hex(&c("term_3")))),
        ("def:preprocessor", format!("foreground=\"{}\"", hex(&c("term_1")))),
        ("def:builtin", format!("foreground=\"{}\"", hex(&c("term_6")))),
        ("def:identifier", format!("foreground=\"{}\"", hex(&fg))),
        ("def:heading", format!("foreground=\"{}\" bold=\"true\"", hex(&c("plane_accent")))),
        ("def:emphasis", "italic=\"true\"".to_owned()),
        ("def:strong-emphasis", "bold=\"true\"".to_owned()),
        ("def:link-text", format!("foreground=\"{}\" underline=\"single\"", hex(&c("term_4")))),
        ("def:error", format!("foreground=\"{}\" underline=\"error\"", hex(&c("plane_bad")))),
        ("diff:added-line", format!("foreground=\"{}\"", hex(&c("plane_ok")))),
        ("diff:removed-line", format!("foreground=\"{}\"", hex(&c("plane_bad")))),
        ("diff:changed-line", format!("foreground=\"{}\"", hex(&c("plane_warn")))),
        ("diff:location", format!("foreground=\"{}\" bold=\"true\"", hex(&c("plane_accent")))),
        ("diff:diff-file", format!("foreground=\"{}\" bold=\"true\"", hex(&dim))),
        ("diff:special-case", format!("foreground=\"{}\"", hex(&dim))),
    ];
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<style-scheme id=\"plane\" name=\"Plane\" version=\"1.0\">\n");
    for (name, attrs) in styles {
        xml.push_str(&format!("  <style name=\"{name}\" {attrs}/>\n"));
    }
    xml.push_str("</style-scheme>\n");
    xml
}

/// A theme color moved toward the terminal background, solid: tints for marked lines.
pub fn tint(colors: &HashMap<String, gdk::RGBA>, key: &str, t: f32) -> gdk::RGBA {
    let bg = colors.get("term_bg").copied().unwrap_or(gdk::RGBA::BLACK);
    let c = colors.get(key).copied().unwrap_or(gdk::RGBA::WHITE);
    let m = |x: f32, y: f32| x + (y - x) * t;
    gdk::RGBA::new(m(bg.red(), c.red()), m(bg.green(), c.green()), m(bg.blue(), c.blue()), 1.0)
}
