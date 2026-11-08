//! What a typed address means.

const SEARCH: &str = "https://www.google.com/search?q=";

/// The URL `input` means: a URL as it is, a bare port or local host over HTTP, a file by its
/// path, something that looks like a domain over HTTPS, and anything else searched for.
pub fn resolve(input: &str) -> Option<String> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    let lower = s.to_ascii_lowercase();
    if lower.contains("://") || ["about:", "data:", "mailto:"].iter().any(|p| lower.starts_with(p)) {
        return Some(s.to_owned());
    }
    let port = s.strip_prefix(':').unwrap_or(s);
    if !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit()) {
        return Some(format!("http://localhost:{port}"));
    }
    if let Some(path) = path(s) {
        return Some(format!("file://{}", encode(&path, b"/-_.~")));
    }
    let host = lower.split(['/', '?', '#']).next().unwrap_or_default();
    let name = match host.rsplit_once(':') {
        Some((name, port)) if port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    };
    if matches!(name, "localhost" | "[::1]") || name.ends_with(".localhost") || name.parse::<std::net::Ipv4Addr>().is_ok() {
        return Some(format!("http://{s}"));
    }
    if name.contains('.') && !name.starts_with('.') && !name.ends_with('.') && !s.contains(char::is_whitespace) {
        return Some(format!("https://{s}"));
    }
    Some(format!("{SEARCH}{}", encode(s, b"-_.~").replace("%20", "+")))
}

/// An absolute path, or one from the home folder.
fn path(s: &str) -> Option<String> {
    if let Some(rest) = s.strip_prefix("~/") {
        return Some(format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()));
    }
    s.starts_with('/').then(|| s.to_owned())
}

/// Percent-encodes every byte but ASCII letters, digits and `keep`.
fn encode(s: &str, keep: &[u8]) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || keep.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::resolve;

    #[test]
    fn typed_addresses_go_where_they_mean() {
        let home = std::env::var("HOME").unwrap();
        for (typed, url) in [
            ("https://example.com/a?b", "https://example.com/a?b"),
            ("  example.com  ", "https://example.com"),
            ("docs.rs/rune", "https://docs.rs/rune"),
            ("localhost:3000/app", "http://localhost:3000/app"),
            ("3000", "http://localhost:3000"),
            (":5173", "http://localhost:5173"),
            ("127.0.0.1:8080", "http://127.0.0.1:8080"),
            ("api.localhost", "http://api.localhost"),
            ("/tmp/my report.html", "file:///tmp/my%20report.html"),
            ("about:blank", "about:blank"),
            ("rust borrow checker", "https://www.google.com/search?q=rust+borrow+checker"),
            ("c++ & go", "https://www.google.com/search?q=c%2B%2B+%26+go"),
            ("localhost", "http://localhost"),
            ("README.md is long", "https://www.google.com/search?q=README.md+is+long"),
        ] {
            assert_eq!(resolve(typed).as_deref(), Some(url), "{typed}");
        }
        assert_eq!(resolve("~/site/index.html"), Some(format!("file://{home}/site/index.html")));
        assert_eq!(resolve("   "), None);
    }
}
