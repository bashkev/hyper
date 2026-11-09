//! Browser: web pages in tabs ([`tabs`]), in the pane Files and Changes use, beside the
//! terminal. Rune draws the address bar and the tabs and leaves an empty element for the page
//! ([`view`]), and the shown tab's WKWebView ([`web`]) goes over it.
//!
//! Rune draws below the page, so the page hides while a dialog or toast is up.

mod address;
mod keys;
mod tabs;
mod view;
mod web;

pub use tabs::CloseTab;
pub use view::{BACK, FORWARD, GLOBE, pane, tab_bar};
pub use web::on_window;
use keys::set_page_keys;

use std::cell::Cell;

use rune::{TextEdit, Tx};
use crate::project::Mode;
use crate::settings::Links;
use crate::{App, View};

pub struct Browser {
    /// Never empty: a browser with nowhere to go has one empty tab.
    tabs: Vec<tabs::Tab>,
    /// The tab shown.
    active: usize,
    /// Recently closed pages, with their former positions, newest last.
    closed_tabs: Vec<(usize, String)>,
    /// What ⌘F looks for in the shown page, while its bar is up.
    find: TextEdit,
    finding: bool,
    /// Where Rune laid out the page's element, once it has.
    frame: Option<[f32; 4]>,
    /// How wide Rune laid out the tab strip, which the tabs share. 0 until it has.
    strip: f32,
    reveal_restored_tab: bool,
    /// Typing goes to the page once it's on screen, which may be a frame or two after the
    /// page is asked for. A hidden page would take the keyboard unseen.
    focus_page: Cell<bool>,
}

impl Default for Browser {
    fn default() -> Browser {
        Browser {
            tabs: vec![tabs::Tab::default()],
            active: 0,
            closed_tabs: Vec::new(),
            find: TextEdit::default(),
            finding: false,
            frame: None,
            strip: 0.0,
            reveal_restored_tab: false,
            focus_page: Cell::new(false),
        }
    }
}

impl Browser {
    /// Where the shown page is, or is about to go.
    pub fn url(&self) -> &str {
        self.tab().url()
    }

    /// Does `f` to the shown page, once there is one.
    fn with_page(&self, f: impl FnOnce(&web::WebView)) {
        self.tab().with_page(f);
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
        if b.tabs.iter().any(|t| t.web.is_some()) {
            set_page_keys(&self.settings);
        }
        let shown = self.browser_shown() && !covered && b.frame.is_some();
        // The others hide first, so the keyboard they let go of can go to the one shown.
        for (i, tab) in b.tabs.iter().enumerate() {
            if i != b.active {
                tab.with_page(|web| web.set_shown(false));
            }
        }
        let tab = b.tab();
        let shown = shown && tab.error.is_none() && !tab.url().is_empty();
        tab.with_page(|web| {
            web.set_shown(shown);
            if shown && b.focus_page.take() {
                web.focus();
            }
        });
    }

    /// Whether typing goes to the address bar or the page.
    pub fn browser_has_keyboard(&self) -> bool {
        let b = &self.browser;
        let page = b.tab().web.as_ref().is_some_and(web::WebView::has_keyboard);
        self.browser_shown() && (b.tab().address.focused || b.find.focused || page)
    }

    /// Typing goes back to Rune, as a session or another view takes the screen.
    pub fn browser_release_keyboard(&mut self) {
        self.browser_release_fields();
        let b = &mut self.browser;
        b.focus_page.set(false);
        for tab in &b.tabs {
            tab.with_page(web::WebView::release_keyboard);
        }
    }

    /// The address and find bars let go of the keyboard.
    fn browser_release_fields(&mut self) {
        let b = &mut self.browser;
        for tab in &mut b.tabs {
            tab.address.focused = false;
        }
        b.find.focused = false;
    }

    /// Typing goes to the shown page, once it's on screen.
    fn browser_focus_page(&mut self, tx: &mut Tx) {
        self.browser_release_fields();
        tx.blur();
        self.browser.focus_page.set(true);
    }

    /// ⇧⌘L and the pane's button: shows or hides the page. One opened with nowhere to show yet
    /// is ready for an address.
    pub fn toggle_browser(&mut self) {
        self.toggle_project_panel(Mode::Browser);
        if self.browser_open() && self.browser.tab().is_empty() {
            self.focus_address();
        }
    }

    /// Opens a link clicked elsewhere in Hyper, where Settings says. A web page goes to a tab in
    /// the pane, and typing goes with it; anything else goes to macOS.
    pub fn open_link(&mut self, url: &str, tx: &mut Tx) {
        let web = url.starts_with("http://") || url.starts_with("https://");
        if self.settings.links == Links::Browser || !web {
            let _ = tx.open_url(url);
            return;
        }
        self.browser_open_tab(url, tx);
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
        self.focus_browser_field(|b| &mut b.tab_mut().address);
    }

    /// One of the pane's fields takes the keyboard, with what's in it selected. Rune moves focus
    /// to a field only when no other field still asks for it, so the others let go.
    fn focus_browser_field(&mut self, field: fn(&mut Browser) -> &mut TextEdit) {
        for other in [&mut self.ws.query, &mut self.ws.worktree_name, &mut self.ws.find, &mut self.search.field] {
            other.focused = false;
        }
        self.browser_release_fields();
        let b = &mut self.browser;
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
        let tab = self.browser.tab_mut();
        tab.address = TextEdit::new(&url);
        tab.load(url);
        self.browser.focus_page.set(true);
    }

    pub fn browser_back(&self) {
        self.browser.with_page(web::WebView::back);
    }

    pub fn browser_forward(&self) {
        self.browser.with_page(web::WebView::forward);
    }

    /// Reloads the page, or tries again where a load failed.
    pub fn browser_reload(&mut self) {
        let tab = self.browser.tab_mut();
        match tab.error.take().and_then(|e| e.url) {
            Some(url) => tab.load(url),
            None => tab.with_page(web::WebView::reload),
        }
    }

    fn browser_laid_out(&mut self, frame: [f32; 4]) {
        self.browser.frame = Some(frame);
        self.browser_place(self.browser.active);
    }

    /// Escape in the address bar: what's typed goes, the address of the page comes back, and
    /// typing goes to the page.
    pub fn browser_address_escape(&mut self, tx: &mut Tx) {
        let tab = self.browser.tab_mut();
        tab.address = TextEdit::new(tab.url());
        tx.blur();
        self.browser.focus_page.set(true);
    }
}
