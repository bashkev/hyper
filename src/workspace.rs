//! The Workspace: where agents and shells are started and run, each in a terminal. The sidebar
//! switches between them and everything keeps running behind the one on screen.

//!
//! - [`launcher`]: where to start, and what: the palette over the terminal, or the page when
//!   nothing is running.
//! - [`alerts`]: what the terminals say, and telling the person when one needs them.
//! - [`tidy`]: a worktree whose last terminal closed is removed, or asked about.
//! - [`keyboard`]: whether typing goes to the terminal on screen, and telling its program.
//! - [`find`]: ⌘F in a terminal.
//! - [`handoff`]: terminals passed from this build to the next on a reload.
//! - [`view`]: the sidebar's rows, the terminal pane and the Workspace's dialogs.

mod alerts;
mod find;
mod handoff;
mod keyboard;
mod launcher;
mod tidy;
mod view;

pub use tidy::tidy_dialog;
pub use view::{live_row, live_title, new_button, pane, plus, quit_dialog, resume_button, title, worktree_row};

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rune::{Latest, PopupMenu, TextEdit, Tx};

use crate::data::{Session, Source};
use crate::live::Activity;
use crate::notifications::Event as Notice;
use crate::settings::Order;
use crate::sidebar::{Item, SIDEBAR_SCROLL, Worktree};
use crate::term::{self, Term};
use crate::{App, View, attention, prompt, toggle_in, worktree};
use tidy::Tidy;

/// Sessions with no real folder behind them, so nothing can start there.
const NO_PROJECT: &str = "No project";

/// A worktree made for the launcher: what to start in it, or why it couldn't be made.
type Made = (Tool, String, Result<PathBuf, String>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Agent(Source),
    Shell,
}

impl Tool {
    /// Each agent, then the shell.
    pub const ALL: [Tool; Source::ALL.len() + 1] = {
        let mut all = [Tool::Shell; Source::ALL.len() + 1];
        let mut i = 0;
        while i < Source::ALL.len() {
            all[i] = Tool::Agent(Source::ALL[i]);
            i += 1;
        }
        all
    };

    pub fn label(self) -> &'static str {
        match self {
            Tool::Agent(source) => source.label(),
            Tool::Shell => "Shell",
        }
    }

    /// Its name in settings.
    pub fn id(self) -> &'static str {
        match self {
            Tool::Agent(source) => source.agent().id(),
            Tool::Shell => "shell",
        }
    }

    pub fn from_id(id: &str) -> Option<Tool> {
        Tool::ALL.into_iter().find(|t| t.id() == id)
    }

    /// Where it sits in the launcher's tool picker.
    pub fn index(self) -> usize {
        Tool::ALL.iter().position(|&t| t == self).unwrap_or(0)
    }

    /// The command that starts it new, and its session's id when that can be given up front.
    /// Other sessions are matched to theirs when it appears.
    fn start(self, id: &str) -> (Vec<String>, Option<String>) {
        match self {
            Tool::Agent(source) => source.agent().start(id),
            Tool::Shell => (vec![term::login_shell()], None),
        }
    }
}

/// An agent's command with what Hyper adds when it starts one: how it tells Hyper what happens,
/// and `prompt` appended to its system prompt. Only the agent's plain program gets them.
fn with_hyper_args(tool: Tool, mut argv: Vec<String>, prompt: &str) -> Vec<String> {
    let Tool::Agent(source) = tool else { return argv };
    let agent = source.agent();
    if argv.first().is_none_or(|a| a != agent.id()) {
        return argv;
    }
    let added = agent.hyper_args(prompt);
    argv.splice(1..1, added);
    argv
}

