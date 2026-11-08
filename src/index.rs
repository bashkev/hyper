//! Every tool's sessions, newest first, and whether each is running. Read once at launch,
//! then kept current from what the watcher sees, a few changed files at a time.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rune::{Latest, Tx, Waker};

use crate::data::{self, Refreshed, Session, Source};
use crate::format::now;
use crate::live::{self, Activity, Changes};
use crate::usage::Prices;
use crate::{App, Load};

/// With no registry to ask, a session counts as working this long after a write, in seconds.
pub const QUIET: i64 = 30;

/// Session files that changed, read again one batch at a time.
#[derive(Default)]
pub struct Stale {
    files: HashSet<(Source, PathBuf)>,
    /// Whether the titles tools keep apart from their sessions changed.
    names: bool,
    /// The batch `job` was last given, until its answer is taken.
    reading: (Vec<(Source, PathBuf)>, bool),
    pub job: Latest<Refreshed>,
}

impl Stale {
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn add(&mut self, files: impl IntoIterator<Item = (Source, PathBuf)>, names: bool) {
        self.files.extend(files);
        self.names |= names;
    }

    /// What to read next, unless a read is already out or nothing has changed.
    fn next_batch(&mut self) -> Option<(Vec<(Source, PathBuf)>, bool)> {
        if self.job.is_pending() || (self.files.is_empty() && !self.names) {
            return None;
        }
        // A new request drops an answer that has landed but not been taken, so what that answer
        // held is read again.
        let (unanswered, names) = std::mem::take(&mut self.reading);
        self.files.extend(unanswered);
        let files: Vec<(Source, PathBuf)> = self.files.drain().collect();
        let names = names | std::mem::take(&mut self.names);
        self.reading = (files.clone(), names);
        Some((files, names))
    }

    fn take(&mut self) -> Option<Refreshed> {
        let refreshed = self.job.take()?;
        self.reading = (Vec::new(), false);
        Some(refreshed)
    }
}

impl App {
    pub fn request_index(&self) {
        self.index_job.request(self.roots.clone(), |roots| data::load_index(&roots));
        self.request_config();
        self.prices_job.request(self.roots.clone(), |roots| Prices::load(&roots));
    }

    pub fn watch(&mut self, waker: Waker) {
        self.watcher = Some(live::Watcher::start(&self.roots, &self.home.join(".claude/sessions"), waker));
    }

    /// Whether a session's activity comes from how recently it was written.
    pub fn guessed(&self, session: &Session) -> bool {
        !session.source.agent().registered() || !self.running.as_ref().is_some_and(|r| r.trusted)
    }

    pub fn activity(&self, session: &Session) -> Option<Activity> {
        self.activity_at(session, now())
    }

    pub fn activity_at(&self, session: &Session, now: i64) -> Option<Activity> {
        let running = self.running.as_ref()?;
        if self.guessed(session) {
            if let Some(said) = self.said_here(&session.id) {
                return said.then_some(Activity::Working);
            }
            let working = match session.turn {
                // In a terminal here its process is known to be alive, so its file can be believed.
                Some(going) if self.running_here(&session.id) => going,
                Some(false) => false,
                _ => now - session.updated < QUIET,
            };
            return working.then_some(Activity::Working);
        }
        running.sessions.get(&session.id).copied()
    }

    pub fn sessions(&self) -> &[Session] {
        self.index.ready().map_or(&[], |sessions| sessions.as_slice())
    }

    pub fn session(&self, id: &str) -> Option<&Session> {
        self.sessions().iter().find(|s| s.id == id)
    }

    pub fn set_index(&mut self, index: Result<Vec<Session>, String>) {
        self.index = index.map(Arc::new).into();
        self.open_newest();
        self.search_changed();
        self.keep_open_session(true);
        self.refresh();
    }

    /// Takes in what the watcher saw. Says whether anything shown changed.
    pub fn absorb(&mut self, changes: Changes) -> bool {
        let running = changes.running.is_some();
        if running {
            self.running = changes.running;
            self.relist();
        }
        if changes.rescan {
            self.request_index();
        }
        self.stale.add(changes.files, changes.titles);
        if self.reader.selected.as_ref().is_some_and(|id| changes.agents.contains(id)) {
            self.tail();
        }
        self.refresh();
        running
    }

    /// Reads the files that changed, unless a read is already out.
    pub fn refresh(&mut self) {
        if !matches!(self.index, Load::Ready(_)) {
            return;
        }
        let Some((files, names)) = self.stale.next_batch() else { return };
        let roots = self.roots.clone();
        self.stale.job.request((), move |()| data::refresh(&roots, &files, names));
    }

    pub fn set_refreshed(&mut self, refreshed: Refreshed) {
        let open = self.selected_session().is_some();
        let Load::Ready(index) = &mut self.index else { return };
        let sessions = Arc::make_mut(index);
        let selected = self.reader.selected.as_deref();
        let mut open_changed = false;
        for (path, session) in refreshed.sessions {
            let Some(session) = session else {
                sessions.retain(|s| s.path != path);
                // A file that lists no session may be one of the open session's subagents.
                open_changed |= open;
                continue;
            };
            open_changed |= selected == Some(session.id.as_str());
            // A session keeps its id when Codex moves it to a compressed file.
            let same = |s: &Session| s.source == session.source && s.id == session.id;
            match sessions.iter().position(|s| s.path == path).or_else(|| sessions.iter().position(same)) {
                Some(at) => sessions[at] = session,
                None => sessions.push(session),
            }
        }
        for (source, titles) in refreshed.titles.iter().flatten() {
            for (id, title) in titles {
                if let Some(session) = sessions.iter_mut().find(|s| s.source == *source && s.id == *id) {
                    session.title.clone_from(title);
                }
            }
        }
        data::sort(sessions);
        self.relist();
        self.keep_open_session(open_changed);
        self.refresh();
    }

    /// Redraws when a session goes quiet, and asks to wake for the next one. Sessions are newest
    /// first, so only the recent few are looked at.
    pub fn settle(&mut self, tx: &mut Tx) -> bool {
        let now = now();
        let passed = self.quiet_at.is_some_and(|at| at <= now);
        let recent = self.sessions().iter().take_while(|s| now - s.updated < QUIET);
        self.quiet_at = recent.filter(|s| self.guessed(s)).map(|s| s.updated + QUIET).min();
        if let Some(at) = self.quiet_at {
            tx.wake_after(Duration::from_secs((at - now).max(1) as u64));
        }
        if passed {
            self.relist();
        }
        passed
    }
    pub fn take_index(&mut self) -> bool {
        let Some(index) = self.index_job.take() else { return false };
        self.set_index(index);
        true
    }

    pub fn take_prices(&mut self) -> bool {
        let Some(prices) = self.prices_job.take() else { return false };
        self.prices = prices;
        self.reprice();
        true
    }

    /// Takes in what the watcher has seen since it was last asked.
    pub fn take_changes(&mut self) -> bool {
        let Some(changes) = self.watcher.as_mut().map(live::Watcher::take).filter(|c| !c.is_empty()) else { return false };
        self.absorb(changes)
    }

    pub fn take_refreshed(&mut self) -> bool {
        let Some(refreshed) = self.stale.take() else { return false };
        self.set_refreshed(refreshed);
        true
    }
}
