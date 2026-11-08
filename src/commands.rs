//! One catalog for keyboard routing, native menus, and the Settings shortcut editor.

use rune::{Key, KeyEvent, MenuItem, Shortcut, Tx};
use crate::{App, View, settings::{self, Appearance, Settings}, term::Paging, workspace};

macro_rules! catalog {
    ($( $variant:ident, $id:literal, $title:literal, $menu:literal, $shortcut:literal; )*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
        pub enum Action { $( $variant, )* }
        impl Action {
            pub const ALL: &'static [Action] = &[$( Action::$variant, )*];
            pub fn id(self) -> &'static str { match self { $( Self::$variant => $id, )* } }
            pub fn title(self) -> &'static str { match self { $( Self::$variant => $title, )* } }
            /// The top-level menu it's in, which also groups it in Settings.
            pub fn menu(self) -> &'static str { match self { $( Self::$variant => $menu, )* } }
            pub fn default_shortcut(self) -> &'static str { match self { $( Self::$variant => $shortcut, )* } }
            fn menu_item(self) -> MenuItem<App> {
                match self { $( Self::$variant => MenuItem::new($title, |s: &mut App, tx| s.dispatch(Action::$variant, tx)).id($id).menu($menu), )* }
            }
        }
    }
}

catalog! {
    NewSession, "new_session", "New Session", "File", "cmd+n";
    CommandPalette, "command_palette", "Command Palette…", "File", "cmd+shift+n";
    ChooseFolder, "choose_folder", "Choose Folder…", "File", "cmd+o";
    CloseSession, "close_session", "Close Session", "File", "cmd+w";
    Settings, "settings", "Settings…", "Hyper", "cmd+,";
    // Runs the build on disk, which takes over the terminals: their programs keep running.
    ReloadApp, "reload_app", "Build and Reload Hyper", "Hyper", "ctrl+cmd+r";
    Search, "search", "Search", "Edit", "cmd+f";
    ToggleSidebar, "toggle_sidebar", "Toggle Sidebar", "View", "cmd+b";
    ToggleDetails, "toggle_details", "Toggle Details", "View", "cmd+alt+i";
    ToggleFiles, "toggle_files", "Toggle Files", "View", "cmd+shift+e";
    ToggleChanges, "toggle_changes", "Toggle Changes", "View", "cmd+shift+g";
    ToggleBrowser, "toggle_browser", "Toggle Browser", "View", "cmd+shift+l";
    OpenLocation, "open_location", "Open Location…", "File", "cmd+l";
    Workspace, "workspace", "Workspace", "View", "ctrl+cmd+1";
    Browse, "browse", "Browse", "View", "ctrl+cmd+2";
    Reload, "reload", "Reload", "View", "cmd+r";
    ToggleTools, "toggle_tools", "Toggle Tool Calls", "View", "cmd+shift+t";
    // In the web page while it has the keyboard, as in Safari.
    GoBack, "go_back", "Back", "Go", "cmd+[";
    GoForward, "go_forward", "Forward", "Go", "cmd+]";
    // These size the text being read: the session in Workspace, the transcript in Browse.
    IncreaseFont, "increase_font", "Zoom In", "View", "cmd+=";
    DecreaseFont, "decrease_font", "Zoom Out", "View", "cmd+-";
    ResetFont, "reset_font", "Actual Size", "View", "cmd+0";
    IncreaseInterface, "increase_interface", "Bigger Interface Text", "View", "cmd+alt+=";
    DecreaseInterface, "decrease_interface", "Smaller Interface Text", "View", "cmd+alt+-";
    NextSession, "next_session", "Next Session", "Session", "cmd+shift+]";
    PreviousSession, "previous_session", "Previous Session", "Session", "cmd+shift+[";
    NextAttention, "next_attention", "Next Session Needing Attention", "Session", "cmd+shift+a";
    // Numbered as the sidebar lists them; ⌘9 is the last, as in browsers.
    Session1, "session_1", "Session 1", "Session", "cmd+1";
    Session2, "session_2", "Session 2", "Session", "cmd+2";
    Session3, "session_3", "Session 3", "Session", "cmd+3";
    Session4, "session_4", "Session 4", "Session", "cmd+4";
    Session5, "session_5", "Session 5", "Session", "cmd+5";
    Session6, "session_6", "Session 6", "Session", "cmd+6";
    Session7, "session_7", "Session 7", "Session", "cmd+7";
    Session8, "session_8", "Session 8", "Session", "cmd+8";
    LastSession, "last_session", "Last Session", "Session", "cmd+9";
    // Sent to the program, which scrolls its own view: Page Up, Page Down, ⌃Home, ⌃End.
    PageUp, "page_up", "Page Up", "Session", "cmd+u";
    PageDown, "page_down", "Page Down", "Session", "cmd+j";
    ScrollToTop, "scroll_to_top", "Scroll to Top", "Session", "cmd+shift+u";
    ScrollToBottom, "scroll_to_bottom", "Scroll to Bottom", "Session", "cmd+shift+j";
}

