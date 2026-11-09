//! The page: a WKWebView inside Rune's view, over the element Rune leaves for it. Rune can't
//! draw a web page, so this native view sits on top of what Rune draws there.
//!
//! A delegate tells the app when the page's address, title, loading or history change, when a
//! load fails, and when the page opens a tab or closes its own. The app reads the rest from the
//! view.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use block2::{Block, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject};
use objc2::{ClassType, DeclaredClass, class, declare_class, msg_send, msg_send_id, mutability};
use objc2_foundation::{NSError, NSPoint, NSRect, NSSize, NSString, NSURL};
use rune::{Waker, WindowHandle};

use super::keys::page_class;

#[link(name = "WebKit", kind = "framework")]
unsafe extern "C" {}

/// What the toolbar shows follows these.
const OBSERVED: [&str; 5] = ["URL", "title", "loading", "canGoBack", "canGoForward"];
/// Schemes the page opens itself. Others (`mailto:`, an app's own) go to macOS.
const SCHEMES: [&str; 6] = ["http", "https", "about", "file", "data", "blob"];
/// Some sites serve a reduced page to a browser they don't recognize.
const USER_AGENT: &str = "Version/26.0 Safari/605.1.15";
// WKNavigationTypeLinkActivated.
const CLICKED: isize = 0;

