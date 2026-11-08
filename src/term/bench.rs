//! Terminal timings: `cargo test --release bench_term -- --ignored --nocapture`.

use std::hint::black_box;
use std::time::Instant;

use libghostty_vt::Terminal;
use rune::Color;
use rune_kit::Kit;

use super::screen::Screen;
use super::{keep_scrollback, view};

#[test]
#[ignore = "benchmark"]
fn bench_term() {
    let (cols, rows) = (200, 60);
    let mut term = Terminal::new(cols, rows).unwrap();
    keep_scrollback(&mut term).unwrap();
    // Coloured output like a build log or `ls --color`: 200 bytes a line.
    let mut bytes = Vec::new();
    for i in 0..200_000 {
        bytes.extend_from_slice(format!("\x1b[3{}m{i:>8}\x1b[0m \x1b[1mbold\x1b[0m {}\r\n", i % 8, "x".repeat(170)).as_bytes());
    }
    let started = Instant::now();
    for chunk in bytes.chunks(1 << 16) {
        term.vt_write(chunk);
    }
    let seconds = started.elapsed().as_secs_f64();
    println!("vt_write {} MB coloured: {:.0} ms, {:.0} MB/s", bytes.len() >> 20, seconds * 1000.0, bytes.len() as f64 / seconds / 1e6);

    let mut screen = Screen::new().unwrap();
    let runs = 500;
    let started = Instant::now();
    let mut frame = None;
    for _ in 0..runs {
        frame = screen.frame(&term);
    }
    println!("Screen::frame {cols}x{rows}: {:.1} µs", started.elapsed().as_secs_f64() * 1e6 / f64::from(runs));

    let frame = frame.unwrap();
    let style = view::style(&Kit::neutral(true));
    let started = Instant::now();
    for _ in 0..runs {
        black_box(view::screen(Some(&frame), style, (8.0, 16.0), true, Color::WHITE));
    }
    println!("view::screen {cols}x{rows}: {:.1} µs", started.elapsed().as_secs_f64() * 1e6 / f64::from(runs));
}