/// Something running in a terminal, or that ended badly and stays to be read.
pub struct Live {
    pub id: u64,
    pub tool: Tool,
    pub project: String,
    cwd: PathBuf,
    /// The worktree of its project it runs in, if not the project itself.
    pub worktree: Option<String>,
    /// Which session it is, once that's known.
    pub session: Option<String>,
    /// The sessions that already were in its folder when it started, so can't be its own.
    known: HashSet<String>,
    pub term: Term,
    /// Set when it failed: its exit code, if it has one.
    pub failed: Option<Option<i32>>,
    /// It finished or asked for something while the person was elsewhere, and they haven't
    /// looked since.
    pub needs_you: bool,
    /// As of the last look, for telling when a turn ends.
    working: bool,
    /// Whether its turn is going, as it last told Hyper itself, which beats what its session file
    /// says: that file can't tell a turn going on from one waiting for an answer to a dialog.
    /// `None` until it says it's working, so an agent that never says so goes by its file.
    said: Option<bool>,
    /// The person has looked since its last turn ended, so a reminder that it's idle isn't news.
    seen: bool,
    /// Its notification is up.
    posted: bool,
    posted_event: Option<Notice>,
}

impl Live {
    #[cfg(test)]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn running(&self) -> bool {
        self.failed.is_none()
    }

    #[cfg(test)]
    pub fn working(&self) -> bool {
        self.working
    }

    #[cfg(test)]
    pub fn said(&self) -> Option<bool> {
        self.said
    }
}

/// A launcher row: somewhere to start, a session to pick up, or a folder to look for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    Folder(String, PathBuf),
    Resume(String),
    ChooseFolder,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Main {
    Launcher,
    Terminal(u64),
}

/// The size the terminal pane was last laid out at, and its cell size.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pane {
    width: f32,
    height: f32,
    cell: (f32, f32),
}

pub struct Workspace {
    pub live: Vec<Live>,
    next: u64,
    pub main: Main,
    last_terminal: Option<u64>,
    pane: Option<Pane>,
    /// Where something can be started, as of the last relist.
    projects: Vec<(String, PathBuf)>,
    project: Option<String>,
    /// Folders picked explicitly, including ones with no session history.
    folders: Vec<PathBuf>,
    folder: Option<PathBuf>,
    error: Option<String>,
    pub tool: usize,
    /// Whether Start makes a new worktree, and what to call it.
    pub worktree: bool,
    pub worktree_name: TextEdit,
    making: Latest<Made>,
    /// A worktree is being made, so Start waits.
    pub creating: bool,
    /// Worktree groups the user folded, by key. They start open.
    closed_worktrees: HashSet<String>,
    /// Worktrees being checked or removed report back here.
    tidy: (std::sync::mpsc::Sender<Tidy>, std::sync::mpsc::Receiver<Tidy>),
    /// Worktrees still being checked or removed, so each is only worked on once at a time.
    tidying: HashSet<PathBuf>,
    /// Those of them being removed, which nothing may start in.
    removing: HashSet<PathBuf>,
    /// Worktrees with work in them that would be lost, waiting for the person to say whether
    /// to remove them. The first is being asked about.
    pub asking: Vec<(PathBuf, worktree::Unsaved)>,
    /// What tests start in place of the real tools.
    #[cfg(test)]
    pub argv: Option<Vec<String>>,
    /// The launcher is up over the terminal on screen. With no terminal, it's the main pane.
    pub palette: bool,
    pub query: TextEdit,
    /// What ⌘F looks for in a terminal, and the terminal its bar is over.
    pub find: TextEdit,
    finding: Option<u64>,
    /// The terminal last told whether it has the keyboard, and what it was told.
    told: Option<(u64, bool)>,
    /// The launcher row the keyboard is on.
    active: Option<Choice>,
    /// The launcher is asking what to call a new worktree.
    pub naming: bool,
    window_focused: bool,
    pub confirming_quit: bool,
    /// Rows keep their place when the list changes, which would leave a project that's just
    /// moved to the top above the view.
    scroll_up: bool,
    /// What the Dock icon shows.
    badged: usize,
}