thread_local! {
    static WINDOW: RefCell<Option<WindowHandle>> = const { RefCell::new(None) };
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

/// A tab the page opened.
pub enum Opened {
    /// A link meant for a new window, or `window.open`: shown, in a page WebKit made from this
    /// one's configuration, so the new page's `window.opener` still reaches this one, as a
    /// sign-in popup needs.
    Shown(WebView),
    /// A link ⌘-clicked or middle-clicked, to load behind this one.
    Behind(WebView),
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
    /// Tabs the page opened, until the app takes them.
    opened: RefCell<Vec<Opened>>,
    /// Whether the page's script closed it.
    closed: Cell<bool>,
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
        fn decide(&self, web: *mut AnyObject, action: *mut AnyObject, decide: &Block<dyn Fn(isize)>) {
            const CANCEL: isize = 0;
            const ALLOW: isize = 1;
            // SAFETY: a WKNavigationAction's request, URL and target frame, which may be nil.
            let (request, url, target, kind): (*mut AnyObject, *mut AnyObject, *mut AnyObject, isize) = unsafe {
                let request: *mut AnyObject = msg_send![action, request];
                (request, msg_send![request, URL], msg_send![action, targetFrame], msg_send![action, navigationType])
            };
            // SAFETY: a live NSURL.
            let scheme = if url.is_null() { None } else { unsafe { string(msg_send_id![url, scheme]) }.map(|s| s.to_ascii_lowercase()) };
            if scheme.as_deref().is_none_or(|s| SCHEMES.contains(&s)) {
                // A nil target already goes through the new-window delegate with its request intact.
                // A blob that still targets this frame needs to stay in its originating document.
                if !target.is_null() && !url.is_null() && scheme.as_deref() != Some("blob")
                    // SAFETY: the navigation action is live for the duration of this callback.
                    && unsafe { opens_behind(action, kind) }
                    && let Some(parent) = rune_view()
                {
                    // SAFETY: use the source view's configuration and the original NSURLRequest.
                    let page = unsafe {
                        let config: Retained<AnyObject> = msg_send_id![web, configuration];
                        let page = WebView::with_configuration(&parent, &config, self.ivars().wake.clone());
                        *page.delegate.ivars().asked.borrow_mut() = string(msg_send_id![url, absoluteString]);
                        let _: *mut AnyObject = msg_send![&*page.view, loadRequest: request];
                        page
                    };
                    self.ivars().opened.borrow_mut().push(Opened::Behind(page));
                    (self.ivars().wake)();
                    return decide.call((CANCEL,));
                }
                return decide.call((ALLOW,));
            }
            // Another app's link opens only when it's clicked, so a page's script can't launch an
            // app on its own.
            if kind == CLICKED {
                // SAFETY: NSWorkspace with a live NSURL.
                unsafe {
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

        /// A link meant for a new window, or `window.open`: it opens in a tab. WebKit loads it
        /// in the page returned, which has to be made from `config`.
        #[method(webView:createWebViewWithConfiguration:forNavigationAction:windowFeatures:)]
        fn new_window(&self, _web: *mut AnyObject, config: *mut AnyObject, action: *mut AnyObject, _features: *mut AnyObject) -> *mut AnyObject {
            // SAFETY: a WKNavigationAction's navigation type, modifiers and button.
            let behind = unsafe {
                opens_behind(action, msg_send![action, navigationType])
            };
            // SAFETY: the WKWebViewConfiguration WebKit passes, which it keeps alive.
            let Some(config) = (unsafe { Retained::retain(config) }) else { return std::ptr::null_mut() };
            let Some(parent) = rune_view() else { return std::ptr::null_mut() };
            let page = WebView::with_configuration(&parent, &config, self.ivars().wake.clone());
            // The tab keeps the page, so WebKit gets it unretained, as it expects.
            let view = Retained::as_ptr(&page.view).cast_mut();
            self.ivars().opened.borrow_mut().push(if behind { Opened::Behind(page) } else { Opened::Shown(page) });
            (self.ivars().wake)();
            view
        }

        /// The page's script closed it, as a sign-in popup does when it's done.
        #[method(webViewDidClose:)]
        fn did_close(&self, _web: *mut AnyObject) {
            self.ivars().closed.set(true);
            (self.ivars().wake)();
        }
    }
);

/// Whether a link click asks for a background tab. `action` must be a live WKNavigationAction.
unsafe fn opens_behind(action: *mut AnyObject, kind: isize) -> bool {
    const COMMAND: usize = 1 << 20;
    // WebKit numbers the middle button as NSEvent's button mask does.
    const MIDDLE: isize = 1 << 2;
    // SAFETY: the action is live and the properties return NSEvent flags and a button number.
    let (flags, button): (usize, isize) = unsafe { (msg_send![action, modifierFlags], msg_send![action, buttonNumber]) };
    kind == CLICKED && (flags & COMMAND != 0 || button == MIDDLE)
}

impl Delegate {
    fn new(wake: Waker) -> Retained<Delegate> {
        let this = Delegate::alloc().set_ivars(Ivars {
            wake,
            error: RefCell::new(None),
            asked: RefCell::new(None),
            found: Cell::new(None),
            opened: RefCell::new(Vec::new()),
            closed: Cell::new(false),
        });
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
        // SAFETY: WebKit's documented setup, on the main thread.
        let config = unsafe {
            let config: Retained<AnyObject> = msg_send_id![class!(WKWebViewConfiguration), new];
            let _: () = msg_send![&*config, setApplicationNameForUserAgent: &*NSString::from_str(USER_AGENT)];
            // As in Safari, a page plays sound only after a click: WKAudiovisualMediaTypeAudio.
            let _: () = msg_send![&*config, setMediaTypesRequiringUserActionForPlayback: 1usize];
            // Inspect Element in the page's context menu.
            let preferences: *mut AnyObject = msg_send![&*config, preferences];
            let yes: *mut AnyObject = msg_send![class!(NSNumber), numberWithBool: Bool::YES];
            let _: () = msg_send![preferences, setValue: yes, forKey: &*NSString::from_str("developerExtrasEnabled")];
            // Keep blob navigation in WebKit so it can carry the source document's blob context.
            let world: *mut AnyObject = msg_send![class!(WKContentWorld), defaultClientWorld];
            let source = NSString::from_str(include_str!("blob_links.js"));
            let script: Retained<AnyObject> = msg_send_id![msg_send_id![class!(WKUserScript), alloc],
                initWithSource: &*source, injectionTime: 0isize, forMainFrameOnly: Bool::NO, inContentWorld: world];
            let controller: *mut AnyObject = msg_send![&*config, userContentController];
            let _: () = msg_send![controller, addUserScript: &*script];
            config
        };
        Some(WebView::with_configuration(&parent, &config, wake))
    }

    /// A hidden page in `parent` made from `config`: [`WebView::new`]'s, or the one WebKit gives
    /// a page another opened.
    fn with_configuration(parent: &AnyObject, config: &AnyObject, wake: Waker) -> WebView {
        let delegate = Delegate::new(wake);
        // SAFETY: WebKit's documented setup, on the main thread.
        let view = unsafe {
            let view: Retained<AnyObject> =
                msg_send_id![msg_send_id![page_class(), alloc], initWithFrame: NSRect::ZERO, configuration: config];
            let _: () = msg_send![&*view, setAllowsBackForwardNavigationGestures: Bool::YES];
            let _: () = msg_send![&*view, setAllowsMagnification: Bool::YES];
            let _: () = msg_send![&*view, setHidden: Bool::YES];
            let _: () = msg_send![&*view, setNavigationDelegate: &*delegate];
            let _: () = msg_send![&*view, setUIDelegate: &*delegate];
            for key in OBSERVED {
                let _: () = msg_send![&*view, addObserver: &*delegate, forKeyPath: &*NSString::from_str(key), options: 0usize, context: std::ptr::null_mut::<c_void>()];
            }
            let _: () = msg_send![parent, addSubview: &*view];
            view
        };
        WebView { view, delegate }
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
                url: url.and_then(|u| string(msg_send_id![&*u, absoluteString]))
                    .or_else(|| self.delegate.ivars().asked.borrow().clone()).unwrap_or_default(),
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

    /// The tabs the page opened since it was last asked.
    pub fn take_opened(&self) -> Vec<Opened> {
        self.delegate.ivars().opened.take()
    }

    /// Whether the page's script closed it.
    pub fn closed(&self) -> bool {
        self.delegate.ivars().closed.get()
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
        super::keys::page_has_keyboard(&self.view)
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
