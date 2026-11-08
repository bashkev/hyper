//! Links in what a terminal shows: one a program attached to its text (OSC 8), or a URL written
//! out, which may run on over rows the terminal wrapped.

use super::{Frame, Line};

/// The link at `col` on screen row `row`.
pub fn link_at(frame: &Frame, col: u16, row: usize) -> Option<String> {
    let line = frame.lines.get(row)?;
    if let Some(link) = line.links.iter().find(|l| (l.start..l.end).contains(&col)) {
        return Some(link.url.clone());
    }
    // The rows the terminal wrapped into one line with this one.
    let wraps = |r: usize| frame.lines.get(r).is_some_and(|l| l.wrapped);
    let first = (0..row).rev().take_while(|&r| wraps(r)).last().unwrap_or(row);
    let last = (row..frame.lines.len()).find(|&r| !wraps(r)).unwrap_or(frame.lines.len() - 1);
    let mut text = String::new();
    let mut at = None;
    for r in first..=last {
        let line = &frame.lines[r];
        // A wrapped row runs to the edge, though its trailing blanks aren't in its runs.
        let width = if line.wrapped { usize::from(frame.cols) } else { 0 };
        for (c, ch) in cells(line, width).into_iter().enumerate() {
            if r == row && c == usize::from(col) {
                at = Some(text.len());
            }
            text.push(ch);
        }
    }
    url_at(&text, at?)
}

/// A row's characters by column, at least `width` of them.
fn cells(line: &Line, width: usize) -> Vec<char> {
    let mut out = vec![' '; width];
    for run in &line.runs {
        for (i, ch) in run.text.chars().enumerate() {
            let c = usize::from(run.col) + i;
            if out.len() <= c {
                out.resize(c + 1, ' ');
            }
            out[c] = ch;
        }
    }
    out
}

/// The http or https URL in `text` covering byte `at`.
fn url_at(text: &str, at: usize) -> Option<String> {
    let mut from = 0;
    while let Some(found) = ["https://", "http://"].iter().filter_map(|scheme| text[from..].find(scheme)).min() {
        let start = from + found;
        let end = start + url_len(&text[start..]);
        if (start..end).contains(&at) {
            return Some(text[start..end].to_owned());
        }
        from = end.max(start + 1);
    }
    None
}

/// How much of `s`, which starts with a scheme, is the URL: up to a space or a character URLs
/// can't hold, less the punctuation that ends a sentence or closes a bracket the URL didn't open.
fn url_len(s: &str) -> usize {
    let end = s.find(|c: char| c.is_whitespace() || c.is_control() || "<>\"`{}|\\^".contains(c)).unwrap_or(s.len());
    let mut url = &s[..end];
    loop {
        let unbalanced = |open: char, close: char| url.ends_with(close) && url.matches(open).count() < url.matches(close).count();
        if url.ends_with(['.', ',', ';', ':', '!', '?', '\'', '*']) || unbalanced('(', ')') || unbalanced('[', ']') {
            url = &url[..url.len() - 1];
        } else {
            return url.len();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{link_at, url_at};
    use crate::term::Frame;
    use crate::term::screen::Screen;
    use libghostty_vt::Terminal;

    fn frame(cols: u16, bytes: &[u8]) -> Frame {
        let mut term = Terminal::new(cols, 4).unwrap();
        term.vt_write(bytes);
        Screen::new().unwrap().frame(&term).unwrap()
    }

    #[test]
    fn a_url_ends_where_its_sentence_or_brackets_do() {
        let text = "see (https://en.wikipedia.org/wiki/Rust_(language)), or [docs](https://docs.rs/x). Done";
        let wiki = text.find("wiki").unwrap();
        assert_eq!(url_at(text, wiki).as_deref(), Some("https://en.wikipedia.org/wiki/Rust_(language)"));
        assert_eq!(url_at(text, text.find("docs.rs").unwrap()).as_deref(), Some("https://docs.rs/x"));
        assert_eq!(url_at(text, text.find("Done").unwrap()), None);
        assert_eq!(url_at("go to http://localhost:3000/a?b=1#c!", 8).as_deref(), Some("http://localhost:3000/a?b=1#c"));
    }

    #[test]
    fn a_url_the_terminal_wrapped_opens_whole_from_either_row() {
        let f = frame(20, b"> https://example.com/a/long/path done");
        assert!(f.lines[0].wrapped && !f.lines[1].wrapped);
        let whole = Some("https://example.com/a/long/path".to_owned());
        assert_eq!(link_at(&f, 5, 0), whole);
        assert_eq!(link_at(&f, 3, 1), whole);
        assert_eq!(link_at(&f, 0, 0), None, "the prompt isn't the link");
        assert_eq!(link_at(&f, 15, 1), None, "the word after it isn't either");
    }

    #[test]
    fn a_program_s_own_link_wins_over_the_text_it_shows() {
        let f = frame(40, b"\x1b]8;;https://example.com/pr/6\x1b\\PR #6\x1b]8;;\x1b\\ and https://x.dev");
        assert_eq!(link_at(&f, 2, 0).as_deref(), Some("https://example.com/pr/6"));
        assert_eq!(link_at(&f, 6, 0), None, "the link ends with its text");
        assert_eq!(link_at(&f, 14, 0).as_deref(), Some("https://x.dev"));
    }
}
