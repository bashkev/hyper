//! The page's view: WKWebView, made to hand the app's shortcuts and the mouse's side buttons to
//! the app rather than keep them for the page.

use std::cell::RefCell;
use std::sync::OnceLock;

use objc2::declare::ClassBuilder;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
use objc2::{class, msg_send, msg_send_id, sel};
use objc2_foundation::NSString;
use rune::Shortcut;

use crate::commands;
use crate::history::Way;
use crate::settings::Settings;

#[derive(Default)]
struct PageKeys {
    editing: Vec<Shortcut>,
    history: Vec<(Shortcut, Way)>,
}

thread_local! {
    static PAGE_KEYS: RefCell<PageKeys> = RefCell::new(PageKeys::default());
}

/// WKWebView, with the app's menu first to see a shortcut, but for the page's own keys. WebKit
/// keeps every key equivalent it's offered for the page, and doesn't hand back one the page
/// leaves alone, so without the menu first ⌘W, ⌘1 or ⌘L would do nothing while the page has the
/// keyboard. Copy, Paste and Undo still reach the page: their menu items act on whatever has the
/// keyboard.
pub fn page_class() -> &'static AnyClass {
    static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();
    CLASS.get_or_init(|| {
        extern "C" fn perform_key_equivalent(this: *mut AnyObject, _: Sel, event: *mut AnyObject) -> Bool {
            // SAFETY: an NSEvent's keys, AppKit's main menu and WKWebView's own method, on the
            // main thread.
            unsafe {
                let key = shortcut(event);
                let page_first = key.as_ref().is_some_and(|key| PAGE_KEYS.with_borrow(|keys| keys.editing.contains(key)));
                if !page_first {
                    let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
                    let menu: *mut AnyObject = msg_send![app, mainMenu];
                    if !menu.is_null() && msg_send![menu, performKeyEquivalent: event] {
                        return Bool::YES;
                    }
                    if let Some(key) = key
                        && let Some(way) = PAGE_KEYS.with_borrow(|keys| {
                            keys.history.iter().find_map(|(binding, way)| (*binding == key).then_some(*way))
                        })
                        && page_has_keyboard(&*this)
                        && {
                            // WebKit only provides a text input context for editable content.
                            let context: *mut AnyObject = msg_send![this, inputContext];
                            context.is_null()
                        }
                    {
                        match way {
                            Way::Back => { let _: *mut AnyObject = msg_send![this, goBack]; }
                            Way::Forward => { let _: *mut AnyObject = msg_send![this, goForward]; }
                        }
                        return Bool::YES;
                    }
                }
                msg_send![super(&*this, class!(WKWebView)), performKeyEquivalent: event]
            }
        }
        // The mouse's side buttons go back and forward in the page, as in Safari.
        extern "C" fn other_mouse_up(this: *mut AnyObject, _: Sel, event: *mut AnyObject) {
            // SAFETY: an NSEvent for a mouse button, and WKWebView's own methods, on the main
            // thread.
            unsafe {
                let button: isize = msg_send![event, buttonNumber];
                match button {
                    3 => { let _: *mut AnyObject = msg_send![this, goBack]; }
                    4 => { let _: *mut AnyObject = msg_send![this, goForward]; }
                    _ => { let _: () = msg_send![super(&*this, class!(WKWebView)), otherMouseUp: event]; }
                }
            }
        }
        let mut builder = ClassBuilder::new("HyperWebView", class!(WKWebView)).expect("HyperWebView is declared once");
        // SAFETY: the signatures match -[NSView performKeyEquivalent:] and -[NSResponder otherMouseUp:].
        unsafe {
            builder.add_method(
                sel!(performKeyEquivalent:),
                perform_key_equivalent as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject) -> Bool,
            );
            builder.add_method(sel!(otherMouseUp:), other_mouse_up as extern "C" fn(*mut AnyObject, Sel, *mut AnyObject));
        }
        builder.register()
    })
}

/// The shortcut a key event is, as Rune names it.
///
/// # Safety
/// `event` is a live NSEvent for a key.
unsafe fn shortcut(event: *mut AnyObject) -> Option<Shortcut> {
    const SHIFT: usize = 1 << 17;
    const CONTROL: usize = 1 << 18;
    const OPTION: usize = 1 << 19;
    const COMMAND: usize = 1 << 20;
    let (key, flags): (Option<Retained<NSString>>, usize) = unsafe { (msg_send_id![event, charactersIgnoringModifiers], msg_send![event, modifierFlags]) };
    let mut name: String = [(CONTROL, "ctrl+"), (OPTION, "alt+"), (SHIFT, "shift+"), (COMMAND, "cmd+")]
        .into_iter()
        .filter(|(bit, _)| flags & bit != 0)
        .map(|(_, part)| part)
        .collect();
    let key = key?.to_string().to_lowercase();
    name.push_str(match key.as_str() {
        "\u{f702}" => "left",
        "\u{f703}" => "right",
        _ => &key,
    });
    name.parse().ok()
}

/// Whether the page, or a field in it, has the keyboard.
pub(super) fn page_has_keyboard(view: &AnyObject) -> bool {
    // SAFETY: the WKWebView and its window are on the main thread.
    unsafe {
        let window: *mut AnyObject = msg_send![view, window];
        if window.is_null() {
            return false;
        }
        let responder: *mut AnyObject = msg_send![window, firstResponder];
        !responder.is_null()
            && msg_send![responder, isKindOfClass: class!(NSView)]
            && msg_send![responder, isDescendantOf: view]
    }
}

pub fn set_page_keys(settings: &Settings) {
    let editing = commands::page_keys(settings);
    let history = commands::browser_history_aliases(settings).collect();
    PAGE_KEYS.with_borrow_mut(|current| *current = PageKeys { editing, history });
}
