//! What Hyper needs from each agent it runs and browses. Each agent implements this in its own
//! module (`claude`, `codex`, `pi`), and the rest of Hyper asks through `Source::agent`
//! instead of naming one, so another agent is another module and a `Source` variant.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::config::ConfigItem;
use crate::data::{Session, Transcript};
use crate::logos::Layer;
use crate::notifications::Event;
use crate::usage::Usage;

/// Session titles kept apart from the session files, by session id.
pub type Titles = HashMap<String, String>;

/// What a changed path under an agent's session folder is.
#[derive(Debug, PartialEq)]
pub enum Found {
    Session,
    /// A subagent's transcript, of the session with this id.
    Subagent(String),
    /// Where its titles are kept.
    Titles,
}

pub trait Agent: Sync {
    fn label(&self) -> &'static str;
    /// Its name in settings, and the program that runs it.
    fn id(&self) -> &'static str;
    fn logo(&self) -> &'static [Layer];

    /// Where it keeps its sessions under `home`.
    fn sessions(&self, home: &Path) -> PathBuf;
    /// The folder watched for them: `root`, or one above it.
    fn watched(&self, root: &Path) -> PathBuf {
        root.to_owned()
    }
    /// What a changed path, relative to its session folder, is.
    fn classify(&self, rest: &Path) -> Option<Found>;
    /// Whether its session files are followed with kqueue, because macOS reports a write only
    /// once the writer closes the file, and it keeps them open.
    fn keeps_open(&self) -> bool {
        true
    }
    fn files(&self, root: &Path) -> Vec<PathBuf>;
    fn titles(&self, _root: &Path) -> Titles {
        Titles::new()
    }
    fn read_session(&self, path: &Path, titles: &Titles) -> Option<Session>;
    /// The file that now holds what was at `path`, when it moves it.
    fn moved(&self, _path: &Path) -> Option<PathBuf> {
        None
    }

    fn load(&self, path: &Path) -> Result<Transcript, String>;
    /// Whether what's new in a session's file can simply be added to what was read before.
    fn appends(&self) -> bool {
        true
    }
    /// Whether a line holds a record worth parsing as more of a transcript.
    fn wanted(&self, _line: &[u8]) -> bool {
        false
    }
    fn add_record(&self, _out: &mut Transcript, _record: &Value, _line: usize) {}
    /// Whether a line of its file says a turn is going (`true`) or over (`false`).
    fn turn(&self, _line: &[u8]) -> Option<bool> {
        None
    }
    /// A record's id and the text search covers in it.
    fn searchable(&self, record: &Value, line: usize) -> Option<(String, Vec<String>)>;

    /// Who prices its models, in the order to look.
    fn providers(&self) -> &'static [&'static str] {
        &[]
    }
    /// Its subagents' transcripts, and files found not to be this session's. `None` when it
    /// keeps none apart.
    fn subagent_files(&self, _session: &Session, _others: &HashSet<PathBuf>) -> Option<(Vec<PathBuf>, Vec<PathBuf>)> {
        None
    }
    /// Adds a subagent record's usage.
    fn add_usage(&self, _usage: &mut Usage, _record: &Value) {}

    /// The command that starts it new, and its session's id if it can be told one up front.
    fn start(&self, id: &str) -> (Vec<String>, Option<String>);
    fn resume(&self, id: &str) -> Vec<String>;
    /// What Hyper puts after the program's name: how it says what happens, and `prompt` added
    /// to its system prompt.
    fn hyper_args(&self, prompt: &str) -> Vec<String>;
    /// Whether it tells Hyper when `event` happens.
    fn reports(&self, event: Event) -> bool {
        event == Event::TurnCompleted
    }
    /// What a notification in its own words, not Hyper's, means.
    fn notice(&self, _message: &str) -> Event {
        Event::Notification
    }
    /// Whether it repeats what's already been seen, such as being idle, so that's no news.
    fn repeats_itself(&self) -> bool {
        false
    }
    /// Whether `live::Running`, from its process registry, knows when its sessions run.
    fn registered(&self) -> bool {
        false
    }

    /// Its global setup under `home`, if it's installed.
    fn config(&self, home: &Path) -> Vec<ConfigItem>;
}
