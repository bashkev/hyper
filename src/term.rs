//! A real terminal in a pane: a program on a PTY, `libghostty-vt` reading what it prints, and
//! Rune drawing the result.
//!
//! The engine's types can't leave their thread, so each terminal has one thread that owns the
//! engine and the PTY. It sleeps in `poll` until the program prints or the UI pokes it, and hands
//! the UI plain [`Frame`]s. A frame is only built once the UI has taken the last one, so a flood
//! of output costs one frame per draw, not one per read.
//!
//! - [`pty`]: starting the program on its PTY, through the login shell.
//! - [`reap`]: hearing how programs end, across a reload.
//! - [`screen`]: reading the engine's screen into a [`Frame`].
//! - [`keys`]: keys, paging and the wheel, encoded as the program wants them.
//! - [`selection`]: selecting with the pointer, and copying it.
//! - [`find`]: finding text, scrollback included.
//! - [`view`]: drawing a frame.

mod find;
mod keys;
mod links;
mod pty;
mod reap;
mod screen;
mod selection;
mod view;
#[cfg(test)]
mod bench;

pub use find::{Found, Toward};
pub use pty::{login_shell, shell_command};
pub use view::{Marks, cell, screen, style};

use find::Finding;
use keys::{Keys, engine_mods, key_input};
use pty::{pipe, set_flags, spawn, winsize};
use screen::{IMAGE_DECODED, IMAGE_WAKE, Screen};
use selection::{Selecting, selected_text};

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::hash::Hash;
use std::io::Write as _;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, Once, PoisonError};
use std::time::Duration;

use libghostty_vt::kitty::graphics;
use libghostty_vt::snapshot::Decoder;
use libghostty_vt::style::RgbColor;
use libghostty_vt::terminal::{Mode, ScrollViewport};
use libghostty_vt::{Terminal, focus, key, paste};
use rune::{Color, Drag, Image, KeyEvent, Phase, Tx, Waker, WheelEvent};

/// Space between the pane's edge and the cells.
pub const INSET: f32 = 10.0;
/// A program that starts a synchronized update and never ends it can't freeze the screen for longer.
const HOLD_LIMIT_MS: libc::c_int = 1000;
/// What one terminal keeps of the images programs send it.
const IMAGE_STORAGE: u64 = 64 << 20;
/// The longest escape sequence or character a snapshot can carry half-read.
const CONTINUATION: usize = 64 << 10;
/// How long a terminal has to hand itself over before a reload goes on without it.
pub const HANDOFF_WAIT: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl From<RgbColor> for Rgb {
    fn from(c: RgbColor) -> Rgb {
        Rgb(c.r, c.g, c.b)
    }
}

impl Rgb {
    pub fn color(self) -> Color {
        rune::color::hex(&format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)).unwrap_or(Color { r: 0.0, g: 0.0, b: 0.0, a: 1.0 })
    }

    fn engine(self) -> RgbColor {
        RgbColor { r: self.0, g: self.1, b: self.2 }
    }
}

/// Text in one style, starting at a column.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Run {
    pub col: u16,
    pub text: String,
    pub fg: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub underline: bool,
}

/// A stretch of cells with a background of their own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Fill {
    pub col: u16,
    pub width: u16,
    pub color: Rgb,
}

/// Cells a program linked to a URL (OSC 8), from `start` up to `end`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Link {
    pub start: u16,
    pub end: u16,
    pub url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Line {
    pub fills: Vec<Fill>,
    pub runs: Vec<Run>,
    pub selected: Option<(u16, u16)>,
    /// Matches of what's being found, first and last column.
    pub found: Vec<(u16, u16)>,
    /// The part of the selected match on this line.
    pub current: Option<(u16, u16)>,
    pub links: Vec<Link>,
    /// The terminal wrapped this line onto the next one.
    pub wrapped: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Block,
    Hollow,
    Bar,
    Underline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caret {
    pub col: u16,
    pub row: u16,
    pub shape: Shape,
}

/// A program's image on screen. `x`, `y`, `w` and `h` are the part inside the grid; `whole` is
/// the placement, which can run past it, so scrolling it moves the same image rather than cutting
/// a new one. Positions are in the engine's pixels, whose cells are whole numbers: the view
/// scales them to the measured cell.
#[derive(Clone, Debug)]
pub struct Picture {
    pub image: Image,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub whole: [f32; 4],
    pub above_text: bool,
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub lines: Vec<Line>,
    /// Cells across each line.
    pub cols: u16,
    pub pictures: Vec<Picture>,
    pub caret: Option<Caret>,
    pub fg: Rgb,
    pub bg: Rgb,
    /// How finding is going, while something is being found.
    pub found: Option<Found>,
}

/// What the program asked of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Bell,
    Notification { title: String, body: String },
    Exited(Option<i32>),
    Copy(String),
}

