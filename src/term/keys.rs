//! Keys, paging and the wheel, encoded as the program on the terminal asked for them.

use std::collections::VecDeque;

use libghostty_vt::screen::Screen as EngineScreen;
use libghostty_vt::terminal::{Mode, ScrollViewport};
use libghostty_vt::{Terminal, key, mouse};
use rune::{Key, KeyEvent, PhysicalKeyCode, Tx};

use super::{KeyInput, Paged, Paging, Size, WheelInput, Wheeled};

pub(super) struct Keys<'a> {
    encoder: key::Encoder<'a>,
    event: key::Event<'a>,
    mouse: mouse::Encoder<'a>,
    press: mouse::Event<'a>,
    bytes: Vec<u8>,
}

impl Keys<'_> {
    pub(super) fn new() -> Option<Self> {
        Some(Keys {
            encoder: key::Encoder::new().ok()?,
            event: key::Event::new().ok()?,
            mouse: mouse::Encoder::new().ok()?,
            press: mouse::Event::new().ok()?,
            bytes: Vec::with_capacity(64),
        })
    }

    pub(super) fn key(&mut self, term: &Terminal<'_, '_>, input: &KeyInput, out: &mut VecDeque<u8>) {
        self.bytes.clear();
        self.event
            .set_action(if input.repeat { key::Action::Repeat } else { key::Action::Press })
            .set_key(input.key)
            .set_mods(input.mods)
            .set_consumed_mods(input.consumed)
            .set_unshifted_codepoint(input.unshifted)
            .set_utf8(input.text.clone());
        if self.encoder.set_options_from_terminal(term).encode_to_vec(&self.event, &mut self.bytes).is_ok() {
            out.extend(&self.bytes);
        }
    }

    /// Pages a full-screen program with its own keys, and otherwise the scrollback, where an
    /// inline program such as a shell or Pi's regular mode leaves its history.
    pub(super) fn page(&mut self, term: &mut Terminal<'_, '_>, paging: Paging, repeat: bool, size: Size, out: &mut VecDeque<u8>) -> Paged {
        if term.active_screen().is_ok_and(|s| s == EngineScreen::Alternate) {
            self.key(term, &KeyInput { repeat, ..paging.input() }, out);
            return Paged::Keys;
        }
        let page = size.rows.max(2) as isize - 1;
        term.scroll_viewport(match paging {
            Paging::PageUp => ScrollViewport::Delta(-page),
            Paging::PageDown => ScrollViewport::Delta(page),
            Paging::Top => ScrollViewport::Top,
            Paging::Bottom => ScrollViewport::Bottom,
        });
        Paged::Scrollback
    }

    /// Reports the wheel to a program that reads the mouse, sends arrow keys to a full-screen
    /// program that asked for alternate scroll, and otherwise scrolls the scrollback, as
    /// xterm and Ghostty do.
    pub(super) fn wheel(&mut self, term: &mut Terminal<'_, '_>, input: WheelInput, size: Size, out: &mut VecDeque<u8>) -> Wheeled {
        if !term.is_mouse_tracking().unwrap_or(false) {
            let alternate = term.active_screen().is_ok_and(|s| s == EngineScreen::Alternate);
            if alternate && term.mode(Mode::ALT_SCROLL).unwrap_or(false) {
                let application = term.mode(Mode::DECCKM).unwrap_or(false);
                let arrow: &[u8] = match (input.lines < 0, application) {
                    (true, false) => b"\x1b[A",
                    (false, false) => b"\x1b[B",
                    (true, true) => b"\x1bOA",
                    (false, true) => b"\x1bOB",
                };
                for _ in 0..input.lines.unsigned_abs() {
                    out.extend(arrow);
                }
                return Wheeled::Arrows;
            }
            term.scroll_viewport(ScrollViewport::Delta(input.lines));
            return Wheeled::Scrollback;
        }
        self.mouse.set_options_from_terminal(term).set_size(mouse::EncoderSize {
            screen_width: u32::from(size.cols) * u32::from(size.cell_w),
            screen_height: u32::from(size.rows) * u32::from(size.cell_h),
            cell_width: u32::from(size.cell_w),
            cell_height: u32::from(size.cell_h),
            padding_top: 0,
            padding_bottom: 0,
            padding_right: 0,
            padding_left: 0,
        });
        // The encoder drops events without a position. Use the same cell coordinates Rune
        // draws at, converted to the engine's rounded cell dimensions.
        self.press.set_position(mouse::Position {
            x: (input.x * f32::from(size.cell_w)).clamp(0.0, f32::from(size.cols) * f32::from(size.cell_w) - 1.0),
            y: (input.y * f32::from(size.cell_h)).clamp(0.0, f32::from(size.rows) * f32::from(size.cell_h) - 1.0),
        });
        // A wheel has no release: xterm and Ghostty report each step as a lone press.
        let button = if input.lines < 0 { mouse::Button::Four } else { mouse::Button::Five };
        self.press.set_button(Some(button)).set_action(mouse::Action::Press).set_mods(input.mods);
        for _ in 0..input.lines.unsigned_abs() {
            self.bytes.clear();
            if self.mouse.encode_to_vec(&self.press, &mut self.bytes).is_ok() {
                out.extend(&self.bytes);
            }
        }
        // Don't leak the wheel's modifiers into the next event.
        self.press.set_mods(key::Mods::empty());
        Wheeled::Reported
    }
}

