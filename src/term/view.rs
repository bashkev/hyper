//! Drawing a [`Frame`] with Rune.

use std::hash::{DefaultHasher, Hash, Hasher};

use rune::{Color, El, Span, TextStyle, col, memo, text};
use rune_kit::Kit;

use super::{Frame, INSET, Line, Picture, Shape};

/// The style every cell is set in.
pub fn style(k: &Kit) -> TextStyle {
    k.ty.mono
}

/// A cell's width and height in points.
pub fn cell(style: TextStyle) -> (f32, f32) {
    (rune::text::advance("M", style), style.line_height)
}

fn line_view(line: &Line, style: TextStyle, (cell_w, cell_h): (f32, f32), selection_color: Color) -> El {
    let fills = line.fills.iter().map(|f| {
        col().absolute(f32::from(f.col) * cell_w, 0.0).size(f32::from(f.width) * cell_w, cell_h).fill(f.color.color())
    });
    let runs = line.runs.iter().map(|r| {
        let font = TextStyle { weight: if r.bold { 700.0 } else { style.weight }, italic: r.italic, ..style };
        let color = if r.faint { r.fg.color().alpha(0.6) } else { r.fg.color() };
        let mut run = text(r.text.clone()).font(font).color(color).single_line().absolute(f32::from(r.col) * cell_w, 0.0);
        if r.underline {
            run = run.spans(vec![Span::new(0, r.text.len()).underline(color)]);
        }
        run
    });
    let selection = line.selected.map(|(start, end)| col().key("terminal-selection").absolute(f32::from(start) * cell_w, 0.0).size(f32::from(end - start + 1) * cell_w, cell_h).fill(selection_color));
    col().w_full().h(cell_h).children(fills).children(selection).children(runs)
}

/// The screen, without the handlers that belong to the app.
pub fn screen(frame: Option<&Frame>, style: TextStyle, cell: (f32, f32), focused: bool, selection_color: Color) -> El {
    let Some(frame) = frame else { return col() };
    let (cell_w, cell_h) = cell;
    let lines = frame.lines.iter().enumerate().map(|(i, line)| {
        let mut version = DefaultHasher::new();
        line.hash(&mut version);
        (cell_w.to_bits(), cell_h.to_bits()).hash(&mut version);
        for c in [selection_color.r, selection_color.g, selection_color.b, selection_color.a] { c.to_bits().hash(&mut version); }
        let id = format!("terminal-line-{i}");
        col().absolute(0.0, i as f32 * cell_h).w_full().h(cell_h).child(memo(id, version.finish(), || line_view(line, style, cell, selection_color)))
    });
    let caret = frame.caret.map(|c| {
        let (x, y) = (f32::from(c.col) * cell_w, f32::from(c.row) * cell_h);
        let color = frame.fg.color();
        let mark = match c.shape {
            Shape::Bar => col().absolute(x, y).size(2.0, cell_h).fill(color),
            Shape::Underline => col().absolute(x, y + cell_h - 2.0).size(cell_w, 2.0).fill(color),
            Shape::Block if focused => col().absolute(x, y).size(cell_w, cell_h).fill(color.alpha(0.6)),
            _ => col().absolute(x, y).size(cell_w, cell_h).fill(color.alpha(0.25)),
        };
        if focused { mark.ime_cursor() } else { mark }
    });
    // The engine's cells are whole pixels; the measured ones aren't.
    let (sx, sy) = (cell_w / cell_w.ceil(), cell_h / cell_h.ceil());
    let picture = |(i, p): (usize, &Picture)| {
        let [x, y, w, h] = p.whole;
        let whole = col().absolute((x - p.x) * sx, (y - p.y) * sy).size(w * sx, h * sy).pixels(p.image.pixels());
        col().key(format!("terminal-image-{i}")).absolute(p.x * sx, p.y * sy).size(p.w * sx, p.h * sy).clip_rounded(0.0).child(whole)
    };
    let under = frame.pictures.iter().enumerate().filter(|(_, p)| !p.above_text).map(picture);
    let over = frame.pictures.iter().enumerate().filter(|(_, p)| p.above_text).map(picture);
    col().absolute(INSET, INSET).w_full().h_full().children(under).children(lines).children(over).children(caret)
}
