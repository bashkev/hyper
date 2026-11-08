//! What the tools' transcripts have in common: the session list, a transcript's rows, search.
//! Each tool's own format is in its module, behind `Agent`. Nothing here writes to disk.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use serde_json::Value;

use crate::agent::{Agent, Titles};
use crate::usage::{Agents, Usage};
use crate::{claude, codex, pi};

mod records;
mod search;
mod text;
mod tools;

pub use records::{Cursor, each_line, is_compressed, read_records};
pub use search::{Hit, MAX_HITS, search};
pub use text::{between, contains, one_line, parse_time, project_name, str_field};
pub use tools::{Tool, clip, describe_tool, fence, result_text};

#[cfg(test)]
mod tests;

pub const TITLE_CHARS: usize = 90;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    Claude,
    Codex,
    Pi,
}

impl Source {
    pub const ALL: [Source; 3] = [Source::Claude, Source::Codex, Source::Pi];

    /// Everything about it that differs from the others.
    pub fn agent(self) -> &'static dyn Agent {
        match self {
            Source::Claude => &claude::Claude,
            Source::Codex => &codex::Codex,
            Source::Pi => &pi::Pi,
        }
    }

    pub fn label(self) -> &'static str {
        self.agent().label()
    }
}

/// Where each tool keeps its sessions.
#[derive(Clone, Debug)]
pub struct Roots(BTreeMap<Source, PathBuf>);

impl Roots {
    pub fn home() -> Roots {
        Roots::at_home(&std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default())
    }

    /// Where each tool keeps them for someone whose home is `home`.
    pub fn at_home(home: &Path) -> Roots {
        Roots(Source::ALL.into_iter().map(|s| (s, s.agent().sessions(home))).collect())
    }

    /// Each tool's in its own folder of `dir`, named by its id.
    #[cfg(test)]
    pub fn under(dir: &Path) -> Roots {
        Roots(Source::ALL.into_iter().map(|s| (s, dir.join(s.agent().id()))).collect())
    }

    /// These, with `source`'s at `path`.
    #[cfg(test)]
    pub fn with(mut self, source: Source, path: PathBuf) -> Roots {
        self.0.insert(source, path);
        self
    }

    pub fn get(&self, source: Source) -> &Path {
        &self.0[&source]
    }

    /// Each tool's titles kept apart from its sessions.
    fn titles(&self) -> HashMap<Source, Titles> {
        Source::ALL.into_iter().map(|s| (s, s.agent().titles(self.get(s)))).collect()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Session {
    pub id: String,
    pub source: Source,
    pub path: PathBuf,
    pub project: String,
    pub cwd: String,
    pub title: String,
    pub branch: String,
    /// Seconds since the epoch, from the file's modification time.
    pub updated: i64,
    pub size: u64,
    /// Whether its last turn is still going, by its file. `None` when the file can't say.
    pub turn: Option<bool>,
}

impl Session {
    /// What picks this session up again, for a terminal in its folder.
    pub fn resume_argv(&self) -> Vec<String> {
        self.source.agent().resume(&self.id)
    }

    /// A command that picks this session up again in its own folder.
    pub fn resume_command(&self) -> String {
        format!("cd '{}' && {}", self.cwd.replace('\'', r"'\''"), self.resume_argv().join(" "))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    User,
    Assistant,
    Thinking,
    Compacted,
    Tool(Tool),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// `<record id>:<block index>`.
    pub id: String,
    pub kind: Kind,
    pub text: String,
}

/// A session as far as it's been read, with each tool call waiting for its result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transcript {
    pub rows: Vec<Row>,
    pub model: String,
    pub started: Option<i64>,
    pub prompts: usize,
    pub replies: usize,
    pub tools: usize,
    pub read: Cursor,
    /// Claude repeats a resumed session's records, so each is added once.
    pub seen: HashSet<String>,
    pub usage: Usage,
    pub agents: Arc<Agents>,
    calls: HashMap<String, usize>,
}

impl Transcript {
    pub fn push(&mut self, id: String, kind: Kind, text: String) {
        self.rows.push(Row { id, kind, text });
    }

    pub fn call(&mut self, call_id: &str, id: String, tool: Tool) {
        self.calls.insert(call_id.to_owned(), self.rows.len());
        self.push(id, Kind::Tool(tool), String::new());
    }

    /// Files a result under the call it answers.
    pub fn result(&mut self, call_id: &str, text: String, error: bool) {
        let Some(&row) = self.calls.get(call_id) else { return };
        if let Kind::Tool(tool) = &mut self.rows[row].kind {
            tool.result = Some(text);
            tool.error = error;
        }
    }

    pub fn recount(&mut self) {
        let count = |f: fn(&Kind) -> bool| self.rows.iter().filter(|r| f(&r.kind)).count();
        (self.prompts, self.replies, self.tools) =
            (count(|k| matches!(k, Kind::User)), count(|k| matches!(k, Kind::Assistant)), count(|k| matches!(k, Kind::Tool(_))));
    }

    /// Takes in what `read_new` found. Says whether anything changed.
    pub fn apply(&mut self, source: Source, update: Update) -> bool {
        match update {
            Update::Whole(transcript) => {
                let changed = *transcript != *self;
                *self = *transcript;
                changed
            }
            Update::Records { records, read, agents } => {
                self.read = read;
                let changed = !records.is_empty() || !Arc::ptr_eq(&agents, &self.agents);
                self.agents = agents;
                for (line, record) in records {
                    source.agent().add_record(self, &record, line);
                }
                self.recount();
                changed
            }
        }
    }
}

/// What a session's file holds beyond what's been read.
#[derive(Debug)]
pub enum Update {
    Records { records: Vec<(usize, Value)>, read: Cursor, agents: Arc<Agents> },
    /// Its rows can't be had by adding to the end, so here they all are.
    Whole(Box<Transcript>),
}

/// Every session of every tool, newest first. Missing history folders are an empty index:
/// the first session can then arrive through the same refresh path as any later one.
pub fn load_index(roots: &Roots) -> Result<Vec<Session>, String> {
    let files: Vec<(Source, PathBuf)> = Source::ALL.into_iter().flat_map(|s| s.agent().files(roots.get(s)).into_iter().map(move |p| (s, p))).collect();
    let titles = roots.titles();
    let next = AtomicUsize::new(0);
    let mut sessions: Vec<Session> = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..workers())
            .map(|_| {
                scope.spawn(|| {
                    let mut out = Vec::new();
                    while let Some((source, path)) = files.get(next.fetch_add(1, Ordering::Relaxed)) {
                        out.extend(source.agent().read_session(path, &titles[source]));
                    }
                    out
                })
            })
            .collect();
        workers.into_iter().flat_map(|w| w.join().unwrap_or_default()).collect()
    });
    sort(&mut sessions);
    Ok(sessions)
}

