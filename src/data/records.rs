//! Reading plain and compressed JSONL, including resumable partial records.

use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

/// How much of a session's file has been read.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cursor {
    pub offset: u64,
    pub line: usize,
}

pub fn is_compressed(path: &Path) -> bool {
    path.extension().is_some_and(|x| x == "zst")
}

fn reader(path: &Path) -> std::io::Result<Box<dyn BufRead>> {
    let file = File::open(path)?;
    if is_compressed(path) {
        return Ok(Box::new(BufReader::with_capacity(1 << 20, zstd::Decoder::new(file)?)));
    }
    Ok(Box::new(BufReader::with_capacity(1 << 20, file)))
}

/// Calls `f` with each `wanted` record after `from`, and says how far it read. A last line
/// still being written is left for next time; a file cut short ends at what could be read.
pub fn read_records(path: &Path, from: Cursor, wanted: impl Fn(&[u8]) -> bool, mut f: impl FnMut(usize, Value)) -> Result<Cursor, String> {
    let open = || -> std::io::Result<Box<dyn BufRead>> {
        if from.offset == 0 {
            return reader(path);
        }
        let mut file = File::open(path)?;
        file.seek(SeekFrom::Start(from.offset))?;
        Ok(Box::new(BufReader::with_capacity(1 << 20, file)))
    };
    let mut reader = open().map_err(|e| format!("{}: {e}", path.display()))?;
    let mut at = from;
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return Ok(at),
            Ok(_) => {}
        }
        let ended = line.ends_with(b"\n");
        let wants = wanted(&line);
        let record = if wants || !ended { serde_json::from_slice::<Value>(&line).ok() } else { None };
        if !ended && record.is_none() {
            return Ok(at);
        }
        at.offset += line.len() as u64;
        // The end of a last line that was taken whole before its newline arrived.
        if line == b"\n" {
            continue;
        }
        at.line += 1;
        if let Some(record) = record.filter(|_| wants) {
            f(at.line, record);
        }
    }
}

/// Calls `f` with each line's number and bytes until it returns false. A file cut short (one
/// being written, a truncated archive) ends at what could be read.
pub fn each_line(path: &Path, mut f: impl FnMut(usize, &[u8]) -> bool) -> Result<(), String> {
    let mut reader = reader(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut line = Vec::new();
    let mut number = 0;
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return Ok(()),
            Ok(_) => {}
        }
        number += 1;
        if !f(number, &line) {
            return Ok(());
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(std::path::PathBuf);

    impl Fixture {
        fn new(bytes: &[u8], compressed: bool) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let extension = if compressed { "jsonl.zst" } else { "jsonl" };
            let path = std::env::temp_dir().join(format!("hyper-records-{}-{id}.{extension}", std::process::id()));
            let bytes = if compressed { zstd::encode_all(bytes, 0).unwrap() } else { bytes.to_vec() };
            std::fs::write(&path, bytes).unwrap();
            Self(path)
        }

        fn append(&self, bytes: &[u8]) {
            let mut file = std::fs::OpenOptions::new().append(true).open(&self.0).unwrap();
            file.write_all(bytes).unwrap();
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn partial_records_resume_without_losing_filtering_or_line_numbers() {
        let start = b"{\"keep\":1}\nnot json\n{\"skip\":2}\n{\"keep\":";
        let file = Fixture::new(start, false);
        let mut records = Vec::new();
        let wanted = |line: &[u8]| line.windows(6).any(|part| part == b"\"keep\"");
        let read = read_records(&file.0, Cursor::default(), wanted, |line, value| records.push((line, value))).unwrap();
        assert_eq!(read.line, 3);
        assert_eq!(read.offset, start.len() as u64 - b"{\"keep\":".len() as u64);
        file.append(b"3}\n");
        let end = read_records(&file.0, read, wanted, |line, value| records.push((line, value))).unwrap();
        assert_eq!(records, vec![(1, serde_json::json!({"keep": 1})), (4, serde_json::json!({"keep": 3}))]);
        assert_eq!(end.line, 4);
        assert_eq!(end.offset, std::fs::metadata(&file.0).unwrap().len());
    }

    #[test]
    fn a_delayed_newline_does_not_change_record_ids() {
        let file = Fixture::new(b"{\"n\":1}", false);
        let mut lines = Vec::new();
        let read = read_records(&file.0, Cursor::default(), |_| true, |line, _| lines.push(line)).unwrap();
        file.append(b"\n{\"n\":2}\n");
        let read = read_records(&file.0, read, |_| true, |line, _| lines.push(line)).unwrap();
        assert_eq!(lines, [1, 2]);
        assert_eq!(read.line, 2);
        assert_eq!(read.offset, std::fs::metadata(&file.0).unwrap().len());
    }

    #[test]
    fn compressed_and_plain_files_yield_the_same_records_and_lines() {
        let bytes = b"{\"n\":1}\nnot json\n{\"n\":2}";
        let mut results = Vec::new();
        for compressed in [false, true] {
            let file = Fixture::new(bytes, compressed);
            assert_eq!(is_compressed(&file.0), compressed);
            let mut records = Vec::new();
            let read = read_records(&file.0, Cursor::default(), |_| true, |line, value| records.push((line, value))).unwrap();
            let mut lines = Vec::new();
            each_line(&file.0, |number, line| {
                lines.push((number, line.to_vec()));
                number < 2
            }).unwrap();
            results.push((read, records, lines));
        }
        assert_eq!(results[0], results[1]);
        assert_eq!(results[0].0, Cursor { offset: bytes.len() as u64, line: 3 });
        assert_eq!(results[0].1.len(), 2);
        assert_eq!(results[0].2.len(), 2, "each_line respects early termination");
    }

    #[test]
    fn read_errors_identify_the_file() {
        let file = Fixture::new(b"", false);
        std::fs::remove_file(&file.0).unwrap();
        let records = read_records(&file.0, Cursor::default(), |_| true, |_, _| {}).unwrap_err();
        let lines = each_line(&file.0, |_, _| true).unwrap_err();
        let path = file.0.display().to_string();
        assert!(records.starts_with(&path));
        assert!(lines.starts_with(&path));
    }
}