impl Default for Workspace {
    fn default() -> Workspace {
        Workspace { live: Vec::new(), next: 1, main: Main::Launcher, last_terminal: None, pane: None, projects: Vec::new(), project: None, folders: Vec::new(), folder: None, error: None, tool: 0, worktree: false, worktree_name: TextEdit::new(""), making: Latest::default(), creating: false, closed_worktrees: HashSet::new(), tidy: std::sync::mpsc::channel(), tidying: HashSet::new(), removing: HashSet::new(), asking: Vec::new(),
            #[cfg(test)]
            argv: None, palette: false, query: focused(""), find: TextEdit::new(""), finding: None, told: None, active: None, naming: false, window_focused: true, confirming_quit: false, scroll_up: false, badged: 0 }
    }
}

/// A field that has the keyboard as soon as it shows.
fn focused(text: &str) -> TextEdit {
    let mut edit = TextEdit::new(text);
    edit.focused = true;
    edit
}

/// A random UUID, as Claude's `--session-id` wants.
fn uuid() -> Option<String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom").ok()?.read_exact(&mut b).ok()?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    Some(format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]))
}

impl App {
    pub fn workspace_folder(&self) -> Option<PathBuf> {
        self.current().map(|l| l.cwd.clone()).or_else(|| self.ws.folder.clone())
    }

    /// The launcher's tool and worktree switch as Settings choose them.
    fn launcher_defaults(&mut self) {
        self.ws.tool = self.default_tool().index();
        self.ws.worktree = self.settings.default_worktree;
    }

    pub fn configure_workspace(&mut self) {
        self.launcher_defaults();
        if self.settings.remember_project && let Some(folder) = self.settings.last_folder.clone().filter(|p| p.is_dir()) {
            self.choose_folder(folder);
        }
    }

    pub fn current(&self) -> Option<&Live> {
        let Main::Terminal(id) = self.ws.main else { return None };
        self.live(id)
    }

    fn current_mut(&mut self) -> Option<&mut Live> {
        let Main::Terminal(id) = self.ws.main else { return None };
        self.live_mut(id)
    }

    pub fn live(&self, id: u64) -> Option<&Live> {
        self.ws.live.iter().find(|l| l.id == id)
    }

    fn live_mut(&mut self, id: u64) -> Option<&mut Live> {
        self.ws.live.iter_mut().find(|l| l.id == id)
    }

    /// Whether a terminal's agent is in the middle of a turn: as it last said, or else as its
    /// session's file says.
    fn live_working(&self, live: &Live) -> bool {
        let session = live.session.as_deref().and_then(|id| self.session(id));
        live.said.unwrap_or_else(|| session.and_then(|s| self.activity(s)) == Some(Activity::Working))
    }

    /// Agents that quitting would stop. A shell has nothing that can't be started again.
    pub fn running_agents(&self) -> usize {
        self.ws.live.iter().filter(|l| l.running() && matches!(l.tool, Tool::Agent(_))).count()
    }

    fn live_for(&self, session: &str) -> Option<u64> {
        self.ws.live.iter().find(|l| l.session.as_deref() == Some(session)).map(|l| l.id)
    }

    /// Whether a terminal here is running this session, so its process is known to be alive.
    pub fn running_here(&self, session: &str) -> bool {
        self.ws.live.iter().any(|l| l.running() && l.session.as_deref() == Some(session))
    }

    /// Whether this session's turn is going, as the terminal here running it last said.
    pub fn said_here(&self, session: &str) -> Option<bool> {
        self.ws.live.iter().filter(|l| l.running() && l.session.as_deref() == Some(session)).find_map(|l| l.said)
    }

    /// Every project something can be started in, newest first, with its folder.
    pub fn projects(&self) -> &[(String, PathBuf)] {
        &self.ws.projects
    }

