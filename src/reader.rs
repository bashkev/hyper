//! The open session: its transcript, kept up to date as the agent writes, and how it's shown.
//! Browse opens the session picked in its list; the Workspace opens the one behind the terminal
//! on screen, for its details.

mod details;
mod transcript;

pub use details::details;
pub use transcript::{disclosure, tools_toggle, transcript_pane};
#[cfg(test)]
pub use transcript::content_markdown;

use std::collections::HashSet;

use rune::{Latest, Tx, Waker};

use crate::data::{self, Kind, Row, Session, Transcript, Update};
use crate::usage::Report;
use crate::{App, Load, View, toggle_in};

/// A transcript read in the background, for the session it belongs to.
type Loaded = (String, Result<Transcript, String>);

pub struct Reader {
    /// The open session's id.
    pub selected: Option<String>,
    pub transcript: Load<Transcript>,
    transcript_job: Latest<Loaded>,
    /// One read of the open session's new lines at a time, so none is applied twice.
    pub tail_job: Latest<(String, Result<Update, String>)>,
    /// The open session changed while `tail_job` was reading.
    tail_again: bool,
    /// What the open session used, priced.
    pub report: Option<Report>,
    /// How many times the open session's rows were replaced, which can change a row in place.
    pub rewrites: u64,
    /// Indices into the transcript's rows, in order.
    pub shown: Vec<usize>,
    /// User prompts, by their position in `shown`.
    pub prompts: Vec<usize>,
    pub prompt: usize,
    pub prompt_hover: Option<String>,
    pub show_tools: bool,
    /// Rows, and details sections, that are open.
    pub expanded: HashSet<String>,
    /// A record to scroll to once its transcript has loaded.
    pub reveal: Option<String>,
}

impl Default for Reader {
    fn default() -> Reader {
        Reader {
            selected: None,
            transcript: Load::Waiting,
            transcript_job: Latest::default(),
            tail_job: Latest::default(),
            tail_again: false,
            report: None,
            rewrites: 0,
            shown: Vec::new(),
            prompts: Vec::new(),
            prompt: 0,
            prompt_hover: None,
            show_tools: true,
            expanded: HashSet::new(),
            reveal: None,
        }
    }
}

impl Reader {
    pub fn set_waker(&self, waker: Waker) {
        self.transcript_job.set_waker(waker.clone());
        self.tail_job.set_waker(waker);
    }

    /// Forgets the session that was open, for `id` to be read.
    fn open(&mut self, id: &str) {
        self.selected = Some(id.to_owned());
        self.transcript = Load::Waiting;
        self.report = None;
        self.shown.clear();
        self.prompts.clear();
        self.prompt = 0;
        self.prompt_hover = None;
        self.expanded.clear();
        self.tail_job.cancel();
        self.tail_again = false;
    }

    /// Works out which rows show, and which of them are prompts.
    pub fn reshow(&mut self) {
        let Load::Ready(t) = &self.transcript else { return };
        let tools = self.show_tools;
        self.shown = t
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| tools || !matches!(r.kind, Kind::Tool(_) | Kind::Thinking))
            .map(|(i, _)| i)
            .collect();
        self.prompts = self.shown.iter().enumerate()
            .filter(|(_, i)| matches!(t.rows[**i].kind, Kind::User))
            .map(|(i, _)| i)
            .collect();
        self.prompt = self.prompt.min(self.prompts.len().saturating_sub(1));
    }

    pub fn toggle_tools(&mut self) {
        self.show_tools = !self.show_tools;
        self.reshow();
    }

    /// Opens or closes a row, or a details section.
    pub fn toggle(&mut self, id: &str) {
        toggle_in(&mut self.expanded, id);
    }

    /// The `i`th row shown.
    pub fn row(&self, i: usize) -> Option<&Row> {
        let Load::Ready(t) = &self.transcript else { return None };
        t.rows.get(*self.shown.get(i)?)
    }

    pub fn jump_prompt(&mut self, prompt: usize, tx: &mut Tx) {
        let Some(r) = self.prompts.get(prompt).and_then(|&i| self.row(i)) else { return };
        let key = r.id.clone();
        self.prompt = prompt;
        self.prompt_hover = None;
        // Reveal mounts an offscreen virtual row; then the native anchor puts it at the top,
        // clamped to the content's end when there isn't enough below it.
        tx.scroll_into_view(&key);
        tx.scroll_key_to(key, 0.0);
        tx.scroll_into_view(format!("prompt-mark-{prompt}"));
    }

    pub fn visible_prompts(&mut self, visible: std::ops::Range<usize>) -> bool {
        // Keep a clicked prompt selected when the end prevents it from reaching the top.
        if self.prompts.get(self.prompt).is_some_and(|i| visible.contains(i)) {
            return false;
        }
        let prompt = self.prompts.partition_point(|&i| i <= visible.start).saturating_sub(1);
        if prompt == self.prompt {
            return false;
        }
        self.prompt = prompt;
        true
    }

    /// The row a pending reveal points at, by the key the list knows it by.
    fn take_reveal(&mut self) -> Option<String> {
        let uuid = self.reveal.take()?;
        let Load::Ready(t) = &self.transcript else { return None };
        let prefix = format!("{uuid}:");
        t.rows.iter().find(|r| r.id.starts_with(&prefix)).map(|r| r.id.clone())
    }
}