pub struct KeyInput {
    key: key::Key,
    mods: key::Mods,
    consumed: key::Mods,
    unshifted: char,
    text: Option<String>,
    repeat: bool,
}

/// Keys that full-screen programs page through their own view with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paging {
    PageUp,
    PageDown,
    /// ⌃Home.
    Top,
    /// ⌃End.
    Bottom,
}

impl Paging {
    fn input(self) -> KeyInput {
        let (key, mods) = match self {
            Paging::PageUp => (key::Key::PageUp, key::Mods::empty()),
            Paging::PageDown => (key::Key::PageDown, key::Mods::empty()),
            Paging::Top => (key::Key::Home, key::Mods::CTRL),
            Paging::Bottom => (key::Key::End, key::Mods::CTRL),
        };
        KeyInput { key, mods, consumed: key::Mods::empty(), unshifted: '\0', text: None, repeat: false }
    }
}

enum Input {
    Key(KeyInput),
    Page(Paging, bool),
    Text(String),
    Paste(String),
    Wheel(WheelInput),
    Resize(Size),
    Focus(bool),
    Select(SelectionInput),
    Copy,
    /// What to find. Empty stops finding.
    Find(String),
    FindNext(Toward),
    Stop,
    /// Save the screen to this file and give up the PTY without stopping the program.
    Handoff(PathBuf, Sender<Handed>),
}

#[derive(Clone, Copy)]
struct WheelInput {
    /// Lines to scroll, up negative.
    lines: isize,
    /// Pointer position in cell units, as for selection.
    x: f32,
    y: f32,
    mods: key::Mods,
}

/// Where a paging command went.
#[derive(Debug, PartialEq)]
enum Paged {
    /// The viewport moved through the scrollback.
    Scrollback,
    /// Sent as a key to the full-screen program, which has no scrollback here.
    Keys,
}

/// Where a wheel's lines went.
#[derive(Debug, PartialEq)]
enum Wheeled {
    /// The viewport moved through the scrollback.
    Scrollback,
    /// Reported to a program that reads the mouse.
    Reported,
    /// Sent as arrow keys to a full-screen program that doesn't (alternate scroll, DECSET 1007).
    Arrows,
}

#[derive(Clone, Copy)]
struct SelectionInput {
    phase: Phase,
    /// Position in cell units, preserving the fraction within a cell.
    x: f32,
    y: f32,
    rectangle: bool,
    cancelled: bool,
    clicks: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Size {
    cols: u16,
    rows: u16,
    cell_w: u16,
    cell_h: u16,
}

impl Size {
    fn to_array(self) -> [u16; 4] {
        [self.cols, self.rows, self.cell_w, self.cell_h]
    }

    fn from_array([cols, rows, cell_w, cell_h]: [u16; 4]) -> Size {
        Size { cols: cols.max(2), rows: rows.max(1), cell_w: cell_w.max(1), cell_h: cell_h.max(1) }
    }
}

/// A terminal being given up, by [`Term::handoff`].
pub enum Handoff {
    Done(Handed),
    Asked(Receiver<Handed>, [u16; 4]),
}

impl Handoff {
    /// How it was given up. None if it didn't answer by `deadline`.
    pub fn wait(self, deadline: std::time::Instant) -> Option<Handed> {
        let (answer, size) = match self {
            Handoff::Done(handed) => return Some(handed),
            Handoff::Asked(answer, size) => (answer, size),
        };
        match answer.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())) {
            Ok(handed) => {
                // Only now that it's surely handed over may it outlive `exec`.
                if let Handed::Running { fd: Some(fd), .. } = handed {
                    unsafe { libc::fcntl(fd, libc::F_SETFD, 0) };
                }
                Some(handed)
            }
            // Its thread already ended, as when its program couldn't start: only the terminal is left.
            Err(mpsc::RecvTimeoutError::Disconnected) => Some(Handed::Running { fd: None, pid: None, size, saved: false, pending: false }),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
        }
    }
}