    /// A project's sessions may have run in several folders under it, such as its worktrees.
    /// The shortest is its root, and the newest of equals is the one in use.
    fn find_projects(&self) -> Vec<(String, PathBuf)> {
        let mut places: HashMap<&str, usize> = HashMap::new();
        let mut projects: Vec<(String, PathBuf)> = Vec::new();
        let mut looked = HashSet::new();
        for session in self.sessions() {
            if session.project == NO_PROJECT || session.cwd.is_empty() || !looked.insert(session.cwd.as_str()) || !Path::new(&session.cwd).is_dir() {
                continue;
            }
            // A worktree's own folder is never the project's root while its repository is there.
            let folder = worktree::repo_of(Path::new(&session.cwd)).filter(|repo| repo.is_dir()).unwrap_or_else(|| PathBuf::from(&session.cwd));
            match places.get(session.project.as_str()) {
                Some(&at) if folder.as_os_str().len() < projects[at].1.as_os_str().len() => projects[at].1 = folder,
                Some(_) => {}
                None => {
                    places.insert(&session.project, projects.len());
                    projects.push((session.project.clone(), folder));
                }
            }
        }
        for path in &self.ws.folders {
            if path.is_dir() && !projects.iter().any(|(_, p)| p == path) {
                projects.push((folder_project(path), path.clone()));
            }
        }
        // Each project's kept worktrees come right after it, so they can be started in again.
        let roots: Vec<PathBuf> = projects.iter().map(|(_, p)| p.clone()).collect();
        let mut listed: Vec<(String, PathBuf)> = Vec::new();
        for (name, path) in projects {
            if worktree::repo_of(&path).is_some_and(|repo| roots.contains(&repo)) {
                continue;
            }
            let trees = worktree::list(&path);
            listed.push((name.clone(), path));
            listed.extend(trees.into_iter().filter(|tree| !self.ws.removing.contains(tree)).map(|tree| (name.clone(), tree)));
        }
        listed
    }

    /// Starts the tool of the terminal on screen again, as [`App::start_here`] does. With no
    /// terminal, the launcher opens with Settings' default tool.
    pub fn new_session_here(&mut self, tx: &mut Tx) {
        match self.current().map(|live| live.tool) {
            Some(tool) => self.start_here(tool, tx),
            None if self.ws.creating => {}
            None => self.new_session(None, Some(tx)),
        }
    }

    /// Starts `tool` where the program on screen is working, which a shell's `cd` may have
    /// moved from the folder it started in, keeping its project. With no terminal there's no
    /// folder to go by, so the launcher opens with `tool` picked.
    pub fn start_here(&mut self, tool: Tool, tx: &mut Tx) {
        if self.ws.creating {
            return;
        }
        let here = self.current().map(|live| {
            // An ended program's pid could belong to another process by now.
            let now = live.term.pid().filter(|_| live.running()).and_then(worktree::cwd_of);
            (live.project.clone(), now.unwrap_or_else(|| live.cwd.clone()))
        });
        let Some((project, cwd)) = here else {
            self.new_session(None, Some(tx));
            self.ws.tool = tool.index();
            return;
        };
        self.set_view(View::Workspace);
        self.start_in(tool, project, cwd);
        tx.blur();
    }

    /// Opens the launcher: over the terminal on screen, or as the main pane when there's none.
    /// `project` is chosen if one was asked for, else the folder of the terminal on screen.
    pub fn new_session(&mut self, project: Option<String>, tx: Option<&mut Tx>) {
        self.set_view(View::Workspace);
        // Its search takes the keyboard, from a web page too.
        self.browser_release_keyboard();
        self.launcher_defaults();
        self.ws.error = None;
        self.ws.naming = false;
        // Rune moves focus to a field only when no other field still asks for it.
        self.ws.find.focused = false;
        self.ws.query = focused("");
        self.ws.worktree_name = TextEdit::new("");
        self.ws.active = None;
        if project.is_some() {
            self.ws.project = project;
            self.ws.folder = None;
        } else if let Some(cwd) = self.current().map(|l| l.cwd.clone()) {
            self.ws.folder = Some(cwd);
        }
        self.ws.palette = matches!(self.ws.main, Main::Terminal(_));
        self.tell_focus();
        if let Some(tx) = tx {
            tx.scroll_to_y(SIDEBAR_SCROLL, 0.0);
        }
    }

