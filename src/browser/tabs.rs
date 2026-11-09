//! Tabs: each a page of its own, with its address, history and what failed. Only the one shown
//! is over the pane; the others stay hidden in the window, as they were.

use std::rc::{Rc, Weak};

use rune::{TextEdit, Tx};

use super::web::{self, Opened, WebView};
use super::Browser;
use crate::{App, View};
use crate::project::Mode;

#[derive(Default)]
pub struct Tab {
    pub address: TextEdit,
    id: Rc<()>,
    opener: Weak<()>,
    /// Made when it's first shown, or by the page that opened it.
    pub(super) web: Option<WebView>,
    pub(super) page: web::Page,
    pub(super) error: Option<web::Failure>,
    /// Where to go once the page exists.
    pending: Option<String>,
}

impl Tab {
    /// A tab that goes to `url` once it's shown, or an empty one for `""`.
    fn to(url: String) -> Tab {
        if url.is_empty() {
            return Tab::default();
        }
        Tab { address: TextEdit::new(&url), pending: Some(url), ..Tab::default() }
    }

    /// Where the page is, or is about to go.
    pub fn url(&self) -> &str {
        self.pending
            .as_deref()
            .or_else(|| self.error.as_ref().and_then(|e| e.url.as_deref()))
            .unwrap_or(&self.page.url)
    }

    pub(super) fn is_empty(&self) -> bool {
        self.url().is_empty() && !self.page.loading
    }

    /// What the tab is called: the page's title, else where it is.
    pub(super) fn name(&self) -> &str {
        match self.url() {
            "" => "New Tab",
            _ if self.error.is_none() && !self.page.title.is_empty() => &self.page.title,
            url => url,
        }
    }

    /// Goes to `url`, now or once the page exists.
    pub(super) fn load(&mut self, url: String) {
        self.error = None;
        match &self.web {
            Some(web) => {
                self.error = web.load(&url).err();
                self.page = web.page();
            }
            None => self.pending = Some(url),
        }
    }

    /// Does `f` to the page, once there is one.
    pub(super) fn with_page(&self, f: impl FnOnce(&WebView)) {
        if let Some(web) = &self.web {
            f(web);
        }
    }

    /// Takes in what the page says changed. Says whether anything shown did.
    fn take(&mut self) -> bool {
        let Some(web) = &self.web else { return false };
        let (page, error) = (web.page(), web.error());
        if page == self.page && error == self.error {
            return false;
        }
        self.page = page;
        self.error = error;
        // What's being typed stays. A load that failed shows where it was going.
        if !self.address.focused && self.address.text != self.url() {
            self.address = TextEdit::new(self.url());
        }
        true
    }
}

/// WebKit adds `/` to an HTTP URL with an empty path. Keep paths, queries and fragments distinct.
fn link_key(url: &str) -> (&str, &str) {
    let Some(authority) = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://")) else { return (url, "") };
    let boundary = url.len() - authority.len() + authority.find(['/', '?', '#']).unwrap_or(authority.len());
    let (origin, suffix) = url.split_at(boundary);
    let suffix = match suffix.as_bytes() {
        [b'/'] | [b'/', b'?' | b'#', ..] => &suffix[1..],
        _ => suffix,
    };
    (origin, suffix)
}

pub enum CloseTab {
    Active,
    At(usize),
    Script(usize),
}

impl Browser {
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    pub(super) fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active]
    }

    /// Closes the find bar, which was looking in the page shown.
    fn stop_finding(&mut self) {
        self.finding = false;
        self.find.focused = false;
        self.tab().with_page(WebView::clear_found);
    }

    /// Puts `tab` at `at`, keeping the one shown.
    fn insert(&mut self, at: usize, tab: Tab) {
        self.tabs.insert(at, tab);
        if at <= self.active {
            self.active += 1;
        }
    }
}

impl App {
    /// ⌘T and the strip's button: an empty tab, its address ready for typing. One already
    /// empty is new enough.
    pub fn new_browser_tab(&mut self, tx: &mut Tx) {
        if self.view == View::Settings {
            return;
        }
        if !self.browser.tab().is_empty() {
            self.browser.tabs.push(Tab::default());
            self.select_browser_tab(self.browser.tabs.len() - 1, tx);
        }
        self.open_location();
    }

