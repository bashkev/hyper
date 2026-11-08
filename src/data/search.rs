//! Cancellable, parallel transcript search. Results retain the session index's order.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use aho_corasick::AhoCorasick;
use serde_json::Value;

use super::{Session, each_line, one_line, workers};

/// Records bigger than this are images or blobs; search doesn't parse them.
const MAX_SEARCH_LINE: usize = 4 * 1024 * 1024;
/// How many matches per session search counts. A count past it reads "200+".
pub const MAX_HITS: usize = 200;

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub session: String,
    pub count: usize,
    pub snippet: String,
    /// The first matching record, for scrolling to it.
    pub uuid: String,
}

/// Sessions whose prompts or replies contain `query`, ignoring ASCII case, in `sessions`'s
/// order. Returns early, with what it has, once `generation` moves past `mine`.
pub fn search(sessions: &[Session], query: &str, generation: &Arc<AtomicU64>, mine: u64) -> Vec<Hit> {
    let matcher_for = |text: &str| AhoCorasick::builder().ascii_case_insensitive(true).build([text]);
    let Ok(matcher) = matcher_for(query) else { return Vec::new() };
    // Lines are raw JSON, where a quote or backslash in the text is stored escaped.
    let escaped = serde_json::to_string(query).unwrap_or_default();
    let Ok(raw) = matcher_for(escaped.trim_matches('"')) else { return Vec::new() };
    // Biggest first, so one huge file doesn't run alone at the end.
    let mut order: Vec<usize> = (0..sessions.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(sessions[i].size));
    let next = AtomicUsize::new(0);
    let mut found: Vec<(usize, Hit)> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..workers())
            .map(|_| {
                scope.spawn(|| {
                    let mut out = Vec::new();
                    while let Some(&i) = order.get(next.fetch_add(1, Ordering::Relaxed)) {
                        if generation.load(Ordering::Relaxed) != mine {
                            break;
                        }
                        if let Some(hit) = search_file(&sessions[i], &raw, &matcher) {
                            out.push((i, hit));
                        }
                    }
                    out
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect()
    });
    found.sort_by_key(|(i, _)| *i);
    found.into_iter().map(|(_, hit)| hit).collect()
}

fn search_file(session: &Session, raw: &AhoCorasick, matcher: &AhoCorasick) -> Option<Hit> {
    let mut hit: Option<Hit> = None;
    each_line(&session.path, |number, line| {
        if line.len() > MAX_SEARCH_LINE || !raw.is_match(line) {
            return true;
        }
        let Ok(record) = serde_json::from_slice::<Value>(line) else { return true };
        let readable = session.source.agent().searchable(&record, number);
        let Some((id, texts)) = readable else { return true };
        for text in texts {
            let Some(m) = matcher.find(&text) else { continue };
            let current = hit.get_or_insert_with(|| Hit {
                session: session.id.clone(),
                count: 0,
                snippet: snippet(&text, m.start()),
                uuid: id.clone(),
            });
            current.count += matcher.find_iter(&text).count();
            if current.count >= MAX_HITS {
                return false;
            }
        }
        true
    })
    .ok()?;
    hit
}

fn snippet(text: &str, start: usize) -> String {
    const LEAD: usize = 24;
    let mut from = start.saturating_sub(LEAD);
    while !text.is_char_boundary(from) {
        from -= 1;
    }
    let body = one_line(&text[from..], 70);
    if from > 0 { format!("…{body}") } else { body }
}