    /// Starts `tool` at once, from the sidebar.
    pub fn start_now(&mut self, project: String, folder: PathBuf, tool: Tool, new_worktree: bool) {
        // One worktree at a time, and the launcher still shows where it's going.
        if self.ws.creating {
            return;
        }
        self.set_view(View::Workspace);
        self.ws.project = Some(project);
        self.ws.folder = Some(folder);
        self.ws.tool = tool.index();
        self.ws.worktree = new_worktree;
        self.ws.worktree_name = TextEdit::new("");
        self.start_from_launcher();
    }

    /// Where a project in the sidebar starts: its own folder, never one of its worktrees.
    fn project_folder(&self, project: &str) -> Option<PathBuf> {
        let listed = self.projects().iter().find(|(name, path)| name == project && worktree::name_of(path).is_none()).map(|(_, path)| path.clone());
        listed.or_else(|| {
            let live = self.ws.live.iter().find(|l| l.project == project)?;
            if live.worktree.is_some() { worktree::repo_of(&live.cwd) } else { Some(live.cwd.clone()) }
        })
    }

    /// The sidebar's other ways to start: each tool, and each in a new worktree.
    fn start_menu(&mut self, project: String, folder: PathBuf, worktrees: bool, anchor: rune::PopupAnchor, tx: &mut Tx) {
        let mut menu = PopupMenu::new().items(Tool::ALL.map(|t| rune::PopupItem::new(format!("here-{}", t.id()), t.label())));
        if worktrees {
            let trees = Tool::ALL.map(|t| rune::PopupItem::new(format!("tree-{}", t.id()), t.label()));
            menu = menu.separator().submenu("New Worktree", PopupMenu::new().items(trees));
        }
        tx.popup_menu(menu, anchor, move |s: &mut App, picked, _| {
            let Some(picked) = picked else { return };
            let (new_worktree, id) = match picked.split_once('-') { Some(("tree", id)) => (true, id), Some((_, id)) => (false, id), None => return };
            if let Some(tool) = Tool::from_id(id) {
                s.start_now(project, folder, tool, new_worktree);
            }
        });
    }

    fn start_in(&mut self, tool: Tool, project: String, cwd: PathBuf) {
        let id = uuid().unwrap_or_default();
        #[allow(unused_mut)]
        let (mut argv, session) = tool.start(&id);
        #[cfg(test)]
        if let Some(fake) = &self.ws.argv {
            argv = fake.clone();
        }
        self.launch(tool, project, cwd, argv, session);
    }

    fn launch(&mut self, tool: Tool, project: String, cwd: PathBuf, argv: Vec<String>, session: Option<String>) {
        if self.removing(&cwd) {
            self.ws.error = Some("That worktree is being removed".to_owned());
            return;
        }
        let argv = with_hyper_args(tool, argv, &prompt::text());
        attention::ask();
        let id = self.ws.next;
        self.ws.next += 1;
        let term = Term::new(argv, cwd.clone(), self.dark.get(), self.wake());
        let folder = cwd.to_string_lossy();
        let known = self.sessions().iter().filter(|s| s.cwd == folder).map(|s| s.id.clone()).collect();
        let worktree = worktree::name_of(&cwd);
        self.ws.live.push(Live { id, tool, project: project.clone(), cwd, worktree, session, known, term, failed: None, needs_you: false, working: false, said: None, seen: true, posted: false, posted_event: None });
        self.closed_projects.remove(&project);
        self.open_projects.insert(project);
        self.ws.scroll_up = true;
        self.show(Main::Terminal(id));
    }

    #[cfg(test)]
    pub fn badged(&self) -> usize {
        self.ws.badged
    }

    #[cfg(test)]
    pub fn launch_for_test(&mut self, tool: Tool, cwd: &str, argv: Vec<String>) {
        self.launch(tool, "alpha".to_owned(), cwd.into(), argv, None);
    }

