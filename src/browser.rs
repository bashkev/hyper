//! Browser: a web page in the pane Files and Changes use, beside the terminal. Rune draws the
//! address bar and leaves an empty element for the page ([`view`]), and a WKWebView ([`web`])
//! goes over it.
//!
//! Rune draws below the page, so the page hides while a dialog or toast is up.

mod address;
mod view;
mod web;

pub use view::{BACK, FORWARD, GLOBE, bar, pane};
pub use web::on_window;

use std::cell::Cell;

use rune::{TextEdit, Tx};
use serde_json::{Value, json};

use crate::project::Mode;
use crate::settings::Links;
use crate::{App, View};

#[derive(Default)]
pub struct Browser {
    pub address: TextEdit,
    /// What ⌘F looks for in the page, while its bar is up.
    find: TextEdit,
    finding: bool,
    /// Made when its element is first laid out.
    web: Option<web::WebView>,
    page: web::Page,
    error: Option<web::Failure>,
    /// Where to go once the page exists.
    pending: Option<String>,
    /// Whether the page is where Rune laid it out.
    placed: bool,
    /// Typing goes to the page once it's on screen, which may be a frame or two after the
    /// page is asked for. A hidden page would take the keyboard unseen.
    focus_page: Cell<bool>,
}

impl Browser {
    /// Where the page is, or is about to go.
    pub fn url(&self) -> &str {
        self.pending
            .as_deref()
            .or_else(|| self.error.as_ref().and_then(|e| e.url.as_deref()))
            .unwrap_or(&self.page.url)
    }

    /// Goes to `url`, now or once the page exists.
    fn load(&mut self, url: String) {
        self.error = None;
        match &self.web {
            Some(web) => self.error = web.load(&url).err(),
            None => self.pending = Some(url),
        }
    }

    /// Does `f` to the page, once there is one.
    fn with_page(&self, f: impl FnOnce(&web::WebView)) {
        if let Some(web) = &self.web {
            f(web);
        }
    }
}

impl App {
    pub fn browser_open(&self) -> bool {
        self.inspector.is_open() && self.project.mode == Mode::Browser
    }

    /// Whether the page's pane is on screen, with nothing of Rune's over it.
    fn browser_shown(&self) -> bool {
        self.view != View::Settings && self.browser_open()
    }

    /// Shows or hides the page to match the pane, and hides it while something Rune draws, like
    /// a dialog, would be `covered` by it. The view calls it: the page is native, so it doesn't
    /// come and go with Rune's elements.
    pub fn browser_sync(&self, covered: bool) {
        let b = &self.browser;
        if b.web.is_some() {
            web::set_page_keys(crate::commands::page_keys(&self.settings));
        }
        let shown = self.browser_shown() && !covered && b.placed && b.error.is_none() && !b.url().is_empty();
        b.with_page(|web| {
            web.set_shown(shown);
            if shown && b.focus_page.take() {
                web.focus();
            }
        });
    }

    /// Whether typing goes to the address bar or the page.
    pub fn browser_has_keyboard(&self) -> bool {
        let b = &self.browser;
        self.browser_shown() && (b.address.focused || b.find.focused || b.web.as_ref().is_some_and(web::WebView::has_keyboard))
    }

    /// Typing goes back to Rune, as a session or another view takes the screen.
    pub fn browser_release_keyboard(&mut self) {
        let b = &mut self.browser;
        b.address.focused = false;
        b.find.focused = false;
        b.focus_page.set(false);
        b.with_page(web::WebView::release_keyboard);
    }

    /// ⇧⌘L and the pane's button: shows or hides the page. One opened with nowhere to show yet
    /// is ready for an address.
    pub fn toggle_browser(&mut self) {
        self.toggle_project_panel(Mode::Browser);
        if self.browser_open() && self.browser.url().is_empty() {
            self.focus_address();
        }
    }

    /// ⌘W from the page: it goes, as a browser's tab would, and so does the pane. The next one
    /// starts empty.
    pub fn browser_close(&mut self) {
        // Before the page leaves the window, or the keyboard would leave with it.
        self.browser_release_keyboard();
        self.browser = Browser::default();
        self.inspector.set_open(false);
    }

    /// Opens a link clicked elsewhere in Hyper, where Settings says. A web page goes to the
    /// pane, and typing goes with it; anything else goes to macOS.
    pub fn open_link(&mut self, url: &str, tx: &mut Tx) {
        let web = url.starts_with("http://") || url.starts_with("https://");
        if self.settings.links == Links::Browser || !web {
            let _ = tx.open_url(url);
            return;
        }
        self.browser.address = TextEdit::new(url);
        self.browser.load(url.to_owned());
        self.browser.focus_page.set(true);
        if !self.browser_open() {
            self.toggle_project_panel(Mode::Browser);
        }
    }

