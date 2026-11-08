//! The address bar, where Files and Changes have their tabs, and the page's place below it.

use rune::{El, Icon, Key, col, row, text};
use rune_kit::{Button, ButtonVariant, Kit, TextInput, glyphs};

use super::web::WebView;
use crate::App;
use crate::commands::Action;
use crate::ui::{EXTERNAL, keys, pane_button};

pub static GLOBE: Icon = Icon {
    name: "browser-globe",
    view: 16.0,
    body: r#"<circle cx="8" cy="8" r="6" stroke="currentColor" stroke-width="1.1" fill="none"/><path d="M2 8H14 M8 2C6.2 4 6.2 12 8 14 M8 2C9.8 4 9.8 12 8 14" stroke="currentColor" stroke-width="1.1" fill="none"/>"#,
};
pub static BACK: Icon = Icon {
    name: "browser-back",
    view: 16.0,
    body: r#"<path d="M10 3L5 8L10 13" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};
pub static FORWARD: Icon = Icon {
    name: "browser-forward",
    view: 16.0,
    body: r#"<path d="M6 3L11 8L6 13" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};
static RELOAD: Icon = Icon {
    name: "browser-reload",
    view: 16.0,
    body: r#"<path d="M13 8A5 5 0 1 1 11.5 4.5 M11.5 1.5V4.5H8.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

/// Back, forward, reload or stop, and the address.
pub fn bar(s: &App, k: &Kit) -> El {
    let b = &s.browser;
    let field = TextInput::new(&b.address)
        .width(None)
        .placeholder("Address")
        .label("Address")
        .on_edit(|s: &mut App| &mut s.browser.address)
        .on_key(|s: &mut App, ev, tx| {
            let escape = ev.key == Key::Escape;
            if escape {
                s.browser_address_escape(tx);
            }
            escape
        })
        .on_submit(|s: &mut App, tx| {
            let input = s.browser.address.text.clone();
            tx.blur();
            s.browser_go(&input);
        })
        .build(k)
        .key("browser-address")
        .grow(1.0)
        .min_w(0.0)
        .help(if b.page.title.is_empty() { b.url().to_owned() } else { b.page.title.clone() });
    let reload = if b.page.loading {
        pane_button(&glyphs::CLOSE, "Stop", "", "browser-stop", true, k, |s, _| s.browser.with_page(WebView::stop))
    } else {
        let reloadable = b.error.is_some() || !b.page.url.is_empty();
        // ⌘R reloads the page only while the browser has the keyboard.
        let shortcut = if s.browser_has_keyboard() { Action::Reload.hint(&s.settings) } else { String::new() };
        pane_button(&RELOAD, "Reload page", &shortcut, "browser-reload", reloadable, k, |s, _| s.browser_reload())
    };
    row()
        .h(32.0)
        .gap(2.0)
        .center_y()
        .window_drag()
        .child(pane_button(&BACK, "Back", "", "browser-back", b.page.back, k, |s, _| s.browser_back()))
        .child(pane_button(&FORWARD, "Forward", "", "browser-forward", b.page.forward, k, |s, _| s.browser_forward()))
        .child(reload)
        .child(field)
}

/// What to find, red when the page hasn't got it, and the way to the next and previous match.
fn find_bar(s: &App, k: &Kit) -> El {
    let b = &s.browser;
    let field = TextInput::new(&b.find)
        .width(None)
        .placeholder("Find")
        .label("Find in page")
        .leading_icon(&glyphs::SEARCH)
        .invalid(b.page.found == Some(false))
        .on_edit(|s: &mut App| &mut s.browser.find)
        .on_change(|s: &mut App, _| s.browser_find_changed())
        .on_key(|s: &mut App, ev, tx| match ev.key {
            Key::Escape => {
                s.browser_close_find(tx);
                true
            }
            Key::Enter if ev.mods.shift => {
                s.browser_find_next(true);
                true
            }
            _ => false,
        })
        .on_submit(|s: &mut App, _| s.browser_find_next(false))
        .build(k)
        .key("browser-find")
        .grow(1.0)
        .min_w(0.0);
    row()
        .grow(1.0)
        .min_w(0.0)
        .gap(2.0)
        .center_y()
        .child(field)
        .child(pane_button(&glyphs::CHEVRON_UP, "Previous match", keys(b.find.focused, "shift+enter"), "browser-find-previous", true, k, |s, _| s.browser_find_next(true)))
        .child(pane_button(&glyphs::CHEVRON_DOWN, "Next match", keys(b.find.focused, "enter"), "browser-find-next", true, k, |s, _| s.browser_find_next(false)))
        .child(pane_button(&glyphs::CLOSE, "Close find", keys(b.find.focused, "escape"), "browser-find-close", true, k, App::browser_close_find))
}

/// The page's title and controls, then the element the page goes over, or why it failed.
pub fn pane(s: &App, k: &Kit) -> El {
    let b = &s.browser;
    let url = b.url().to_owned();
    let lead = if b.finding {
        find_bar(s, k)
    } else {
        let title = if b.error.is_some() { String::new() } else { b.page.title.clone() };
        text(title).font(k.ty.small).color(k.c.text_muted).grow(1.0).min_w(0.0).truncate()
    };
    let tools = row()
        .w_full()
        .h(36.0)
        .px(10.0)
        .gap(4.0)
        .center_y()
        .child(lead)
        .children((!url.is_empty()).then(|| {
            pane_button(&EXTERNAL, "Open in Default Browser", "", "browser-external", true, k, move |_, tx| {
                let _ = tx.open_url(&url);
            })
        }))
        .child(crate::project::expand_control(s, k));
    let mut page = col().key("browser-page").w_full().grow(1.0).min_h(0.0).on_layout(|s: &mut App, frame| s.browser_laid_out(frame));
    if let Some(e) = &b.error {
        page = page.center().gap(12.0).child(text(e.message.clone()).font(k.ty.ui).color(k.c.text_muted)).child(
            Button::new("Try Again").variant(ButtonVariant::Ghost).on_press(|s: &mut App| s.browser_reload()).build(k).key("browser-retry"),
        );
    }
    col().key("browser-pane").w_full().h_full().min_h(0.0).fill(k.c.bg_base).child(tools).child(page)
}
