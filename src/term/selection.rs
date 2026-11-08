//! Selecting with the pointer, as Ghostty does, and the text that's selected.

use libghostty_vt::Terminal;
use libghostty_vt::selection::{FormatOptions, gesture::{Behavior, Behaviors, DragEvent, Geometry, Gesture, PressEvent, ReleaseEvent}};
use libghostty_vt::terminal::{Point, PointCoordinate};
use rune::Phase;

use super::{SelectionInput, Size};

/// Ghostty owns selection anchors, word/line clicks, wide cells and wrapped lines.
pub(super) struct Selecting<'a> {
    pub(super) gesture: Gesture<'a>,
    press: PressEvent<'a>,
    drag: DragEvent<'a>,
    release: ReleaseEvent<'a>,
    active: bool,
}

impl Selecting<'_> {
    pub(super) fn new() -> Option<Self> {
        Some(Self { gesture: Gesture::new().ok()?, press: PressEvent::new().ok()?, drag: DragEvent::new().ok()?, release: ReleaseEvent::new().ok()?, active: false })
    }

    pub(super) fn clear(&mut self, term: &Terminal<'_, '_>) {
        self.active = false;
        self.gesture.reset(term);
        let _ = term.set_selection(None);
    }

    pub(super) fn apply(&mut self, term: &Terminal<'_, '_>, input: SelectionInput, size: Size) -> libghostty_vt::error::Result<()> {
        if input.cancelled {
            self.clear(term);
            return Ok(());
        }
        let point = Point::Viewport(PointCoordinate {
            x: (input.x.floor() as u16).min(size.cols - 1),
            y: (input.y.floor() as u32).min(u32::from(size.rows - 1)),
        });
        let grid = term.grid_ref(point)?;
        let (x, y) = (f64::from(input.x) * f64::from(size.cell_w), f64::from(input.y) * f64::from(size.cell_h));
        match input.phase {
            Phase::Down => {
                self.active = true;
                // Rune already has the system's click count and interval. Choose the
                // behavior from that, so the engine doesn't run a second click timer.
                self.gesture.reset(term);
                let behavior = match input.clicks.saturating_sub(1) % 3 {
                    1 => Behavior::Word,
                    2 => Behavior::Line,
                    _ => Behavior::Cell,
                };
                let behaviors = Behaviors::new().with_single_click_behavior(behavior);
                let selected = self.press.set_behaviors(&behaviors)?.set_position(x, y)?.apply(&mut self.gesture, term, grid)?;
                term.set_selection(selected.as_ref())?;
            }
            Phase::Move if self.active => {
                let geometry = Geometry { columns: u32::from(size.cols), cell_width: u32::from(size.cell_w), padding_left: 0, screen_height: u32::from(size.rows) * u32::from(size.cell_h) };
                let selected = self.drag.set_rectangle(input.rectangle)?.set_position(x, y)?.apply(&mut self.gesture, term, grid, geometry)?;
                term.set_selection(selected.as_ref())?;
            }
            Phase::Up if self.active => {
                self.release.apply(&mut self.gesture, term, Some(grid))?;
                self.active = false;
            }
            _ => {},
        }
        Ok(())
    }
}

pub(super) fn selected_text(term: &Terminal<'_, '_>) -> Option<String> {
    let options = FormatOptions::new().with_unwrap(true).with_trim(true);
    term.format_selection_alloc(None, options).ok()?.map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::screen::Screen;

    fn select(term: &Terminal<'_, '_>, selection: &mut Selecting<'_>, size: Size, phase: Phase, x: f32, y: f32, rectangle: bool) {
        selection.apply(term, SelectionInput { phase, x, y, rectangle, cancelled: false, clicks: 1 }, size).unwrap();
    }

    #[test]
    fn copying_wide_cells_across_a_soft_wrap_preserves_the_original_text() {
        let mut term = Terminal::new(8, 3).unwrap();
        term.vt_write("ab日cdéfgh".as_bytes());
        let size = Size { cols: 8, rows: 3, cell_w: 8, cell_h: 17 };
        let mut selection = Selecting::new().unwrap();
        select(&term, &mut selection, size, Phase::Down, 0.1, 0.5, false);
        select(&term, &mut selection, size, Phase::Move, 1.9, 1.5, false);
        select(&term, &mut selection, size, Phase::Up, 1.9, 1.5, false);
        assert_eq!(selected_text(&term).as_deref(), Some("ab日cdéfgh"));
        let frame = Screen::new().unwrap().frame(&term).unwrap();
        assert_eq!(frame.lines[0].selected, Some((0, 7)));
        assert_eq!(frame.lines[1].selected, Some((0, 1)));
    }

    #[test]
    fn a_reverse_drag_and_a_rectangle_copy_their_cells_and_a_click_clears_them() {
        let mut term = Terminal::new(10, 3).unwrap();
        term.vt_write(b"one two\r\nred blue");
        let size = Size { cols: 10, rows: 3, cell_w: 8, cell_h: 17 };
        let mut selection = Selecting::new().unwrap();
        select(&term, &mut selection, size, Phase::Down, 6.9, 0.5, false);
        select(&term, &mut selection, size, Phase::Move, 4.1, 0.5, false);
        select(&term, &mut selection, size, Phase::Up, 4.1, 0.5, false);
        assert_eq!(selected_text(&term).as_deref(), Some("two"));
        selection.clear(&term);
        select(&term, &mut selection, size, Phase::Down, 0.1, 0.5, true);
        select(&term, &mut selection, size, Phase::Move, 2.9, 1.5, true);
        select(&term, &mut selection, size, Phase::Up, 2.9, 1.5, true);
        assert_eq!(selected_text(&term).as_deref(), Some("one\nred"));
        selection.clear(&term);
        select(&term, &mut selection, size, Phase::Down, 8.1, 2.5, false);
        select(&term, &mut selection, size, Phase::Up, 8.1, 2.5, false);
        assert_eq!(selected_text(&term), None);
    }

    #[test]
    fn repeat_clicks_select_a_word_then_a_line() {
        let mut term = Terminal::new(10, 3).unwrap();
        term.vt_write(b"one two");
        let size = Size { cols: 10, rows: 3, cell_w: 8, cell_h: 17 };
        let mut selection = Selecting::new().unwrap();
        for (i, expected) in [None, Some("two"), Some("one two")].into_iter().enumerate() {
            selection.apply(&term, SelectionInput { phase: Phase::Down, x: 5.5, y: 0.5, rectangle: false, cancelled: false, clicks: i as u32 + 1 }, size).unwrap();
            select(&term, &mut selection, size, Phase::Up, 5.5, 0.5, false);
            assert_eq!(selected_text(&term).as_deref(), expected);
        }
    }
}
