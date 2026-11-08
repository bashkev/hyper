//! Browse and search Claude Code, Codex and Pi session transcripts, and run them in a terminal.
//!
//! `cargo run --release` opens the window. Headless, on real data:
//! `hyper --scene real --snap out.png --size 1240x800 [session=<id>] [query=<text>] [settings]`.
//!
//! [`App`] is the whole state. Each part of the window keeps its own state in a field and adds
//! its behaviour to `App` from its own module:
//!
//! - [`index`]: every tool's sessions, kept current from what the watcher sees.
//! - [`reader`]: the open session's transcript and the details beside it.
//! - [`browse`]: Browse's sessions and config lists, with search and filters.
//! - [`workspace`]: the terminals, the launcher and worktrees.
//! - [`sidebar`] and [`window`]: what lays those out.

mod access;
mod agent;
mod attention;
mod browse;
mod browser;
mod claude;
mod codex;
mod commands;
mod config;
mod data;
mod fonts;
mod format;
mod history;
mod index;
mod live;
mod logos;
mod notifications;
mod pi;
mod preferences;
mod project;
mod prompt;
mod quit;
mod reader;
mod reload;
mod scene;
mod settings;
mod sidebar;
mod term;
mod ui;
mod usage;
mod window;
mod workspace;
mod worktree;
#[cfg(test)]
mod tests;

use std::collections::HashSet;
use std::path::PathBuf;

use browse::{ConfigList, Mode, Search};
use data::{Roots, Session, Source};
use index::Stale;
use live::Running;
use reader::Reader;
use rune::{AppSpec, Chrome, KeyEvent, Latest, Tx, Waker};
use rune_kit::shell::PaneState;
use sidebar::Item;
use usage::Prices;

/// Something read in the background.
#[derive(Clone, PartialEq)]
enum Load<T> {
    Waiting,
    Ready(T),
    Failed(String),
}

impl<T> Load<T> {
    fn ready(&self) -> Option<&T> {
        match self {
            Load::Ready(value) => Some(value),
            _ => None,
        }
    }
}

