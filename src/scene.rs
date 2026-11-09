//! Headless renders on the person's own data: `hyper --scene real --snap out.png ...`.

use std::sync::Arc;

use rune::TextEdit;

use crate::browse::Mode;
use crate::data::{self, Kind, Source};
use crate::usage::Prices;
use crate::{App, Load, View, config};

/// `name=value` from the command line.
pub fn arg(name: &str) -> Option<String> {
    rune::extra_args().iter().find_map(|a| a.strip_prefix(name)?.strip_prefix('=').map(str::to_owned))
}

/// A bare word on the command line.
fn flag(name: &str) -> bool {
    rune::extra_args().iter().any(|a| a == name)
}

/// Headless: loads real data synchronously, as the workers would in a window.
pub fn real(s: &mut App) {
    s.index_job.cancel();
    s.index = Load::Ready(Arc::new(data::load_index(&s.roots).unwrap_or_default()));
    s.config.job.cancel();
    s.config.items = Arc::new(config::load(&s.home, &s.settings, &s.settings_path));
    s.prices_job.cancel();
    s.prices = Prices::load(&s.roots);
    s.source = arg("source").and_then(|name| Source::ALL.into_iter().find(|x| x.label().eq_ignore_ascii_case(&name)));
    s.open_newest();
    s.relist();
    if flag("settings") {
        return s.set_view(View::Settings);
    }
    // Sessions and Config are Browse's: the Workspace lists no session to start from.
    if rune::extra_args().iter().any(|a| a == "config" || a.starts_with("session=") || a.starts_with("query=")) {
        s.set_view(View::Browse);
    }
    if let Some(query) = arg("query") {
        s.search.field = TextEdit::new(&query);
        let hits = data::search(s.sessions(), &query, &s.search.generation, 0);
        s.search.found(query, hits);
        s.relist();
    }
    let first = s.first_listed().map(|x| x.id.clone());
    let session = arg("session").or(first).and_then(|id| s.session(&id).cloned());
    if let Some(session) = &session {
        s.reader.reveal = s.search.hits.get(&session.id).map(|h| h.uuid.clone());
        s.reader.selected = Some(session.id.clone());
        s.set_transcript(data::load_transcript(session));
    }
    // `tabs=<url>,<url>` gives the browser those tabs, the last one shown.
    if let Some(urls) = arg("tabs") {
        let urls: Vec<&str> = urls.split(',').collect();
        s.adopt_browser(&serde_json::json!({ "tabs": urls, "active": urls.len() - 1 }));
    }
    // `panel=files|changes|browser` opens that pane beside what's shown.
    let panel = match arg("panel").as_deref() {
        Some("files") => Some(crate::project::Mode::Files),
        Some("changes") => Some(crate::project::Mode::Changes),
        Some("browser") => Some(crate::project::Mode::Browser),
        _ => None,
    };
    if let Some(mode) = panel {
        if !(s.inspector.is_open() && s.project.mode == mode) {
            s.toggle_project_panel(mode);
        }
        if mode.lists_files() {
            crate::project::load_scene(s);
        }
    }
    if flag("expanded") {
        s.sidebar.set_open(false);
        s.toggle_project_expanded();
    }
    if session.is_none() {
        return;
    }
    if flag("config") {
        s.set_mode(Mode::Config);
        // Config was read above; opening it here shouldn't read it again in the background.
        s.config.job.cancel();
        if let Some(id) = arg("item") {
            s.config.selected = Some(id);
        }
    }
    if flag("details") {
        s.inspector.toggle();
    }
    if flag("no-tools") {
        s.reader.show_tools = false;
        s.reader.reshow();
    }
    if flag("open-tools") {
        let ids: Vec<String> = match &s.reader.transcript {
            Load::Ready(t) => t.rows.iter().filter(|r| matches!(r.kind, Kind::Tool(_))).take(4).map(|r| r.id.clone()).collect(),
            _ => Vec::new(),
        };
        s.reader.expanded.extend(ids);
    }
}
