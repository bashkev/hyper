//! Reading the engine's screen into a plain [`Frame`], with the images programs placed on it.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use libghostty_vt::Terminal;
use libghostty_vt::kitty::graphics::ImageFormat;
use libghostty_vt::render::{CellIterator, CursorVisualStyle, ImageIterator, ImageLayer, ImageProtocol, RenderState, RowIterator, Snapshot};
use libghostty_vt::style::Underline;
use libghostty_vt::terminal::{Point, PointCoordinate};
use rune::{Image, ImageCache, Waker};

use super::{Caret, Fill, Frame, Line, Link, Picture, Rgb, Run, Shape};

/// Reads the engine's screen into a plain [`Frame`].
pub(super) struct Screen<'a> {
    state: RenderState<'a>,
    rows: RowIterator<'a>,
    cells: CellIterator<'a>,
    placements: ImageIterator<'a>,
    /// Images already handed to Rune, by image, its generation and the part shown.
    pictures: HashMap<(u32, u64, [u32; 4]), Image>,
}

impl<'a> Screen<'a> {
    pub(super) fn new() -> Option<Self> {
        Some(Screen {
            state: RenderState::new().ok()?,
            rows: RowIterator::new().ok()?,
            cells: CellIterator::new().ok()?,
            placements: ImageIterator::new().ok()?,
            pictures: HashMap::new(),
        })
    }

    pub(super) fn frame(&mut self, term: &Terminal<'a, '_>) -> Option<Frame> {
        let snapshot = self.state.update(term).ok()?;
        let colors = snapshot.colors().ok()?;
        let (fg, bg): (Rgb, Rgb) = (colors.foreground.into(), colors.background.into());
        let mut lines = Vec::new();
        let mut rows = self.rows.update(&snapshot).ok()?;
        let mut glyph = String::new();
        let mut cols = 0;
        while let Some(row) = rows.next() {
            let y = lines.len() as u32;
            let mut cells = self.cells.update(row).ok()?;
            let mut line = LineBuilder::default();
            line.line.selected = row.selection().ok()?.map(|range| (range.start_x, range.end_x));
            let raw = row.raw_row().ok()?;
            line.line.wrapped = raw.is_wrapped().unwrap_or(false);
            let linked = raw.has_hyperlink().unwrap_or(false);
            while let Some(cell) = cells.next() {
                if linked && cell.raw_cell().ok()?.has_hyperlink().unwrap_or(false) {
                    line.link(term, y);
                }
                glyph.clear();
                if cell.graphemes_len().ok()? > 0 {
                    cell.graphemes_utf8(&mut glyph).ok()?;
                }
                let style = cell.has_styling().ok()?.then(|| cell.style().ok()).flatten();
                let mut cell_fg = cell.fg_color().ok()?.map_or(fg, Rgb::from);
                let mut cell_bg = cell.bg_color().ok()?.map(Rgb::from);
                let (mut bold, mut italic, mut faint, mut underline, mut hidden) = (false, false, false, false, false);
                if let Some(style) = style {
                    if style.inverse {
                        let swapped = cell_bg.unwrap_or(bg);
                        cell_bg = Some(cell_fg);
                        cell_fg = swapped;
                    }
                    (bold, italic, faint, hidden) = (style.bold, style.italic, style.faint, style.invisible);
                    underline = style.underline != Underline::None;
                }
                let look = Look { fg: cell_fg, bold, italic, faint, underline };
                // A Kitty placeholder cell marks where an image goes; the image is drawn instead.
                let placeholder = glyph.starts_with('\u{10EEEE}');
                line.push(if hidden || placeholder { "" } else { &glyph }, look, cell_bg);
            }
            cols = line.col;
            lines.push(line.finish());
        }
        let pictures = pictures(&mut self.placements, &mut self.pictures, term, &snapshot).unwrap_or_default();
        let cursor = snapshot.cursor().ok()?;
        let caret = cursor.viewport.filter(|_| cursor.visible).map(|at| Caret {
            col: at.x,
            row: at.y,
            shape: match cursor.visual_style {
                CursorVisualStyle::Bar => Shape::Bar,
                CursorVisualStyle::Underline => Shape::Underline,
                CursorVisualStyle::BlockHollow => Shape::Hollow,
                _ => Shape::Block,
            },
        });
        Some(Frame { lines, cols, pictures, caret, fg, bg, found: None })
    }
}

