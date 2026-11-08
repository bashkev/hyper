//! The page: a WKWebView inside Rune's view, over the element Rune leaves for it. Rune can't
//! draw a web page, so this native view sits on top of what Rune draws there.
//!
//! A delegate tells the app when the page's address, title, loading or history change, and when
//! a load fails. The app reads the rest from the view.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::OnceLock;

use block2::{Block, RcBlock};
use objc2::rc::Retained;
use objc2::declare::ClassBuilder;
use objc2::runtime::{AnyClass, AnyObject, Bool, NSObject, Sel};
use objc2::{ClassType, DeclaredClass, class, declare_class, msg_send, msg_send_id, mutability, sel};
use objc2_foundation::{NSError, NSPoint, NSRect, NSSize, NSString, NSURL};
use rune::{Shortcut, Waker, WindowHandle};

#[link(name = "WebKit", kind = "framework")]
unsafe extern "C" {}

/// What the toolbar shows follows these.
const OBSERVED: [&str; 5] = ["URL", "title", "loading", "canGoBack", "canGoForward"];
/// Schemes the page opens itself. Others (`mailto:`, an app's own) go to macOS.
const SCHEMES: [&str; 6] = ["http", "https", "about", "file", "data", "blob"];
/// Some sites serve a reduced page to a browser they don't recognize.
const USER_AGENT: &str = "Version/26.0 Safari/605.1.15";

thread_local! {
    static WINDOW: RefCell<Option<WindowHandle>> = const { RefCell::new(None) };
    static PAGE_KEYS: RefCell<Vec<Shortcut>> = const { RefCell::new(Vec::new()) };
}

/// Keeps the window, for the view the page goes into.
pub fn on_window(handle: &WindowHandle) {
    WINDOW.with_borrow_mut(|w| *w = Some(handle.clone()));
}

/// Rune's view: the window's content view, which is flipped, so its points are Rune's.
fn rune_view() -> Option<Retained<AnyObject>> {
    use rune::raw_window_handle::RawWindowHandle;
    let window = WINDOW.with_borrow(Clone::clone)?;
    window
        .with_raw_handle(|handle| {
            let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
            // SAFETY: on the UI thread, inside the callback that keeps the view alive.
            unsafe { Retained::retain(handle.ns_view.as_ptr().cast::<AnyObject>()) }
        })
        .ok()
        .flatten()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Page {
    pub url: String,
    pub title: String,
    pub loading: bool,
    pub back: bool,
    pub forward: bool,
    /// Whether the last find matched, once it's answered.
    pub found: Option<bool>,
}

/// A load that failed: why, and where it was going.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub message: String,
    pub url: Option<String>,
}

struct Ivars {
    wake: Waker,
    /// The last load that failed, until one commits.
    error: RefCell<Option<Failure>>,
    /// Where the app last sent the page, until a page commits: WebKit leaves the address out
    /// of some failures, like a port out of range.
    asked: RefCell<Option<String>>,
    found: Cell<Option<bool>>,
}

