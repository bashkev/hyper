//! The launcher: type to find a project or a recent session, Tab picks the tool, and Return
//! starts it there or resumes the session. It opens over the terminal on screen, which keeps
//! running, and is the whole pane when nothing is.

use std::path::{Path, PathBuf};

use rune::{Color, Cursor, El, FileDialogOptions, Key, KeyEvent, ScrollSpec, TextEdit, TextStyle, Tx, Waker, col, el, icon, row, text};
use rune_kit::{Keybind, Kit, SegmentedControl, Spinner, TextInput};

use super::{Choice, Main, Tool, folder_project, worktree_key};
use crate::commands::Action;
use crate::format::ago;
use crate::ui::{BRANCH, FOLDER, PLUS};
use crate::{App, Load, View, logos, worktree};

/// How many past sessions the launcher offers with nothing typed, and for a search. Browse
/// has the rest.
const RECENT: usize = 5;
const FOUND: usize = 8;
/// The launcher's width, and the most its list grows before it scrolls.
const PALETTE_WIDTH: f32 = 600.0;
const PALETTE_LIST: f32 = 360.0;
const CHOICE_ROW: f32 = 34.0;
const SECTION_ROW: f32 = 28.0;

impl App {
    pub fn close_palette(&mut self) {
        self.ws.palette = false;
        self.ws.naming = false;
        self.ws.query = TextEdit::new("");
        self.ws.worktree_name.focused = false;
        if !self.ws.creating {
            self.ws.error = None;
        }
        // The terminal is told on the next poll, once Rune has given focus back to what had it
        // before, which can be the find bar.
    }

    /// Escape: out of naming a worktree, then away from the launcher.
    pub fn palette_back(&mut self) {
        if self.ws.naming {
            self.ws.naming = false;
            self.ws.worktree = self.settings.default_worktree;
            self.ws.worktree_name = TextEdit::new("");
            self.ws.query.focused = true;
        } else if self.ws.palette {
            self.close_palette();
        } else if let Some(id) = self.ws.last_terminal.filter(|id| self.ws.live.iter().any(|l| l.id == *id)) {
            self.show(Main::Terminal(id));
        }
    }

    /// Sets the launcher to start in a folder as it is.
    #[cfg(test)]
    pub fn launch_in(&mut self, folder: PathBuf) {
        self.set_view(View::Workspace);
        self.show(Main::Launcher);
        self.ws.worktree = false;
        self.ws.worktree_name = TextEdit::new("");
        self.choose_folder(folder);
    }

    pub fn set_making_waker(&self, waker: Waker) {
        self.ws.making.set_waker(waker);
    }

    fn launcher_project(&self) -> Option<(String, PathBuf)> {
        if let Some(folder) = &self.ws.folder {
            return folder.is_dir().then(|| {
                let name = self.projects().iter().find(|(_, p)| p == folder).map_or_else(|| folder_project(folder), |(name, _)| name.clone());
                (name, folder.clone())
            });
        }
        let projects = self.projects();
        match &self.ws.project {
            // A project with its folder gone isn't quietly swapped for another.
            Some(name) => projects.iter().find(|(n, _)| n == name).cloned(),
            None => projects.first().cloned(),
        }
    }

    pub(super) fn choose_folder(&mut self, path: PathBuf) -> bool {
        if !path.is_dir() {
            self.ws.error = Some("Folder is unavailable".into());
            return false;
        }
        self.ws.error = None;
        if !self.ws.folders.contains(&path) {
            self.ws.folders.push(path.clone());
        }
        self.ws.folder = Some(path);
        self.relist();
        true
    }

    /// Asks for a folder and starts the tool picked in it.
    pub fn pick_folder(&mut self, tx: &mut Tx) {
        let directory = self.launcher_project().map(|(_, path)| path).or_else(|| Some(self.home.clone()));
        tx.open_folder(FileDialogOptions { title: "Choose folder".into(), directory, ..Default::default() }, |s: &mut App, result, _| match result {
            Ok(Some(path)) => {
                if s.choose_folder(path) {
                    s.start_from_launcher();
                }
            }
            Ok(None) => {},
            Err(_) => s.ws.error = Some("Could not open folder picker".into()),
        });
    }

