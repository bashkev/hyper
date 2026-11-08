//! How times, sizes, token counts and money read, wherever they're shown.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::usage::Spend;

/// Seconds since the epoch.
pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// How long ago, as briefly as a list row can say it.
pub fn ago(updated: i64) -> String {
    let seconds = (now() - updated).max(0);
    match seconds {
        0..60 => "now".to_owned(),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        86_400..2_592_000 => format!("{}d", seconds / 86_400),
        _ => date(updated, "%b %-d"),
    }
}

/// A moment in local time, in a `chrono` format.
pub fn date(seconds: i64, format: &str) -> String {
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|t| t.with_timezone(&chrono::Local).format(format).to_string())
        .unwrap_or_default()
}

/// The date and time the details pane shows.
pub fn timestamp(seconds: i64) -> String {
    date(seconds, "%b %-d, %Y %H:%M")
}

pub fn size(bytes: u64) -> String {
    match bytes {
        0..1_000_000 => format!("{} KB", bytes.div_ceil(1_000)),
        _ => format!("{:.1} MB", bytes as f64 / 1e6),
    }
}

/// Dollars to the cent, with what can't be priced shown as more to come.
pub fn money(cost: f64) -> String {
    if cost > 0.0 && cost < 0.005 { "<$0.01".to_owned() } else { format!("${cost:.2}") }
}

pub fn spent(spend: &Spend) -> String {
    let cost = spend.cost.total();
    match (spend.unpriced, cost) {
        (0, _) => money(cost),
        (_, 0.0) => "No price".to_owned(),
        _ => format!("{}+", money(cost)),
    }
}

pub fn token_count(tokens: u64) -> String {
    let n = tokens as f64;
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{:.1}K", n / 1e3),
        1_000_000..1_000_000_000 => format!("{:.1}M", n / 1e6),
        _ => format!("{:.2}B", n / 1e9),
    }
}