pub(super) fn engine_mods(m: rune::Mods) -> key::Mods {
    let mut mods = key::Mods::empty();
    mods.set(key::Mods::SHIFT, m.shift);
    mods.set(key::Mods::ALT, m.alt);
    mods.set(key::Mods::CTRL, m.ctrl);
    mods
}

/// What the engine needs to know about a key. `None` for the app's own: anything with Command.
pub(super) fn key_input(ev: &KeyEvent, tx: &Tx) -> Option<KeyInput> {
    if ev.mods.cmd {
        return None;
    }
    let mods = engine_mods(ev.mods);
    let special = |key| (key, None);
    let (key, typed) = match &ev.key {
        Key::Text(text) => (physical(tx.physical_key()), (!ev.mods.ctrl).then(|| text.clone())),
        Key::Enter => special(key::Key::Enter),
        Key::Backspace => special(key::Key::Backspace),
        Key::Delete => special(key::Key::Delete),
        Key::Left => special(key::Key::ArrowLeft),
        Key::Right => special(key::Key::ArrowRight),
        Key::Up => special(key::Key::ArrowUp),
        Key::Down => special(key::Key::ArrowDown),
        Key::Home => special(key::Key::Home),
        Key::End => special(key::Key::End),
        Key::PageUp => special(key::Key::PageUp),
        Key::PageDown => special(key::Key::PageDown),
        Key::Insert => special(key::Key::Insert),
        Key::Escape => special(key::Key::Escape),
        Key::Tab => special(key::Key::Tab),
        Key::Function(n) => special(function(*n)?),
        _ => return None,
    };
    let mut consumed = key::Mods::empty();
    if typed.is_some() {
        consumed.set(key::Mods::SHIFT, ev.mods.shift);
        consumed.set(key::Mods::ALT, ev.mods.alt);
    }
    Some(KeyInput { key, mods, consumed, unshifted: tx.key_base().unwrap_or('\0'), text: typed, repeat: tx.key_repeat() })
}

fn function(n: u8) -> Option<key::Key> {
    use key::Key::*;
    [F1, F2, F3, F4, F5, F6, F7, F8, F9, F10, F11, F12].get(usize::from(n).checked_sub(1)?).copied()
}