/// Set when an image finishes decoding, so the app draws it although no new frame came.
pub(super) static IMAGE_DECODED: AtomicBool = AtomicBool::new(false);
/// The app's waker, for when an image finishes decoding.
pub(super) static IMAGE_WAKE: Mutex<Option<Waker>> = Mutex::new(None);

pub(super) fn decoded_image() {
    IMAGE_DECODED.store(true, Ordering::Release);
    let wake = IMAGE_WAKE.lock().unwrap_or_else(PoisonError::into_inner).clone();
    if let Some(wake) = wake {
        wake();
    }
}

/// The images on screen, cut to the grid, in drawing order. One image the engine can't describe
/// is left out rather than losing the frame.
fn pictures<'a>(placements: &mut ImageIterator<'a>, cache: &mut HashMap<(u32, u64, [u32; 4]), Image>, term: &Terminal<'a, '_>, snapshot: &Snapshot<'a, '_>) -> Option<Vec<Picture>> {
    static IMAGES: OnceLock<ImageCache> = OnceLock::new();
    let mut shown = Vec::new();
    let Ok(Some(graphics)) = term.kitty_graphics() else {
        cache.clear();
        return Some(Vec::new());
    };
    let (grid_w, grid_h) = (term.width_px().ok()? as f32, term.height_px().ok()? as f32);
    let (cell_w, cell_h) = (grid_w / f32::from(term.cols().ok()?), grid_h / f32::from(term.rows().ok()?));
    let mut used = HashSet::new();
    let mut it = placements.update(snapshot).ok()?;
    while let Some(p) = it.next() {
        let Ok(ImageProtocol::Kitty) = p.protocol() else { continue };
        let mut placed = || -> Option<_> {
            let (id, generation) = (p.image_id().ok()?, p.image_generation().ok()?);
            let image = graphics.image(id)?;
            let x = p.viewport_col().ok()? as f32 * cell_w + p.cell_offset_x().ok()? as f32;
            let y = p.viewport_row().ok()? as f32 * cell_h + p.cell_offset_y().ok()? as f32;
            let (w, h) = (p.pixel_width().ok()? as f32, p.pixel_height().ok()? as f32);
            let (left, top, right, bottom) = (x.max(0.0), y.max(0.0), (x + w).min(grid_w), (y + h).min(grid_h));
            if right <= left || bottom <= top {
                return None;
            }
            let (image_w, image_h) = (image.width().ok()?, image.height().ok()?);
            let x0 = p.source_x().ok()?.min(image_w);
            let y0 = p.source_y().ok()?.min(image_h);
            let x1 = x0.saturating_add(p.source_width().ok()?).min(image_w);
            let y1 = y0.saturating_add(p.source_height().ok()?).min(image_h);
            if x1 <= x0 || y1 <= y0 {
                return None;
            }
            let key = (id, generation, [x0, y0, x1 - x0, y1 - y0]);
            let picture = match cache.get(&key) {
                Some(picture) => picture.clone(),
                None => {
                    // `None` while it's still arriving.
                    let data = image.data().ok()??;
                    let rgba = rgba(data, image.format().ok()?, image_w, key.2)?;
                    let picture = IMAGES.get_or_init(|| ImageCache::new(Arc::new(decoded_image))).rgba(key.2[2], key.2[3], rgba);
                    cache.insert(key, picture.clone());
                    picture
                }
            };
            let above_text = p.layer().ok()? == ImageLayer::AboveText;
            let picture = Picture { image: picture, x: left, y: top, w: right - left, h: bottom - top, whole: [x, y, w, h], above_text };
            Some((key, p.z().ok()?, picture))
        };
        if let Some((key, z, picture)) = placed() {
            used.insert(key);
            shown.push((z, picture));
        }
    }
    cache.retain(|key, _| used.contains(key));
    shown.sort_by_key(|(z, p)| (p.above_text, *z));
    Some(shown.into_iter().map(|(_, p)| p).collect())
}