    /// Shows tab `i`: typing goes to its page, or to the address of one with nowhere to go yet.
    pub fn select_browser_tab(&mut self, i: usize, tx: &mut Tx) {
        if i >= self.browser.tabs.len() {
            return;
        }
        self.show_browser_tab(i, tx);
        if self.browser.tab().is_empty() {
            self.focus_address();
        } else {
            self.browser_focus_page(tx);
        }
    }

    fn show_browser_tab(&mut self, i: usize, tx: &mut Tx) {
        if i != self.browser.active {
            self.browser.stop_finding();
            self.browser.active = i;
        }
        self.browser_place(i);
        tx.scroll_into_view_in("browser-tabs", format!("browser-tab-{i}"));
    }

    /// The strip's width, which Rune laid out, for the tabs to share.
    pub(super) fn browser_strip_laid_out(&mut self, frame: [f32; 4], tx: &mut Tx) {
        self.browser.strip = frame[2];
        if std::mem::take(&mut self.browser.reveal_restored_tab) {
            tx.scroll_into_view_in("browser-tabs", format!("browser-tab-{}", self.browser.active));
        }
    }

    /// ⇧⌘] and ⇧⌘[ while the browser has the keyboard: the tab `step` along, wrapping around.
    pub fn step_browser_tab(&mut self, step: isize, tx: &mut Tx) {
        let count = self.browser.tab_count();
        let at = (self.browser.active as isize + step).rem_euclid(count as isize);
        self.select_browser_tab(at as usize, tx);
    }

    /// ⌘W from the page, a tab's close button, and a page's own script: the tab goes, and the
    /// opener or adjacent tab shows. The last tab closes the browser pane.
    pub fn close_browser_tab(&mut self, target: CloseTab, tx: &mut Tx) {
        let (i, from_script) = match target {
            CloseTab::Active => (self.browser.active, false),
            CloseTab::At(i) => (i, false),
            CloseTab::Script(i) => (i, true),
        };
        let browser_open = self.browser_open();
        let transfer_focus = browser_open && (!from_script || self.browser_has_keyboard());
        let b = &mut self.browser;
        let Some(tab) = b.tabs.get(i) else { return };
        let opener = tab.opener.upgrade();
        if !from_script && !tab.url().is_empty() {
            let url = tab.url().to_owned();
            if b.closed_tabs.len() == 20 {
                b.closed_tabs.remove(0);
            }
            b.closed_tabs.push((i, url));
        }
        if b.tabs.len() == 1 {
            // Before the page leaves the window, or the keyboard would leave with it.
            self.browser_release_keyboard();
            self.browser = Browser { closed_tabs: std::mem::take(&mut self.browser.closed_tabs), ..Browser::default() };
            if browser_open {
                self.inspector.set_open(false);
            }
            return;
        }
        if i == b.active {
            b.stop_finding();
            b.focus_page.set(false);
        }
        b.tabs[i].with_page(WebView::release_keyboard);
        b.tabs.remove(i);
        if i < b.active {
            b.active -= 1;
        } else if i == b.active {
            b.active = opener.and_then(|id| b.tabs.iter().position(|tab| Rc::ptr_eq(&tab.id, &id)))
                .unwrap_or_else(|| i.min(b.tabs.len() - 1));
            if transfer_focus {
                self.select_browser_tab(self.browser.active, tx);
            } else {
                self.show_browser_tab(self.browser.active, tx);
            }
        }
    }

    /// Reopens the newest page closed by the user, in its former slot when possible.
    pub fn reopen_browser_tab(&mut self, tx: &mut Tx) {
        let open = self.browser_open();
        let b = &mut self.browser;
        let Some((i, url)) = b.closed_tabs.pop() else { return };
        let at = if !open && b.tabs.len() == 1 && b.tab().is_empty() {
            b.tabs[0] = Tab::to(url);
            0
        } else {
            let at = i.min(b.tabs.len());
            b.insert(at, Tab::to(url));
            at
        };
        if !open {
            self.toggle_project_panel(Mode::Browser);
        }
        self.select_browser_tab(at, tx);
    }

