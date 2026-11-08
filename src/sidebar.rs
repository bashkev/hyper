//! The sidebar: switches between the Workspace and Browse, and lists what the view on screen
//! has, as one flattened list of [`Item`]s that `relist` builds again whenever it changes.

use std::path::PathBuf;

use rune::{Anim, El, Icon, TextStyle, Tx, col, el, icon, row, text};
use rune_kit::shell::{self, Sidebar, SidebarRow};
use rune_kit::{Kit, SegmentedControl};

use crate::browse::{self, Mode, config_row, session_row};
use crate::config::Owner;
use crate::ui::{FOLDER, GEAR, attention_dot, live_dot};
use crate::{App, View, logos, toggle_in, workspace};

pub const ROW: f32 = 45.0;
pub const COMPACT_ROW: f32 = 32.0;
const HEADING_ROW: f32 = 34.0;
const TOOL_ROW: f32 = 44.0;
/// Rows sit this far in from the sidebar's sides, as the kit's own do.
pub const ROW_INSET: f32 = 8.0;
const DISCLOSURE: f32 = 12.0;
pub const SIDEBAR_SCROLL: &str = "sidebar-scroll";

/// A row of the sidebar's flattened list.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// Heads Hyper's or a tool's config groups under All.
    Tool(Owner),
    /// `attention` when a terminal folded under it needs the person.
    Project { name: String, key: String, count: usize, open: bool, live: bool, attention: bool },
    /// An index into the index.
    Session(usize),
    /// An index into the config items.
    Config(usize),
    /// A terminal in the Workspace, by id.
    Live(u64),
    /// Heads the terminals running in one of a project's worktrees.
    Worktree(Worktree),
}

/// A worktree heading in the Workspace sidebar, with the folder to start another terminal in.
#[derive(Clone, Debug, PartialEq)]
pub struct Worktree {
    pub name: String,
    pub key: String,
    pub count: usize,
    pub open: bool,
    /// A terminal folded under it needs the person.
    pub attention: bool,
    pub folder: PathBuf,
}

impl App {
    /// Lists what the view on screen has.
    pub fn relist(&mut self) {
        if self.view == View::Workspace {
            return self.relist_workspace();
        }
        if self.view == View::Settings {
            return self.listed.clear();
        }
        if self.mode == Mode::Config {
            return self.relist_config();
        }
        self.relist_sessions();
    }

    pub fn toggle_project(&mut self, name: &str, key: &str) {
        // A search shows every heading open, so a click here would change nothing visible and
        // only surprise the user once the search clears. Browse's search and mode are its own.
        let browsing = self.view == View::Browse;
        if browsing && !self.search.query().is_empty() {
            return;
        }
        match self.mode {
            Mode::Config if browsing => toggle_in(&mut self.config.closed, key),
            _ => {
                if self.open_projects.remove(name) {
                    self.closed_projects.insert(name.to_owned());
                } else {
                    self.closed_projects.remove(name);
                    self.open_projects.insert(name.to_owned());
                }
            }
        }
        self.relist();
    }
}

/// A tool's heading: the top of the config list under All.
fn source_heading(source: Owner, first: bool, k: &Kit) -> El {
    let title = text(source.label()).font(TextStyle { weight: 700.0, ..k.ty.body }).color(k.c.text_base).a11y_hidden();
    let rule = (!first).then(|| el().w_full().h(1.0).fill(k.c.border_base));
    let name = row().px(ROW_INSET).gap(8.0).center_y().child(logos::logo(source, 16.0, k)).child(title);
    col().label(source.label()).role("heading").w_full().h(TOOL_ROW).justify_end().gap(8.0).pb(4.0).children(rule).child(name)
}

/// A project's or config group's heading, with how many rows it has or a button at its end.
fn project_row(name: &str, key: &str, count: usize, open: bool, mark: Option<El>, plus: Option<El>, k: &Kit) -> El {
    let (toggled, toggled_key) = (name.to_owned(), key.to_owned());
    let base = row()
        .label(name.to_owned())
        .description(format!("{count} items"))
        .w_full()
        .h(HEADING_ROW)
        .pt(6.0)
        .px(ROW_INSET)
        .on_press_tx(move |s: &mut App, _| s.toggle_project(&toggled, &toggled_key));
    let end = plus.unwrap_or_else(|| text(count.to_string()).key("count").font(k.ty.small_medium).color(k.c.text_faint).a11y_hidden());
    group_header(base, name, open, (&FOLDER, "folder"), mark, end, k)
}