/// Older chords that still work while their action keeps its default and nothing else claims
/// them: ⌘K searched before ⌘F did, and Rune's sidebar answers ⇧⌘B and ⌃⌘S elsewhere.
const ALIASES: &[(Action, &str)] = &[
    (Action::Search, "cmd+k"),
    (Action::ToggleSidebar, "cmd+shift+b"),
    (Action::ToggleSidebar, "ctrl+cmd+s"),
];

/// These belong to macOS and to text editing in every field and terminal.
/// The launcher's own chords: ⌘Return and ⇧⌘Return start in a new worktree.
const LAUNCHER: &[&str] = &["cmd+enter", "cmd+shift+enter"];
const RESERVED: &[&str] = &["cmd+q", "cmd+h", "cmd+alt+h", "cmd+m", "cmd+c", "cmd+v", "cmd+x", "cmd+a", "cmd+z", "cmd+shift+z"];

impl Action {
    /// A web page in the Browser pane keeps these keys while it has the keyboard: ⌘B bolds and
    /// ⌘U underlines in a web editor, and paging only ever moves the terminal.
    fn yields_to_page(self) -> bool {
        matches!(self, Action::ToggleSidebar | Action::PageUp | Action::PageDown | Action::ScrollToTop | Action::ScrollToBottom)
    }

    pub fn from_id(id: &str) -> Option<Action> {
        Action::ALL.iter().copied().find(|a| a.id() == id)
    }

    /// What runs it. A saved value is checked when it's recorded, so a hand-edited one that
    /// wouldn't pass is ignored here rather than taking keys from the terminals. Defaults
    /// yield to saved bindings, including when a later version introduces a new command.
    pub fn binding(self, settings: &Settings) -> Option<Shortcut> {
        let saved = settings.shortcuts.get(self.id());
        let value = saved.map(String::as_str).unwrap_or(self.default_shortcut());
        let shortcut: Shortcut = value.parse().ok()?;
        if !shortcut.to_string().contains("super+") {
            return None;
        }
        if saved.is_none() && settings.shortcuts.iter().any(|(id, value)| {
            Action::from_id(id).is_some() && value.parse::<Shortcut>().ok().as_ref() == Some(&shortcut)
        }) {
            return None;
        }
        Some(shortcut)
    }

    /// Which running session a numbered action shows, counted from 0 in sidebar order.
    fn session_index(self, count: usize) -> Option<usize> {
        let at = match self {
            Action::Session1 => 0, Action::Session2 => 1, Action::Session3 => 2, Action::Session4 => 3,
            Action::Session5 => 4, Action::Session6 => 5, Action::Session7 => 6, Action::Session8 => 7,
            Action::LastSession => return count.checked_sub(1),
            _ => return None,
        };
        (at < count).then_some(at)
    }

    /// The menus actions sit in, in the order they first appear.
    pub fn menus() -> Vec<&'static str> {
        let mut menus = Vec::new();
        for action in Action::ALL {
            if !menus.contains(&action.menu()) {
                menus.push(action.menu());
            }
        }
        menus
    }

    pub fn hint(self, settings: &Settings) -> String {
        self.binding(settings).map_or(String::new(), |key| key.menu_keycaps().join(""))
    }
}

