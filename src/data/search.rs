//! Cancellable, parallel transcript search. Results retain the session index's order.

use std::iter::successors;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use aho_corasick::packed;
use aho_corasick::{AhoCorasick, Input, Span};
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
    let Some(matcher) = Needle::new(query) else { return Vec::new() };
    // Lines are raw JSON, where a quote or backslash in the text is stored escaped.
    let escaped = serde_json::to_string(query).unwrap_or_default();
    let Some(raw) = Needle::new(escaped.trim_matches('"')) else { return Vec::new() };
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

fn search_file(session: &Session, raw: &Needle, matcher: &Needle) -> Option<Hit> {
    let mut hit: Option<Hit> = None;
    each_line(&session.path, |number, line| {
        if line.len() > MAX_SEARCH_LINE || raw.find(line, 0).is_none() {
            return true;
        }
        let Ok(record) = serde_json::from_slice::<Value>(line) else { return true };
        let readable = session.source.agent().searchable(&record, number);
        let Some((id, texts)) = readable else { return true };
        for text in texts {
            let Some(start) = matcher.find(text.as_bytes(), 0) else { continue };
            let current = hit.get_or_insert_with(|| Hit {
                session: session.id.clone(),
                count: 0,
                snippet: snippet(&text, start),
                uuid: id.clone(),
            });
            current.count += matcher.count(text.as_bytes(), start);
            if current.count >= MAX_HITS {
                return false;
            }
        }
        true
    })
    .ok()?;
    hit
}

/// Finds a query in bytes, ignoring ASCII case. A case-insensitive automaton has no fast
/// prefilter and scans about 2.5 GB/s; Teddy looking for each case spelling of the query's first
/// few bytes, with each candidate then checked in full, scans about 10 GB/s.
struct Needle {
    query: Vec<u8>,
    prefix: Prefix,
}

enum Prefix {
    Teddy(packed::Searcher),
    /// Where Teddy isn't available: on a CPU without the SIMD it needs, or for an empty query.
    Automaton(AhoCorasick),
}

impl Needle {
    /// How many of the query's bytes Teddy looks for: at most 16 spellings, within the 64 it takes.
    const PREFIX: usize = 4;

    fn new(query: &str) -> Option<Needle> {
        let query = query.as_bytes().to_vec();
        let start = &query[..query.len().min(Self::PREFIX)];
        let mut spellings = vec![Vec::new()];
        for &byte in start {
            let cases = if byte.is_ascii_alphabetic() { vec![byte.to_ascii_lowercase(), byte.to_ascii_uppercase()] } else { vec![byte] };
            spellings = spellings.iter().flat_map(|spelling: &Vec<u8>| cases.iter().map(move |&case| [spelling.as_slice(), &[case]].concat())).collect();
        }
        let prefix = match packed::Config::new().builder().extend(&spellings).build() {
            Some(teddy) => Prefix::Teddy(teddy),
            None => Prefix::Automaton(AhoCorasick::new(&spellings).ok()?),
        };
        Some(Needle { query, prefix })
    }

    /// Where the query next starts at or after `from`.
    fn find(&self, haystack: &[u8], mut from: usize) -> Option<usize> {
        loop {
            if from > haystack.len() {
                return None;
            }
            let start = match &self.prefix {
                Prefix::Teddy(teddy) => teddy.find_in(haystack, Span { start: from, end: haystack.len() })?.start(),
                Prefix::Automaton(automaton) => automaton.find(Input::new(haystack).range(from..))?.start(),
            };
            if haystack.get(start..start + self.query.len()).is_some_and(|found| found.eq_ignore_ascii_case(&self.query)) {
                return Some(start);
            }
            // A prefix that isn't the whole query may still overlap the next one that is.
            from = start + 1;
        }
    }

    /// How many times the query appears without overlapping, from its first match at `first`.
    fn count(&self, haystack: &[u8], first: usize) -> usize {
        successors(Some(first), |&at| self.find(haystack, at + self.query.len().max(1))).count()
    }
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
