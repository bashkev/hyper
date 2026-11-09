//! Browse's Config: each tool's rules, settings, servers, skills and hooks, and Hyper's own.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use rune::{El, Latest, ScrollSpec, col, el, text};
use rune_kit::markdown::{self, Case};
use rune_kit::shell::SidebarRow;
use rune_kit::{Button, ButtonVariant, Kit, Markdown};

use crate::config::{self, ConfigItem, Owner};
use crate::format::timestamp;
use crate::sidebar::{COMPACT_ROW, Item, ROW};
use crate::ui::{center_message, centered, copy_button, field};
use crate::{App, View};

/// The config items, as last read.
#[derive(Default)]
pub struct ConfigList {
    pub items: Arc<Vec<ConfigItem>>,
    pub job: Latest<Vec<ConfigItem>>,
    pub selected: Option<String>,
    /// The keys of groups the user has closed; they start open.
    pub closed: HashSet<String>,
}

impl ConfigList {
    pub fn item(&self, id: &str) -> Option<&ConfigItem> {
        self.items.iter().find(|i| i.id == id)
    }

    pub fn selected_item(&self) -> Option<&ConfigItem> {
        self.selected.as_deref().and_then(|id| self.item(id))
    }
}

impl App {
    /// Setup files change rarely, so they're read again each time Config comes into view
    /// rather than watched.
    pub fn request_config(&self) {
        self.config.job.request((self.home.clone(), self.settings.clone(), self.settings_path.clone()), |(home, settings, path)| config::load(&home, &settings, &path));
    }

    /// Config items that pass the source filter and the box's text, in their tools' groups.
    fn config_groups(&self) -> Vec<(Owner, &'static str, Vec<usize>)> {
        let query = self.search.query().to_lowercase();
        let mut groups: Vec<(Owner, &'static str, Vec<usize>)> = Vec::new();
        for (i, item) in self.config.items.iter().enumerate() {
            let passes = self.source.is_none_or(|source| item.source == Owner::Tool(source));
            let found = query.is_empty()
                || item.group.to_lowercase().contains(&query)
                || item.title.to_lowercase().contains(&query)
                || item.subtitle.to_lowercase().contains(&query)
                || (!item.placeholder && item.markdown.to_lowercase().contains(&query));
            if !passes || !found {
                continue;
            }
            match groups.last_mut() {
                Some((source, group, items)) if *source == item.source && *group == item.group => items.push(i),
                _ => groups.push((item.source, item.group, vec![i])),
            }
        }
        groups
    }

    /// Takes config that's been read.
    pub fn take_config(&mut self) -> bool {
        let Some(items) = self.config.job.take() else { return false };
        self.config.items = Arc::new(items);
        self.relist();
        // Read again, the selected item may be gone, or the list was empty when Config opened.
        if self.mode == crate::browse::Mode::Config && self.config.selected_item().is_none() {
            self.config.selected = self.first_config();
        }
        true
    }

    pub fn relist_config(&mut self) {
        let searching = !self.search.query().is_empty();
        let mut listed = Vec::new();
        let mut tool = None;
        for (source, group, items) in self.config_groups() {
            if self.source.is_none() && tool != Some(source) {
                tool = Some(source);
                listed.push(Item::Tool(source));
            }
            let key = format!("config-{}-{group}", source.label());
            let open = searching || !self.config.closed.contains(&key);
            let count = items.iter().filter(|&&i| !self.config.items[i].placeholder).count();
            listed.push(Item::Project { key, name: group.to_owned(), count, open, live: false, attention: false });
            if open {
                listed.extend(items.into_iter().map(Item::Config));
            }
        }
        self.listed = listed;
    }

    pub fn first_config(&self) -> Option<String> {
        self.listed.iter().find_map(|item| match item {
            Item::Config(i) => self.config.items.get(*i).map(|c| c.id.clone()),
            _ => None,
        })
    }
}

pub fn config_row(s: &App, n: usize, k: &Kit) -> El {
    let Some(item) = s.config.items.get(n) else { return el() };
    let id = item.id.clone();
    let mut r = SidebarRow::new(item.title.clone())
        .depth(1)
        .height(if item.subtitle.is_empty() { COMPACT_ROW } else { ROW })
        .selected(s.config.selected.as_deref() == Some(item.id.as_str()))
        .on_select(move |s: &mut App, tx| {
            s.stepping(tx);
            s.config.selected = Some(id.clone());
        });
    if !item.subtitle.is_empty() {
        r = r.subtitle(item.subtitle.clone());
    }
    r.build(k).key(item.id.clone())
}

pub fn config_pane(s: &App, k: &Kit) -> El {
    let Some(item) = s.config.selected_item() else {
        return center_message(text("Nothing selected").font(k.ty.ui).color(k.c.text_muted));
    };
    let query = s.search.query();
    let mut body = col().w_full().gap(14.0).py(16.0);
    if let Some(path) = &item.path {
        body = body.child(text(path.display().to_string()).key("config-path").font(k.ty.small).color(k.c.text_faint).wrap_anywhere().selectable(true));
    }
    let markdown = Markdown::new(item.markdown.clone()).selectable(true).scroll_wide(true).highlights(markdown::find(&item.markdown, &query, Case::Insensitive));
    body = body.child(markdown.build(k));
    let viewport = col().key(format!("config-{}", item.id)).w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }).child(centered(body));
    col().w_full().h_full().min_h(0.0).child(crate::history::swipeable(viewport))
}

pub fn config_details(s: &App, k: &Kit) -> El {
    let field = |label: &str, value: String| field(label, value, k);
    let mut body = col().key("details").w_full().padding(16.0).gap(14.0);
    let Some(item) = s.config.selected_item() else { return body };
    body = body.child(field("Source", item.source.label().to_owned())).child(field("Type", item.group.to_owned()));
    let tab = config::SETTINGS_TABS.iter().position(|t| *t == item.title).filter(|_| item.source == Owner::Hyper);
    if let Some(tab) = tab {
        let open = Button::new("Open in Settings").variant(ButtonVariant::Neutral).on_press_tx(move |s: &mut App, _| {
            s.preferences.tab = tab;
            s.set_view(View::Settings);
        });
        body = body.child(open.build(k).key("open-settings"));
    }
    let Some(path) = item.path.clone() else { return body };
    body = body.child(field("Path", path.display().to_string()));
    let modified = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(UNIX_EPOCH).ok());
    if let Some(modified) = modified {
        body = body.child(field("Updated", timestamp(modified.as_secs() as i64)));
    }
    body.child(copy_button(s, "Copy path", "copy-path", path.display().to_string(), k))
}
