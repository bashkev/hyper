//! Browse: every session, grouped by project and filtered by tool, searched through, and each
//! tool's config. The open session is the [`reader`](crate::reader)'s.

mod config;

pub use config::{ConfigList, config_details, config_pane, config_row};

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rune::{El, KeyEvent, Latest, TextEdit, TextStyle, Tx, el, row, text};
use rune_kit::shell::{RowStatus, SidebarRow};
use rune_kit::{Kit, SegmentedControl, Spinner, TextInput, glyphs};

use crate::data::{self, Hit, Session, Source};
use crate::format::ago;
use crate::live::Activity;
use crate::sidebar::{COMPACT_ROW, Item, ROW, SIDEBAR_SCROLL};
use crate::ui::live_dot;
use crate::{App, Load, View, logos};

/// How many of the newest projects start open.
const OPEN_PROJECTS: usize = 3;

/// A search shorter than this would match nearly everything in 2 GB of text.
const MIN_QUERY: usize = 3;

/// All, then each tool: equal segments would crowd a logo and a long name, and leave Pi's empty.
const SEGMENT_SHARES: [f32; 4] = [0.75, 1.25, 1.2, 0.8];

/// What Browse lists.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    Sessions,
    Config,
}

/// Browse's search: the box, and what the last search through every transcript found.
pub struct Search {
    pub field: TextEdit,
    /// The sessions the last finished search found, by id.
    pub hits: HashMap<String, Hit>,
    /// The query `hits` answers.
    pub searched: String,
    pub job: Latest<(String, Vec<Hit>)>,
    /// Bumped by each search, so one that's been superseded stops.
    pub generation: Arc<AtomicU64>,
}

impl Default for Search {
    fn default() -> Search {
        Search {
            field: TextEdit::new(""),
            hits: HashMap::new(),
            searched: String::new(),
            job: Latest::default().debounce(Duration::from_millis(200)),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }
}

impl Search {
    pub fn query(&self) -> String {
        self.field.text.trim().to_owned()
    }