    /// The launcher's rows, best first: places to start, then sessions to pick up.
    pub fn choices(&self) -> Vec<Choice> {
        // At launch, rows wait for the history rather than reshuffle as it arrives, and Return
        // can't pick one that isn't shown.
        if matches!(self.index, Load::Waiting) {
            return Vec::new();
        }
        let query = self.ws.query.text.trim().to_lowercase();
        let first = self.launcher_project().map(|(_, path)| path);
        let projects = self.projects();
        // Ranked by how well they match, and the folder already in use first among equals.
        let mut folders: Vec<((i32, bool), Choice)> = projects.iter().filter_map(|(name, path)| {
            let matched = if query.is_empty() {
                0
            } else {
                let label = folder_label(projects, name, path, &self.home);
                score(&query, &label).max(score(&query, &short_path(&path.to_string_lossy(), &self.home)))?
            };
            Some(((matched, first.as_ref() == Some(path)), Choice::Folder(name.clone(), path.clone())))
        }).collect();
        // A folder picked in Finder that isn't a project yet still comes first.
        if query.is_empty() && let Some(path) = first.filter(|p| !projects.iter().any(|(_, q)| q == p)) {
            folders.push(((0, true), Choice::Folder(folder_project(&path), path)));
        }
        folders.sort_by_key(|(rank, _)| std::cmp::Reverse(*rank));
        // Until something is typed, the newest few leave room for recent sessions.
        if query.is_empty() {
            folders.truncate(RECENT);
        }
        let sessions = self.sessions().iter()
            .filter(|s| self.live_for(&s.id).is_none() && self.can_resume(s))
            .filter(|s| query.is_empty() || score(&query, &s.title).or(score(&query, &s.project)).is_some())
            .take(if query.is_empty() { RECENT } else { FOUND })
            .map(|s| Choice::Resume(s.id.clone()));
        folders.into_iter().map(|(_, c)| c).chain(sessions).chain(std::iter::once(Choice::ChooseFolder)).collect()
    }

    fn active_choice(&self, choices: &[Choice]) -> usize {
        self.ws.active.as_ref().and_then(|a| choices.iter().position(|c| c == a)).unwrap_or(0)
    }

    /// The launcher's keys, wherever its keyboard focus is: the arrows move, Tab picks the
    /// tool, Return starts, ⌘Return starts in a new worktree (or not, if that's the default)
    /// and ⇧⌘Return names the worktree first.
    pub fn palette_key(&mut self, ev: &KeyEvent, tx: &mut Tx) -> bool {
        let tools = Tool::ALL.len();
        match &ev.key {
            Key::Escape => self.palette_back(),
            Key::Tab if !ev.mods.cmd => self.ws.tool = if ev.mods.shift { (self.ws.tool + tools - 1) % tools } else { (self.ws.tool + 1) % tools },
            _ if self.ws.creating => return false,
            Key::Enter if self.ws.naming => self.start_from_launcher(),
            Key::Enter => {
                let choices = self.choices();
                if let Some(choice) = choices.get(self.active_choice(&choices)).cloned() {
                    self.activate(choice, ev.mods.cmd, ev.mods.cmd && ev.mods.shift, tx);
                }
            }
            Key::Down | Key::Up if !self.ws.naming && !ev.mods.cmd => self.move_active(if ev.key == Key::Down { 1 } else { -1 }, tx),
            Key::Text(c) if !self.ws.naming && ev.mods.ctrl && (c == "n" || c == "p") => self.move_active(if c == "n" { 1 } else { -1 }, tx),
            _ => return false,
        }
        true
    }

    fn move_active(&mut self, by: isize, tx: &mut Tx) {
        let choices = self.choices();
        if choices.is_empty() {
            return;
        }
        let at = (self.active_choice(&choices) as isize + by).rem_euclid(choices.len() as isize) as usize;
        self.ws.active = Some(choices[at].clone());
        tx.scroll_into_view(format!("choice-{at}"));
    }

    /// Starts in a folder, picks a session up or asks for a folder. `other` flips whether a
    /// new worktree is made; `name` asks what to call it first.
    pub fn activate(&mut self, choice: Choice, other: bool, name: bool, tx: &mut Tx) {
        match choice {
            Choice::Folder(project, path) => {
                self.ws.project = Some(project);
                self.ws.folder = Some(path);
                self.ws.worktree_name = TextEdit::new("");
                if name {
                    self.ws.worktree = true;
                    self.ws.naming = true;
                    self.ws.query.focused = false;
                    self.ws.worktree_name.focused = true;
                    return;
                }
                self.ws.worktree = self.settings.default_worktree != other;
                self.start_from_launcher();
            }
            Choice::Resume(id) => self.resume(&id),
            Choice::ChooseFolder => {
                self.ws.worktree = self.settings.default_worktree != other;
                self.pick_folder(tx);
            }
        }
    }