fn physical(code: Option<PhysicalKeyCode>) -> key::Key {
    use PhysicalKeyCode as P;
    use key::Key as K;
    match code {
        Some(P::KeyA) => K::A,
        Some(P::KeyB) => K::B,
        Some(P::KeyC) => K::C,
        Some(P::KeyD) => K::D,
        Some(P::KeyE) => K::E,
        Some(P::KeyF) => K::F,
        Some(P::KeyG) => K::G,
        Some(P::KeyH) => K::H,
        Some(P::KeyI) => K::I,
        Some(P::KeyJ) => K::J,
        Some(P::KeyK) => K::K,
        Some(P::KeyL) => K::L,
        Some(P::KeyM) => K::M,
        Some(P::KeyN) => K::N,
        Some(P::KeyO) => K::O,
        Some(P::KeyP) => K::P,
        Some(P::KeyQ) => K::Q,
        Some(P::KeyR) => K::R,
        Some(P::KeyS) => K::S,
        Some(P::KeyT) => K::T,
        Some(P::KeyU) => K::U,
        Some(P::KeyV) => K::V,
        Some(P::KeyW) => K::W,
        Some(P::KeyX) => K::X,
        Some(P::KeyY) => K::Y,
        Some(P::KeyZ) => K::Z,
        Some(P::Digit0) => K::Digit0,
        Some(P::Digit1) => K::Digit1,
        Some(P::Digit2) => K::Digit2,
        Some(P::Digit3) => K::Digit3,
        Some(P::Digit4) => K::Digit4,
        Some(P::Digit5) => K::Digit5,
        Some(P::Digit6) => K::Digit6,
        Some(P::Digit7) => K::Digit7,
        Some(P::Digit8) => K::Digit8,
        Some(P::Digit9) => K::Digit9,
        Some(P::Backquote) => K::Backquote,
        Some(P::Backslash) => K::Backslash,
        Some(P::BracketLeft) => K::BracketLeft,
        Some(P::BracketRight) => K::BracketRight,
        Some(P::Comma) => K::Comma,
        Some(P::Equal) => K::Equal,
        Some(P::Minus) => K::Minus,
        Some(P::Period) => K::Period,
        Some(P::Quote) => K::Quote,
        Some(P::Semicolon) => K::Semicolon,
        Some(P::Slash) => K::Slash,
        Some(P::Space) => K::Space,
        _ => K::Unidentified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::keep_scrollback;
    use crate::term::screen::Screen;

    #[test]
    fn wheel_reports_reach_a_mouse_tracking_program_at_the_pointer() {
        let mut term = Terminal::new(40, 20).unwrap();
        term.vt_write(b"\x1b[?1000h\x1b[?1006h");
        assert!(term.is_mouse_tracking().unwrap());
        assert_eq!(wheel(&mut term, -1, key::Mods::empty()), (Wheeled::Reported, b"\x1b[<64;21;11M".to_vec()));
    }

    #[test]
    fn wheel_reports_carry_modifiers() {
        let mut term = Terminal::new(40, 20).unwrap();
        term.vt_write(b"\x1b[?1000h\x1b[?1006h");
        // Wheel down (65) plus ctrl (16).
        assert_eq!(wheel(&mut term, 1, key::Mods::CTRL), (Wheeled::Reported, b"\x1b[<81;21;11M".to_vec()));
    }

    #[test]
    fn wheel_sends_arrows_to_full_screen_programs_without_mouse_tracking() {
        let mut term = Terminal::new(40, 20).unwrap();
        term.vt_write(b"\x1b[?1049h");
        assert_eq!(wheel(&mut term, -2, key::Mods::empty()), (Wheeled::Arrows, b"\x1b[A\x1b[A".to_vec()));
        assert_eq!(wheel(&mut term, 1, key::Mods::empty()), (Wheeled::Arrows, b"\x1b[B".to_vec()));
        // Application cursor keys, as less and vim set.
        term.vt_write(b"\x1b[?1h");
        assert_eq!(wheel(&mut term, -1, key::Mods::empty()), (Wheeled::Arrows, b"\x1bOA".to_vec()));
        // A program can turn alternate scroll off.
        term.vt_write(b"\x1b[?1007l");
        assert_eq!(wheel(&mut term, -1, key::Mods::empty()), (Wheeled::Scrollback, vec![]));
    }

    #[test]
    fn wheel_scrolls_the_scrollback_on_the_primary_screen() {
        let mut term = Terminal::new(40, 20).unwrap();
        keep_scrollback(&mut term).unwrap();
        for i in 0..100 {
            term.vt_write(format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(wheel(&mut term, -5, key::Mods::empty()), (Wheeled::Scrollback, vec![]));
        assert!(!term.viewport_active().unwrap());
    }

    fn page(term: &mut Terminal<'_, '_>, paging: Paging) -> (Paged, Vec<u8>) {
        let mut out = VecDeque::new();
        let size = Size { cols: 40, rows: 20, cell_w: 8, cell_h: 17 };
        let went = Keys::new().unwrap().page(term, paging, false, size, &mut out);
        (went, out.into())
    }

    fn first_row(term: &Terminal<'_, '_>) -> String {
        let frame = Screen::new().unwrap().frame(term).unwrap();
        frame.lines[0].runs.iter().map(|r| r.text.as_str()).collect::<String>().trim().to_owned()
    }

    #[test]
    fn paging_moves_through_the_scrollback_of_an_inline_program() {
        let mut term = Terminal::new(40, 20).unwrap();
        keep_scrollback(&mut term).unwrap();
        for i in 0..100 {
            term.vt_write(format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(first_row(&term), "line 81");
        assert_eq!(page(&mut term, Paging::PageUp), (Paged::Scrollback, vec![]));
        assert_eq!(first_row(&term), "line 62", "a page is the screen less one row");
        assert_eq!(page(&mut term, Paging::Top), (Paged::Scrollback, vec![]));
        assert_eq!(first_row(&term), "line 0");
        page(&mut term, Paging::PageDown);
        assert_eq!(first_row(&term), "line 19");
        page(&mut term, Paging::Bottom);
        assert_eq!(first_row(&term), "line 81");
        assert!(term.viewport_active().unwrap());
    }

    #[test]
    fn paging_a_full_screen_program_sends_it_the_keys() {
        let mut term = Terminal::new(40, 20).unwrap();
        term.vt_write(b"\x1b[?1049h");
        assert_eq!(page(&mut term, Paging::PageUp), (Paged::Keys, b"\x1b[5~".to_vec()));
        assert_eq!(page(&mut term, Paging::Top), (Paged::Keys, b"\x1b[1;5H".to_vec()));
    }

    fn wheel(term: &mut Terminal<'_, '_>, lines: isize, mods: key::Mods) -> (Wheeled, Vec<u8>) {
        let mut out = VecDeque::new();
        let size = Size { cols: 40, rows: 20, cell_w: 8, cell_h: 17 };
        let went = Keys::new().unwrap().wheel(term, WheelInput { lines, x: 20.5, y: 10.5, mods }, size, &mut out);
        (went, out.into())
    }

    fn typed(key: key::Key, mods: key::Mods, text: Option<&str>, unshifted: char) -> Vec<u8> {
        let term = Terminal::new(10, 2).unwrap();
        let mut keys = Keys::new().unwrap();
        let mut out = VecDeque::new();
        let input = KeyInput { key, mods, consumed: key::Mods::empty(), unshifted, text: text.map(str::to_owned), repeat: false };
        keys.key(&term, &input, &mut out);
        out.into()
    }

    #[test]
    fn keys_reach_the_program_as_a_terminal_sends_them() {
        assert_eq!(typed(key::Key::A, key::Mods::empty(), Some("a"), 'a'), b"a");
        assert_eq!(typed(key::Key::C, key::Mods::CTRL, None, 'c'), [3]);
        assert_eq!(typed(key::Key::Enter, key::Mods::empty(), None, '\0'), b"\r");
        assert_eq!(typed(key::Key::ArrowUp, key::Mods::empty(), None, '\0'), b"\x1b[A");
        assert_eq!(typed(key::Key::Tab, key::Mods::SHIFT, None, '\0'), b"\x1b[Z");
    }

    #[test]
    fn paging_keys_reach_the_program_as_page_up_down_and_ctrl_home_end() {
        let sent = |paging: Paging| {
            let input = paging.input();
            typed(input.key, input.mods, None, '\0')
        };
        assert_eq!(sent(Paging::PageUp), b"\x1b[5~");
        assert_eq!(sent(Paging::PageDown), b"\x1b[6~");
        assert_eq!(sent(Paging::Top), b"\x1b[1;5H");
        assert_eq!(sent(Paging::Bottom), b"\x1b[1;5F");
    }
}