    /// The pane, with its address bar ready for typing.
    pub fn open_location(&mut self) {
        if !self.browser_open() {
            self.toggle_project_panel(Mode::Browser);
        }
        self.focus_address();
    }

    fn focus_address(&mut self) {
        self.focus_browser_field(|b| &mut b.address);
    }

    /// One of the pane's fields takes the keyboard, with what's in it selected. Rune moves focus
    /// to a field only when no other field still asks for it, so the others let go.
    fn focus_browser_field(&mut self, field: fn(&mut Browser) -> &mut TextEdit) {
        let b = &mut self.browser;
        for other in [&mut self.ws.query, &mut self.ws.worktree_name, &mut self.ws.find, &mut self.search.field, &mut b.address, &mut b.find] {
            other.focused = false;
        }
        b.focus_page.set(false);
        b.with_page(web::WebView::release_keyboard);
        let edit = field(b);
        edit.focused = true;
        edit.set_selection(0, edit.text.len());
    }

    /// ⌘F: the find bar, ready for what to look for.
    pub fn browser_find(&mut self) {
        self.browser.finding = true;
        self.focus_browser_field(|b| &mut b.find);
    }

    /// The next match, or the one before it.
    pub fn browser_find_next(&self, backwards: bool) {
        let b = &self.browser;
        if !b.find.text.is_empty() {
            b.with_page(|web| web.find(&b.find.text, backwards, false));
        }
    }

    /// What to look for changed: the match from where the last one starts.
    pub fn browser_find_changed(&self) {
        let b = &self.browser;
        b.with_page(|web| if b.find.text.is_empty() { web.clear_found() } else { web.find(&b.find.text, false, true) });
    }

    /// Escape: the find bar goes, and typing goes back to the page.
    pub fn browser_close_find(&mut self, tx: &mut Tx) {
        let b = &mut self.browser;
        b.finding = false;
        b.find.focused = false;
        tx.blur();
        b.focus_page.set(true);
        b.with_page(web::WebView::clear_found);
    }

    /// Goes where `input` means, and typing goes to the page.
    pub fn browser_go(&mut self, input: &str) {
        let Some(url) = address::resolve(input) else { return };
        let b = &mut self.browser;
        b.address = TextEdit::new(&url);
        b.load(url);
        b.focus_page.set(true);
    }

    pub fn browser_back(&self) {
        self.browser.with_page(web::WebView::back);
    }

    pub fn browser_forward(&self) {
        self.browser.with_page(web::WebView::forward);
    }

    /// Reloads the page, or tries again where a load failed.
    pub fn browser_reload(&mut self) {
        let b = &mut self.browser;
        match b.error.take().and_then(|e| e.url) {
            Some(url) => b.load(url),
            None => b.with_page(web::WebView::reload),
        }
    }

    fn browser_laid_out(&mut self, frame: [f32; 4]) {
        if self.browser.web.is_none() {
            self.browser.web = web::WebView::new(self.wake());
        }
        let b = &mut self.browser;
        let Some(web) = &b.web else { return };
        web.place(frame);
        b.placed = true;
        if let Some(url) = b.pending.take() {
            b.load(url);
        }
    }

    /// Takes in what the page says changed. Says whether anything shown did.
    pub fn take_browser(&mut self) -> bool {
        let b = &mut self.browser;
        let Some(web) = &b.web else { return false };
        let (page, error) = (web.page(), web.error());
        if page == b.page && error == b.error {
            return false;
        }
        b.page = page;
        b.error = error;
        // What's being typed stays. A load that failed shows where it was going.
        if !b.address.focused && b.address.text != b.url() {
            b.address = TextEdit::new(b.url());
        }
        true
    }

    /// Escape in the address bar: what's typed goes, the address of the page comes back, and
    /// typing goes to the page.
    pub fn browser_address_escape(&mut self, tx: &mut Tx) {
        let b = &mut self.browser;
        b.address = TextEdit::new(b.url());
        tx.blur();
        b.focus_page.set(true);
    }

    pub fn browser_hand_off(&self) -> Value {
        json!({ "url": self.browser.url(), "open": self.browser_open() })
    }

    pub fn adopt_browser(&mut self, state: &Value) {
        if let Some(url) = state["url"].as_str().filter(|u| !u.is_empty()) {
            self.browser.address = TextEdit::new(url);
            self.browser.load(url.to_owned());
        }
        if state["open"].as_bool() == Some(true) {
            self.toggle_project_panel(Mode::Browser);
        }
    }
}
