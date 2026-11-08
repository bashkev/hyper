//! What's happening now: macOS says when a session's file changes, and when a running agent's
//! process exits. Nothing here polls.
//!
//! macOS only reports a write to a file once the writer closes it, and Codex keeps its session
//! open for as long as it runs. So a session of an agent that does that (`Agent::keeps_open`)
//! seen changing is also followed with kqueue, which hears each write as it happens.

use std::collections::{HashMap, HashSet};
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
use rune::Waker;

use crate::agent::Found;
use crate::claude;
use crate::data::{self, Roots, Source};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activity {
    Working,
    /// Running, and waiting for its user.
    Open,
}

impl Activity {
    pub fn label(self) -> &'static str {
        match self {
            Activity::Working => "Working",
            Activity::Open => "Open",
        }
    }
}

/// The Claude sessions with a running process.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Running {
    pub sessions: HashMap<String, Activity>,
    /// False when the registry is missing or unreadable, so it can't be relied on.
    pub trusted: bool,
}

/// What's changed since the last look.
#[derive(Default)]
pub struct Changes {
    pub files: HashSet<(Source, PathBuf)>,
    /// Claude sessions whose subagents wrote.
    pub agents: HashSet<String>,
    /// Titles kept apart from sessions.
    pub titles: bool,
    /// macOS dropped events, so anything may have changed.
    pub rescan: bool,
    pub running: Option<Running>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty() && self.agents.is_empty() && !self.titles && !self.rescan && self.running.is_none()
    }
}

#[derive(Debug, PartialEq)]
enum Change {
    File(Source, PathBuf),
    /// A subagent of the session with this id.
    Agent(String),
    Titles,
    Registry,
}

/// A folder as the app knows it, and as macOS reports it once links are resolved.
struct Folder {
    given: PathBuf,
    real: PathBuf,
}

impl Folder {
    /// One that doesn't exist yet may still appear, under a parent that does.
    fn new(given: &Path) -> Folder {
        let real = given.ancestors().find_map(|parent| Some(parent.canonicalize().ok()?.join(given.strip_prefix(parent).ok()?))).unwrap_or_else(|| given.to_owned());
        Folder { given: given.to_owned(), real }
    }
}

struct Folders {
    /// Where each tool keeps its sessions.
    sessions: Vec<(Source, Folder)>,
    registry: Folder,
}

impl Folders {
    fn new(roots: &Roots, registry: &Path) -> Folders {
        Folders { sessions: Source::ALL.into_iter().map(|s| (s, Folder::new(roots.get(s)))).collect(), registry: Folder::new(registry) }
    }

    /// What a changed path means.
    fn classify(&self, path: &Path) -> Option<Change> {
        for (source, folder) in &self.sessions {
            let Ok(rest) = path.strip_prefix(&folder.real) else { continue };
            return match source.agent().classify(rest)? {
                Found::Session => Some(Change::File(*source, folder.given.join(rest))),
                Found::Subagent(session) => Some(Change::Agent(session)),
                Found::Titles => Some(Change::Titles),
            };
        }
        let entry = path.starts_with(&self.registry.real) && path.extension().is_some_and(|x| x == "json");
        entry.then_some(Change::Registry)
    }
}

struct Inner {
    changes: Mutex<Changes>,
    wake: Waker,
    folders: Folders,
    /// Tells the exit thread about each process to wait for.
    queue: i32,
    /// Held from reading the registry to noting it, so an older read can't land last.
    scanning: Mutex<()>,
    followed: Mutex<Followed>,
}

/// How many session files are followed at once, each holding a descriptor.
const FOLLOWED_MAX: usize = 64;

/// Session files followed for writes, by the number their events carry.
#[derive(Default)]
struct Followed {
    next: usize,
    files: HashMap<usize, (Source, PathBuf, OwnedFd)>,
}

impl Inner {
    fn note(&self, change: impl FnOnce(&mut Changes)) {
        change(&mut self.changes.lock().unwrap());
        (self.wake)();
    }

    fn event(&self, event: notify::Event) {
        if event.need_rescan() {
            self.note(|c| c.rescan = true);
        }
        for path in &event.paths {
            // Missing history may appear several folders at a time. Wake the app to move
            // the watch down and rescan files written before the new watch is installed.
            let mut folders = self.folders.sessions.iter().map(|(_, f)| f).chain([&self.folders.registry]);
            if folders.any(|folder| folder.real.starts_with(path)) {
                self.note(|c| c.rescan = true);
            }
            match self.folders.classify(path) {
                Some(Change::File(source, path)) => {
                    if source.agent().keeps_open() {
                        self.follow(source, &path);
                    }
                    self.note(|c| {
                        c.files.insert((source, path));
                    });
                }
                Some(Change::Agent(session)) => self.note(|c| {
                    c.agents.insert(session);
                }),
                Some(Change::Titles) => self.note(|c| c.titles = true),
                Some(Change::Registry) => self.scan(),
                None => {}
            }
        }
    }

