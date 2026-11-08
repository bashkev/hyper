//! Telling the person an agent needs them while they're elsewhere: a notification with a sound,
//! which a click turns into that agent's terminal, and a count on the Dock icon.
//!
//! rune-mac's notifications are silent, so they're posted here with the default sound, and
//! still clicked through its delegate. macOS only shows them from a bundled app; the badge
//! works either way.

use std::sync::Mutex;
use std::sync::OnceLock;

use block2::{Block, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send, msg_send_id};
use rune::Waker;
use rune_mac::notify;

#[link(name = "UserNotifications", kind = "framework")]
unsafe extern "C" {}

// UNAuthorizationOptionBadge | Sound | Alert. macOS shows a Dock badge only with the first.
const AUTH: usize = (1 << 0) | (1 << 1) | (1 << 2);
const LIVE: &str = "live";
/// Which run of the app posted it: a terminal's id means nothing to the next one.
const RUN: &str = "run";

static CLICKED: Mutex<Option<u64>> = Mutex::new(None);
static WAKE: OnceLock<Waker> = OnceLock::new();

fn run() -> String {
    std::process::id().to_string()
}

/// Hears clicks from now on, including one that launched the app.
pub fn listen(wake: Waker) {
    if WAKE.set(wake).is_err() {
        return;
    }
    notify::on_click(|click| {
        if click.get(RUN) == Some(run().as_str()) {
            *CLICKED.lock().unwrap() = click.get(LIVE).and_then(|id| id.parse().ok());
        }
        if let Some(wake) = WAKE.get() {
            wake();
        }
    });
}

/// The terminal whose notification was clicked since the last look.
pub fn clicked() -> Option<u64> {
    CLICKED.lock().unwrap().take()
}

/// Whether the app is in front, where a notification would only be noise.
pub fn app_focused() -> bool {
    // AppKit can't be asked from a test's thread.
    !cfg!(test) && notify::app_focused()
}

pub fn bring_to_front() {
    notify::bring_to_front();
}

fn key(live: u64) -> String {
    format!("live-{live}")
}

/// Asks the person, once, to allow notifications and the badge. Asked before anything needs
/// them, so the first notification isn't lost while the question is open.
pub fn ask() {
    static ASKED: std::sync::Once = std::sync::Once::new();
    if cfg!(test) || !notify::available() {
        return;
    }
    ASKED.call_once(|| authorize(|_| {}));
}

fn authorize(then: impl Fn(bool) + 'static) {
    let done = RcBlock::new(move |granted: Bool, _error: *mut AnyObject| then(granted.as_bool()));
    unsafe {
        let _: () = msg_send![center(), requestAuthorizationWithOptions: AUTH, completionHandler: &*done];
    }
}

#[cfg(test)]
thread_local! {
    /// What this test thread posted: the terminal, the words and whether it sounded.
    pub static POSTED: std::cell::RefCell<Vec<(u64, String, bool)>> = const { std::cell::RefCell::new(Vec::new()) };
    /// The terminals whose notifications this test thread took back.
    pub static REMOVED: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Posts, or replaces, the notification for a terminal. A replacement can be quiet, so the
/// same news arriving twice sounds once.
pub fn post(live: u64, title: &str, body: &str, sound: bool) {
    #[cfg(test)]
    POSTED.with_borrow_mut(|posted| posted.push((live, body.to_owned(), sound)));
    if !notify::available() {
        return;
    }
    let (title, body) = (title.to_owned(), body.to_owned());
    authorize(move |granted| {
        if granted {
            unsafe { add(live, &title, &body, sound) };
        }
    });
}

/// Takes a terminal's notification back out of Notification Center.
pub fn remove(live: u64) {
    #[cfg(test)]
    REMOVED.with_borrow_mut(|removed| removed.push(live));
    notify::remove(&[&key(live)]);
}

/// The number on the Dock icon. Zero takes it away.
pub fn badge(count: usize) {
    if cfg!(test) {
        return;
    }
    let label = if count == 0 { String::new() } else { count.to_string() };
    objc2::rc::autoreleasepool(|_| unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let tile: *mut AnyObject = msg_send![app, dockTile];
        let _: () = msg_send![tile, setBadgeLabel: ns(&label)];
    });
}

unsafe fn center() -> *mut AnyObject {
    unsafe { msg_send![class!(UNUserNotificationCenter), currentNotificationCenter] }
}

unsafe fn ns(s: &str) -> *mut AnyObject {
    let c = std::ffi::CString::new(s.replace('\0', "")).unwrap_or_default();
    unsafe { msg_send![class!(NSString), stringWithUTF8String: c.as_ptr()] }
}

unsafe fn add(live: u64, title: &str, body: &str, sound: bool) {
    objc2::rc::autoreleasepool(|_| unsafe {
        let content: Retained<AnyObject> = msg_send_id![class!(UNMutableNotificationContent), new];
        let content = Retained::as_ptr(&content).cast_mut();
        let _: () = msg_send![content, setTitle: ns(title)];
        let _: () = msg_send![content, setBody: ns(body)];
        if sound {
            let sound: *mut AnyObject = msg_send![class!(UNNotificationSound), defaultSound];
            let _: () = msg_send![content, setSound: sound];
        }
        let (keys, values) = ([ns(LIVE), ns(RUN)], [ns(&live.to_string()), ns(&run())]);
        let info: *mut AnyObject = msg_send![class!(NSDictionary), dictionaryWithObjects: values.as_ptr(), forKeys: keys.as_ptr(), count: 2usize];
        let _: () = msg_send![content, setUserInfo: info];
        let request: *mut AnyObject = msg_send![
            class!(UNNotificationRequest),
            requestWithIdentifier: ns(&key(live)),
            content: content,
            trigger: std::ptr::null::<AnyObject>()
        ];
        let _: () = msg_send![center(), addNotificationRequest: request, withCompletionHandler: std::ptr::null::<Block<dyn Fn(*mut AnyObject)>>()];
    });
}