    pub fn start_from_launcher(&mut self) {
        if self.ws.creating {
            return;
        }
        let Some((project, cwd)) = self.launcher_project() else { return };
        let tool = Tool::ALL[self.ws.tool];
        self.ws.project = Some(project.clone());
        self.ws.folder = Some(cwd.clone());
        if self.settings.remember_project && self.settings.last_folder.as_ref() != Some(&cwd) {
            let mut changed = self.settings.clone(); changed.last_folder = Some(cwd.clone()); self.persist_settings(changed);
        }
        if !self.ws.worktree {
            return self.start_in(tool, project, cwd);
        }
        // Checking a branch out can take a while in a big repository.
        self.ws.error = None;
        self.ws.creating = true;
        let name = self.ws.worktree_name.text.clone();
        self.ws.making.request((tool, project, cwd, name), |(tool, project, cwd, name)| (tool, project, worktree::add(&cwd, &name)));
    }

    #[cfg(test)]
    pub fn launcher_folder(&self) -> Option<PathBuf> {
        self.launcher_project().map(|(_, path)| path)
    }

    /// Starts what was waiting on a new worktree. Says whether anything shown changed.
    pub fn take_worktree(&mut self) -> bool {
        let Some((tool, project, made)) = self.ws.making.take() else { return false };
        self.ws.creating = false;
        match made {
            Ok(cwd) => {
                self.ws.worktree = false;
                self.ws.worktree_name = TextEdit::new("");
                if let Some(name) = worktree::name_of(&cwd) {
                    self.ws.closed_worktrees.remove(&worktree_key(&project, &name));
                }
                self.start_in(tool, project, cwd);
            }
            Err(error) => {
                // Started from the sidebar, the launcher wasn't up to say why.
                if matches!(self.ws.main, Main::Terminal(_)) && !self.ws.palette {
                    self.new_session(None, None);
                }
                self.ws.error = Some(error);
            }
        }
        true
    }