/// A terminal given up for the next build of the app, which keeps its program running.
#[derive(Debug)]
pub enum Handed {
    /// It was never laid out, so its program never started.
    Unstarted { argv: Vec<String> },
    Running {
        /// The PTY, left open across `exec`. None once the program has ended.
        fd: Option<RawFd>,
        pid: Option<libc::pid_t>,
        /// Columns, rows and the cell's pixels.
        size: [u16; 4],
        /// The screen and scrollback were saved to the file asked for.
        saved: bool,
        /// Input the program hadn't read yet was saved next to it, by [`pending_path`].
        pending: bool,
    },
}

/// Where a point in the terminal's element falls on the grid, in cells.
fn grid_point(x: f32, y: f32, (cell_w, cell_h): (f32, f32)) -> (f32, f32) {
    ((x - INSET) / cell_w, (y - INSET) / cell_h)
}

/// Where a handed-over terminal keeps the input its program hadn't read yet.
pub fn pending_path(snapshot: &Path) -> PathBuf {
    snapshot.with_extension("pending")
}

/// A program already running on a PTY, taken over from the build before this one.
pub struct Adopt {
    pub fd: Option<RawFd>,
    pub pid: Option<libc::pid_t>,
    pub size: [u16; 4],
    /// The engine's snapshot of the screen and scrollback.
    pub snapshot: Option<Vec<u8>>,
    /// Input still to write to the program.
    pub pending: Vec<u8>,
}

#[derive(Default)]
struct Shared {
    frame: Mutex<Option<Frame>>,
    events: Mutex<Vec<Event>>,
    /// The UI has taken the last frame.
    wanted: AtomicBool,
    /// The program's pid once it's running, else 0.
    pid: AtomicI32,
}

struct Launch {
    argv: Vec<String>,
    cwd: PathBuf,
    dark: bool,
    adopt: Option<Adopt>,
}

/// The UI's end of a terminal.
pub struct Term {
    launch: Option<Launch>,
    wake: Waker,
    shared: Arc<Shared>,
    inputs: Sender<Input>,
    receiver: Option<Receiver<Input>>,
    poke: Option<OwnedFd>,
    size: Option<Size>,
    /// Points of wheel travel not yet worth a line.
    wheel: f32,
    pub frame: Option<Frame>,
}

impl Term {
    /// The program starts at the first [`Term::layout`], when its size is known.
    pub fn new(argv: Vec<String>, cwd: PathBuf, dark: bool, wake: Waker) -> Term {
        let (inputs, receiver) = mpsc::channel();
        Term {
            launch: Some(Launch { argv, cwd, dark, adopt: None }),
            wake,
            shared: Arc::new(Shared { wanted: AtomicBool::new(true), ..Shared::default() }),
            inputs,
            receiver: Some(receiver),
            poke: None,
            size: None,
            wheel: 0.0,
            frame: None,
        }
    }

    /// Takes over a program the build before this one was running. It starts reading at once,
    /// at the size it had, so a program behind another terminal can't fill its PTY and stall.
    pub fn adopt(adopt: Adopt, cwd: PathBuf, dark: bool, wake: Waker) -> Term {
        let size = Size::from_array(adopt.size);
        let mut term = Term::new(Vec::new(), cwd, dark, wake);
        if let Some(launch) = &mut term.launch {
            launch.adopt = Some(adopt);
        }
        term.size = Some(size);
        if let (Some(launch), Some(inputs)) = (term.launch.take(), term.receiver.take()) {
            term.start(launch, inputs, size);
        }
        term
    }

    /// Asks the terminal to give itself up for the next build: to save its screen to `snapshot`
    /// and leave its PTY open across `exec`, without stopping the program. Asking every terminal
    /// before waiting on any lets them answer together.
    pub fn handoff(&mut self, snapshot: &Path) -> Handoff {
        if let Some(launch) = &self.launch {
            return Handoff::Done(Handed::Unstarted { argv: launch.argv.clone() });
        }
        let (reply, answer) = mpsc::channel();
        self.send(Input::Handoff(snapshot.to_owned(), reply));
        Handoff::Asked(answer, self.size.map_or([80, 24, 8, 17], Size::to_array))
    }