impl App {
    /// The open session, while the index still has it.
    pub fn selected_session(&self) -> Option<&Session> {
        self.reader.selected.as_deref().and_then(|id| self.session(id))
    }

    pub fn select(&mut self, id: &str) {
        self.reader.reveal = self.search.hits.get(id).map(|h| h.uuid.clone());
        if self.reader.selected.as_deref() == Some(id) {
            return;
        }
        let Some(session) = self.session(id).cloned() else { return };
        self.reader.open(id);
        let id = id.to_owned();
        self.reader.transcript_job.request(session, move |session| (id, data::load_transcript(&session)));
    }

    pub fn set_transcript(&mut self, transcript: Result<Transcript, String>) {
        self.reader.transcript = transcript.into();
        self.reader.reshow();
        self.reprice();
    }

    pub fn reprice(&mut self) {
        let source = self.selected_session().map(|s| s.source);
        self.reader.report = match (&self.reader.transcript, source) {
            (Load::Ready(t), Some(source)) => Some(Report::new(source, &t.usage, &t.agents, &self.prices)),
            _ => None,
        };
    }

    /// After the index changed: moves on if the open session has gone, or brings it up to date
    /// if it may have changed. A session that failed to load gets another try.
    pub fn keep_open_session(&mut self, changed: bool) {
        if self.selected_session().is_none() {
            self.reader.selected = None;
            // The Workspace keeps showing what it was.
            if self.view == View::Browse {
                self.select_first();
            }
        } else if changed && matches!(self.reader.transcript, Load::Failed(_)) {
            if let Some(id) = self.reader.selected.take() {
                self.select(&id);
            }
        } else if changed {
            self.tail();
        }
    }

    /// Reads what the open session has added since it was last read.
    pub fn tail(&mut self) {
        let Load::Ready(transcript) = &self.reader.transcript else { return };
        if self.reader.tail_job.is_pending() {
            self.reader.tail_again = true;
            return;
        }
        let Some(session) = self.selected_session().cloned() else { return };
        let (read, agents) = (transcript.read, transcript.agents.clone());
        self.reader.tail_job.request(session, move |session| (session.id.clone(), data::read_new(&session, read, &agents)));
    }

    /// Adds what `tail` found. Says whether anything shown changed.
    fn set_tail(&mut self, id: &str, update: Result<Update, String>) -> bool {
        let source = self.session(id).map(|s| s.source);
        let mut changed = false;
        if let (Load::Ready(transcript), Some(source), Ok(update)) = (&mut self.reader.transcript, source, update) && self.reader.selected.as_deref() == Some(id) {
            let whole = matches!(update, Update::Whole(_));
            changed = transcript.apply(source, update);
            self.reader.rewrites += u64::from(whole && changed);
        }
        if changed {
            self.reader.reshow();
            self.reprice();
        }
        if std::mem::take(&mut self.reader.tail_again) {
            self.tail();
        }
        changed
    }

    /// Takes a transcript that's been read, if it's still the one wanted.
    pub fn take_transcript(&mut self) -> bool {
        let Some((id, transcript)) = self.reader.transcript_job.take() else { return false };
        if self.reader.selected.as_deref() != Some(id.as_str()) {
            return false;
        }
        self.set_transcript(transcript);
        // It may have grown while it was being read.
        self.tail();
        true
    }

    pub fn take_tail(&mut self) -> bool {
        let Some((id, update)) = self.reader.tail_job.take() else { return false };
        self.set_tail(&id, update)
    }

    /// Scrolls to a search hit once its transcript is there.
    pub fn reveal(&mut self, tx: &mut Tx) -> bool {
        if !matches!(self.reader.transcript, Load::Ready(_)) || self.reader.reveal.is_none() {
            return false;
        }
        let Some(key) = self.reader.take_reveal() else { return false };
        tx.scroll_center(key);
        true
    }
}