pub fn sort(sessions: &mut [Session]) {
    sessions.sort_by(|a, b| b.updated.cmp(&a.updated).then_with(|| a.id.cmp(&b.id)));
}

/// Sessions read again after their files changed. `None` for a file that no longer lists one.
pub struct Refreshed {
    pub sessions: Vec<(PathBuf, Option<Session>)>,
    /// Each tool's titles kept apart from its sessions, when they're what changed.
    pub titles: Option<HashMap<Source, Titles>>,
}

pub fn refresh(roots: &Roots, files: &[(Source, PathBuf)], titles_changed: bool) -> Refreshed {
    let titles = roots.titles();
    let sessions = files
        .iter()
        .map(|(source, path)| {
            let agent = source.agent();
            let path = agent.moved(path).unwrap_or_else(|| path.clone());
            let session = agent.read_session(&path, &titles[source]);
            (path, session)
        })
        .collect();
    Refreshed { sessions, titles: titles_changed.then_some(titles) }
}

fn workers() -> usize {
    std::thread::available_parallelism().map_or(4, std::num::NonZero::get).clamp(2, 8)
}

/// Read an append-only agent's records with the same filtering and numbering used when tailing.
pub fn load_records(path: &Path, agent: &dyn Agent) -> Result<Transcript, String> {
    let mut out = Transcript::default();
    out.read = read_records(path, Cursor::default(), |line| agent.wanted(line), |line, record| agent.add_record(&mut out, &record, line))?;
    out.recount();
    Ok(out)
}

/// One session in full.
pub fn load_transcript(session: &Session) -> Result<Transcript, String> {
    let path = &session.path;
    let len = file_len(path)?;
    let mut transcript = session.source.agent().load(path)?;
    if !appends(session) {
        transcript.read.offset = len;
    }
    transcript.agents = Agents::advanced(&transcript.agents, session);
    Ok(transcript)
}

fn file_len(path: &Path) -> Result<u64, String> {
    std::fs::metadata(path).map(|m| m.len()).map_err(|e| format!("{}: {e}", path.display()))
}

/// Whether what's new in a session's file can simply be added to its rows.
fn appends(session: &Session) -> bool {
    session.source.agent().appends() && !is_compressed(&session.path)
}

/// What's in a session's file past `from`. One that was cut, or that doesn't just append, is
/// read again in full.
pub fn read_new(session: &Session, from: Cursor, agents: &Arc<Agents>) -> Result<Update, String> {
    let len = file_len(&session.path)?;
    if len == from.offset {
        return Ok(Update::Records { records: Vec::new(), read: from, agents: Agents::advanced(agents, session) });
    }
    if len < from.offset || !appends(session) {
        return load_transcript(session).map(|transcript| Update::Whole(Box::new(transcript)));
    }
    let agent = session.source.agent();
    let mut records = Vec::new();
    let read = read_records(&session.path, from, |line| agent.wanted(line), |line, record| records.push((line, record)))?;
    Ok(Update::Records { records, read, agents: Agents::advanced(agents, session) })
}

/// How far back from a file's end a turn's start or end is looked for.
const TURN_TAIL: u64 = 256 * 1024;
/// Only a session written this recently, in seconds, can be in a turn worth knowing about.
const TURN_RECENT: i64 = 3_600;

/// Whether a session's last turn is still going, from the end of its file, for a tool whose
/// lines say. `None` when that's too far back to see, or the session is old.
pub fn turn(path: &Path, source: Source, updated: i64) -> Option<bool> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    if is_compressed(path) || now - updated > TURN_RECENT {
        return None;
    }
    let mut file = File::open(path).ok()?;
    let from = file.metadata().ok()?.len().saturating_sub(TURN_TAIL);
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).ok()?;
    turn_in(&bytes, from > 0, source)
}

fn turn_in(bytes: &[u8], cut: bool, source: Source) -> Option<bool> {
    let mut lines: Vec<&[u8]> = bytes.split(|&b| b == b'\n').collect();
    // What's before the first newline may be the end of a line, not one.
    if cut {
        lines.remove(0);
    }
    lines.into_iter().rev().find_map(|line| source.agent().turn(line))
}
