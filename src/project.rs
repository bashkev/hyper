//! The project beside a running terminal: file tabs and an outline on their right.

mod data;
#[cfg(test)]
mod tests;
mod view;
mod watch;

pub use view::{CHANGES, expand_control, pane, tabs};

use crate::{App, Load, View};
use data::{Area, Change, Code, Context, Entry, Preview, Status};
use rune::{Key, KeyEvent, Latest, PopupAnchor, PopupItem, PopupMenu, TextEdit, Tx, Waker};
use rune_kit::diff::DiffState;
use rune_kit::{TreeKey, TreeModel};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// The narrowest Files or Changes pane, and the room it leaves the terminal beside it.
const PANE_MIN: f32 = 320.0;
const TERMINAL_MIN: f32 = 360.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Details,
    Files,
    Changes,
    Browser,
}
impl Mode {
    /// Files and Changes follow the session's repository; Details and Browser don't.
    pub fn lists_files(self) -> bool {
        matches!(self, Mode::Files | Mode::Changes)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Tab {
    path: PathBuf,
    change: Option<Change>,
}
impl Tab {
    fn mode(&self) -> Mode {
        if self.change.is_some() { Mode::Changes } else { Mode::Files }
    }
    fn id(&self) -> String {
        self.change.as_ref().map_or_else(|| data::key(&self.path), Change::key)
    }
}

#[derive(Clone)]
struct Node {
    path: PathBuf,
    label: String,
    dir: bool,
    ignored: bool,
    placeholder: bool,
    change: Option<Change>,
    line: Option<usize>,
    /// A Changes group's number of files.
    count: Option<usize>,
}
impl Node {
    fn label(&self) -> &str {
        &self.label
    }
}
type Directories = HashMap<PathBuf, Result<Vec<Entry>, String>>;
/// A folder and what listing it found, or why it couldn't.
type DirListing = (PathBuf, Result<Vec<Entry>, String>);
struct Opened {
    ctx: Context,
    watch: Result<watch::Watch, String>,
    entries: Result<Vec<Entry>, String>,
}

pub struct Project {
    pub mode: Mode,
    pub window_width: f32,
    details_width: f32,
    width: Option<f32>,
    expanded: bool,
    tree_width: f32,
    tree_drag: f32,
    tree_shown: bool,
    show_ignored: bool,
    wrap: bool,
    seed: Option<PathBuf>,
    ctx: Option<Context>,
    watcher: Option<watch::Watch>,
    watch_error: Option<String>,
    opening: Latest<Result<Opened, String>>,
    starting: bool,
    directories: Directories,
    pending_dirs: HashSet<PathBuf>,
    dir_job: Latest<Vec<DirListing>>,
    files: TreeModel<Node>,
    changes: TreeModel<Node>,
    status: Load<Status>,
    status_job: Latest<Result<Status, String>>,
    query: TextEdit,
    search: Latest<Result<Vec<data::SearchHit>, String>>,
    search_paths: Option<Result<Vec<data::SearchHit>, String>>,
    open_folders: HashSet<String>,
    tabs: Vec<Tab>,
    active: Option<usize>,
    preview: Load<()>,
    preview_job: Latest<(Tab, Result<Preview, String>)>,
    code: Option<Code>,
    diff: DiffState,
    note: Option<String>,
    find: TextEdit,
    finding: bool,
    matches: Vec<(usize, std::ops::Range<usize>)>,
    match_lines: HashMap<usize, std::ops::Range<usize>>,
    matched: usize,
    code_rev: u64,
    tree_rev: u64,
    pub(crate) keyboard: bool,
    reveal_line: Option<usize>,
    breadcrumb: Option<(PathBuf, PopupAnchor)>,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            mode: Mode::Details,
            window_width: 1240.0,
            details_width: 260.0,
            width: None,
            expanded: false,
            tree_width: 210.0,
            tree_drag: 0.0,
            tree_shown: true,
            show_ignored: false,
            wrap: true,
            seed: None,
            ctx: None,
            watcher: None,
            watch_error: None,
            opening: Latest::default(),
            starting: false,
            directories: HashMap::new(),
            pending_dirs: HashSet::new(),
            dir_job: Latest::default().debounce(Duration::from_millis(80)),
            files: TreeModel::new().key("project-files"),
            changes: TreeModel::new().key("project-changes-tree"),
            status: Load::Waiting,
            status_job: Latest::default().debounce(Duration::from_millis(100)),
            query: TextEdit::new(""),
            search: Latest::default().debounce(Duration::from_millis(120)),
            search_paths: None,
            open_folders: HashSet::new(),
            tabs: Vec::new(),
            active: None,
            preview: Load::Ready(()),
            preview_job: Latest::default().debounce(Duration::from_millis(25)),
            code: None,
            diff: DiffState::default(),
            note: None,
            find: TextEdit::new(""),
            finding: false,
            matches: Vec::new(),
            match_lines: HashMap::new(),
            matched: 0,
            code_rev: 1,
            tree_rev: 1,
            keyboard: false,
            reveal_line: None,
            breadcrumb: None,
        }
    }
}