    #[cfg(test)]
    pub fn launch_in_project_for_test(&mut self, project: &str, cwd: &str, argv: Vec<String>) {
        self.launch(Tool::Shell, project.to_owned(), cwd.into(), argv, None);
    }

    /// Picks a past session up in a terminal, or goes to the one it's already in.
    pub fn resume(&mut self, session: &str) {
        self.set_view(View::Workspace);
        if let Some(id) = self.live_for(session) {
            return self.show(Main::Terminal(id));
        }
        let Some(found) = self.session(session).cloned() else { return };
        if let Some(watcher) = &self.watcher {
            watcher.follow(found.source, &found.path);
        }
        self.launch(Tool::Agent(found.source), found.project.clone(), PathBuf::from(&found.cwd), found.resume_argv(), Some(found.id));
    }

    /// Changes what the main pane shows.
    pub fn show(&mut self, main: Main) {
        self.browser_release_keyboard();
        if let Main::Terminal(id) = main { self.ws.last_terminal = Some(id); }
        if self.ws.finding.is_some_and(|id| main != Main::Terminal(id)) {
            self.end_terminal_find();
        }
        self.ws.main = main;
        if main == Main::Launcher {
            // With nothing else on screen, typing goes to the launcher's search.
            self.ws.palette = false;
            self.ws.naming = false;
            self.ws.query.focused = true;
        } else {
            self.close_palette();
        }
        if self.view == View::Workspace {
            // The details pane follows the session behind the terminal.
            self.reader.selected = None;
            self.reader.report = None;
            if let Some(session) = self.current().and_then(|l| l.session.clone()) {
                self.select(&session);
            }
        }
        self.fit_terminal();
        self.tell_focus();
        self.see_current();
        if let Some(live) = self.current_mut() {
            // It wasn't asked for frames behind another: ask, so it's up to date.
            live.term.take_frame();
        }
        self.relist();
    }

    pub fn set_pane(&mut self, width: f32, height: f32, cell: (f32, f32)) {
        // An expanded file pane covers the terminal; keep its PTY at its last useful size.
        // A terminal first started while covered still needs an initial layout to launch.
        if self.project_is_expanded() {
            if self.ws.pane.is_none() {
                self.ws.pane = Some(Pane { width: self.project_terminal_pane_width(), height, cell });
                self.fit_terminal();
            }
            return;
        }
        self.ws.pane = Some(Pane { width, height, cell });
        self.fit_terminal();
    }

    fn fit_terminal(&mut self) {
        let Some(pane) = self.ws.pane else { return };
        if let Some(live) = self.current_mut() {
            live.term.layout(pane.width - 2.0 * term::INSET, pane.height - 2.0 * term::INSET, pane.cell);
        }
    }

    pub fn window_focus(&mut self, focused: bool) {
        self.ws.window_focused = focused;
        if !focused {
            self.swipe_lost();
        }
        self.tell_focus();
        self.see_current();
    }

    /// Closes a terminal, stopping what's in it, and moves on if it was on screen.
    pub fn close_live(&mut self, id: u64) {
        let Some(at) = self.ws.live.iter().position(|l| l.id == id) else { return };
        let closed = self.ws.live.remove(at);
        if closed.posted {
            attention::remove(id);
        }
        self.rebadge();
        self.tidy_after(&closed.cwd);
        if self.ws.main != Main::Terminal(id) {
            return self.relist();
        }
        let same_project = self.ws.live.iter().find(|l| l.project == closed.project);
        let next = same_project.or(self.ws.live.last()).map(|l| l.id);
        // The launcher goes back to the project, not into one of its worktrees.
        self.ws.folder = if closed.worktree.is_some() { worktree::repo_of(&closed.cwd) } else { Some(closed.cwd.clone()) };
        self.ws.project = Some(closed.project);
        self.show(next.map_or(Main::Launcher, Main::Terminal));
    }

