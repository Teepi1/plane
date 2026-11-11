//! Small text helpers, ported from the Python sidebar.

use gtk::glib;
use serde_json::Value;

pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

/// "just now", "5m ago", "3h ago", "2d ago".
pub fn ago(ts: f64) -> String {
    let secs = (now() - ts).max(0.0);
    if secs < 90.0 {
        "just now".into()
    } else if secs < 3600.0 {
        format!("{}m ago", (secs / 60.0) as i64)
    } else if secs < 86400.0 {
        format!("{}h ago", (secs / 3600.0) as i64)
    } else {
        format!("{}d ago", (secs / 86400.0) as i64)
    }
}

/// When a usage window resets: "now", "42m", "3h05m", or a weekday and time past a day.
pub fn fmt_reset(reset: &Value) -> String {
    let at = match reset {
        Value::Number(n) => {
            let n = n.as_f64().unwrap_or(0.0);
            Some(if n > 1e12 { n / 1000.0 } else { n })
        }
        Value::String(s) => glib::DateTime::from_iso8601(s, Some(&glib::TimeZone::utc())).ok().map(|d| d.to_unix() as f64),
        _ => None,
    };
    let Some(at) = at else { return String::new() };
    let secs = at - now();
    if secs <= 0.0 {
        return "now".into();
    }
    if secs < 86400.0 {
        let mins = (secs / 60.0) as i64;
        let (h, m) = (mins / 60, mins % 60);
        return if h > 0 { format!("{h}h{m:02}m") } else { format!("{m}m") };
    }
    glib::DateTime::from_unix_local(at as i64)
        .ok()
        .and_then(|d| d.format("%a %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_default()
}

/// What a blocked session is asking, short enough to read: the first line, squeezed.
pub fn short_needs(text: &str, limit: usize) -> String {
    let first = text.split('\n').next().unwrap_or("");
    let squeezed = first.split_whitespace().collect::<Vec<_>>().join(" ");
    if squeezed.chars().count() <= limit {
        return squeezed;
    }
    let cut: String = squeezed.chars().take(limit.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// `~/...` for paths under home.
pub fn tilde(path: &str, home: &str) -> String {
    if !home.is_empty() && path.starts_with(home) { path.replacen(home, "~", 1) } else { path.to_owned() }
}

pub fn esc(text: &str) -> String {
    glib::markup_escape_text(text).to_string()
}