declare_class!(
    struct Delegate;

    // SAFETY: NSObject has no subclassing requirements, and Delegate doesn't implement Drop.
    unsafe impl ClassType for Delegate {
        type Super = NSObject;
        type Mutability = mutability::InteriorMutable;
        const NAME: &'static str = "HyperWebDelegate";
    }

    impl DeclaredClass for Delegate {
        type Ivars = Ivars;
    }

    // SAFETY: each signature matches the WebKit and KVO method it implements.
    unsafe impl Delegate {
        #[method(observeValueForKeyPath:ofObject:change:context:)]
        fn observe(&self, _path: *mut NSString, _object: *mut AnyObject, _change: *mut AnyObject, _context: *mut c_void) {
            (self.ivars().wake)();
        }

        #[method(webView:didCommitNavigation:)]
        fn committed(&self, _web: *mut AnyObject, _navigation: *mut AnyObject) {
            self.ivars().error.borrow_mut().take();
            self.ivars().asked.borrow_mut().take();
            self.ivars().found.set(None);
            (self.ivars().wake)();
        }

        #[method(webView:didFailProvisionalNavigation:withError:)]
        fn failed_to_start(&self, _web: *mut AnyObject, _navigation: *mut AnyObject, error: &NSError) {
            self.record(error);
        }

        #[method(webView:didFailNavigation:withError:)]
        fn failed_later(&self, _web: *mut AnyObject, _navigation: *mut AnyObject, error: &NSError) {
            self.record(error);
        }

        #[method(webView:decidePolicyForNavigationAction:decisionHandler:)]
        fn decide(&self, _web: *mut AnyObject, action: *mut AnyObject, decide: &Block<dyn Fn(isize)>) {
            const CANCEL: isize = 0;
            const ALLOW: isize = 1;
            // SAFETY: a WKNavigationAction's request and its URL, either of which may be nil.
            let url: *mut AnyObject = unsafe {
                let request: *mut AnyObject = msg_send![action, request];
                msg_send![request, URL]
            };
            let scheme = if url.is_null() { None } else { unsafe { string(msg_send_id![url, scheme]) } };
            if scheme.is_none_or(|s| SCHEMES.contains(&s.to_ascii_lowercase().as_str())) {
                return decide.call((ALLOW,));
            }
            // Another app's link opens only when it's clicked, so a page's script can't launch an
            // app on its own. WKNavigationTypeLinkActivated.
            const CLICKED: isize = 0;
            // SAFETY: a WKNavigationAction's type, and a live NSURL.
            unsafe {
                let kind: isize = msg_send![action, navigationType];
                if kind == CLICKED {
                    let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
                    let _: Bool = msg_send![workspace, openURL: url];
                }
            }
            decide.call((CANCEL,));
        }

        /// WebKit's content process ended (it crashed, or macOS reclaimed it): the page is blank
        /// until it loads again.
        #[method(webViewWebContentProcessDidTerminate:)]
        fn terminated(&self, web: *mut AnyObject) {
            // SAFETY: the WKWebView WebKit names.
            let _: *mut AnyObject = unsafe { msg_send![web, reload] };
        }

        /// A link meant for a new window, or `window.open`: it opens here, as there are no tabs.
        #[method(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:)]
        fn new_window(&self, web: *mut AnyObject, _config: *mut AnyObject, action: *mut AnyObject, _features: *mut AnyObject) -> *mut AnyObject {
            // SAFETY: the WKWebView and WKNavigationAction WebKit names.
            unsafe {
                let request: *mut AnyObject = msg_send![action, request];
                let _: *mut AnyObject = msg_send![web, loadRequest: request];
            }
            std::ptr::null_mut()
        }
    }
);

impl Delegate {
    fn new(wake: Waker) -> Retained<Delegate> {
        let this = Delegate::alloc().set_ivars(Ivars { wake, error: RefCell::new(None), asked: RefCell::new(None), found: Cell::new(None) });
        // SAFETY: NSObject's init.
        unsafe { msg_send_id![super(this), init] }
    }

    fn record(&self, error: &NSError) {
        // A load another one replaced, or a response WebKit handed elsewhere, isn't a failure.
        let (domain, code) = (error.domain().to_string(), error.code());
        if (domain == "NSURLErrorDomain" && code == -999) || (domain == "WebKitErrorDomain" && matches!(code, 102 | 204)) {
            return;
        }
        // SAFETY: an NSError's userInfo, where NSURL errors keep the address they failed at.
        let url = unsafe {
            let info: *mut AnyObject = msg_send![error, userInfo];
            let url: *mut AnyObject = msg_send![info, objectForKey: &*NSString::from_str("NSErrorFailingURLKey")];
            if url.is_null() {
                string(msg_send_id![info, objectForKey: &*NSString::from_str("NSErrorFailingURLStringKey")])
            } else {
                string(msg_send_id![url, absoluteString])
            }
        };
        let url = url.or_else(|| self.ivars().asked.borrow().clone());
        *self.ivars().error.borrow_mut() = Some(Failure { message: error.localizedDescription().to_string(), url });
        (self.ivars().wake)();
    }
}