    pub fn layout(&mut self, width: f32, height: f32, (cell_w, cell_h): (f32, f32)) {
        let size = Size {
            cols: ((width / cell_w).floor() as u16).max(2),
            rows: ((height / cell_h).floor() as u16).max(1),
            cell_w: cell_w.ceil() as u16,
            cell_h: cell_h.ceil() as u16,
        };
        if self.size == Some(size) {
            return;
        }
        self.size = Some(size);
        if let (Some(launch), Some(inputs)) = (self.launch.take(), self.receiver.take()) {
            self.start(launch, inputs, size);
        } else {
            self.send(Input::Resize(size));
        }
    }

    fn start(&mut self, launch: Launch, inputs: Receiver<Input>, size: Size) {
        decode_png();
        let (shared, wake) = (self.shared.clone(), self.wake.clone());
        *IMAGE_WAKE.lock().unwrap_or_else(PoisonError::into_inner) = Some(wake.clone());
        let Some((poke, poked)) = pipe() else {
            shared.events.lock().unwrap().push(Event::Exited(None));
            return wake();
        };
        let reaper = poke.try_clone().ok();
        self.poke = Some(poke);
        let _ = std::thread::Builder::new().name("terminal".into()).spawn(move || {
            if reaper.and_then(|reaper| run(&launch, size, inputs, poked, reaper, &shared, &wake)).is_none() {
                shared.events.lock().unwrap().push(Event::Exited(None));
                wake();
            }
        });
    }

    fn send(&self, input: Input) {
        if self.inputs.send(input).is_ok() {
            self.poke();
        }
    }

    fn poke(&self) {
        if let Some(fd) = &self.poke {
            unsafe { libc::write(fd.as_raw_fd(), [0u8].as_ptr().cast(), 1) };
        }
    }

    /// Takes the newest frame, if there is one, and asks for the next. A terminal that isn't
    /// on screen isn't asked, so it builds no frames.
    pub fn take_frame(&mut self) -> bool {
        let frame = self.shared.frame.lock().unwrap().take();
        // `poll` runs every frame of any animation, so only a change of mind wakes the thread.
        if !self.shared.wanted.swap(true, Ordering::AcqRel) {
            self.poke();
        }
        // A picture that finished decoding shows on the next draw.
        let changed = frame.is_some() || IMAGE_DECODED.swap(false, Ordering::AcqRel);
        if frame.is_some() {
            self.frame = frame;
        }
        changed
    }

    /// The program's pid, once it has started. It stays after the program ends.
    pub fn pid(&self) -> Option<libc::pid_t> {
        Some(self.shared.pid.load(Ordering::Acquire)).filter(|&pid| pid > 0)
    }