    /// Hears each write to a session file from now on, until it's deleted or moved.
    fn follow(&self, source: Source, path: &Path) {
        let mut followed = self.followed.lock().unwrap();
        if data::is_compressed(path) || followed.files.values().any(|(_, p, _)| p == path) {
            return;
        }
        let Ok(name) = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()) else { return };
        // SAFETY: a valid C string. O_EVTONLY opens it for events only.
        let fd = unsafe { libc::open(name.as_ptr(), libc::O_EVTONLY) };
        if fd < 0 {
            return;
        }
        // SAFETY: just opened, and owned from here.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        followed.next += 1;
        let id = followed.next;
        let change = libc::kevent {
            ident: std::os::fd::AsRawFd::as_raw_fd(&fd) as usize,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: libc::NOTE_WRITE | libc::NOTE_EXTEND | libc::NOTE_DELETE | libc::NOTE_RENAME,
            data: 0,
            udata: id as *mut libc::c_void,
        };
        // SAFETY: one change from a live local, and no events asked for.
        if unsafe { libc::kevent(self.queue, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) } == 0 {
            followed.files.insert(id, (source, path.to_owned(), fd));
        }
        // A file that's never deleted would otherwise hold its descriptor for as long as the app
        // runs. The one followed longest goes, and is followed again if it's seen changing.
        if followed.files.len() > FOLLOWED_MAX
            && let Some(&oldest) = followed.files.keys().min()
        {
            followed.files.remove(&oldest);
        }
    }

    /// A followed file was written to, or has gone, which closes it and ends its events.
    fn written(&self, id: usize, gone: bool) {
        let mut followed = self.followed.lock().unwrap();
        let Some((source, path, _)) = followed.files.get(&id) else { return };
        let file = (*source, path.clone());
        if gone {
            followed.files.remove(&id);
        }
        drop(followed);
        self.note(|c| {
            c.files.insert(file);
        });
    }

    /// Reads the registry. An entry counts only if its process can be watched for exit, which
    /// also weeds out what a crash left behind: macOS refuses a process that's gone, even a
    /// zombie that `kill(pid, 0)` would still find.
    fn scan(&self) {
        let _one_at_a_time = self.scanning.lock().unwrap();
        let (agents, trusted) = claude::agents(&self.folders.registry.given);
        let mut running = Running { sessions: HashMap::new(), trusted };
        for agent in agents {
            if is_same_process(&agent) && wait_for_exit(self.queue, agent.pid) {
                running.sessions.insert(agent.session, if agent.busy { Activity::Working } else { Activity::Open });
            }
        }
        self.note(|c| c.running = Some(running));
    }
}

/// False for an entry left by a crash whose pid has gone to another process.
fn is_same_process(agent: &claude::Process) -> bool {
    let (Some(entry), Some(actual)) = (agent.started, started_at(agent.pid)) else { return true };
    (entry - actual).abs() <= 1
}

/// When a process started, in seconds since the epoch.
fn started_at(pid: i32) -> Option<i64> {
    // SAFETY: a zeroed proc_bsdinfo is valid, and it's only written to.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: `info` is a live local of the size given.
    let read = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, (&raw mut info).cast(), size) };
    (read == size).then_some(info.pbi_start_tvsec as i64)
}