/// Whether `value` is what `id` has anyway, so saving it would only pin today's default.
pub fn is_default(id: &str, value: &str) -> bool {
    let Some(action) = Action::from_id(id) else { return false };
    parse(value).ok() == Some(parse(action.default_shortcut()).ok().flatten())
}

pub fn menu(settings: &Settings) -> Vec<MenuItem<App>> {
    Action::ALL.iter().map(|action| {
        let item = action.menu_item().keys_first();
        match action.binding(settings) {
            Some(shortcut) => item.key_equivalent(shortcut),
            None => item,
        }
    }).collect()
}

/// The shortcuts a web page keeps from the menu while it has the keyboard, as they're bound now.
pub fn page_keys(settings: &Settings) -> Vec<Shortcut> {
    Action::ALL.iter().filter(|a| a.yields_to_page()).filter_map(|a| a.binding(settings)).collect()
}

pub fn parse(value: &str) -> Result<Option<Shortcut>, String> {
    if value.trim().is_empty() { return Ok(None); }
    let shortcut: Shortcut = value.parse().map_err(|e| format!("{e}"))?;
    // ⌃ and ⌥ keys belong to the programs in the terminals: Ctrl-C, Ctrl-R, Option as Meta.
    if !shortcut.to_string().contains("super+") {
        return Err(format!("Include ⌘: terminals need {} themselves.", keys(&shortcut)));
    }
    if RESERVED.iter().any(|r| shortcut == r.parse::<Shortcut>().unwrap()) {
        return Err(format!("{} belongs to macOS or text editing.", keys(&shortcut)));
    }
    if LAUNCHER.iter().any(|r| shortcut == r.parse::<Shortcut>().unwrap()) {
        return Err(format!("{} starts in a new worktree from the launcher.", keys(&shortcut)));
    }
    Ok(Some(shortcut))
}

fn keys(shortcut: &Shortcut) -> String {
    shortcut.menu_keycaps().join("")
}

/// Whether `action` may take `shortcut`: refused, with why, when it's reserved or another
/// action has it.
pub fn check(settings: &Settings, action: Action, shortcut: &Shortcut) -> Result<(), String> {
    parse(&shortcut.to_string())?;
    match Action::ALL.iter().find(|&&other| other != action && other.binding(settings).as_ref() == Some(shortcut)) {
        Some(other) => Err(format!("{} is used by {}.", keys(shortcut), other.title())),
        None => Ok(()),
    }
}

/// Binds `action` to `shortcut` (None turns it off), saving only what differs from the default.
pub fn bind(settings: &mut Settings, action: Action, shortcut: Option<&Shortcut>) {
    let value = shortcut.map_or(String::new(), Shortcut::to_string);
    if is_default(action.id(), &value) {
        settings.shortcuts.remove(action.id());
    } else {
        settings.shortcuts.insert(action.id().into(), value);
    }
}

/// Use Rune's accelerator parser for normalization. Character bindings follow the keyboard
/// layout like the native menu, including Shift/Option characters through `key_base`.
pub fn event_shortcut(ev: &KeyEvent, tx: &Tx) -> Option<Shortcut> {
    let named = match &ev.key {
        Key::Command(c) => tx.key_base().unwrap_or(*c).to_ascii_lowercase().to_string(),
        Key::Text(s) => tx.key_base().map(|c| c.to_string()).unwrap_or_else(|| s.to_lowercase()),
        Key::Enter => "enter".into(), Key::Escape => "escape".into(), Key::Tab => "tab".into(),
        Key::Left => "left".into(), Key::Right => "right".into(), Key::Up => "up".into(), Key::Down => "down".into(),
        Key::PageUp => "pageup".into(), Key::PageDown => "pagedown".into(), Key::Home => "home".into(), Key::End => "end".into(),
        Key::Backspace => "backspace".into(), Key::Delete => "delete".into(), Key::Function(n) => format!("f{n}"),
        _ => return None,
    };
    let named = match named.as_str() { "+" => "=", "{" => "[", "}" => "]", " " => "space", _ => named.as_str() };
    let key = named.parse::<Shortcut>().ok()?.physical_key();
    Shortcut::from_physical_key(key, ev.mods).ok()
}