    /// Says the program ended, as when the build before this one saw it end but its UI didn't.
    pub fn exited(&self, code: Option<i32>) {
        self.shared.events.lock().unwrap().push(Event::Exited(code));
        (self.wake)();
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut *self.shared.events.lock().unwrap())
    }

    /// True when the terminal took the key.
    pub fn key(&self, ev: &KeyEvent, tx: &Tx) -> bool {
        match key_input(ev, tx) {
            Some(input) => {
                self.send(Input::Key(input));
                true
            }
            None => false,
        }
    }

    pub fn text(&self, text: &str) {
        self.send(Input::Text(text.to_owned()));
    }

    /// Pages through the scrollback, or presses the paging key for a full-screen program.
    pub fn page(&self, paging: Paging, repeat: bool) {
        self.send(Input::Page(paging, repeat));
    }

    /// Whether the window has the keyboard. Programs use this to decide whether to notify.
    pub fn focus(&self, focused: bool) {
        self.send(Input::Focus(focused));
    }

    pub fn paste(&self, text: &str) {
        self.send(Input::Paste(text.to_owned()));
    }

    pub fn select(&self, drag: &Drag, cell: (f32, f32), rectangle: bool) {
        let (x, y) = grid_point(drag.x, drag.y, cell);
        self.send(Input::Select(SelectionInput {
            phase: drag.phase,
            x,
            y,
            rectangle,
            cancelled: drag.cancelled(),
            clicks: drag.clicks,
        }));
    }

    /// The link at a point in the terminal's element, as the screen shows it.
    pub fn link_at(&self, x: f32, y: f32, cell: (f32, f32)) -> Option<String> {
        let (col, row) = grid_point(x, y, cell);
        if col < 0.0 || row < 0.0 {
            return None;
        }
        links::link_at(self.frame.as_ref()?, col as u16, row as usize)
    }

    pub fn copy(&self) {
        self.send(Input::Copy);
    }

    /// Marks where `needle` is, and goes to the newest match. Empty stops finding.
    pub fn find(&self, needle: &str) {
        self.send(Input::Find(needle.to_owned()));
    }

    pub fn find_next(&self, toward: Toward) {
        self.send(Input::FindNext(toward));
    }

    pub fn wheel(&mut self, ev: WheelEvent, cell: (f32, f32)) {
        self.wheel += ev.dy;
        let lines = (self.wheel / cell.1).trunc();
        if lines != 0.0 {
            self.wheel -= lines * cell.1;
            // Content moving down shows what's above, so it's the negative direction.
            self.send(Input::Wheel(WheelInput {
                lines: -(lines as isize),
                x: (ev.x - INSET) / cell.0,
                y: (ev.y - INSET) / cell.1,
                mods: engine_mods(ev.mods),
            }));
        }
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        self.send(Input::Stop);
    }
}

fn keep_scrollback(term: &mut Terminal) -> libghostty_vt::error::Result<()> {
    // libghostty also caps scrollback at 10 KB by default, which would win over the line limit.
    term.set_scrollback_max_bytes(None)?.set_scrollback_max_lines(Some(10_000))?;
    Ok(())
}

/// libghostty keeps one PNG decoder for the process. The first terminal installs it before its
/// thread starts, while no other thread can be inside libghostty.
fn decode_png() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = unsafe { graphics::set_png_decoder(Some(Box::new(graphics::RustPngDecoder::new()))) };
    });
}

/// The Kitty graphics protocol, which the engine turns away until it has somewhere to keep images.
fn allow_images(term: &mut Terminal) -> libghostty_vt::error::Result<()> {
    term.set_kitty_image_storage_limit(IMAGE_STORAGE)?
        .set_kitty_image_from_file_allowed(true)?
        .set_kitty_image_temp_file_dir(Some(&std::env::temp_dir()))?
        .set_kitty_image_from_shared_mem_allowed(true)?;
    Ok(())
}

/// Writes the engine's whole state, both screens and the scrollback, to `path`.
fn save(term: &mut Terminal, path: &Path) -> bool {
    let Ok(file) = std::fs::File::create(path) else { return false };
    let mut out = std::io::BufWriter::new(file);
    term.encode_snapshot(&mut out).is_ok() && out.flush().is_ok()
}

