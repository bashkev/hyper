//! Timings on the person's own sessions, run with
//! `cargo test --release bench_ -- --ignored --nocapture --test-threads=1`.
//! The terminal's are in `term/bench.rs`.

use std::hint::black_box;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

use crate::data::{self, Roots, Session, Source};
use crate::usage::Prices;
use crate::{config, settings};

/// Prints the fastest and median of `runs` calls to `f`, and returns the last result.
fn time<T>(name: &str, runs: usize, mut f: impl FnMut() -> T) -> T {
    let mut times = Vec::new();
    let mut last = None;
    for _ in 0..runs {
        let started = Instant::now();
        last = Some(black_box(f()));
        times.push(started.elapsed());
    }
    times.sort();
    let millis = |duration: Duration| duration.as_secs_f64() * 1000.0;
    println!("{name:<44} min {:>9.2} ms   median {:>9.2} ms", millis(times[0]), millis(times[times.len() / 2]));
    last.expect("at least one run")
}

#[test]
#[ignore = "benchmark on real data"]
fn bench_loading() {
    let roots = Roots::home();
    let sessions = time("load_index (all tools)", 5, || data::load_index(&roots).unwrap());
    println!("  {} sessions, {} MB", sessions.len(), sessions.iter().map(|s| s.size).sum::<u64>() >> 20);
    time("Prices::load", 5, || Prices::load(&roots));
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let path = settings::Settings::path(&home);
    let settings = settings::Settings::load(&path);
    time("config::load", 5, || config::load(&home, &settings, &path));
    for source in Source::ALL {
        let Some(biggest) = sessions.iter().filter(|s| s.source == source).max_by_key(|s| s.size) else { continue };
        let name = format!("load_transcript {} biggest ({} MB)", source.label(), biggest.size >> 20);
        let transcript = time(&name, 3, || data::load_transcript(biggest).unwrap());
        println!("  {} rows", transcript.rows.len());
    }
}

#[test]
#[ignore = "benchmark on real data"]
fn bench_search() {
    let sessions = data::load_index(&Roots::home()).unwrap();
    let generation = Arc::new(AtomicU64::new(0));
    for query in ["the", "performance", "zzqxj-no-match"] {
        let hits = time(&format!("search {query:?}"), 3, || data::search(&sessions, query, &generation, 0));
        println!("  {} hits", hits.len());
    }
    let (compressed, plain): (Vec<Session>, Vec<Session>) = sessions.into_iter().partition(|s| data::is_compressed(&s.path));
    time(&format!("search {} plain files, no match", plain.len()), 3, || data::search(&plain, "zzqxj-no-match", &generation, 0));
    time(&format!("search {} compressed files, no match", compressed.len()), 3, || data::search(&compressed, "zzqxj-no-match", &generation, 0));
}
