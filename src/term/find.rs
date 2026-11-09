//! Finding text in a terminal, scrollback included, and marking the matches on its frames.
//!
//! The engine's [`Search`] does the finding and keeps up as the program prints, reflows and
//! switches screens. It's advanced a little before each frame, so a long scrollback is searched
//! over a few frames rather than holding one up.

use std::time::{Duration, Instant};

use libghostty_vt::Terminal;
use libghostty_vt::error::Result;
use libghostty_vt::search::{MatchBuffer, Search, Status};
use libghostty_vt::selection::Selection;
use libghostty_vt::terminal::PointSpace;

use super::Frame;

/// How long one frame's search may take.
const BUDGET: Duration = Duration::from_millis(4);

/// How many matches there are of what's being found, across the scrollback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Found {
    pub total: usize,
    /// Counted from the newest match.
    pub selected: Option<usize>,
    /// The whole scrollback has been searched, so `total` is all there are.
    pub complete: bool,
}

/// Which way the next match is from the one selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toward {
    /// Up the screen and into the scrollback, the way a search from the prompt goes first.
    Older,
    Newer,
}

#[derive(Default)]
pub(super) struct Finding {
    /// None while nothing is being looked for.
    search: Option<Search<'static, 'static>>,
    buffer: MatchBuffer,
    /// The newest match is selected as soon as there is one, as a find bar does while typing.
    select: bool,
}

impl Finding {
    /// Looks for `needle` instead, or stops looking when it's empty.
    pub(super) fn set(&mut self, term: &mut Terminal<'static, '_>, needle: &str) -> Result<()> {
        if needle.is_empty() {
            self.search = None;
            return Ok(());
        }
        let search = match &mut self.search {
            Some(search) => search,
            None => self.search.insert(Search::new(term)?),
        };
        // The same needle in other case keeps its results and the match selected.
        search.set_needle(term, needle.as_bytes())?;
        self.select = search.selected_index()?.is_none();
        Ok(())
    }

    /// Selects the next match `toward` older or newer output, scrolling to it.
    pub(super) fn step(&mut self, term: &mut Terminal<'static, '_>, toward: Toward) -> Result<()> {
        let Some(search) = &mut self.search else { return Ok(()) };
        let selected = match toward {
            Toward::Older => search.select_next(term)?,
            Toward::Newer => search.select_prev(term)?,
        };
        // Before the scrollback has turned anything up, the first match found is selected.
        if selected {
            self.select = false;
        }
        Ok(())
    }

    /// Searches for a little while. True when there's more to search.
    pub(super) fn advance(&mut self, term: &mut Terminal<'static, '_>) -> Result<bool> {
        let Some(search) = &mut self.search else { return Ok(false) };
        let started = Instant::now();
        search.feed(term)?;
        let mut status = search.status()?;
        // Each feed hands the search one chunk of the scrollback.
        while status != Status::Complete && started.elapsed() < BUDGET {
            if status == Status::FeedRequired {
                search.feed(term)?;
            }
            status = search.tick()?;
        }
        if self.select && search.select_next(term)? {
            self.select = false;
        }
        Ok(status != Status::Complete)
    }

    /// Marks the matches on screen, and the selected one, on `frame`'s lines.
    pub(super) fn mark(&mut self, term: &mut Terminal<'static, '_>, frame: &mut Frame) -> Result<()> {
        let Some(search) = &mut self.search else { return Ok(()) };
        let found = Found { total: search.total_matches()?, selected: search.selected_index()?, complete: search.status()? == Status::Complete };
        frame.found = Some(found);
        let top = term.scrollbar()?.offset;
        let snapshot = search.snapshot(term)?;
        let term = snapshot.terminal();
        for selection in snapshot.viewport_matches(&mut self.buffer)? {
            for (row, cols) in rows(term, &selection, top, frame)? {
                frame.lines[row].found.push(cols);
            }
        }
        if let Some(selection) = snapshot.selected_match()? {
            for (row, cols) in rows(term, &selection, top, frame)? {
                frame.lines[row].current = Some(cols);
            }
        }
        Ok(())
    }
}