/// A terminal as [`save`] left it. None if this build's engine can't read it.
fn restore<'cb>(snapshot: &[u8]) -> Option<Terminal<'static, 'cb>> {
    let mut decoder = Decoder::new_buf(snapshot).ok()?;
    decoder.set_kitty_image_storage_limit(IMAGE_STORAGE).ok()?;
    // Tracking turned on after decoding can't recover a sequence the snapshot left unfinished,
    // and the next snapshot would then fail.
    decoder.set_max_continuation_bytes(CONTINUATION).ok()?.set_retain_continuation(true).ok()?;
    decoder.decode().ok()
}
/// The terminal thread. Returns the program's exit code.
fn run(launch: &Launch, size: Size, inputs: Receiver<Input>, poked: OwnedFd, reaper: OwnedFd, shared: &Shared, wake: &Waker) -> Option<()> {
    let (fg, bg) = if launch.dark { (Rgb(0xe6, 0xe6, 0xe6), Rgb(0x1a, 0x1a, 0x1a)) } else { (Rgb(0x1f, 0x1f, 0x1f), Rgb(0xff, 0xff, 0xff)) };

    let replies: Rc<RefCell<VecDeque<u8>>> = Rc::default();
    let held = Rc::new(Cell::new(false));
    let adopt = launch.adopt.as_ref();
    // What the build before couldn't write before handing over goes first.
    replies.borrow_mut().extend(adopt.map_or(&[][..], |a| &a.pending));
    let restored = adopt.and_then(|a| a.snapshot.as_deref()).and_then(restore);
    let redraw = adopt.is_some() && restored.is_none();
    let mut term = match restored {
        Some(term) => term,
        None => Terminal::new(size.cols, size.rows).ok()?,
    };
    // A snapshot can only be taken in the middle of an escape sequence if the engine kept it.
    term.set_continuation_max_bytes(CONTINUATION).ok()?;
    term.resize(size.cols, size.rows, u32::from(size.cell_w), u32::from(size.cell_h)).ok()?;
    term.set_default_fg_color(Some(fg.engine())).ok()?;
    term.set_default_bg_color(Some(bg.engine())).ok()?;
    keep_scrollback(&mut term).ok()?;
    allow_images(&mut term).ok()?;
    let (out, hold) = (replies.clone(), held.clone());
    term.on_pty_write(move |_, data| out.borrow_mut().extend(data))
        .ok()?
        .on_render_hold(move |_, on| hold.set(on))
        .ok()?
        .on_bell(|_| {
            shared.events.lock().unwrap().push(Event::Bell);
            wake();
        })
        .ok()?
        .on_desktop_notification(|_, n| {
            let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
            shared.events.lock().unwrap().push(Event::Notification { title: text(n.title()), body: text(n.body()) });
            wake();
        })
        .ok()?;

    let mut screen = Screen::new()?;
    let (master, group) = match adopt {
        // A program that had already ended left only its screen to read.
        Some(adopt) => (adopt.fd.map(|fd| {
            set_flags(fd);
            unsafe { OwnedFd::from_raw_fd(fd) }
        }), adopt.pid.filter(|_| adopt.fd.is_some())),
        None => match spawn(launch, size) {
            // Its exit is waited for by pid, so the `Child` isn't needed.
            Ok((master, child)) => (Some(master), Some(child.id() as libc::pid_t)),
            Err(e) => {
                // There's no program to print it, and the terminal is the only place it can be seen.
                // It stays as an ended program's terminal does, so a reload keeps the message.
                term.vt_write(format!("{e}: {}\r\n", launch.cwd.display()).as_bytes());
                shared.events.lock().unwrap().push(Event::Exited(None));
                wake();
                (None, None)
            }
        },
    };
    // The program ending says it's over, not the PTY closing: a background job it left behind
    // can hold the PTY open for as long as it likes.
    if let Some(pid) = group {
        shared.pid.store(pid, Ordering::Release);
        reap::watch(pid, reaper);
    }
    let (fd, poked) = (master.as_ref().map_or(-1, AsRawFd::as_raw_fd), poked.as_raw_fd());
    if redraw && fd >= 0 {
        // The screen couldn't be restored, so ask the program to draw it again.
        let foreground = unsafe { libc::tcgetpgrp(fd) };
        unsafe { libc::killpg(if foreground > 0 { foreground } else { group.unwrap_or(-1) }, libc::SIGWINCH) };
    }
    let mut keys = Keys::new()?;
    let mut selection = Selecting::new()?;
    let mut finding = Finding::default();
    let mut size = size;
    let mut dirty = true;
    let mut closed = group.is_none();
    let mut ended = group.is_none();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let mut fds = [
            // A negative descriptor is skipped, so a closed PTY can't spin this loop.
            libc::pollfd { fd: if closed { -1 } else { fd }, events: libc::POLLIN | if replies.borrow().is_empty() { 0 } else { libc::POLLOUT }, revents: 0 },
            libc::pollfd { fd: poked, events: libc::POLLIN, revents: 0 },
        ];
        let timeout = if held.get() { HOLD_LIMIT_MS } else { -1 };
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };
        if ready == 0 {
            held.set(false);
        }
        if fds[1].revents != 0 {
            while unsafe { libc::read(poked, buf.as_mut_ptr().cast(), buf.len()) } > 0 {}
            while let Ok(input) = inputs.try_recv() {
                match input {
                    Input::Stop => {
                        if let Some(group) = group.filter(|_| !ended) {
                            unsafe { libc::killpg(group, libc::SIGHUP) };
                        }
                        if let Some(pid) = group {
                            reap::forget(pid);
                        }
                        selection.gesture.reset(&term);
                        return Some(());
                    }
                    Input::Handoff(path, reply) => {
                        selection.gesture.reset(&term);
                        flush(fd, &mut replies.borrow_mut());
                        let saved = save(&mut term, &path);
                        let unsent: Vec<u8> = std::mem::take(&mut *replies.borrow_mut()).into();
                        let pending = !ended && !unsent.is_empty() && std::fs::write(pending_path(&path), &unsent).is_ok();
                        if let Some(pid) = group {
                            reap::release(pid);
                            // Reaped, but not yet heard of here: the next build couldn't learn how.
                            if let Some(code) = reap::exit(pid).filter(|_| !ended) {
                                ended = true;
                                shared.events.lock().unwrap().push(Event::Exited(code));
                            }
                        }
                        let master = master.filter(|_| !ended);
                        let handed = Handed::Running { fd: master.as_ref().map(AsRawFd::as_raw_fd), pid: group, size: size.to_array(), saved, pending };
                        if reply.send(handed).is_ok() {
                            // Whoever took it closes it.
                            let _ = master.map(IntoRawFd::into_raw_fd);
                            return Some(());
                        }
                        // Answered too late: the app went on without it, so it stops as a closed one does.
                        if let Some(group) = group.filter(|_| !ended) {
                            unsafe { libc::killpg(group, libc::SIGHUP) };
                        }
                        if let Some(pid) = group {
                            reap::forget(pid);
                        }
                        return Some(());
                    }
                    Input::Resize(new) => {
                        size = new;
                        let ws = winsize(new);
                        unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) };
                        let _ = term.resize(new.cols, new.rows, u32::from(new.cell_w), u32::from(new.cell_h));
                        dirty = true;
                    }
                    Input::Wheel(input) => match keys.wheel(&mut term, input, size, &mut replies.borrow_mut()) {
                        Wheeled::Scrollback => dirty = true,
                        // The program redraws, so a selection would cover what it moved away.
                        Wheeled::Arrows => {
                            selection.clear(&term);
                            dirty = true;
                        }
                        Wheeled::Reported => {}
                    },
                    Input::Page(paging, repeat) => {
                        if keys.page(&mut term, paging, repeat, size, &mut replies.borrow_mut()) == Paged::Keys {
                            selection.clear(&term);
                        }
                        dirty = true;
                    }
                    Input::Key(input) => {
                        // Typing returns to the live screen, as every terminal does.
                        term.scroll_viewport(ScrollViewport::Bottom);
                        selection.clear(&term);
                        dirty = true;
                        keys.key(&term, &input, &mut replies.borrow_mut());
                    }
                    Input::Focus(focused) => {
                        if term.mode(Mode::FOCUS_EVENT).unwrap_or(false) {
                            let event = if focused { focus::Event::Gained } else { focus::Event::Lost };
                            let mut bytes = [0u8; 8];
                            if let Ok(n) = event.encode(&mut bytes) {
                                replies.borrow_mut().extend(&bytes[..n]);
                            }
                        }
                    }
                    Input::Text(text) => {
                        selection.clear(&term);
                        term.scroll_viewport(ScrollViewport::Bottom);
                        dirty = true;
                        replies.borrow_mut().extend(text.as_bytes());
                    }
                    Input::Paste(text) => {
                        selection.clear(&term);
                        term.scroll_viewport(ScrollViewport::Bottom);
                        dirty = true;
                        paste_into(&term, text, &mut replies.borrow_mut());
                    }
                    Input::Select(input) => {
                        let _ = selection.apply(&term, input, size);
                        dirty = true;
                    }
                    Input::Copy => {
                        if let Some(text) = selected_text(&term) {
                            shared.events.lock().unwrap().push(Event::Copy(text));
                            wake();
                        }
                    }
                    Input::Find(needle) => {
                        let _ = finding.set(&mut term, &needle);
                        dirty = true;
                    }
                    Input::FindNext(toward) => {
                        let _ = finding.step(&mut term, toward);
                        dirty = true;
                    }
                }
            }
        }
        if fds[0].revents & libc::POLLOUT != 0 || !replies.borrow().is_empty() {
            flush(fd, &mut replies.borrow_mut());
        }
        if fds[0].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                term.vt_write(&buf[..n as usize]);
                dirty = true;
            } else if n == 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::WouldBlock {
                closed = true;
            }
        }
        let code = group.and_then(reap::exit);
        if let Some(code) = code.filter(|_| !ended) {
            // What it printed last is what a failed program left to read.
            loop {
                let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    break;
                }
                term.vt_write(&buf[..n as usize]);
            }
            // Keep the engine for selecting and copying the output of a failed program.
            ended = true;
            closed = true;
            dirty = true;
            held.set(false);
            shared.events.lock().unwrap().push(Event::Exited(code));
            wake();
        }
        if dirty && !held.get() && shared.wanted.swap(false, Ordering::AcqRel) {
            // Before reading the screen, as going to a match can scroll it. What's left is searched
            // for the next frame, which the UI asks for once it has taken this one.
            dirty = finding.advance(&mut term).unwrap_or(false);
            if let Some(mut frame) = screen.frame(&term) {
                let _ = finding.mark(&mut term, &mut frame);
                *shared.frame.lock().unwrap() = Some(frame);
                wake();
            }
        }
    }
}