impl Project {
    fn tree(&self) -> &TreeModel<Node> {
        if self.mode == Mode::Changes { &self.changes } else { &self.files }
    }
    fn tree_mut(&mut self) -> &mut TreeModel<Node> {
        if self.mode == Mode::Changes { &mut self.changes } else { &mut self.files }
    }
    fn tab(&self) -> Option<&Tab> {
        self.active.and_then(|i| self.tabs.get(i))
    }
    fn clear_preview(&mut self) {
        self.code_rev = self.code_rev.wrapping_add(1);
        self.code = None;
        self.diff.set_files(Vec::new());
        self.note = None;
        self.matches.clear();
        self.match_lines.clear();
    }
    fn request_preview(&mut self) {
        self.clear_preview();
        if self.tab().is_none() || self.ctx.is_none() {
            self.preview = Load::Ready(());
            return;
        }
        self.preview = Load::Waiting;
        self.load_preview();
    }
    /// Reads the open tab again, keeping what it shows until the new contents arrive.
    fn refresh_preview(&mut self) {
        if self.code.is_none() && self.diff.files().is_empty() && self.note.is_none() {
            return self.request_preview();
        }
        self.load_preview();
    }
    fn load_preview(&mut self) {
        let (Some(ctx), Some(tab)) = (self.ctx.clone(), self.tab().cloned()) else { return };
        self.preview_job.request((ctx, tab), |(ctx, tab)| {
            let result = match &tab.change {
                Some(c) => data::diff(&ctx, c),
                None => data::file(&ctx, &tab.path),
            };
            (tab, result)
        });
    }
    fn open(&mut self, path: PathBuf, change: Option<Change>) {
        let tab = Tab { path, change };
        if let Some(i) = self.tabs.iter().position(|t| t.id() == tab.id()) {
            if self.active == Some(i) {
                return;
            }
            self.active = Some(i);
        } else {
            self.tabs.push(tab);
            self.active = Some(self.tabs.len() - 1);
        }
        self.finding = false;
        self.request_preview();
    }
    fn select_tab(&mut self, i: usize) {
        if self.active == Some(i) || i >= self.tabs.len() {
            return;
        }
        self.active = Some(i);
        self.finding = false;
        self.request_preview();
    }
    fn close_tab(&mut self, i: usize) {
        self.tabs.remove(i);
        match self.active {
            Some(active) if active == i => {
                self.active = self.tabs.iter().rposition(|t| t.mode() == self.mode);
                self.request_preview();
            }
            Some(active) if active > i => self.active = Some(active - 1),
            _ => {}
        }
    }
    fn request_dirs(&mut self, paths: impl IntoIterator<Item = PathBuf>) {
        let Some(ctx) = self.ctx.clone() else { return };
        self.pending_dirs.extend(paths);
        if self.pending_dirs.is_empty() {
            return;
        }
        let paths: Vec<_> = self.pending_dirs.iter().cloned().collect();
        self.dir_job.request((ctx, paths), |(ctx, paths)| {
            paths
                .into_iter()
                .map(|p| {
                    let result = data::directory(&ctx, &p);
                    (p, result)
                })
                .collect()
        });
    }
    fn remember_folders(&mut self) {
        if self.search_paths.is_some() {
            return;
        }
        for entries in self.directories.values().filter_map(|r| r.as_ref().ok()) {
            for e in entries.iter().filter(|e| e.dir) {
                let id = data::key(&e.path);
                if self.files.is_expanded(&id) {
                    self.open_folders.insert(id);
                } else {
                    self.open_folders.remove(&id);
                }
            }
        }
    }
    fn rebuild_files(&mut self) {
        self.tree_rev = self.tree_rev.wrapping_add(1);
        let selected = self.files.cursor().map(str::to_owned);
        let mut tree = TreeModel::new().key("project-files");
        if let Some(Ok(paths)) = &self.search_paths {
            for hit in paths {
                let label = if let Some(line) = hit.line {
                    format!("{}:{line}", hit.path.display())
                } else {
                    hit.path.to_string_lossy().into_owned()
                };
                tree.insert(
                    None,
                    format!("{}:{}", data::key(&hit.path), hit.line.unwrap_or(0)),
                    Node { path: hit.path.clone(), label, dir: false, ignored: false, placeholder: false, change: None, line: hit.line, count: None },
                );
            }
        } else {
            fn emit(tree: &mut TreeModel<Node>, cache: &Directories, dir: &Path, parent: Option<&str>, ignored: bool) {
                let Some(Ok(entries)) = cache.get(dir) else { return };
                for e in entries.iter().filter(|e| ignored || !e.ignored) {
                    let id = data::key(&e.path);
                    tree.insert(
                        parent,
                        &id,
                        Node {
                            path: e.path.clone(),
                            label: e.name.clone(),
                            dir: e.dir,
                            ignored: e.ignored,
                            placeholder: false,
                            change: None,
                            line: None, count: None,
                        },
                    );
                    if e.dir {
                        match cache.get(&e.path) {
                            Some(Ok(_)) => emit(tree, cache, &e.path, Some(&id), ignored),
                            result => {
                                tree.insert(
                                    Some(&id),
                                    format!("{id}-pending"),
                                    Node {
                                        path: e.path.clone(),
                                        label: result.and_then(|r| r.as_ref().err()).cloned().unwrap_or_else(|| "Loading…".into()),
                                        dir: false,
                                        ignored: false,
                                        placeholder: true,
                                        change: None,
                                        line: None, count: None,
                                    },
                                );
                            }
                        }
                    }
                }
            }
            emit(&mut tree, &self.directories, Path::new(""), None, self.show_ignored);
            for id in &self.open_folders {
                tree.set_expanded(id, true);
            }
        }
        if let Some(id) = selected {
            tree.select(&id);
        }
        self.files = tree;
        if let Some(watcher) = &self.watcher {
            *watcher.ignored.lock().unwrap() = if self.show_ignored {
                Vec::new()
            } else {
                self.directories
                    .values()
                    .filter_map(|r| r.as_ref().ok())
                    .flatten()
                    .filter(|e| e.ignored)
                    .filter_map(|e| self.ctx.as_ref().map(|ctx| ctx.root.join(&e.path)))
                    .collect()
            };
        }
    }
    fn rebuild_changes(&mut self) {
        self.tree_rev = self.tree_rev.wrapping_add(1);
        let old = &self.changes;
        let mut tree = TreeModel::new().key("project-changes-tree");
        if let Load::Ready(status) = &self.status {
            let query = self.query.text.to_lowercase();
            for area in [Area::Staged, Area::Unstaged, Area::Untracked] {
                let changes: Vec<_> =
                    status.changes.iter().filter(|c| c.area == area && c.path.to_string_lossy().to_lowercase().contains(&query)).collect();
                if changes.is_empty() {
                    continue;
                }
                let id = area.label();
                tree.insert(
                    None,
                    id,
                    Node {
                        path: PathBuf::new(),
                        label: area.label().into(),
                        dir: true,
                        ignored: false,
                        placeholder: true,
                        change: None,
                        line: None,
                        count: Some(changes.len()),
                    },
                );
                for change in changes {
                    tree.insert(
                        Some(id),
                        change.key(),
                        Node {
                            path: change.path.clone(),
                            label: change.path.to_string_lossy().into_owned(),
                            dir: false,
                            ignored: false,
                            placeholder: false,
                            change: Some(change.clone()),
                            line: None, count: None,
                        },
                    );
                }
                tree.set_expanded(id, !old.contains(id) || old.is_expanded(id));
            }
        }
        if let Some(id) = old.cursor() {
            tree.select(id);
        }
        self.changes = tree;
    }
    fn filter(&mut self) {
        if self.mode == Mode::Changes {
            self.rebuild_changes();
            return;
        }
        self.remember_folders();
        self.search.cancel();
        if self.query.text.is_empty() {
            self.search_paths = None;
            self.rebuild_files();
            return;
        }
        let Some(ctx) = self.ctx.clone() else { return };
        // Earlier results stay listed until the new ones land, so live updates don't flash.
        if !matches!(self.search_paths, Some(Ok(_))) {
            self.search_paths = Some(Ok(Vec::new()));
        }
        let (query, ignored) = (self.query.text.clone(), self.show_ignored);
        self.search.request((ctx, query, ignored), |(ctx, q, ignored)| data::search(&ctx, &q, ignored));
    }
    fn find_matches(&mut self) {
        self.matches.clear();
        self.match_lines.clear();
        self.matched = 0;
        let query = self.find.text.to_lowercase();
        if query.is_empty() {
            return;
        }
        if let Some(code) = &self.code {
            for (i, line) in code.lines.iter().enumerate() {
                // Unicode lowercase can alter offsets; fall back to literal matching there.
                let lower = line.text.to_lowercase();
                let (hay, needle) = if lower.len() == line.text.len() {
                    (lower.as_str(), query.as_str())
                } else {
                    (line.text.as_str(), self.find.text.as_str())
                };
                let start = self.matches.len();
                self.matches.extend(hay.match_indices(needle).map(|(start, hit)| (i, start..start + hit.len())));
                if self.matches.len() > start {
                    self.match_lines.insert(i, start..self.matches.len());
                }
            }
        }
    }
}

