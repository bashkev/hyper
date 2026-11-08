//! Puts asking first between macOS and quitting.
//!
//! ⌘Q, the Quit menu item and the Dock's Quit all arrive as `terminate:`, which ends the app
//! without asking Rune, so nothing could warn that an agent would stop. Replacing that method
//! turns them into a request the app answers.

use std::ffi::{CStr, c_char, c_void};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use rune::Waker;

static REQUESTED: AtomicBool = AtomicBool::new(false);
static WAKE: OnceLock<Waker> = OnceLock::new();

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> *mut c_void;
    fn sel_registerName(name: *const c_char) -> *mut c_void;
    fn class_replaceMethod(class: *mut c_void, selector: *mut c_void, imp: *const c_void, types: *const c_char) -> *const c_void;
}

extern "C" fn terminate(_: *mut c_void, _: *mut c_void, _: *mut c_void) {
    REQUESTED.store(true, Ordering::Release);
    if let Some(wake) = WAKE.get() {
        wake();
    }
}

/// From now on `terminate:` asks instead. Rune's own quit doesn't go through it.
pub fn intercept(wake: Waker) {
    if WAKE.set(wake).is_err() {
        return;
    }
    let (class, selector, types): (&CStr, &CStr, &CStr) = (c"NSApplication", c"terminate:", c"v@:@");
    unsafe {
        class_replaceMethod(objc_getClass(class.as_ptr()), sel_registerName(selector.as_ptr()), terminate as *const c_void, types.as_ptr());
    }
}

/// Whether macOS asked the app to quit since the last look.
pub fn requested() -> bool {
    REQUESTED.swap(false, Ordering::AcqRel)
}