/// A heading that folds the rows under it: a disclosure triangle, `glyph` and the group's name,
/// then `mark` and `end`. Brighter and heavier than its rows, which indent to its glyph. Pointer
/// only, like the kit's: arrow keys passing it would toggle it. `base` gives its label, size and
/// padding, and what a press does.
pub fn group_header(base: El, name: &str, open: bool, (glyph, glyph_key): (&'static Icon, &'static str), mark: Option<El>, end: El, k: &Kit) -> El {
    let c = &k.c;
    let triangle = if open { k.glyph.triangle_down } else { k.glyph.triangle_right };
    let header = base
        // The glyph lands one indent in, where the rows under it start.
        .gap(shell::INDENT - DISCLOSURE)
        .center_y()
        .radius(k.radius.md)
        .child(icon(triangle, DISCLOSURE).key("disclosure").color(c.icon_muted).a11y_hidden())
        .child(icon(glyph, 16.0).key(glyph_key).color(c.icon_muted).a11y_hidden())
        .child(text(name.to_owned()).key("title").font(TextStyle { weight: 600.0, ..k.ty.ui }).color(c.text_base).truncate().grow(1.0).shrink().pl(2.0).a11y_hidden())
        .children(mark.map(|m| m.key("live")))
        .child(end);
    k.interactive(header).role("button").expanded(open).hover_motion(Anim::INSTANT, Anim::INSTANT).focusable(false)
}

/// What a group's rows would show about themselves, for when they're folded away: that one
/// needs the person, or that something is running.
pub fn folded_mark(folded: bool, attention: bool, k: &Kit) -> Option<El> {
    folded.then(|| if attention { attention_dot(k) } else { live_dot(k) })
}

fn list_row(s: &App, i: usize, k: &Kit) -> El {
    match s.listed.get(i) {
        Some(Item::Tool(source)) => source_heading(*source, i == 0, k).key(source.label()),
        Some(Item::Project { name, count, open, key, live, attention }) => project_row(name, key, *count, *open, folded_mark(*live && !*open, *attention, k), (s.view == View::Workspace).then(|| workspace::plus(s, name, k)), k).key(key.clone()),
        Some(Item::Session(n)) => session_row(s, *n, k),
        Some(Item::Config(n)) => config_row(s, *n, k),
        Some(Item::Live(id)) => workspace::live_row(s, *id, k),
        Some(Item::Worktree(worktree)) => workspace::worktree_row(s, worktree, k).key(worktree.key.clone()),
        None => el(),
    }
}

fn view_filter(s: &App, k: &Kit) -> El {
    let picked = match s.view {
        View::Workspace => Some(0),
        View::Browse => Some(1),
        View::Settings => None,
    };
    SegmentedControl::new(&["Workspace", "Browse"], picked)
        .label("Section")
        .width(None)
        .on_pick(|s: &mut App, i, tx| {
            s.set_view(if i == 0 { View::Workspace } else { View::Browse });
            tx.scroll_to_y(SIDEBAR_SCROLL, 0.0);
            // As ⌘1 and ⌘2 do: a segment left with the keyboard would take Space from the terminal.
            tx.blur();
        })
        .build(k)
        .key("view-filter")
        .preserve_focus_on_press(true)
}

pub fn sidebar(s: &App, k: &Kit) -> El {
    let kit = k.clone();
    let mut bar = Sidebar::new()
        .label("Sessions")
        .inset(0.0)
        .header_gap(8.0)
        .header_padding(0.0, 6.0)
        .row_gap(0.0)
        .row_height(ROW)
        .scroll_with(crate::history::swipeable)
        .header(view_filter(s, k));
    match s.view {
        View::Workspace => bar = bar.header(workspace::new_button(s, k)),
        View::Browse => {
            for header in browse::header(s, k) {
                bar = bar.header(header);
            }
        }
        View::Settings => {}
    }
    bar = bar.footer(
        SidebarRow::new("Settings")
            .icon(&GEAR)
            .height(COMPACT_ROW)
            .selected(s.view == View::Settings)
            .on_select(|s: &mut App, tx: &mut Tx| {
                s.set_view(View::Settings);
                tx.scroll_to_y(SIDEBAR_SCROLL, 0.0);
            })
            .build(k)
            .key("settings-row"),
    );
    bar.virtual_rows(
            s.listed.len(),
            |s: &App, i| match s.listed.get(i) {
                Some(Item::Tool(source)) => source.label().into(),
                Some(Item::Project { key, .. }) => key.as_str().into(),
                Some(Item::Session(n)) => s.sessions().get(*n).map_or("".into(), |x| x.id.as_str().into()),
                Some(Item::Config(n)) => s.config.items.get(*n).map_or("".into(), |x| x.id.as_str().into()),
                Some(Item::Live(id)) => format!("live-{id}").into(),
                Some(Item::Worktree(worktree)) => worktree.key.as_str().into(),
                None => "".into(),
            },
            move |s: &App, i| list_row(s, i, &kit),
        )
        .build(k)
}