impl App {
    pub fn project_is_expanded(&self) -> bool {
        self.project.expanded && self.inspector.is_open() && self.project.mode != Mode::Details && self.view != View::Settings
    }
    pub fn project_uses_full_window(&self) -> bool {
        self.project_is_expanded() && !self.sidebar.is_open()
    }
    pub fn project_terminal_pane_width(&self) -> f32 {
        let available = self.project.window_width - self.sidebar.shown_width();
        (available - self.project.width.unwrap_or(available * 0.5)).max(240.0)
    }
    /// Keeps the pane's resize range right for what it shows: Details stays narrow, Files and
    /// Changes leave the terminal room, and an expanded pane fills the workspace exactly.
    pub fn fit_project_pane(&mut self) -> bool {
        let available = (self.project.window_width - self.sidebar.shown_width()).max(1.0).round();
        if self.project_is_expanded() {
            if !(self.inspector.is_resizing() && self.inspector.width() != available) {
                return self.set_inspector_range(available.min(220.0), available, available);
            }
            // Dragging the edge of an expanded pane leaves it where it's dragged.
            self.project.expanded = false;
        }
        let (min, max) = if self.project.mode == Mode::Details {
            (220.0, 400.0)
        } else {
            (PANE_MIN.min(available), (available - TERMINAL_MIN).max(PANE_MIN))
        };
        self.set_inspector_range(min, max, self.inspector.width())
    }
    fn set_inspector_range(&mut self, min: f32, max: f32, width: f32) -> bool {
        let i = &self.inspector;
        if i.min_width() == min && i.max_width() == max && i.width() == width.round().clamp(min, max) {
            return false;
        }
        self.inspector = self.inspector.range(min, max);
        self.inspector.set_width(width);
        true
    }
    /// Fills the workspace with the pane, or puts it back.
    pub fn toggle_project_expanded(&mut self) {
        if self.project.expanded {
            self.restore_project_pane();
        } else {
            self.project.width = Some(self.inspector.width());
            self.project.expanded = true;
            self.fit_project_pane();
        }
    }
    /// Back from filling the workspace to the width it had before.
    fn restore_project_pane(&mut self) {
        self.project.expanded = false;
        self.fit_project_pane();
        let available = self.project.window_width - self.sidebar.shown_width();
        self.inspector.set_width(self.project.width.unwrap_or(available * 0.5));
    }
    fn project_seed(&self) -> Option<PathBuf> {
        match self.view {
            View::Workspace => self.workspace_folder(),
            View::Browse if self.mode == crate::Mode::Sessions => self
                .selected_session()
                .map(|s| PathBuf::from(&s.cwd))
                .filter(|p| !p.as_os_str().is_empty()),
            _ => None,
        }
    }
    pub fn toggle_project_panel(&mut self, mode: Mode) {
        if self.view == View::Settings || (mode == Mode::Details && !self.details_available()) {
            return;
        }
        if self.project.mode == mode {
            self.inspector.toggle();
        } else {
            if self.project.mode == Mode::Details {
                self.project.details_width = self.inspector.width();
            } else if !self.project.expanded {
                self.project.width = Some(self.inspector.width());
            }
            self.project.expanded = false;
            self.project.mode = mode;
            self.fit_project_pane();
            let width = if mode == Mode::Details {
                self.project.details_width
            } else {
                self.project
                    .width
                    .unwrap_or((self.project.window_width - if self.sidebar.is_open() { self.sidebar.width() } else { 0.0 }) * 0.5)
            };
            self.inspector.set_width(width);
            self.inspector.set_open(true);
            self.project.query = TextEdit::new("");
            self.project.search_paths = None;
            self.project.active = self.project.tabs.iter().rposition(|t| t.mode() == mode);
            self.project.request_preview();
            self.project.rebuild_files();
            self.project.rebuild_changes();
        }
        if mode.lists_files() && self.inspector.is_open() {
            self.project_sync();
        }
        if mode == Mode::Changes
            && self.inspector.is_open()
            && let Some(ctx) = self.project.ctx.clone()
        {
            self.project
                .status_job
                .request(ctx, |ctx| if ctx.git.is_some() { data::status(&ctx) } else { Err("Not a Git repository".into()) });
        }
    }
    fn project_sync(&mut self) {
        let seed = self.project_seed();
        if seed != self.project.seed {
            self.project.opening.cancel();
            self.project.dir_job.cancel();
            self.project.status_job.cancel();
            self.project.search.cancel();
            self.project.preview_job.cancel();
            self.project.seed = seed.clone();
            self.project.ctx = None;
            self.project.watcher = None;
            self.project.starting = false;
            self.project.watch_error = None;
            self.project.directories.clear();
            self.project.pending_dirs.clear();
            self.project.open_folders.clear();
            self.project.tabs.clear();
            self.project.active = None;
            self.project.clear_preview();
            self.project.files.clear();
            self.project.changes.clear();
            self.project.keyboard = false;
            self.project.breadcrumb = None;
            self.project.reveal_line = None;
            self.project.query = TextEdit::new("");
            self.project.search_paths = None;
            self.project.status = Load::Waiting;
        }
        if self.project.watcher.is_none() && !self.project.starting && self.project.watch_error.is_none() && let Some(seed) = seed {
            let wake = self.wake();
            self.project.starting = true;
            self.project.opening.request(seed, move |seed| {
                let ctx = data::discover(&seed)?;
                let watch = watch::Watch::start(&ctx, wake);
                let entries = data::directory(&ctx, Path::new(""));
                Ok(Opened { ctx, watch, entries })
            });
        }
    }
    pub fn project_setup(&self, wake: Waker) {
        self.project.opening.set_waker(wake.clone());
        self.project.dir_job.set_waker(wake.clone());
        self.project.status_job.set_waker(wake.clone());
        self.project.search.set_waker(wake.clone());
        self.project.preview_job.set_waker(wake);
    }
    pub fn project_poll(&mut self, tx: &mut Tx) -> bool {
        let visible = self.inspector.is_open() && self.project.mode.lists_files() && self.view != View::Settings;
        if !visible {
            self.project.watcher = None;
            self.project.opening.cancel();
            self.project.dir_job.cancel();
            self.project.status_job.cancel();
            self.project.preview_job.cancel();
            self.project.search.cancel();
            self.project.starting = false;
            self.project.pending_dirs.clear();
            return false;
        }
        let resized = self.fit_project_pane();
        self.project_sync();
        let mut changed = false;
        if let Some(result) = self.project.opening.take() {
            self.project.starting = false;
            match result {
                Ok(opened) => {
                    self.project.ctx = Some(opened.ctx);
                    match opened.watch {
                        Ok(w) => self.project.watcher = Some(w),
                        Err(e) => self.project.watch_error = Some(format!("Live updates unavailable: {e}")),
                    }
                    self.project.directories.insert(PathBuf::new(), opened.entries);
                    if self.project.mode == Mode::Changes {
                        let ctx = self.project.ctx.clone().unwrap();
                        self.project
                            .status_job
                            .request(ctx, |ctx| if ctx.git.is_some() { data::status(&ctx) } else { Err("Not a Git repository".into()) });
                    }
                    self.project.rebuild_files();
                    self.project.rebuild_changes();
                    // Reopening catches changes made while closed, including expanded folders.
                    let mut paths: Vec<_> = self.project.directories.keys().filter(|p| !p.as_os_str().is_empty()).cloned().collect();
                    for i in 0..self.project.files.visible_len() {
                        if let Some((n, r)) = self.project.files.row(i) && n.dir && r.expanded == Some(true) {
                            paths.push(n.path.clone());
                        }
                    }
                    self.project.request_dirs(paths);
                    self.project.refresh_preview();
                    // Run the filter again: its last results predate the reopen, or were cancelled by the close.
                    if !self.project.query.text.is_empty() {
                        self.project.filter();
                    }
                }
                Err(e) => {
                    self.project.note = Some(e.clone());
                    self.project.watch_error = Some(e);
                }
            }
            changed = true;
        }
        if let Some(watcher) = &self.project.watcher {
            let events = watcher.take();
            if events.rescan || !events.paths.is_empty() {
                let ctx = self.project.ctx.clone().unwrap();
                if self.project.mode == Mode::Changes {
                    self.project.status_job.request(ctx.clone(), |ctx| {
                        if ctx.git.is_some() { data::status(&ctx) } else { Err("Not a Git repository".into()) }
                    });
                } else {
                    self.project.status = Load::Waiting;
                }
                let broad = events.rescan
                    || events.paths.iter().any(|p| p.file_name().is_some_and(|n| n == ".gitignore" || n == "index" || n == "exclude"));
                let paths = self
                    .project
                    .directories
                    .keys()
                    .filter(|p| {
                        broad || events.paths.iter().any(|e| e == &ctx.root.join(p) || e.parent() == Some(ctx.root.join(p).as_path()))
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                self.project.request_dirs(paths);
                if let Some(tab) = self.project.tab()
                    && (broad
                        || events.paths.contains(&ctx.root.join(&tab.path))
                        || (tab.change.is_some() && events.paths.iter().any(|p| !p.starts_with(&ctx.root))))
                {
                    self.project.refresh_preview();
                }
                if !self.project.query.text.is_empty() {
                    self.project.filter();
                }
            }
        }
        if let Some(dirs) = self.project.dir_job.take() {
            self.project.remember_folders();
            for (path, result) in dirs {
                self.project.pending_dirs.remove(&path);
                self.project.directories.insert(path, result);
            }
            self.project.rebuild_files();
            changed = true;
            if let Some((path, anchor)) = self.project.breadcrumb.take() {
                self.project_breadcrumb(path, anchor, tx);
            }
        }
        if let Some(status) = self.project.status_job.take() {
            self.project.status = match status {
                Ok(s) => Load::Ready(s),
                Err(e) => Load::Failed(e),
            };
            self.project.rebuild_changes();
            changed = true;
        }
        if let Some(paths) = self.project.search.take() {
            self.project.search_paths = Some(paths);
            self.project.rebuild_files();
            changed = true;
        }
        if let Some((tab, result)) = self.project.preview_job.take() && self.project.tab().is_some_and(|t| t.id() == tab.id()) {
            // A refresh kept the old contents on screen until now.
            self.project.clear_preview();
            self.project.preview = match result {
                Ok(Preview::Code(code)) => {
                    self.project.code = Some(code);
                    Load::Ready(())
                }
                Ok(Preview::Diff(files)) => {
                    self.project.diff.set_files(files.into_iter().map(data::Patch::into_diff).collect());
                    self.project.diff.set_wrap(self.project.wrap);
                    Load::Ready(())
                }
                Ok(Preview::Note(note)) => {
                    self.project.note = Some(note);
                    Load::Ready(())
                }
                Err(e) => Load::Failed(e),
            };
            self.project.find_matches();
            changed = true;
            if let Some(line) = self.project.reveal_line.take() {
                tx.scroll_center_in("project-code-scroll", format!("code-line-{}", line.saturating_sub(1)));
            }
        }
        if changed {
            tx.instant();
        }
        changed || resized
    }
    fn project_pick(&mut self, id: &str, tx: &mut Tx) {
        self.project.tree_rev = self.project.tree_rev.wrapping_add(1);
        let Some(node) = self.project.tree().get(id).cloned() else { return };
        self.project.keyboard = true;
        if node.dir {
            if self.project.mode == Mode::Files {
                self.project.remember_folders();
                if self.project.tree().is_expanded(id)
                    && !self.project.directories.contains_key(&node.path)
                    && !self.project.pending_dirs.contains(&node.path)
                {
                    self.project.request_dirs([node.path]);
                }
            }
        } else if !node.placeholder {
            self.project.open(node.path, node.change);
            self.project.reveal_line = node.line;
            if let Some(line) = node.line {
                if let Some(query) = self.project.query.text.strip_prefix('?') {
                    self.project.find = TextEdit::new(query);
                    self.project.finding = true;
                    self.project.find_matches();
                }
                if self.project.code.is_some() {
                    tx.scroll_center_in("project-code-scroll", format!("code-line-{}", line.saturating_sub(1)));
                    self.project.reveal_line = None;
                }
            }
            if node.line.is_none() {
                tx.scroll_to_y("project-code-scroll", 0.0);
            }
            tx.scroll_to_y("project-diff/scroll", 0.0);
        }
    }
    fn project_tree_key(&mut self, ev: &KeyEvent, tx: &mut Tx) -> bool {
        let out = self.project.tree_mut().handle_key(ev, Node::label, tx);
        let id = self.project.tree().cursor().map(str::to_owned);
        if let Some(id) = id {
            self.project_pick(&id, tx);
        }
        !matches!(out, TreeKey::Ignored)
    }
    pub fn project_has_keyboard(&self, tx: &Tx) -> bool {
        self.inspector.is_open()
            && self.project.mode.lists_files()
            && self.view != View::Settings
            && (self.project.query.focused
                || self.project.find.focused
                || tx.focused().map_or(self.project.keyboard, |key| {
                    key.starts_with("project-") || key.starts_with("code-line-") || key.starts_with("code-text-")
                }))
    }
    pub fn project_key(&mut self, ev: &KeyEvent, tx: &mut Tx) -> bool {
        if ev.key == Key::Escape {
            if self.project.finding {
                self.project.finding = false;
                self.project.find.focused = false;
            } else if !self.project.query.text.is_empty() {
                self.project.query.text.clear();
                self.project.filter();
            } else {
                self.project.keyboard = false;
                tx.blur();
            }
            return true;
        }
        if ev.is_command('c') {
            if let Some(selection) = tx.text_selection() {
                let text = selection.text().to_owned();
                tx.copy(text);
                return true;
            }
            return self.project.diff.key(ev, tx);
        }
        if tx.focused().is_some_and(|k| k == "project-files" || k == "project-changes-tree")
            || (tx.focused().is_none() && self.project.keyboard)
        {
            return self.project_tree_key(ev, tx);
        }
        if self.project.mode == Mode::Changes {
            return self.project.diff.key(ev, tx);
        }
        false
    }
    pub fn project_find(&mut self) {
        self.project.finding = true;
        self.project.find.focused = true;
    }
    pub fn project_close(&mut self) {
        if let Some(i) = self.project.active {
            self.project.close_tab(i);
        } else {
            self.inspector.set_open(false);
        }
    }
    fn project_menu(&mut self, path: PathBuf, tab: Option<usize>, anchor: PopupAnchor, tx: &mut Tx) {
        let mut menu = PopupMenu::new()
            .item(PopupItem::new("editor", "Open in Editor").symbol("arrow.up.forward.app"))
            .item(PopupItem::new("finder", "Show in Finder").symbol("folder"))
            .separator()
            .submenu(
                "Copy",
                PopupMenu::new().items([
                    PopupItem::new("absolute", "Absolute Path"),
                    PopupItem::new("relative", "Relative Path"),
                    PopupItem::new("filename", "Filename"),
                ]),
            );
        if tab.is_some() {
            menu = menu.separator().item(PopupItem::new("reload", "Reload File"));
            menu = menu.separator().items([
                PopupItem::new("close", "Close File"),
                PopupItem::new("others", "Close Other Files"),
                PopupItem::new("all", "Close All Files"),
            ]);
        }
        tx.popup_menu(menu, anchor, move |s: &mut App, picked, tx| {
            let Some(picked) = picked else { return };
            let Some(ctx) = &s.project.ctx else { return };
            match picked {
                "absolute" => tx.copy(ctx.root.join(&path).to_string_lossy()),
                "relative" => tx.copy(path.to_string_lossy()),
                "filename" => tx.copy(path.file_name().unwrap_or_default().to_string_lossy()),
                "editor" | "finder" => {
                    s.project_external(&path, picked == "finder");
                }
                "reload" => s.project.request_preview(),
                action => {
                    if let Some(i) = tab {
                        s.project_tab_action(action, i, tx);
                    }
                }
            }
        });
    }
    /// The tab menu's Close, Close Others and Close All, among the tabs of the mode on screen.
    pub(crate) fn project_tab_action(&mut self, action: &str, i: usize, _tx: &mut Tx) {
        let Some(mode) = self.project.tabs.get(i).map(Tab::mode) else { return };
        match action {
            "close" => self.project.close_tab(i),
            "others" | "all" => {
                let keep = self.project.tabs[i].id();
                let shown = self.project.tab().map(Tab::id);
                self.project.tabs.retain(|t| t.mode() != mode || (action == "others" && t.id() == keep));
                self.project.active = self.project.tabs.iter().position(|t| action == "others" && t.id() == keep);
                if self.project.tab().map(Tab::id) != shown {
                    self.project.request_preview();
                }
            }
            _ => {}
        }
    }
    fn project_external(&mut self, path: &Path, finder: bool) {
        let Some(ctx) = &self.project.ctx else { return };
        let mut command = std::process::Command::new("/usr/bin/open");
        command.arg(if finder { "-R" } else { "-t" }).arg(ctx.root.join(path));
        if let Err(e) = command.spawn() {
            self.project.note = Some(e.to_string());
        }
    }
    pub fn project_reload(&mut self) {
        if self.project.mode.lists_files() && self.inspector.is_open() {
            self.project.watch_error = None;
            self.project.watcher = None;
            self.project.starting = false;
            self.project.opening.cancel();
        }
    }
    fn project_settings(&mut self, tx: &mut Tx) {
        let p = &self.project;
        let menu = PopupMenu::new()
            .items([
                PopupItem::new("find", "Find in File…").enabled(p.code.is_some()),
                PopupItem::new("wrap", "Word Wrap").checked(p.wrap),
                PopupItem::new("ignored", "Show Ignored Files").checked(p.show_ignored),
            ])
            .separator()
            .item(PopupItem::new("reload", "Reload"));
        tx.popup_menu(menu, "project-settings", |s: &mut App, picked, _| match picked {
            Some("find") => {
                s.project.finding = true;
                s.project.find.focused = true;
            }
            Some("wrap") => {
                s.project.wrap = !s.project.wrap;
                s.project.diff.set_wrap(s.project.wrap);
            }
            Some("ignored") => {
                s.project.show_ignored = !s.project.show_ignored;
                s.project.rebuild_files();
                if !s.project.query.text.is_empty() {
                    s.project.filter();
                }
            }
            Some("reload") => s.project_reload(),
            _ => {}
        });
    }
    fn project_breadcrumb(&mut self, dir: PathBuf, anchor: PopupAnchor, tx: &mut Tx) {
        let Some(result) = self.project.directories.get(&dir) else {
            self.project.breadcrumb = Some((dir.clone(), anchor));
            self.project.request_dirs([dir]);
            return;
        };
        let Ok(entries) = result else { return };
        let paths: Vec<_> = entries.iter().filter(|e| self.project.show_ignored || !e.ignored).cloned().collect();
        let menu = PopupMenu::new().items(
            paths.iter().enumerate().map(|(i, e)| PopupItem::new(i.to_string(), &e.name).symbol(if e.dir { "folder" } else { "doc" })),
        );
        tx.popup_menu(menu, anchor, move |s: &mut App, picked, tx| {
            let Some(entry) = picked.and_then(|i| i.parse::<usize>().ok()).and_then(|i| paths.get(i)) else { return };
            if entry.dir {
                s.project_breadcrumb(entry.path.clone(), (tx.focused().unwrap_or("project-path")).to_owned().into(), tx);
            } else {
                if s.project.mode != Mode::Files {
                    s.toggle_project_panel(Mode::Files);
                }
                s.project.open(entry.path.clone(), None);
            }
        });
    }
}


impl App {
    fn project_reveal_match(&self, tx: &mut Tx) {
        if let Some((line, _)) = self.project.matches.get(self.project.matched) {
            tx.scroll_center_in("project-code-scroll", format!("code-line-{line}"));
        }
    }
    fn project_next_match(&mut self, forward: bool, tx: &mut Tx) {
        let len = self.project.matches.len();
        if len == 0 {
            return;
        }
        self.project.matched = if forward { (self.project.matched + 1) % len } else { (self.project.matched + len - 1) % len };
        self.project_reveal_match(tx);
    }
}


/// Loads the selected project's trees before a headless scene's first frame.
pub fn load_scene(s: &mut App) {
    let Some(root) = s.project_seed() else { return };
    s.project.opening.cancel();
    s.project.status_job.cancel();
    s.project.starting = false;
    s.project.seed = Some(root.clone());
    s.project.ctx = None;
    s.project.watcher = None;
    s.project.watch_error = Some("headless".into());
    let ctx = match data::discover(&root) {
        Ok(ctx) => ctx,
        Err(e) => {
            s.project.directories.insert(PathBuf::new(), Err(e.clone()));
            s.project.status = Load::Failed(e.clone());
            s.project.note = Some(e);
            return;
        }
    };
    s.project.ctx = Some(ctx.clone());
    s.project.directories.insert(PathBuf::new(), data::directory(&ctx, Path::new("")));
    s.project.status = if ctx.git.is_some() {
        match data::status(&ctx) {
            Ok(status) => Load::Ready(status),
            Err(e) => Load::Failed(e),
        }
    } else {
        Load::Failed("Not a Git repository".into())
    };
    s.project.rebuild_files();
    s.project.rebuild_changes();
}

/// A reproducible headless view of real files, without a session index or background races.
pub fn scene(s: &mut App) {
    s.index_job.cancel();
    s.index = Load::Ready(Arc::new(Vec::new()));
    s.settings.remember_project = false;
    let root = crate::scene::arg("project").map(PathBuf::from).unwrap_or_else(|| std::env::current_dir().unwrap());
    s.start_now(root.file_name().unwrap_or_default().to_string_lossy().into_owned(), root, crate::workspace::Tool::Shell, false);
    s.project.mode = if rune::extra_args().iter().any(|a| a == "changes") { Mode::Changes } else { Mode::Files };
    s.fit_project_pane();
    s.inspector.set_width(crate::scene::arg("width").and_then(|w| w.parse().ok()).unwrap_or(600.0));
    s.inspector.set_open(true);
    load_scene(s);
    let Some(ctx) = s.project.ctx.clone() else { return };
    let file = crate::scene::arg("file").map(PathBuf::from);
    if let Some(path) = &file {
        for parent in path.ancestors().skip(1).filter(|p| !p.as_os_str().is_empty()) {
            s.project.directories.insert(parent.to_owned(), data::directory(&ctx, parent));
            s.project.open_folders.insert(data::key(parent));
        }
    }
    s.project.rebuild_files();
    s.project.rebuild_changes();
    let change = if s.project.mode == Mode::Changes {
        if let Load::Ready(status) = &s.project.status {
            status.changes.iter().find(|c| file.as_ref().is_none_or(|p| &c.path == p)).cloned()
        } else {
            None
        }
    } else {
        None
    };
    let path = change.as_ref().map(|c| c.path.clone()).or(file);
    if let Some(path) = path {
        let result = if let Some(change) = &change { data::diff(&ctx, change) } else { data::file(&ctx, &path) };
        let id = change.as_ref().map_or_else(|| data::key(&path), Change::key);
        s.project.tree_mut().select(&id);
        s.project.tabs.push(Tab { path, change });
        s.project.active = Some(0);
        s.project.preview = match result {
            Ok(Preview::Code(code)) => {
                s.project.code = Some(code);
                Load::Ready(())
            }
            Ok(Preview::Diff(files)) => {
                s.project.diff.set_files(files.into_iter().map(data::Patch::into_diff).collect());
                Load::Ready(())
            }
            Ok(Preview::Note(note)) => {
                s.project.note = Some(note);
                Load::Ready(())
            }
            Err(e) => Load::Failed(e),
        };
    }
}