/// The `[x, y, w, h]` part of an image as straight RGBA.
fn rgba(data: &[u8], format: ImageFormat, width: u32, [x, y, w, h]: [u32; 4]) -> Option<Vec<u8>> {
    let bytes = match format {
        ImageFormat::Rgba => 4,
        ImageFormat::Rgb => 3,
        ImageFormat::GrayAlpha => 2,
        ImageFormat::Gray => 1,
        _ => return None,
    };
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    for row in y..y + h {
        let start = (row as usize * width as usize + x as usize) * bytes;
        for px in data.get(start..start + w as usize * bytes)?.chunks_exact(bytes) {
            out.extend_from_slice(&match *px {
                [r, g, b, a] => [r, g, b, a],
                [r, g, b] => [r, g, b, 255],
                [v, a] => [v, v, v, a],
                [v] => [v, v, v, 255],
                _ => return None,
            });
        }
    }
    Some(out)
}

#[derive(Clone, Copy, PartialEq)]
struct Look {
    fg: Rgb,
    bold: bool,
    italic: bool,
    faint: bool,
    underline: bool,
}

/// Whether the mono font is sure to draw `c` one cell wide. Anything else gets a run of its own,
/// so a fallback font's width can't push the rest of the line out of its columns.
fn fits_cell(c: char) -> bool {
    c.is_ascii() || c == '\u{a0}' || ('\u{2500}'..='\u{259f}').contains(&c)
}

/// Rune's fallback draws these from the colour emoji font, as tiles too wide for a cell, and
/// doesn't honour a text variation selector, so they're swapped for plain symbols.
fn text_form(c: char) -> char {
    match c {
        '\u{23fa}' => '\u{25cf}',
        '\u{23f8}' => '\u{2016}',
        '\u{23f9}' => '\u{25a0}',
        c => c,
    }
}

#[derive(Default)]
struct LineBuilder {
    line: Line,
    run: Option<(u16, String, Look)>,
    col: u16,
}

impl LineBuilder {
    /// The cell about to be pushed links to a URL.
    fn link(&mut self, term: &Terminal<'_, '_>, y: u32) {
        let x = self.col;
        let Ok(at) = term.grid_ref(Point::Viewport(PointCoordinate { x, y })) else { return };
        let mut small = [0u8; 512];
        let mut large = Vec::new();
        let url: &[u8] = match at.hyperlink_uri(&mut small) {
            Ok(len) => &small[..len],
            Err(libghostty_vt::error::Error::OutOfSpace { required }) => {
                large.resize(required, 0);
                let len = at.hyperlink_uri(&mut large).unwrap_or(0);
                &large[..len]
            }
            Err(_) => return,
        };
        match self.line.links.last_mut() {
            Some(link) if link.end == x && link.url.as_bytes() == url => link.end += 1,
            _ if !url.is_empty() => self.line.links.push(Link { start: x, end: x + 1, url: String::from_utf8_lossy(url).into_owned() }),
            _ => {}
        }
    }

    fn push(&mut self, glyph: &str, look: Look, bg: Option<Rgb>) {
        let col = self.col;
        self.col += 1;
        if let Some(color) = bg {
            match self.line.fills.last_mut() {
                Some(f) if f.color == color && f.col + f.width == col => f.width += 1,
                _ => self.line.fills.push(Fill { col, width: 1, color }),
            }
        }
        let plain = glyph.chars().all(fits_cell);
        match &mut self.run {
            Some((_, text, current)) if plain && (*current == look || glyph.is_empty()) => {
                text.push_str(if glyph.is_empty() { " " } else { glyph });
            }
            _ => {
                self.end_run();
                if !glyph.is_empty() {
                    let text: String = glyph.chars().map(text_form).collect();
                    self.run = Some((col, text, look));
                    if !plain {
                        self.end_run();
                    }
                }
            }
        }
    }

    fn end_run(&mut self) {
        if let Some((col, text, look)) = self.run.take() {
            let text = text.trim_end().to_owned();
            if !text.is_empty() {
                let Look { fg, bold, italic, faint, underline } = look;
                self.line.runs.push(Run { col, text, fg, bold, italic, faint, underline });
            }
        }
    }