/// WKWebView, with the app's menu first to see a shortcut, but for the page's own keys. WebKit
/// keeps every key equivalent it's offered for the page, and doesn't hand back one the page
/// leaves alone, so without the menu first ⌘W, ⌘1 or ⌘L would do nothing while the page has the
/// keyboard. Copy, Paste and Undo still reach the page: their menu items act on whatever has the
/// keyboard.
fn page_class() -> &'static AnyClass {
    static CLASS: OnceLock<&'static AnyClass> = OnceLock::new();
    CLASS.get_or_init(|| {
        extern "C" fn perform_key_equivalent(this: *mut AnyObject, _: Sel, event: *mut AnyObject) -> Bool {
            // SAFETY: an NSEvent's keys, AppKit's main menu and WKWebView's own method, on the
            // main thread.
            unsafe {
                let page_first = shortcut(event).is_some_and(|key| PAGE_KEYS.with_borrow(|keys| keys.contains(&key)));
                if !page_first {
                    let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
                    let menu: *mut AnyObject = msg_send![app, mainMenu];
                    if !menu.is_null() && msg_send![menu, performKeyEquivalent: event] {
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
    name.push_str(&key?.to_string().to_lowercase());
    name.parse().ok()
}

/// The shortcuts the page keeps while it has the keyboard, ahead of the app's menu.
pub fn set_page_keys(keys: Vec<Shortcut>) {
    PAGE_KEYS.with_borrow_mut(|current| *current = keys);
}

fn string(s: Option<Retained<NSString>>) -> Option<String> {
    s.map(|s| s.to_string())
}

pub struct WebView {
    view: Retained<AnyObject>,
    delegate: Retained<Delegate>,
}

impl WebView {
    /// A hidden page in the window's view. None before the window exists, and headless.
    pub fn new(wake: Waker) -> Option<WebView> {
        let parent = rune_view()?;
        let delegate = Delegate::new(wake);
        // SAFETY: WebKit's documented setup, on the main thread.
        let view = unsafe {
            let config: Retained<AnyObject> = msg_send_id![class!(WKWebViewConfiguration), new];
            let _: () = msg_send![&*config, setApplicationNameForUserAgent: &*NSString::from_str(USER_AGENT)];
            // As in Safari, a page plays sound only after a click: WKAudiovisualMediaTypeAudio.
            let _: () = msg_send![&*config, setMediaTypesRequiringUserActionForPlayback: 1usize];
            // Inspect Element in the page's context menu.
            let preferences: *mut AnyObject = msg_send![&*config, preferences];
            let yes: *mut AnyObject = msg_send![class!(NSNumber), numberWithBool: Bool::YES];
            let _: () = msg_send![preferences, setValue: yes, forKey: &*NSString::from_str("developerExtrasEnabled")];
            let view: Retained<AnyObject> =
                msg_send_id![msg_send_id![page_class(), alloc], initWithFrame: NSRect::ZERO, configuration: &*config];
            let _: () = msg_send![&*view, setAllowsBackForwardNavigationGestures: Bool::YES];
            let _: () = msg_send![&*view, setAllowsMagnification: Bool::YES];
            let _: () = msg_send![&*view, setHidden: Bool::YES];
            let _: () = msg_send![&*view, setNavigationDelegate: &*delegate];
            let _: () = msg_send![&*view, setUIDelegate: &*delegate];
            for key in OBSERVED {
                let _: () = msg_send![&*view, addObserver: &*delegate, forKeyPath: &*NSString::from_str(key), options: 0usize, context: std::ptr::null_mut::<c_void>()];
            }
            let _: () = msg_send![&*parent, addSubview: &*view];
            view
        };
        Some(WebView { view, delegate })
    }

    /// Puts the page at `[x, y, w, h]` in Rune's points.
    pub fn place(&self, [x, y, w, h]: [f32; 4]) {
        let frame = NSRect::new(NSPoint::new(x.into(), y.into()), NSSize::new(w.max(0.0).into(), h.max(0.0).into()));
        // SAFETY: a live NSView, on the main thread.
        unsafe {
            let current: NSRect = msg_send![&*self.view, frame];
            if current != frame {
                let _: () = msg_send![&*self.view, setFrame: frame];
            }
        }
    }

    pub fn set_shown(&self, shown: bool) {
        // SAFETY: a live NSView, on the main thread.
        unsafe {
            let hidden: bool = msg_send![&*self.view, isHidden];
            if hidden == shown {
                if !shown {
                    self.release_keyboard();
                }
                let _: () = msg_send![&*self.view, setHidden: Bool::new(!shown)];
            }
        }
    }

    pub fn load(&self, url: &str) -> Result<(), Failure> {
        self.delegate.ivars().error.borrow_mut().take();
        *self.delegate.ivars().asked.borrow_mut() = Some(url.to_owned());
        // SAFETY: WKWebView's loading methods with live arguments.
        unsafe {
            let Some(target) = NSURL::URLWithString(&NSString::from_str(url)) else {
                // Kept with WebKit's own failures, so it stays until a load commits.
                let failure = Failure { message: format!("{url} isn't an address"), url: Some(url.to_owned()) };
                *self.delegate.ivars().error.borrow_mut() = Some(failure.clone());
                return Err(failure);
            };
            if target.isFileURL() {
                // A local page may load what's beside it.
                let folder: Retained<NSURL> = msg_send_id![&*target, URLByDeletingLastPathComponent];
                let _: *mut AnyObject = msg_send![&*self.view, loadFileURL: &*target, allowingReadAccessToURL: &*folder];
            } else {
                let request: *mut AnyObject = msg_send![class!(NSURLRequest), requestWithURL: &*target];
                let _: *mut AnyObject = msg_send![&*self.view, loadRequest: request];
            }
        }
        Ok(())
    }

    pub fn back(&self) {
        let _: *mut AnyObject = unsafe { msg_send![&*self.view, goBack] };
    }

    pub fn forward(&self) {
        let _: *mut AnyObject = unsafe { msg_send![&*self.view, goForward] };
    }

    pub fn reload(&self) {
        self.delegate.ivars().error.borrow_mut().take();
        let _: *mut AnyObject = unsafe { msg_send![&*self.view, reload] };
    }

    pub fn stop(&self) {
        let _: () = unsafe { msg_send![&*self.view, stopLoading] };
    }

    pub fn page(&self) -> Page {
        // SAFETY: WKWebView's properties, any of which may be nil.
        unsafe {
            let url: Option<Retained<NSURL>> = msg_send_id![&*self.view, URL];
            Page {
                url: url.and_then(|u| string(msg_send_id![&*u, absoluteString])).unwrap_or_default(),
                title: string(msg_send_id![&*self.view, title]).unwrap_or_default(),
                loading: msg_send![&*self.view, isLoading],
                back: msg_send![&*self.view, canGoBack],
                forward: msg_send![&*self.view, canGoForward],
                found: self.delegate.ivars().found.get(),
            }
        }
    }

    /// Finds `text` in the page, selecting and scrolling to it, after the selection or before it
    /// going `backwards`, and wrapping around. `again` finds from the start of the selection, so
    /// a longer search can stay on the same match.
    pub fn find(&self, text: &str, backwards: bool, again: bool) {
        let delegate = self.delegate.clone();
        let done = RcBlock::new(move |result: *mut AnyObject| {
            // SAFETY: the WKFindResult WebKit answers with, on the main thread.
            let found: bool = unsafe { msg_send![result, matchFound] };
            delegate.ivars().found.set(Some(found));
            (delegate.ivars().wake)();
        });
        // SAFETY: WKWebView's find (macOS 11) and script, with live arguments, in order.
        unsafe {
            if again {
                let collapse = NSString::from_str("var s = getSelection(); if (s.rangeCount) s.collapseToStart()");
                let none = std::ptr::null::<Block<dyn Fn(*mut AnyObject, *mut AnyObject)>>();
                let _: () = msg_send![&*self.view, evaluateJavaScript: &*collapse, completionHandler: none];
            }
            let config: Retained<AnyObject> = msg_send_id![class!(WKFindConfiguration), new];
            let _: () = msg_send![&*config, setBackwards: Bool::new(backwards)];
            let _: () = msg_send![&*self.view, findString: &*NSString::from_str(text), withConfiguration: &*config, completionHandler: &*done];
        }
    }

    /// Forgets the last find's answer.
    pub fn clear_found(&self) {
        self.delegate.ivars().found.set(None);
    }

    /// Why the last load failed, until another one commits.
    pub fn error(&self) -> Option<Failure> {
        self.delegate.ivars().error.borrow().clone()
    }

    fn window(&self) -> *mut AnyObject {
        unsafe { msg_send![&*self.view, window] }
    }

    /// Typing goes to the page. Only one on screen: a hidden one would take the keyboard unseen.
    pub fn focus(&self) {
        let window = self.window();
        if !window.is_null() {
            let _: bool = unsafe { msg_send![window, makeFirstResponder: &*self.view] };
        }
    }

    /// Whether the page, or a field in it, has the keyboard.
    pub fn has_keyboard(&self) -> bool {
        let window = self.window();
        if window.is_null() {
            return false;
        }
        // SAFETY: the window's first responder, which may be the window itself or nil.
        unsafe {
            let responder: *mut AnyObject = msg_send![window, firstResponder];
            !responder.is_null()
                && msg_send![responder, isKindOfClass: class!(NSView)]
                && msg_send![responder, isDescendantOf: &*self.view]
        }
    }

    /// Gives the keyboard back to Rune, which has it whenever the page doesn't.
    pub fn release_keyboard(&self) {
        if !self.has_keyboard() {
            return;
        }
        // SAFETY: the page's superview is Rune's view, in the same window.
        unsafe {
            let parent: *mut AnyObject = msg_send![&*self.view, superview];
            let _: bool = msg_send![self.window(), makeFirstResponder: parent];
        }
    }
}

impl Drop for WebView {
    fn drop(&mut self) {
        // SAFETY: undoes `new`, on the main thread.
        unsafe {
            for key in OBSERVED {
                let _: () = msg_send![&*self.view, removeObserver: &*self.delegate, forKeyPath: &*NSString::from_str(key)];
            }
            let _: () = msg_send![&*self.view, setNavigationDelegate: std::ptr::null::<AnyObject>()];
            let _: () = msg_send![&*self.view, setUIDelegate: std::ptr::null::<AnyObject>()];
            let _: () = msg_send![&*self.view, stopLoading];
            let _: () = msg_send![&*self.view, removeFromSuperview];
        }
    }
}