    /// Gives a terminal its session once a new one has appeared in its folder.
    fn link_sessions(&mut self) {
        let mut claimed: Vec<String> = self.ws.live.iter().filter_map(|l| l.session.clone()).collect();
        let mut links = Vec::new();
        for live in self.ws.live.iter().filter(|l| l.session.is_none()) {
            let Tool::Agent(source) = live.tool else { continue };
            let cwd = live.cwd.to_string_lossy();
            let found = self.sessions().iter().find(|s| s.source == source && s.cwd == cwd && !live.known.contains(&s.id) && !claimed.contains(&s.id));
            if let Some(found) = found {
                claimed.push(found.id.clone());
                links.push((live.id, found.id.clone()));
            }
        }
        for (id, session) in links {
            if let Some(live) = self.ws.live.iter_mut().find(|l| l.id == id) {
                live.session = Some(session);
            }
        }
    }

    /// What's running, in the order the sidebar lists it: by project, its own terminals before
    /// each worktree's. ⌘1–9 and next/previous follow it, folded or not.
    pub fn live_in_order(&self) -> Vec<&Live> {
        let mut order: Vec<&Live> = Vec::new();
        for (_, live) in self.live_by_project() {
            order.extend(live.iter().filter(|l| l.worktree.is_none()));
            for tree in worktrees(&live) {
                order.extend(live.iter().filter(|l| l.worktree.as_deref() == Some(tree)));
            }
        }
        order
    }

    /// The terminals of each project, in the order the projects were first started. Within a
    /// project they go oldest or newest first, as the sidebar's order says, and so do its worktrees.
    fn live_by_project(&self) -> Vec<(&str, Vec<&Live>)> {
        let names = distinct(self.ws.live.iter().map(|l| l.project.as_str()));
        let newest = self.settings.sidebar_order == Order::Newest;
        names.into_iter().map(|name| {
            let mut live: Vec<&Live> = self.ws.live.iter().filter(|l| l.project == name).collect();
            if newest {
                live.reverse();
            }
            (name, live)
        }).collect()
    }

    /// Lists what's running, by project. Past sessions belong to the launcher and Browse.
    pub fn relist_workspace(&mut self) {
        self.link_sessions();
        self.ws.projects = self.find_projects();
        let mut listed = Vec::new();
        for (name, live) in self.live_by_project() {
            let open = self.open_projects.contains(name);
            let attention = live.iter().any(|l| l.needs_you);
            listed.push(Item::Project { key: format!("project-{name}"), name: name.to_owned(), count: live.len(), open, live: true, attention });
            if !open {
                continue;
            }
            // The project's own terminals, then each worktree's under it.
            listed.extend(live.iter().filter(|l| l.worktree.is_none()).map(|l| Item::Live(l.id)));
            for tree in worktrees(&live) {
                let inside: Vec<&&Live> = live.iter().filter(|l| l.worktree.as_deref() == Some(tree)).collect();
                let key = worktree_key(name, tree);
                let open = !self.ws.closed_worktrees.contains(&key);
                let attention = inside.iter().any(|l| l.needs_you);
                listed.push(Item::Worktree(Worktree { name: tree.to_owned(), key, count: inside.len(), open, attention, folder: inside[0].cwd.clone() }));
                if open {
                    listed.extend(inside.iter().map(|l| Item::Live(l.id)));
                }
            }
        }
        self.listed = listed;

        // A new agent's session only exists once it's been spoken to.
        let behind = self.current().and_then(|l| l.session.clone());
        if let Some(session) = behind.filter(|id| self.view == View::Workspace && self.reader.selected.as_deref() != Some(id.as_str())) {
            self.select(&session);
        }
    }

    /// Newest or oldest first, for each project's terminals. The pick is saved as it's made.
    pub fn order_menu(&mut self, tx: &mut Tx) {
        let current = self.settings.sidebar_order;
        let items = Order::ALL.map(|o| rune::PopupItem::new(o.id(), o.label()).checked(o == current));
        tx.popup_menu(PopupMenu::new().items(items), "sidebar-order", |s: &mut App, picked, _| {
            let Some(order) = picked.and_then(Order::from_id) else { return };
            let mut changed = s.settings.clone();
            changed.sidebar_order = order;
            if changed != s.settings && s.persist_settings(changed) {
                s.relist();
            }
        });
    }