    /// Keeps what a search for `query` found.
    pub fn found(&mut self, query: String, hits: Vec<Hit>) {
        self.searched = query;
        self.hits = hits.into_iter().map(|h| (h.session.clone(), h)).collect();
    }
}

impl App {
    /// The sessions that pass the source filter and the box's current text, newest first:
    /// name and project matches now, and any content matches of the last finished search.
    pub fn matching(&self) -> Vec<usize> {
        let query = self.search.query().to_lowercase();
        self.sessions()
            .iter()
            .enumerate()
            .filter(|(_, s)| self.source.is_none_or(|source| s.source == source))
            .filter(|(_, s)| {
                query.is_empty()
                    || self.search.hits.contains_key(&s.id)
                    || s.title.to_lowercase().contains(&query)
                    || s.project.to_lowercase().contains(&query)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// Projects in order of their newest session, each with its sessions.
    pub fn grouped(&self) -> Vec<(String, Vec<usize>)> {
        let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
        let mut places: HashMap<&str, usize> = HashMap::new();
        for i in self.matching() {
            let project = self.sessions()[i].project.as_str();
            let place = *places.entry(project).or_insert_with(|| {
                groups.push((project.to_owned(), Vec::new()));
                groups.len() - 1
            });
            groups[place].1.push(i);
        }
        groups
    }

    pub fn set_mode(&mut self, mode: Mode) {
        if mode == Mode::Config && self.mode != mode && self.view == View::Browse {
            self.request_config();
        }
        self.mode = mode;
        self.search_changed();
        if mode == Mode::Config && self.config.selected.is_none() {
            self.config.selected = self.first_config();
        }
    }

    /// Lists sessions by project, the open projects with their sessions under them.
    pub fn relist_sessions(&mut self) {
        let searching = !self.search.query().is_empty();
        let mut listed = Vec::new();
        for (name, sessions) in self.grouped() {
            let open = searching || self.open_projects.contains(&name);
            let live = sessions.iter().any(|&i| self.activity(&self.sessions()[i]).is_some());
            listed.push(Item::Project { key: format!("project-{name}"), name, count: sessions.len(), open, live, attention: false });
            if open {
                listed.extend(sessions.into_iter().map(Item::Session));
            }
        }
        self.listed = listed;
    }

    /// Opens the newest projects of the current view, for when it's first shown.
    pub fn open_newest(&mut self) {
        let newest: Vec<String> = self.grouped().into_iter().take(OPEN_PROJECTS).map(|(name, _)| name).collect();
        self.open_projects.extend(newest.into_iter().filter(|name| !self.closed_projects.contains(name)));
    }

    pub fn set_source(&mut self, source: Option<Source>) {
        self.source = source;
        self.open_newest();
        self.relist();
    }

    pub fn first_listed(&self) -> Option<&Session> {
        self.listed.iter().find_map(|item| match item {
            Item::Session(i) => self.sessions().get(*i),
            _ => None,
        })
    }

    pub fn search_changed(&mut self) {
        let query = self.search.query();
        let mine = self.search.generation.fetch_add(1, Ordering::Relaxed) + 1;
        if query != self.search.searched {
            self.search.hits.clear();
            self.search.searched.clear();
        }
        self.relist();
        if query.len() < MIN_QUERY || self.mode == Mode::Config {
            self.search.job.cancel();
            return;
        }
        let Load::Ready(sessions) = &self.index else { return };
        let (sessions, generation) = (sessions.clone(), self.search.generation.clone());
        self.search.job.request(query, move |query| {
            let hits = data::search(&sessions, &query, &generation, mine);
            (query, hits)
        });
    }

    pub fn clear_search(&mut self) {
        let focused = self.search.field.focused;
        self.search.field = TextEdit::new("");
        self.search.field.focused = focused;
        self.search_changed();
    }

    pub fn select_first(&mut self) {
        if let Some(id) = self.first_listed().map(|s| s.id.clone()) {
            self.select(&id);
        }
    }

    /// Takes what a search found, if the box still asks for it.
    pub fn take_hits(&mut self, tx: &mut Tx) -> bool {
        let Some((query, hits)) = self.search.job.take() else { return false };
        if query != self.search.query() {
            return false;
        }
        self.search.found(query, hits);
        self.relist();
        // Rows that stay hold their place, which would leave new matches above the top.
        tx.scroll_to_y(SIDEBAR_SCROLL, 0.0);
        true
    }
}

pub fn session_row(s: &App, n: usize, k: &Kit) -> El {
    let Some(session) = s.sessions().get(n) else { return el() };
    let hit = s.search.hits.get(&session.id);
    let detail = match hit {
        Some(h) if h.count >= data::MAX_HITS => format!("{}+", data::MAX_HITS),
        Some(h) => h.count.to_string(),
        None => ago(session.updated),
    };
    // Under a project heading the only thing left to say is which tool it was, and a logo says it.
    let tool = s.source.is_none().then_some(session.source);
    let subtitle = hit.map(|h| h.snippet.clone());
    // The logo is hidden from assistive tech, so the row's name carries the tool instead.
    let name: Vec<&str> = [Some(session.title.as_str()), tool.map(Source::label), subtitle.as_deref()].into_iter().flatten().collect();
    let name = name.join(", ");
    let id = session.id.clone();
    let mut r = SidebarRow::new(session.title.clone())
        .depth(1)
        .height(if subtitle.is_some() { ROW } else { COMPACT_ROW })
        .detail(detail)
        .selected(s.reader.selected.as_deref() == Some(session.id.as_str()))
        .on_select(move |s: &mut App, tx| {
            s.stepping(tx);
            s.select(&id);
        });
    if let Some(subtitle) = subtitle {
        r = r.subtitle(subtitle);
    }
    if let Some(tool) = tool {
        r = r.avatar(logos::logo(tool, 16.0, k));
    }
    match s.activity(session) {
        Some(Activity::Working) => r = r.status(RowStatus::Busy),
        Some(Activity::Open) => r = r.status(RowStatus::Other(live_dot(k))),
        None => {}
    }
    r.build(k).label(name).key(session.id.clone())
}

/// What the top of the sidebar holds in Browse.
pub fn header(s: &App, k: &Kit) -> [El; 3] {
    // A full-width field is wider than its parent by its own end padding, so let it shrink.
    [row().w_full().child(search_field(s, k).shrink().min_w(0.0)), mode_filter(s, k), source_filter(s, k)]
}

fn search_field(s: &App, k: &Kit) -> El {
    let searching = s.search.job.is_pending();
    let mut field = TextInput::new(&s.search.field)
        .placeholder("Search")
        .label("Search sessions")
        .leading_icon(&glyphs::SEARCH)
        .on_edit(|s: &mut App| &mut s.search.field)
        .on_change(|s: &mut App, tx| {
            s.search_changed();
            tx.scroll_to_y(SIDEBAR_SCROLL, 0.0);
        })
        .on_key(|s: &mut App, ev: &KeyEvent, _| {
            let escape = matches!(ev.key, rune::Key::Escape);
            if escape {
                s.clear_search();
            }
            escape
        });
    if searching {
        field = field.trailing(Spinner::new().size(14.0).build(k).key("searching"));
    } else if !s.search.field.text.is_empty() {
        field = field.clear_button().on_action(|s: &mut App, tx| {
            s.clear_search();
            tx.scroll_to_y(SIDEBAR_SCROLL, 0.0);
        });
    }
    field.build(k).key("search")
}

fn mode_filter(s: &App, k: &Kit) -> El {
    SegmentedControl::new(&["Sessions", "Config"], Some(usize::from(s.mode == Mode::Config)))
        .label("View")
        .width(None)
        .on_pick(|s: &mut App, i, tx| {
            s.set_mode(if i == 0 { Mode::Sessions } else { Mode::Config });
            tx.scroll_to_y(SIDEBAR_SCROLL, 0.0);
        })
        .build(k)
        .key("mode-filter")
}

/// The kit's `SegmentedControl` at full width, which only takes text, with each tool's logo.
fn source_filter(s: &App, k: &Kit) -> El {
    let c = &k.c;
    let font = TextStyle { line_height: 13.0, ..k.ty.ui };
    let raised = rune::Shadow { spread: 0.5, color: c.border_strong, ..Default::default() };
    let choices = std::iter::once(None).chain(Source::ALL.map(Some));
    let segments = choices.enumerate().map(|(i, source)| {
        let on = s.source == source;
        let name = source.map_or("All", Source::label);
        let mut label = text(name).key("label").font(font).color(if on { c.text_base } else { c.text_muted }).truncate().shrink().a11y_hidden();
        if !on {
            label = label.group_color(c.text_base);
        }
        let segment = row()
            .key(format!("segment{i}"))
            .role("radio")
            .label(name)
            .position_in_set(i + 1, Source::ALL.len() + 1)
            .focusable(true)
            .focus_selected(on)
            .focus_ring(2.0, 1.0, c.border_focus)
            .grow(SEGMENT_SHARES[i])
            .shrink()
            .h(28.0)
            .gap(5.0)
            .center()
            .radius(6.0)
            .children(source.map(|x| logos::logo(x, 14.0, k)))
            .child(label)
            .on_press_tx(move |s: &mut App, tx: &mut Tx| {
                s.set_source(source);
                // Rows kept across a change hold their place, which would leave the top out of view.
                tx.scroll_to_y(SIDEBAR_SCROLL, 0.0);
            });
        if on { segment.fill(c.bg_base).box_shadow(raised) } else { segment.hover(c.hover).press(c.pressed) }
    });
    let bar = row().key("source-filter").role("radiogroup").label("Source").focus_group().w_full().h(28.0).center_y().radius(6.0).fill(c.layer_01);
    bar.box_shadow(rune::Shadow { spread: 0.5, color: c.border_base, ..Default::default() }).children(segments)
}