    #[cfg(test)]
    pub fn wait_for_worktree(&mut self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !self.take_worktree() {
            assert!(std::time::Instant::now() < deadline, "the worktree was never made");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    #[cfg(test)]
    pub fn recent_ids(&self) -> Vec<&str> {
        self.choices().into_iter().filter_map(|c| match c { Choice::Resume(id) => self.session(&id).map(|s| s.id.as_str()), _ => None }).collect()
    }
}

/// What a launcher row calls a folder: a worktree by its project and name, and one of two
/// same-named projects by where it is.
fn folder_label(projects: &[(String, PathBuf)], name: &str, path: &Path, home: &Path) -> String {
    let tree = |path: &Path| worktree::repo_of(path).and(worktree::name_of(path));
    match tree(path) {
        Some(tree) => format!("{name} / {tree}"),
        None if projects.iter().filter(|(n, p)| n == name && tree(p).is_none()).count() > 1 => short_path(&path.to_string_lossy(), home),
        None => name.to_owned(),
    }
}

/// Case-insensitive subsequence matching; runs and word starts rank first.
fn score(query: &str, value: &str) -> Option<i32> {
    let needle: Vec<char> = query.chars().collect();
    let (mut n, mut score, mut previous, mut boundary) = (0, 0, false, true);
    for c in value.to_lowercase().chars() {
        if n < needle.len() && c == needle[n] {
            score += 10 + if previous { 12 } else { 0 } + if boundary { 8 } else { 0 };
            previous = true;
            n += 1;
        } else {
            previous = false;
            // Gaps cost, but what follows a whole match doesn't: "r" fits rune as well as rex.
            if n < needle.len() {
                score -= 1;
            }
        }
        boundary = !c.is_alphanumeric();
    }
    (n == needle.len()).then_some(score)
}

fn section(title: &str, k: &Kit) -> El {
    text(title.to_owned()).font(k.ty.small_medium).color(k.c.text_faint).h(SECTION_ROW).px(10.0).pt(10.0).role("heading")
}

fn choice_row(s: &App, choice: &Choice, at: usize, active: bool, k: &Kit) -> El {
    let c = &k.c;
    let projects = s.projects();
    let (lead, label, detail) = match choice {
        Choice::Folder(name, path) => {
            let mark = if worktree::name_of(path).is_some() { &BRANCH } else { &FOLDER };
            (icon(mark, 16.0).color(c.icon_muted), folder_label(projects, name, path, &s.home), short_path(&path.to_string_lossy(), &s.home))
        }
        Choice::Resume(id) => match s.session(id) {
            Some(session) => (logos::logo(session.source, 16.0, k), session.title.clone(), format!("{} · {}", session.project, ago(session.updated))),
            None => return el(),
        },
        Choice::ChooseFolder => (icon(&PLUS, 16.0).color(c.icon_muted), "Choose folder…".to_owned(), Action::ChooseFolder.hint(&s.settings)),
    };
    let picked = choice.clone();
    let hovered = choice.clone();
    row()
        .key(format!("choice-{at}"))
        .role("option")
        .label(label.clone())
        .focus_selected(active)
        .w_full()
        .h(CHOICE_ROW)
        .px(10.0)
        .gap(10.0)
        .center_y()
        .radius(k.radius.md)
        .fill(if active { c.pressed } else { Color::TRANSPARENT })
        .cursor(Cursor::Pointer)
        .child(lead.a11y_hidden())
        .child(text(label).font(k.ty.ui).color(c.text_base).truncate().shrink().a11y_hidden())
        .child(el().grow(1.0))
        .child({
            // A session's project and age always fit; a long path gives way.
            let detail = text(detail).font(k.ty.ui).color(c.text_faint).a11y_hidden();
            if matches!(choice, Choice::Folder(..)) { detail.truncate().shrink() } else { detail }
        })
        .on_hover(move |s: &mut App| s.ws.active = Some(hovered.clone()))
        .on_press_tx(move |s: &mut App, tx| {
            let cmd = tx.mods().cmd;
            let shift = tx.mods().shift;
            s.activate(picked.clone(), cmd, cmd && shift, tx);
        })
        .preserve_focus_on_press(true)
}

fn hint(keys: &[&str], what: &str, k: &Kit) -> El {
    row().gap(6.0).center_y().child(Keybind::new(keys).build(k)).child(text(what.to_owned()).font(k.ty.small).color(k.c.text_muted))
}

/// Search, tool and where to start: over the terminal from ⌘N, or the main pane when nothing
/// is running.
pub(super) fn palette(s: &App, k: &Kit, home: bool) -> El {
    let c = &k.c;
    let field = if s.ws.naming {
        let named = s.launcher_project().map_or_else(String::new, |(name, _)| format!("New worktree in {name}"));
        TextInput::new(&s.ws.worktree_name)
            .plain()
            .leading_icon(&BRANCH)
            .height(48.0)
            .width(None)
            .placeholder("Random name")
            .label(named)
            .disabled(s.ws.creating)
            .on_edit(|s: &mut App| &mut s.ws.worktree_name)
            .on_key(|s: &mut App, ev, tx| s.palette_key(ev, tx))
            .build(k)
            .key("worktree-name")
    } else {
        TextInput::new(&s.ws.query)
            .plain()
            .leading_icon(k.glyph.search)
            .height(if home { 56.0 } else { 48.0 })
            .width(None)
            .placeholder("Start in a project, or resume a session")
            .text_style(if home { TextStyle::new(17.0, 400.0).lh(24.0) } else { k.ty.ui_compact })
            .label("Search projects and sessions")
            .disabled(s.ws.creating)
            .on_edit(|s: &mut App| &mut s.ws.query)
            .on_change(|s: &mut App, _| s.ws.active = None)
            .on_key(|s: &mut App, ev, tx| s.palette_key(ev, tx))
            .build(k)
            .key("palette-search")
    };
    let names = Tool::ALL.map(Tool::label);
    let tools = SegmentedControl::new(&names, Some(s.ws.tool)).label("Tool").fit().on_pick(|s: &mut App, i, _| s.ws.tool = i).build(k).key("tool").preserve_focus_on_press(true);
    // At home the search has the full width, with the tools on a line of their own.
    let header = if home {
        col().w_full().gap(10.0).pb(12.0).child(field.w_full()).child(row().px(10.0).child(tools))
    } else {
        row().w_full().pl(4.0).pr(10.0).gap(8.0).center_y().child(field.grow(1.0).shrink()).child(tools)
    };

    let choices = s.choices();
    let active = s.active_choice(&choices);
    let mut list = col().key("choices").role("listbox").label("Where to start").w_full().padding(6.0);
    let mut height = 12.0;
    let mut last = None;
    for (at, choice) in choices.iter().enumerate() {
        let kind = std::mem::discriminant(choice);
        if last != Some(kind) {
            let heading = match choice { Choice::Folder(..) => Some("Projects"), Choice::Resume(_) => Some("Recent"), Choice::ChooseFolder => None };
            if let Some(heading) = heading {
                list = list.child(section(heading, k));
                height += SECTION_ROW;
            }
            last = Some(kind);
        }
        list = list.child(choice_row(s, choice, at, at == active && !s.ws.naming, k));
        height += CHOICE_ROW;
    }
    // At home the list keeps its height, so the search above it never moves.
    let list = col().key("choices-scroll").w_full().h(if home { PALETTE_LIST } else { height.min(PALETTE_LIST) }).shrink().min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(list);
    let list = crate::history::swipeable(list);

    let new_worktree = if s.settings.default_worktree { "Here" } else { "New worktree" };
    let footer = if s.ws.creating {
        row().gap(8.0).center_y().child(Spinner::new().size(12.0).build(k)).child(text("Creating worktree…").font(k.ty.small).color(c.text_muted))
    } else if let Some(error) = &s.ws.error {
        row().child(text(error.clone()).font(k.ty.small).color(c.danger.fg).truncate().key("error"))
    } else if s.ws.naming {
        let create = s.launcher_project().map_or_else(|| "Create".to_owned(), |(name, _)| format!("Create in {name}"));
        row().gap(16.0).center_y().child(hint(&["↩"], &create, k)).child(hint(&["⇥"], "Tool", k)).child(hint(&["esc"], "Back", k))
    } else {
        let start = match choices.get(active) { Some(Choice::Resume(_)) => "Resume", Some(Choice::ChooseFolder) => "Choose", _ => "Start" };
        row().gap(16.0).center_y().child(hint(&["↩"], start, k)).child(hint(&["⌘", "↩"], new_worktree, k)).child(hint(&["⇥"], "Tool", k))
    };
    let divider = || el().w_full().h(1.0).fill(c.border_weak).a11y_hidden();
    let mut panel = col().key("launcher").role(if home { "group" } else { "dialog" }).label("New session").w_full().child(header).child(divider());
    if !s.ws.naming {
        panel = panel.child(list).children((!home).then(divider));
    }
    panel = panel.child(footer.key("footer").w_full().h(36.0).px(14.0).mt(if home { 12.0 } else { 0.0 }));
    if home {
        // The page itself, not a card on it.
        return panel.max_w(PALETTE_WIDTH + 40.0);
    }
    panel = panel.max_w(PALETTE_WIDTH).radius(12.0).fill(c.layer_01).border(1.0, c.border_base).clip_soft(12.0);
    for shadow in k.shadow_layers(&k.elevation.overlay) {
        panel = panel.box_shadow(shadow);
    }
    panel
}

/// The launcher floats near the top of the pane, over the terminal, which keeps running.
pub(super) fn palette_overlay(s: &App, k: &Kit) -> Option<El> {
    if !s.ws.palette || s.view != View::Workspace || !matches!(s.ws.main, Main::Terminal(_)) {
        return None;
    }
    // In a short window the list gives way, so the footer and its errors stay in view.
    let panel = palette(s, k, false).shrink().min_h(0.0).modal(true, |s: &mut App| s.palette_back()).modal_click_outside(true).token_motion(k.motion.overlay);
    Some(col().key("launcher-layer").absolute(0.0, 0.0).w_full().h_full().px(16.0).pt(72.0).pb(16.0).items_center().fill(Color::BLACK.alpha(0.18)).token_motion(k.motion.overlay).child(panel))
}

/// A folder as a shell would show it, with the home folder as `~`.
fn short_path(path: &str, home: &Path) -> String {
    match Path::new(path).strip_prefix(home) {
        Ok(rest) if !home.as_os_str().is_empty() => Path::new("~").join(rest).to_string_lossy().trim_end_matches('/').to_owned(),
        _ => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_under_home_is_shortened_and_others_arent() {
        let home = Path::new("/Users/me");
        assert_eq!(short_path("/Users/me/src/app", home), "~/src/app");
        assert_eq!(short_path("/Users/me", home), "~");
        assert_eq!(short_path("/Users/meg/app", home), "/Users/meg/app", "only a whole folder name matches");
        assert_eq!(short_path("/tmp/x", home), "/tmp/x");
    }

    #[test]
    fn a_whole_match_ranks_by_where_it_starts_not_by_what_follows() {
        assert_eq!(score("r", "rune"), score("r", "rex"));
        assert!(score("ru", "rune") > score("ru", "rex-runner"), "a run at the start beats one later");
        assert_eq!(score("x", "rune"), None);
    }
}