    /// Goes to `url` in a tab: one already there, else the one shown if it's empty, else a new
    /// one.
    pub(super) fn browser_open_tab(&mut self, url: &str, tx: &mut Tx) {
        let b = &mut self.browser;
        let key = link_key(url);
        let at = match b.tabs.iter().position(|t| link_key(t.url()) == key) {
            Some(at) => at,
            None if b.tab().is_empty() => {
                b.tab_mut().address = TextEdit::new(url);
                b.tab_mut().load(url.to_owned());
                b.active
            }
            None => {
                b.tabs.push(Tab::to(url.to_owned()));
                b.tabs.len() - 1
            }
        };
        self.select_browser_tab(at, tx);
    }

    /// Puts tab `i`'s page where Rune laid out the pane, making it if it isn't yet, and starts
    /// the load it was waiting for.
    pub(super) fn browser_place(&mut self, i: usize) {
        let Some(frame) = self.browser.frame else { return };
        if self.browser.tabs.get(i).is_some_and(|t| t.web.is_none()) {
            let web = WebView::new(self.wake());
            self.browser.tabs[i].web = web;
        }
        let Some(tab) = self.browser.tabs.get_mut(i) else { return };
        let Some(web) = &tab.web else { return };
        web.place(frame);
        if let Some(url) = tab.pending.take() {
            tab.load(url);
        }
    }

    /// Takes in what the pages say changed: tabs they opened or closed, then each page's
    /// address, title and history. Says whether anything shown did.
    pub fn take_browser(&mut self, tx: &mut Tx) -> bool {
        let mut changed = false;
        let mut i = 0;
        while i < self.browser.tabs.len() {
            let Some(web) = &self.browser.tabs[i].web else {
                i += 1;
                continue;
            };
            let (opened, closed) = (web.take_opened(), web.closed());
            let can_focus = !opened.is_empty() && i == self.browser.active && self.browser_shown() && web.has_keyboard();
            for (n, new) in opened.into_iter().enumerate() {
                let at = i + 1 + n;
                let (web, opener, show) = match new {
                    Opened::Shown(web) => (web, Rc::downgrade(&self.browser.tabs[i].id), can_focus),
                    Opened::Behind(web) => (web, Weak::new(), false),
                };
                // Its address may not be known yet, but WebKit is already loading it.
                let page = web.page();
                let address = TextEdit::new(&page.url);
                self.browser.insert(at, Tab { web: Some(web), opener, page, address, ..Tab::default() });
                if show {
                    self.show_browser_tab(at, tx);
                    self.browser_focus_page(tx);
                }
                changed = true;
            }
            if closed {
                self.close_browser_tab(CloseTab::Script(i), tx);
                changed = true;
                continue;
            }
            changed |= self.browser.tabs[i].take();
            i += 1;
        }
        changed
    }

    /// Opens the browser to `state`'s tabs, each loading when it's first shown. `url` alone is
    /// what a build without tabs hands over.
    pub fn adopt_browser(&mut self, state: &serde_json::Value) {
        let urls: Vec<String> = match state["tabs"].as_array() {
            Some(tabs) => tabs.iter().filter_map(|t| t.as_str()).map(str::to_owned).collect(),
            None => state["url"].as_str().filter(|u| !u.is_empty()).map(str::to_owned).into_iter().collect(),
        };
        if !urls.is_empty() {
            let b = &mut self.browser;
            b.active = state["active"].as_u64().and_then(|a| usize::try_from(a).ok()).filter(|&a| a < urls.len()).unwrap_or(0);
            b.tabs = urls.into_iter().map(Tab::to).collect();
            b.reveal_restored_tab = true;
        }
        if state["open"].as_bool() == Some(true) {
            self.toggle_project_panel(Mode::Browser);
        }
    }

    pub fn browser_hand_off(&self) -> serde_json::Value {
        let b = &self.browser;
        let tabs: Vec<&str> = b.tabs.iter().map(Tab::url).collect();
        serde_json::json!({ "tabs": tabs, "active": b.active, "open": self.browser_open() })
    }
}