impl<T> From<Result<T, String>> for Load<T> {
    fn from(result: Result<T, String>) -> Load<T> {
        match result {
            Ok(value) => Load::Ready(value),
            Err(e) => Load::Failed(e),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum View {
    Workspace,
    Browse,
    Settings,
}

struct App {
    view: View,
    ws: workspace::Workspace,
    waker: Option<Waker>,
    /// Whether the system is dark, as the view last saw.
    dark: std::cell::Cell<bool>,
    /// To bring the window back when a close is cancelled.
    window: Option<rune::AppHandle<App>>,
    sidebar: PaneState,
    inspector: PaneState,
    project: project::Project,
    roots: Roots,
    home: PathBuf,

    settings: settings::Settings,
    preferences: preferences::Preferences,
    settings_path: PathBuf,
    /// Why the last change to a setting couldn't be saved.
    settings_error: Option<String>,
    /// When settings changed only in memory, as a held zoom key repeats, are due on disk.
    settings_due: Option<std::time::Instant>,

    /// Why the last reload didn't happen, until it's been read.
    reload_error: Option<String>,
    /// The output of the build that failed, when that's why a reload didn't happen.
    reload_log: Option<String>,
    build_job: Latest<Result<(), reload::BuildError>>,

    index: Load<std::sync::Arc<Vec<Session>>>,
    index_job: Latest<Result<Vec<Session>, String>>,
    prices: Prices,
    prices_job: Latest<Prices>,
    watcher: Option<live::Watcher>,
    /// `None` until the watcher has looked, and in a headless render.
    running: Option<Running>,
    stale: Stale,
    /// When the next session going quiet changes what's shown.
    quiet_at: Option<i64>,

    /// What the sidebar lists.
    listed: Vec<Item>,
    /// Projects showing their sessions. A search opens them all without changing this.
    open_projects: HashSet<String>,
    /// Projects the user closed, which stay closed.
    closed_projects: HashSet<String>,

    mode: Mode,
    /// `None` shows every tool's sessions.
    source: Option<Source>,
    search: Search,
    config: ConfigList,
    reader: Reader,
    browser: browser::Browser,
    history: history::History,
}

fn init() -> App {
    let mut app = App::at(Roots::home(), std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default());
    app.view = View::Workspace;
    app
}

impl App {
    fn at(roots: Roots, home: PathBuf) -> App {
        let settings_path = settings::Settings::path(&home);
        let settings = settings::Settings::load(&settings_path);
        let preferences = preferences::Preferences::new(&settings);
        let mut app = App {
            view: View::Browse,
            ws: workspace::Workspace::default(),
            waker: None,
            dark: std::cell::Cell::new(true),
            window: None,
            sidebar: PaneState::new(300.0).range(200.0, 520.0),
            inspector: PaneState::new(260.0).range(220.0, 400.0).collapsed(),
            project: project::Project::default(),
            roots,
            home,
            settings,
            preferences,
            settings_path,
            settings_error: None,
            settings_due: None,
            reload_error: None,
            reload_log: None,
            build_job: Latest::default(),
            index: Load::Waiting,
            index_job: Latest::default(),
            prices: Prices::default(),
            prices_job: Latest::default(),
            watcher: None,
            running: None,
            stale: Stale::default(),
            quiet_at: None,
            listed: Vec::new(),
            open_projects: HashSet::new(),
            closed_projects: HashSet::new(),
            mode: Mode::Sessions,
            source: None,
            search: Search::default(),
            config: ConfigList::default(),
            reader: Reader::default(),
            browser: browser::Browser::default(),
            history: history::History::default(),
        };
        app.configure_workspace();
        app.request_index();
        app
    }

    /// Wakes the app from another thread. Before setup, and headless, it does nothing.
    fn wake(&self) -> Waker {
        self.waker.clone().unwrap_or_else(|| std::sync::Arc::new(|| {}))
    }
}

/// Adds `key` to `set` if it's missing, and takes it out if it's there.
fn toggle_in(set: &mut HashSet<String>, key: &str) {
    if !set.remove(key) {
        set.insert(key.to_owned());
    }
}

fn setup(s: &mut App, waker: Waker) {
    s.project_setup(waker.clone());
    s.index_job.set_waker(waker.clone());
    s.search.job.set_waker(waker.clone());
    s.config.job.set_waker(waker.clone());
    s.prices_job.set_waker(waker.clone());
    s.reader.set_waker(waker.clone());
    s.stale.job.set_waker(waker.clone());
    s.build_job.set_waker(waker.clone());
    s.set_making_waker(waker.clone());
    s.watch(waker.clone());
    quit::intercept(waker.clone());
    attention::listen(waker.clone());
    reload::listen(waker.clone());
    s.waker = Some(waker);
    s.adopt_handed();
}

/// Takes in whatever the workers, the watcher, the terminals and the system have for the app.
/// Says whether anything shown changed.
fn poll(s: &mut App, _: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    s.flush_settings(tx);
    changed |= s.poll_terminals(tx);
    changed |= s.project_poll(tx);
    changed |= s.take_tidy();
    if s.take_worktree() {
        tx.instant();
        changed = true;
    }
    if quit::requested() {
        request_quit(s, tx);
        changed = true;
    }
    if reload::requested() {
        s.reload(tx);
        changed = true;
    }
    if let Some(result) = s.build_job.take() {
        s.built(result, tx);
        changed = true;
    }
    changed |= s.take_index();
    changed |= s.take_config();
    changed |= s.take_prices();
    changed |= s.take_hits(tx);
    changed |= s.take_transcript();
    changed |= s.take_changes();
    changed |= s.take_browser();
    changed |= s.take_refreshed();
    changed |= s.take_tail();
    changed |= s.settle(tx);
    changed |= s.attend();
    changed |= s.reveal(tx);
    changed |= s.take_hold(tx);
    changed
}

fn on_key(s: &mut App, ev: &KeyEvent, tx: &mut Tx) -> bool {
    // The question comes first, and Escape answers it.
    if s.ws.confirming_quit {
        return false;
    }
    // Commands come first, so ⌘F and ⌘W follow Settings; Search and Close act on the file pane when it has the keyboard.
    if s.command_key(ev, tx) { return true; }
    if s.project_has_keyboard(tx) { return s.project_key(ev, tx); }
    // The address and find bars answer their own keys; the rest mustn't reach the terminal.
    if s.browser_has_keyboard() { return false; }
    // Here after a focused control has passed on a key, or first with nothing focused.
    if s.terminal_key(ev, tx) {
        return true;
    }
    if s.view == View::Workspace && (s.ws.main == workspace::Main::Launcher || s.ws.palette) && tx.focused().is_none() && s.palette_key(ev, tx) {
        return true;
    }
    false
}

/// Quits, asking first if that would stop an agent.
fn request_quit(s: &mut App, tx: &mut Tx) {
    s.settings_due = Some(std::time::Instant::now());
    s.flush_settings(tx);
    if s.running_agents() == 0 {
        return tx.quit();
    }
    s.ws.confirming_quit = true;
    // A close has hidden the window by now, and the question has to be seen.
    if let Some(window) = &s.window {
        window.open();
    }
}

fn main() {
    reload::start();
    let chrome = Chrome { full_size_content: true, traffic_lights: Some((16.0, 25.0)), drag_regions: true };
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let initial_settings = settings::Settings::load(&settings::Settings::path(&home));
    let app = rune::App::new(AppSpec {
        scenes: &[("real", scene::real), ("project", project::scene)],
        fonts: fonts::for_rune(),
        families: ("Inter", "ui-monospace"),
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        min_size: Some((720.0, 480.0)),
        ..AppSpec::new("Hyper", (1240.0, 800.0), init, window::view)
    });
    let window = app.handle();
    let maximized = initial_settings.open_maximized;
    let mut app = app;
    for item in commands::menu(&initial_settings) { app = app.menu_item(item); }
    app.handle().update(|s, tx| tx.set_font_families(&s.settings.appearance.interface_font, &s.settings.appearance.monospace_font));
    app.init(move || {
        let mut app = init();
        app.window = Some(window);
        app
    })
    .chrome(chrome)
    .on_window(move |handle| {
        // After a reload the window goes back where it was.
        browser::on_window(handle);
        if !reload::on_window(handle) && maximized {
            settings::maximize(handle);
        }
        access::ask(&home);
    })
    // A close hides the window, so one that's cancelled can come back.
    .keep_running()
    .on_close(request_quit)
    .on_focus(|s: &mut App, focused, _| s.window_focus(focused))
    .on_ime(|s: &mut App, ime, tx| match (if s.project_has_keyboard(tx) || s.browser_has_keyboard() { None } else { s.typing_term() }, ime) {
        (Some(t), rune::Ime::Commit(text)) => {
            t.text(text);
            true
        }
        _ => false,
    })
    .run();
}