fn flush(fd: RawFd, out: &mut VecDeque<u8>) {
    while !out.is_empty() {
        let (bytes, _) = out.as_slices();
        let n = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if n <= 0 {
            break;
        }
        out.drain(..n as usize);
    }
}

fn paste_into(term: &Terminal<'_, '_>, text: String, out: &mut VecDeque<u8>) {
    let bracketed = term.mode(Mode::BRACKETED_PASTE).unwrap_or(false);
    let mut data = text.into_bytes();
    let mut encoded = vec![0u8; data.len() + 32];
    if let Ok(n) = paste::encode(&mut data, bracketed, &mut encoded) {
        out.extend(&encoded[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reload in the middle of an escape sequence, then another before it ends, keeps the screen.
    #[test]
    fn unfinished_sequence_survives_two_reloads() {
        let dir = std::env::temp_dir().join(format!("hyper-continuation-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut term = Terminal::new(80, 24).unwrap();
        term.set_continuation_max_bytes(CONTINUATION).unwrap();
        term.vt_write(b"kept\r\n\x1b]2;half a ti");
        assert!(save(&mut term, &dir.join("1")));
        let mut term = restore(&std::fs::read(dir.join("1")).unwrap()).unwrap();
        term.set_continuation_max_bytes(CONTINUATION).unwrap();
        assert!(save(&mut term, &dir.join("2")));
        let mut term = restore(&std::fs::read(dir.join("2")).unwrap()).unwrap();
        term.vt_write(b"tle\x07done");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(term.title().unwrap(), b"half a title");
    }

    #[test]
    fn scrollback_keeps_thousands_of_lines() {
        let mut term = Terminal::new(80, 24).unwrap();
        keep_scrollback(&mut term).unwrap();
        for i in 0..3000 {
            term.vt_write(format!("line {i}\r\n").as_bytes());
        }
        assert!(term.scrollback_rows().unwrap() >= 2900);
    }

    #[test]
    fn asking_for_a_frame_already_asked_for_doesnt_wake_the_thread() {
        let (poke, poked) = pipe().unwrap();
        let mut term = Term::new(Vec::new(), PathBuf::new(), true, Arc::new(|| {}));
        term.poke = Some(poke);
        term.take_frame();
        term.take_frame();
        let mut buf = [0u8; 8];
        assert_eq!(unsafe { libc::read(poked.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) }, -1, "nothing was written");
    }

    #[test]
    fn a_paste_is_bracketed_only_when_the_program_asked() {
        let mut term = Terminal::new(10, 2).unwrap();
        let mut out = VecDeque::new();
        paste_into(&term, "hi".into(), &mut out);
        assert_eq!(out.make_contiguous(), b"hi");
        term.vt_write(b"\x1b[?2004h");
        out.clear();
        paste_into(&term, "hi".into(), &mut out);
        assert_eq!(out.make_contiguous(), b"\x1b[200~hi\x1b[201~");
    }
}