fn wait_for_exit(queue: i32, pid: i32) -> bool {
    let change = libc::kevent {
        ident: pid as usize,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: one change from a live local, and no events asked for.
    unsafe { libc::kevent(queue, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) == 0 }
}

/// Blocks until a process exits or a followed file changes. `None` if the queue has failed.
fn next_event(queue: i32) -> Option<libc::kevent> {
    // SAFETY: a zeroed kevent is valid, and it's only written to.
    let mut event: libc::kevent = unsafe { std::mem::zeroed() };
    // SAFETY: room for one event in a live local, no changes, and no timeout.
    let found = unsafe { libc::kevent(queue, std::ptr::null(), 0, &mut event, 1, std::ptr::null()) };
    // An interruption reads as an exit, which rescans: harmless, and nothing is missed.
    (found >= 0 || std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted).then_some(event)
}

/// Watches for as long as the app runs.
pub struct Watcher {
    inner: Arc<Inner>,
    files: Option<RecommendedWatcher>,
    paths: Vec<PathBuf>,
    watched: HashMap<PathBuf, bool>,
}

impl Watcher {
    pub fn start(roots: &Roots, registry: &Path, wake: Waker) -> Watcher {
        let folders = Folders::new(roots, registry);
        // SAFETY: takes nothing. A failed queue is -1, which refuses every process below.
        let queue = unsafe { libc::kqueue() };
        let inner = Arc::new(Inner { changes: Mutex::default(), wake, folders, queue, scanning: Mutex::default(), followed: Mutex::default() });

        let events = inner.clone();
        let files = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event {
                events.event(event);
            }
        });
        let watched = Source::ALL.map(|s| s.agent().watched(roots.get(s)));
        let paths = registry.parent().map(Path::to_owned).into_iter().chain(watched).collect();
        let mut watcher = Watcher { inner: inner.clone(), files: files.ok(), paths, watched: HashMap::new() };
        watcher.update_watches();

        inner.scan();
        let exits = inner.clone();
        std::thread::Builder::new()
            .name("agent-exits".to_owned())
            .spawn(move || {
                while let Some(event) = next_event(exits.queue) {
                    if event.filter == libc::EVFILT_VNODE {
                        exits.written(event.udata as usize, event.fflags & (libc::NOTE_DELETE | libc::NOTE_RENAME) != 0);
                    } else {
                        exits.scan();
                    }
                }
            })
            .expect("a thread");
        watcher
    }

    /// Watch existing history recursively. For a missing root, watch its nearest existing
    /// parent without recursion, so an uninstalled tool doesn't make us watch all of HOME.
    fn update_watches(&mut self) {
        let Some(files) = &mut self.files else { return };
        let mut next = HashMap::new();
        for path in &self.paths {
            if let Some(parent) = path.ancestors().find(|p| p.is_dir()) {
                let recursive = parent == path;
                next.entry(parent.to_owned()).and_modify(|r| *r |= recursive).or_insert(recursive);
            }
        }
        if next == self.watched {
            return;
        }
        let mut changes = files.paths_mut();
        for (path, recursive) in &next {
            if self.watched.get(path) != Some(recursive) {
                let mode = if *recursive { RecursiveMode::Recursive } else { RecursiveMode::NonRecursive };
                if changes.add(path, mode).is_err() {
                    return;
                }
            }
        }
        for path in self.watched.keys().filter(|path| !next.contains_key(*path)) {
            let _ = changes.remove(path);
        }
        if changes.commit().is_ok() {
            self.watched = next;
        }
    }

    /// Hears each write to a session file from now on, such as one picked up again in a
    /// terminal, whose writes macOS would hold back while it's open.
    pub fn follow(&self, source: Source, path: &Path) {
        if source.agent().keeps_open() {
            self.inner.follow(source, path);
        }
    }

    pub fn take(&mut self) -> Changes {
        let changes = std::mem::take(&mut *self.inner.changes.lock().unwrap());
        if changes.rescan {
            self.update_watches();
        }
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    trait Real {
        fn canonicalize_or_self(self) -> PathBuf;
    }

    impl Real for PathBuf {
        /// Made first so links like /var -> /private/var resolve, as macOS reports them.
        fn canonicalize_or_self(self) -> PathBuf {
            std::fs::create_dir_all(&self).unwrap();
            self.canonicalize().unwrap_or(self)
        }
    }

    fn folders() -> Folders {
        let folder = |p: &str| Folder { given: PathBuf::from(format!("/link{p}")), real: PathBuf::from(format!("/real{p}")) };
        Folders { sessions: vec![(Source::Claude, folder("/projects")), (Source::Codex, folder("/codex")), (Source::Pi, folder("/pi"))], registry: folder("/agents") }
    }

    #[test]
    fn a_changed_path_is_told_apart_and_named_as_the_app_knows_it() {
        let f = folders();
        let file = |source, path: &str| Some(Change::File(source, PathBuf::from(path)));
        assert_eq!(f.classify(Path::new("/real/projects/a/one.jsonl")), file(Source::Claude, "/link/projects/a/one.jsonl"));
        assert_eq!(f.classify(Path::new("/real/pi/--a--/one.jsonl")), file(Source::Pi, "/link/pi/--a--/one.jsonl"));
        assert_eq!(f.classify(Path::new("/real/codex/sessions/2026/01/02/r.jsonl.zst")), file(Source::Codex, "/link/codex/sessions/2026/01/02/r.jsonl.zst"));
        assert_eq!(f.classify(Path::new("/real/codex/session_index.jsonl")), Some(Change::Titles));
        assert_eq!(f.classify(Path::new("/real/agents/12.json")), Some(Change::Registry));
    }

    #[test]
    fn a_subagent_names_its_session_and_isnt_taken_for_one() {
        let f = folders();
        let agent = Some(Change::Agent("one".to_owned()));
        assert_eq!(f.classify(Path::new("/real/projects/a/one/subagents/agent.jsonl")), agent);
        assert_eq!(f.classify(Path::new("/real/projects/a/one/subagents/workflows/wf/agent.jsonl")), agent);
        assert_eq!(f.classify(Path::new("/real/projects/a/one/subagents/agent.meta.json")), None);
    }

    #[test]
    fn what_isnt_a_session_is_ignored() {
        let f = folders();
        assert_eq!(f.classify(Path::new("/real/projects/a/one/tool-results/x.jsonl")), None);
        assert_eq!(f.classify(Path::new("/real/projects/a/memory.md")), None);
        assert_eq!(f.classify(Path::new("/real/agents/12.abc.key")), None);
        assert_eq!(f.classify(Path::new("/real/codex/history.jsonl")), None);
        assert_eq!(f.classify(Path::new("/elsewhere/one.jsonl")), None);
    }

    #[test]
    fn a_folder_yet_to_exist_is_known_by_its_parents_real_path() {
        let parent = std::env::temp_dir().canonicalize().unwrap();
        let missing = std::env::temp_dir().join(format!("sessions-missing-{}", std::process::id()));
        assert_eq!(Folder::new(&missing).real, parent.join(missing.file_name().unwrap()));
    }

    /// Waits for a change to `path` to be noted, taking what's noted meanwhile.
    fn noted(watcher: &mut Watcher, path: &Path) -> bool {
        let started = std::time::Instant::now();
        while started.elapsed() < std::time::Duration::from_secs(5) {
            if watcher.take().files.iter().any(|(_, p)| p == path) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn writes_to_a_session_file_held_open_are_heard_as_they_happen() {
        use std::io::Write;
        let root = std::env::temp_dir().join(format!("sessions-held-open-{}", std::process::id())).canonicalize_or_self();
        let day = root.join("codex/sessions/2026/01/02");
        std::fs::create_dir_all(&day).unwrap();
        let roots = Roots::under(&root);
        let mut watcher = Watcher::start(&roots, &root.join("agents"), Arc::new(|| {}));
        std::thread::sleep(std::time::Duration::from_millis(300));
        let path = day.join("rollout-held.jsonl");
        // Like Codex: opened once and written to for as long as it runs.
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(b"{}\n").unwrap();
        assert!(noted(&mut watcher, &path), "its creation");
        for turn in 0..3 {
            std::thread::sleep(std::time::Duration::from_millis(300));
            watcher.take();
            file.write_all(b"{}\n").unwrap();
            assert!(noted(&mut watcher, &path), "write {turn}, with the file still open");
        }
        drop(file);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_older_read_of_the_registry_never_lands_after_a_newer_one() {
        let dir = std::env::temp_dir().join(format!("sessions-scans-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let folder = |p: &Path| Folder { given: p.to_owned(), real: p.to_owned() };
        let folders = Folders { sessions: Vec::new(), registry: folder(&dir) };
        // SAFETY: takes nothing.
        let queue = unsafe { libc::kqueue() };
        let inner = Arc::new(Inner { changes: Mutex::default(), wake: Arc::new(|| {}), folders, queue, scanning: Mutex::default(), followed: Mutex::default() });
        let pid = std::process::id();
        let write = |status: &str| {
            let draft = dir.join("draft");
            std::fs::write(&draft, format!("{{\"pid\":{pid},\"sessionId\":\"a\",\"status\":\"{status}\"}}")).unwrap();
            std::fs::rename(&draft, dir.join(format!("{pid}.json"))).unwrap();
        };
        for _ in 0..3_000 {
            write("busy");
            let early = inner.clone();
            let older = std::thread::spawn(move || early.scan());
            write("idle");
            inner.scan();
            older.join().unwrap();
            let running = std::mem::take(&mut *inner.changes.lock().unwrap()).running.unwrap();
            assert_eq!(running.sessions["a"], Activity::Open);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