/// The action `key` runs: its binding, else ⌘+ standing for ⌘= as in other macOS apps, else an
/// alias nothing has taken.
pub fn action_for(settings: &Settings, key: &Shortcut) -> Option<Action> {
    let bound = |key: &Shortcut| Action::ALL.iter().copied().find(|a| a.binding(settings).as_ref() == Some(key));
    if let Some(action) = bound(key) {
        return Some(action);
    }
    let equal = "=".parse::<Shortcut>().ok()?.physical_key();
    if key.physical_key() == equal && key.to_string().contains("shift+") {
        let unshifted: Shortcut = key.to_string().replace("shift+", "").parse().ok()?;
        if let Some(action) = bound(&unshifted) {
            return Some(action);
        }
    }
    ALIASES.iter().find(|(action, alias)| {
        alias.parse::<Shortcut>().ok().as_ref() == Some(key) && !settings.shortcuts.contains_key(action.id())
    }).map(|(action, _)| *action)
}

impl App {
    /// Whether a shortcut is being recorded in Settings, which takes every key.
    fn keys_are_taken(&self) -> bool {
        self.ws.confirming_quit || !self.ws.asking.is_empty() || self.preferences.recording()
    }

    pub fn command_key(&mut self, ev: &KeyEvent, tx: &mut Tx) -> bool {
        if self.keys_are_taken() { return false; }
        let Some(key) = event_shortcut(ev, tx) else { return false };
        let Some(action) = action_for(&self.settings, &key) else { return false };
        let repeats = matches!(action, Action::IncreaseFont | Action::DecreaseFont | Action::IncreaseInterface | Action::DecreaseInterface | Action::PageUp | Action::PageDown);
        if !tx.key_repeat() || repeats {
            self.dispatch(action, tx);
        }
        true
    }