    fn default_tool(&self) -> Tool {
        Tool::from_id(&self.settings.default_tool).unwrap_or(Tool::ALL[0])
    }

    fn toggle_worktree(&mut self, key: &str) {
        toggle_in(&mut self.ws.closed_worktrees, key);
        self.relist();
    }

    /// Not if it's running somewhere this app can't reach, or its folder has gone.
    pub fn can_resume(&self, session: &Session) -> bool {
        self.live_for(&session.id).is_some() || (self.activity(session).is_none() && Path::new(&session.cwd).is_dir())
    }
}

/// The worktrees these terminals run in, each once, in the order they're first met.
fn worktrees<'a>(live: &[&'a Live]) -> Vec<&'a str> {
    distinct(live.iter().filter_map(|l| l.worktree.as_deref()))
}

/// Each name once, where it first appears.
fn distinct<'a>(names: impl Iterator<Item = &'a str>) -> Vec<&'a str> {
    let mut seen = Vec::new();
    for name in names {
        if !seen.contains(&name) {
            seen.push(name);
        }
    }
    seen
}

fn worktree_key(project: &str, name: &str) -> String {
    format!("worktree-{project}/{name}")
}

fn folder_name(path: &Path) -> String {
    path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned()
}

/// The project a picked folder belongs to, as its sessions would say: a worktree's is its
/// repository's.
fn folder_project(path: &Path) -> String {
    worktree::repo_of(path).map_or_else(|| folder_name(path), |repo| folder_name(&repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_id_is_a_version_4_uuid() {
        let id = uuid().unwrap();
        let groups: Vec<usize> = id.split('-').map(str::len).collect();
        assert_eq!(groups, [8, 4, 4, 4, 12]);
        assert_eq!(id.as_bytes()[14], b'4');
        assert!(b"89ab".contains(&id.as_bytes()[19]));
        assert_ne!(id, uuid().unwrap());
    }

    #[test]
    fn agents_get_the_prompt_appended_and_codex_can_parse_it() {
        let prompt = "Say \"hi\".\nDocs: /Applications/Hyper.app/Contents/Resources/README.md";
        let claude = with_hyper_args(Tool::Agent(Source::Claude), vec!["claude".into(), "--resume".into(), "abc".into()], prompt);
        let at = claude.iter().position(|a| a == "--append-system-prompt").expect("Claude gets the prompt");
        assert_eq!(claude[at + 1], prompt);
        assert_eq!(claude[claude.len() - 2..], ["--resume", "abc"], "the session stays last");

        let pi = with_hyper_args(Tool::Agent(Source::Pi), vec!["pi".into()], prompt);
        let at = pi.iter().position(|a| a == "--append-system-prompt").expect("Pi gets the prompt");
        assert_eq!(pi[at + 1], prompt);

        let codex = with_hyper_args(Tool::Agent(Source::Codex), vec!["codex".into(), "resume".into(), "abc".into()], prompt);
        let set = codex.iter().find_map(|a| a.strip_prefix("developer_instructions=")).expect("Codex gets the prompt");
        let parsed: toml::Table = format!("v = {set}").parse().expect("Codex reads -c values as TOML");
        assert_eq!(parsed["v"].as_str(), Some(prompt));
        assert_eq!(codex[codex.len() - 2..], ["resume", "abc"]);

        let custom = vec!["npx".to_owned(), "claude".to_owned()];
        assert_eq!(with_hyper_args(Tool::Agent(Source::Claude), custom.clone(), prompt), custom, "only plain commands are changed");
        assert_eq!(with_hyper_args(Tool::Shell, vec!["zsh".into()], prompt), ["zsh"]);
    }
}