/// The columns a match covers on each of the frame's lines, first and last inclusive. A match
/// can wrap onto the lines below, and part of it can be off screen.
fn rows(term: &Terminal<'_, '_>, selection: &Selection<'_>, top: u64, frame: &Frame) -> Result<Vec<(usize, (u16, u16))>> {
    let (Some(start), Some(end)) = (term.point_from_grid_ref(&selection.start(), PointSpace::Screen)?, term.point_from_grid_ref(&selection.end(), PointSpace::Screen)?) else {
        return Ok(Vec::new());
    };
    let last = frame.cols.saturating_sub(1);
    let rows = (u64::from(start.y)..=u64::from(end.y)).filter_map(|y| {
        let row = usize::try_from(y.checked_sub(top)?).ok().filter(|&row| row < frame.lines.len())?;
        let first = if y == u64::from(start.y) { start.x } else { 0 };
        let until = if y == u64::from(end.y) { end.x } else { last };
        Some((row, (first, until)))
    });
    Ok(rows.collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::keep_scrollback;
    use crate::term::screen::Screen;

    /// A line's matches, and the part of the selected one on it.
    type Marked = (Vec<(u16, u16)>, Option<(u16, u16)>);

    /// What's marked on each line of a 10×3 terminal after `bytes`, searched for `needle`, with
    /// the matches selected `steps` times toward older output.
    fn marked(bytes: &[u8], needle: &str, steps: usize) -> (Vec<Marked>, Option<Found>) {
        let mut term = Terminal::new(10, 3).unwrap();
        term.vt_write(bytes);
        let mut finding = Finding::default();
        finding.set(&mut term, needle).unwrap();
        while finding.advance(&mut term).unwrap() {}
        for _ in 0..steps {
            finding.step(&mut term, Toward::Older).unwrap();
        }
        let mut frame = Screen::new().unwrap().frame(&term).unwrap();
        finding.mark(&mut term, &mut frame).unwrap();
        (frame.lines.into_iter().map(|l| (l.found, l.current)).collect(), frame.found)
    }

    #[test]
    fn typing_selects_the_newest_match_and_each_step_goes_further_up() {
        let bytes = b"cat a\r\nls\r\ncat b";
        let (lines, found) = marked(bytes, "CAT", 0);
        assert_eq!(lines, [(vec![(0, 2)], None), (vec![], None), (vec![(0, 2)], Some((0, 2)))]);
        assert_eq!(found, Some(Found { total: 2, selected: Some(0), complete: true }));
        let (lines, found) = marked(bytes, "cat", 1);
        assert_eq!(lines[0], (vec![(0, 2)], Some((0, 2))));
        assert_eq!(found, Some(Found { total: 2, selected: Some(1), complete: true }));
    }

    #[test]
    fn a_match_found_in_the_scrollback_is_scrolled_to_and_marked_where_it_shows() {
        let mut bytes = b"needle".to_vec();
        for i in 0..50 {
            bytes.extend(format!("\r\nline {i}").as_bytes());
        }
        let (lines, found) = marked(&bytes, "needle", 0);
        assert_eq!(found, Some(Found { total: 1, selected: Some(0), complete: true }));
        assert_eq!(lines[0], (vec![(0, 5)], Some((0, 5))));
    }

    #[test]
    fn going_to_the_next_match_before_the_scrollback_turns_one_up_selects_it_once_found() {
        let mut term = Terminal::new(80, 24).unwrap();
        keep_scrollback(&mut term).unwrap();
        term.vt_write(b"needle");
        for i in 0..9000 {
            term.vt_write(format!("\r\nline {i}").as_bytes());
        }
        let mut finding = Finding::default();
        finding.set(&mut term, "needle").unwrap();
        finding.step(&mut term, Toward::Older).unwrap();
        while finding.advance(&mut term).unwrap() {}
        let mut frame = Screen::new().unwrap().frame(&term).unwrap();
        finding.mark(&mut term, &mut frame).unwrap();
        assert_eq!(frame.found, Some(Found { total: 1, selected: Some(0), complete: true }));
    }

    #[test]
    fn a_match_wrapped_onto_the_next_line_is_marked_on_both() {
        let (lines, _) = marked(b"12345678 needle", "needle", 0);
        assert_eq!(lines[0].0, [(9, 9)]);
        assert_eq!(lines[1].0, [(0, 4)]);
    }

    #[test]
    fn an_empty_needle_stops_finding() {
        let mut term = Terminal::new(10, 3).unwrap();
        term.vt_write(b"hit");
        let mut finding = Finding::default();
        finding.set(&mut term, "hit").unwrap();
        finding.set(&mut term, "").unwrap();
        let mut frame = Screen::new().unwrap().frame(&term).unwrap();
        finding.mark(&mut term, &mut frame).unwrap();
        assert_eq!((frame.found, &frame.lines[0].found), (None, &Vec::new()));
    }
}