    pub fn dispatch(&mut self, action: Action, tx: &mut Tx) {
        if self.keys_are_taken() { return; }
        match action {
            Action::NewSession => self.new_session_here(tx),
            Action::CommandPalette => self.new_session(None, Some(tx)),
            Action::ChooseFolder => {
                // From the launcher on screen, the tool already picked there is the one to start.
                let launcher = self.view == View::Workspace && (self.ws.palette || self.ws.main == crate::workspace::Main::Launcher);
                if launcher {
                    self.ws.worktree = self.settings.default_worktree;
                } else {
                    self.new_session(None, Some(tx));
                }
                self.pick_folder(tx);
            }
            Action::Settings => { self.set_view(View::Settings); tx.blur(); }
            Action::ToggleSidebar => self.sidebar.toggle(),
            Action::ToggleDetails => self.toggle_project_panel(crate::project::Mode::Details),
            Action::ToggleFiles => self.toggle_project_panel(crate::project::Mode::Files),
            Action::ToggleChanges => self.toggle_project_panel(crate::project::Mode::Changes),
            Action::ToggleBrowser => self.toggle_browser(),
            Action::OpenLocation => self.open_location(),
            Action::Workspace => { self.set_view(View::Workspace); tx.blur(); }
            Action::Browse => { self.set_view(View::Browse); tx.blur(); }
            Action::Search if self.project_has_keyboard(tx) => self.project_find(),
            Action::Search if self.browser_has_keyboard() => self.browser_find(),
            Action::Search => {
                self.set_view(View::Browse);
                if !self.sidebar.is_open() { self.sidebar.toggle(); }
                self.search.field.focused = true;
            }
            Action::Reload if self.browser_has_keyboard() => self.browser_reload(),
            Action::Reload => { self.request_index(); self.project_reload(); }
            Action::ReloadApp => self.build_and_reload(tx),
            // ⌘W closes the launcher, not the terminal behind it.
            Action::CloseSession if self.ws.palette => self.close_palette(),
            Action::CloseSession if self.project_has_keyboard(tx) => self.project_close(),
            Action::CloseSession if self.browser_has_keyboard() => self.browser_close(),
            Action::CloseSession => match self.current().map(|l| l.id) {
                Some(id) if self.view == View::Workspace => self.close_live(id),
                // Nothing to close here, so ⌘W closes the window as it would without Hyper's menu.
                _ => crate::request_quit(self, tx),
            },
            Action::NextSession | Action::PreviousSession | Action::NextAttention => {
                let ids: Vec<_> = self.live_in_order().into_iter().filter(|l| action != Action::NextAttention || l.needs_you).map(|l| l.id).collect();
                if !ids.is_empty() {
                    let current = self.current().map(|l| l.id);
                    let at = ids.iter().position(|id| Some(*id) == current);
                    let next = match at { Some(at) if action == Action::PreviousSession => (at + ids.len() - 1) % ids.len(), Some(at) => (at + 1) % ids.len(), None => 0 };
                    self.set_view(View::Workspace);
                    self.show(workspace::Main::Terminal(ids[next]));
                    tx.blur();
                }
            }
            Action::Session1 | Action::Session2 | Action::Session3 | Action::Session4 | Action::Session5
            | Action::Session6 | Action::Session7 | Action::Session8 | Action::LastSession => {
                let ids: Vec<_> = self.live_in_order().into_iter().map(|l| l.id).collect();
                if let Some(at) = action.session_index(ids.len()) {
                    self.set_view(View::Workspace);
                    self.show(workspace::Main::Terminal(ids[at]));
                    tx.blur();
                }
            }
            Action::IncreaseFont | Action::DecreaseFont | Action::ResetFont | Action::IncreaseInterface | Action::DecreaseInterface => {
                let mut changed = self.settings.clone();
                let defaults = Appearance::default();
                let a = &mut changed.appearance;
                let (size, range, default) = match action {
                    Action::IncreaseInterface | Action::DecreaseInterface => (&mut a.interface_size, &settings::INTERFACE_SIZE, defaults.interface_size),
                    _ => (&mut a.content_size, &settings::CONTENT_SIZE, defaults.content_size),
                };
                *size = match action {
                    Action::ResetFont => default,
                    Action::IncreaseFont | Action::IncreaseInterface => range.clamp(*size + range.step),
                    _ => range.clamp(*size - range.step),
                };
                if changed == self.settings {
                } else if tx.key_repeat() {
                    self.save_settings_soon(changed, tx);
                } else {
                    self.save_settings(changed, tx);
                }
            }
            Action::PageUp | Action::PageDown | Action::ScrollToTop | Action::ScrollToBottom => {
                let paging = match action {
                    Action::PageUp => Paging::PageUp,
                    Action::PageDown => Paging::PageDown,
                    Action::ScrollToTop => Paging::Top,
                    _ => Paging::Bottom,
                };
                if !self.project_has_keyboard(tx) && !self.browser_has_keyboard() && let Some(term) = self.typing_term() {
                    term.page(paging, tx.key_repeat());
                }
            }
            Action::GoBack if self.browser_has_keyboard() => self.browser_back(),
            Action::GoForward if self.browser_has_keyboard() => self.browser_forward(),
            Action::GoBack => self.go(crate::history::Way::Back, 0, tx),
            Action::GoForward => self.go(crate::history::Way::Forward, 0, tx),
            Action::ToggleTools => { if self.view == View::Browse { self.reader.toggle_tools(); } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_taken_or_reserved_shortcut_is_refused_with_who_has_it() {
        let settings = Settings::default();
        let cmd_n: Shortcut = "cmd+n".parse().unwrap();
        assert_eq!(check(&settings, Action::ToggleSidebar, &cmd_n).unwrap_err(), "⌘N is used by New Session.");
        assert!(check(&settings, Action::NewSession, &cmd_n).is_ok(), "its own shortcut isn't a conflict");
        assert!(check(&settings, Action::ToggleSidebar, &"cmd+c".parse().unwrap()).unwrap_err().contains("macOS"));
        for terminal_key in ["ctrl+c", "ctrl+r", "alt+b", "f5"] {
            assert!(check(&settings, Action::ToggleSidebar, &terminal_key.parse().unwrap()).unwrap_err().contains("Include ⌘"), "{terminal_key}");
        }
        for bad in ["b", "shift+b", "cmd++n", "cmd+unknown"] { assert!(parse(bad).is_err(), "{bad}"); }
    }

    #[test]
    fn defaults_yield_to_saved_bindings_until_the_shortcut_is_free_again() {
        let mut settings = Settings::default();
        let key: Shortcut = "cmd+shift+n".parse().unwrap();
        bind(&mut settings, Action::ToggleSidebar, Some(&key));
        assert_eq!(action_for(&settings, &key), Some(Action::ToggleSidebar));
        assert_eq!(Action::CommandPalette.binding(&settings), None);
        assert!(Action::CommandPalette.hint(&settings).is_empty());
        assert!(check(&settings, Action::ToggleSidebar, &key).is_ok());
        assert_eq!(check(&settings, Action::CommandPalette, &key).unwrap_err(), "⇧⌘N is used by Toggle Sidebar.");

        bind(&mut settings, Action::ToggleSidebar, Some(&"cmd+b".parse().unwrap()));
        assert_eq!(action_for(&settings, &key), Some(Action::CommandPalette), "the default returns when the saved binding moves away");
        assert_eq!(Action::CommandPalette.binding(&settings), Some(key.clone()));

        bind(&mut settings, Action::CommandPalette, None);
        bind(&mut settings, Action::ToggleSidebar, Some(&key));
        bind(&mut settings, Action::ToggleSidebar, None);
        assert_eq!(action_for(&settings, &key), None, "an explicitly disabled shortcut stays disabled");

        settings.shortcuts.clear();
        settings.shortcuts.insert("unknown_command".into(), key.to_string());
        assert_eq!(action_for(&settings, &key), Some(Action::CommandPalette), "unknown commands cannot claim a shortcut");
    }

    #[test]
    fn only_shortcuts_that_differ_from_the_default_are_kept() {
        let mut settings = Settings::default();
        bind(&mut settings, Action::ToggleSidebar, Some(&"cmd+alt+b".parse().unwrap()));
        assert_eq!(settings.shortcuts.get("toggle_sidebar").map(String::as_str), Some("alt+super+b"));
        bind(&mut settings, Action::ToggleSidebar, Some(&"cmd+b".parse().unwrap()));
        assert!(settings.shortcuts.is_empty(), "back to the default, nothing is pinned");
        bind(&mut settings, Action::Search, None);
        assert_eq!(Action::Search.binding(&settings), None);
        assert_eq!(settings.shortcuts.get("search").map(String::as_str), Some(""));
    }

    #[test]
    fn a_web_page_keeps_the_sidebar_and_paging_keys_wherever_they_re_bound() {
        let mut settings = Settings::default();
        let key = |s: &str| s.parse::<Shortcut>().unwrap();
        assert!(page_keys(&settings).contains(&key("cmd+b")), "⌘B bolds in a web editor");
        assert!(page_keys(&settings).contains(&key("cmd+u")));
        assert!(!page_keys(&settings).contains(&key("cmd+l")), "⌘L still goes to the address");
        bind(&mut settings, Action::ToggleSidebar, Some(&key("cmd+alt+b")));
        assert!(page_keys(&settings).contains(&key("cmd+alt+b")) && !page_keys(&settings).contains(&key("cmd+b")));
    }

    #[test]
    fn aliases_and_command_plus_reach_their_actions_until_something_takes_them() {
        let mut settings = Settings::default();
        let key = |s: &str| s.parse::<Shortcut>().unwrap();
        assert_eq!(action_for(&settings, &key("cmd+k")), Some(Action::Search));
        assert_eq!(action_for(&settings, &key("cmd+shift+b")), Some(Action::ToggleSidebar));
        assert_eq!(action_for(&settings, &key("cmd+shift+=")), Some(Action::IncreaseFont));
        assert_eq!(action_for(&settings, &key("cmd+alt+shift+=")), Some(Action::IncreaseInterface));
        bind(&mut settings, Action::Reload, Some(&key("cmd+k")));
        bind(&mut settings, Action::Reload, Some(&key("cmd+k")));
        assert_eq!(action_for(&settings, &key("cmd+k")), Some(Action::Reload), "a binding beats an alias");
        bind(&mut settings, Action::ToggleSidebar, Some(&key("cmd+alt+b")));
        assert_eq!(action_for(&settings, &key("cmd+shift+b")), None, "a rebound action drops its aliases");
        bind(&mut settings, Action::ToggleTools, Some(&key("cmd+shift+=")));
        assert_eq!(action_for(&settings, &key("cmd+shift+=")), Some(Action::ToggleTools));
    }
}