    fn finish(mut self) -> Line {
        self.end_run();
        self.line
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::{allow_images, decode_png};

    fn frame(bytes: &[u8]) -> Frame {
        let mut term = Terminal::new(40, 4).unwrap();
        term.vt_write(bytes);
        Screen::new().unwrap().frame(&term).unwrap()
    }

    fn base64(bytes: &[u8]) -> String {
        const ABC: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        bytes.chunks(3).flat_map(|c| {
            let n = u32::from(c[0]) << 16 | u32::from(*c.get(1).unwrap_or(&0)) << 8 | u32::from(*c.get(2).unwrap_or(&0));
            (0..4).map(move |i| if i <= c.len() { ABC[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' })
        }).collect()
    }

    /// A 10×4 grid of 8×16 pixel cells, taking images as a pane does.
    fn pictures(bytes: &[u8]) -> Vec<(f32, f32, f32, f32, bool, [u32; 2])> {
        decode_png();
        let mut term = Terminal::new(10, 4).unwrap();
        term.resize(10, 4, 8, 16).unwrap();
        allow_images(&mut term).unwrap();
        term.vt_write(bytes);
        let frame = Screen::new().unwrap().frame(&term).unwrap();
        frame.pictures.iter().map(|p| (p.x, p.y, p.w, p.h, p.above_text, p.image.wait().unwrap())).collect()
    }

    #[test]
    fn an_image_shows_at_the_cursor_over_the_cells_it_asked_for() {
        // A 2×2 RGB image over 2 columns and 1 row, at row 2, column 4.
        let shown = pictures(b"\x1b[2;4H\x1b_Ga=T,f=24,s=2,v=2,c=2,r=1,q=2;////////////////\x1b\\");
        assert_eq!(shown, [(24.0, 16.0, 16.0, 16.0, true, [2, 2])]);
    }

    #[test]
    fn a_png_image_is_decoded() {
        let shown = pictures(b"\x1b_Ga=T,f=100,q=2;iVBORw0KGgoAAAANSUhEUgAAAAMAAAACCAYAAACddGYaAAAAEUlEQVR4nGP4z8DwH4YZkDkAm34L9XKwuTwAAAAASUVORK5CYII=\x1b\\");
        assert_eq!(shown, [(0.0, 0.0, 3.0, 2.0, true, [3, 2])]);
    }

    #[test]
    fn an_image_can_come_from_a_file() {
        // Tools like `kitty icat` send a path instead of the pixels when they run locally.
        let path = std::env::temp_dir().join(format!("hyper-image-{}.rgb", std::process::id()));
        std::fs::write(&path, [255u8; 12]).unwrap();
        let encoded = base64(path.to_str().unwrap().as_bytes());
        let shown = pictures(format!("\x1b_Ga=T,t=f,f=24,s=2,v=2,q=2;{encoded}\x1b\\").as_bytes());
        let _ = std::fs::remove_file(&path);
        assert_eq!(shown, [(0.0, 0.0, 2.0, 2.0, true, [2, 2])]);
    }

    #[test]
    fn an_image_past_the_edge_is_cut_to_the_grid() {
        // 4×1 pixels over 4 columns from column 9 of 10: only its left half fits.
        let shown = pictures(b"\x1b[1;9H\x1b_Ga=T,f=32,s=4,v=1,c=4,r=1,q=2;/////////////////////w==\x1b\\");
        assert_eq!(shown, [(64.0, 0.0, 16.0, 16.0, true, [4, 1])], "the whole image, shown through the part that fits");
    }

    #[test]
    fn an_image_scrolling_off_the_top_stays_the_same_image() {
        decode_png();
        let mut term = Terminal::new(10, 4).unwrap();
        term.resize(10, 4, 8, 16).unwrap();
        allow_images(&mut term).unwrap();
        term.vt_write(b"\x1b_Ga=T,f=24,s=2,v=2,c=2,r=2,q=2;////////////////\x1b\\");
        let mut screen = Screen::new().unwrap();
        let before = screen.frame(&term).unwrap().pictures;
        term.vt_write(b"\x1b[4;1H\n");
        let after = screen.frame(&term).unwrap().pictures;
        assert_eq!((after[0].y, after[0].h, after[0].whole), (0.0, 16.0, [0.0, -16.0, 16.0, 32.0]), "half of it is off the top");
        assert_eq!(after[0].image.id(), before[0].image.id(), "so it doesn't blank while a new cut decodes");
    }

    #[test]
    fn an_image_with_a_negative_z_goes_under_the_text_and_is_drawn_first() {
        let shown = pictures(b"\x1b_Ga=T,f=24,s=2,v=2,i=1,q=2;////////////////\x1b\\\x1b_Ga=T,f=24,s=2,v=2,i=2,z=-1,q=2;////////////////\x1b\\");
        assert_eq!(shown.iter().map(|p| p.4).collect::<Vec<_>>(), [false, true]);
    }

    #[test]
    fn the_part_of_an_image_shown_becomes_rgba() {
        // A 2×2 RGB image; its right column.
        let data: Vec<u8> = (0..12).collect();
        assert_eq!(rgba(&data, ImageFormat::Rgb, 2, [1, 0, 1, 2]).unwrap(), [3, 4, 5, 255, 9, 10, 11, 255]);
        assert_eq!(rgba(&[7, 9], ImageFormat::GrayAlpha, 1, [0, 0, 1, 1]).unwrap(), [7, 7, 7, 9]);
        assert_eq!(rgba(&data, ImageFormat::Rgb, 2, [0, 1, 2, 2]), None, "past the end");
    }

    #[test]
    fn kitty_placeholder_cells_leave_room_for_their_image() {
        // tmux shows images as U+10EEEE cells, with diacritics saying which part goes where.
        let f = frame("a\u{10EEEE}\u{305}\u{305}\u{10EEEE}\u{305}\u{30D}b".as_bytes());
        assert_eq!(f.lines[0].runs.iter().map(|r| (r.col, r.text.as_str())).collect::<Vec<_>>(), [(0, "a  b")]);
    }

    #[test]
    fn colours_split_a_line_into_runs_at_their_columns() {
        let f = frame(b"ab\x1b[31mcd\x1b[0m e");
        let runs = &f.lines[0].runs;
        assert_eq!(runs.iter().map(|r| (r.col, r.text.as_str())).collect::<Vec<_>>(), [(0, "ab"), (2, "cd"), (4, " e")]);
        assert_ne!(runs[1].fg, runs[0].fg);
    }

    #[test]
    fn a_symbol_from_another_font_cannot_shift_the_text_after_it() {
        let f = frame("a✻b".as_bytes());
        let runs = &f.lines[0].runs;
        assert_eq!(runs.iter().map(|r| (r.col, r.text.as_str())).collect::<Vec<_>>(), [(0, "a"), (1, "✻"), (2, "b")]);
    }

    #[test]
    fn a_wide_character_takes_two_columns() {
        let f = frame("日x".as_bytes());
        assert_eq!(f.lines[0].runs.last().map(|r| r.col), Some(2));
    }

    #[test]
    fn symbols_the_emoji_font_would_take_become_plain_ones() {
        let f = frame("\u{23fa}".as_bytes());
        assert_eq!(f.lines[0].runs[0].text, "\u{25cf}");
    }

    #[test]
    fn inverse_swaps_the_colours_into_a_fill() {
        let mut term = Terminal::new(10, 2).unwrap();
        term.set_default_fg_color(Some(Rgb(1, 2, 3).engine())).unwrap();
        term.set_default_bg_color(Some(Rgb(4, 5, 6).engine())).unwrap();
        term.vt_write(b"\x1b[7mhi");
        let f = Screen::new().unwrap().frame(&term).unwrap();
        assert_eq!(f.lines[0].fills, [Fill { col: 0, width: 2, color: Rgb(1, 2, 3) }]);
        assert_eq!(f.lines[0].runs[0].fg, Rgb(4, 5, 6));
    }

    #[test]
    fn the_caret_follows_the_cursor_and_hides_with_it() {
        assert_eq!(frame(b"abc").caret.map(|c| (c.col, c.row)), Some((3, 0)));
        assert_eq!(frame(b"abc\x1b[?25l").caret, None);
    }
}
