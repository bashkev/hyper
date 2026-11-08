//! Accessibility and Screen Recording, which programs in Hyper's terminals need to drive other
//! apps (`osascript`, System Events, keystrokes) and capture the screen (`screencapture`). macOS
//! judges them by the app they run in, Hyper, so agents testing a UI fail without them.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

type CFTypeRef = *const c_void;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: CFTypeRef) -> bool;
    static kAXTrustedCheckOptionPrompt: CFTypeRef;
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDictionaryCreate(allocator: CFTypeRef, keys: *const CFTypeRef, values: *const CFTypeRef, count: isize, key_callbacks: *const c_void, value_callbacks: *const c_void) -> CFTypeRef;
    fn CFRelease(object: CFTypeRef);
    static kCFBooleanTrue: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
}

/// The permissions Hyper lacks, by their names in System Settings.
pub fn missing() -> Vec<&'static str> {
    let (accessibility, screen) = unsafe { (AXIsProcessTrusted(), CGPreflightScreenCaptureAccess()) };
    [(!accessibility).then_some("Accessibility"), (!screen).then_some("Screen Recording")].into_iter().flatten().collect()
}

/// Shows macOS's requests for what's missing, once per app: `app` is the bundle, so Hyper and
/// Hyper Dev each ask. Once refused, they're granted in System Settings.
pub fn ask(home: &Path) {
    let Some(app) = bundle() else { return };
    let missing = missing();
    let marker = marker(home, &app);
    if missing.is_empty() || marker.exists() {
        return;
    }
    if let Some(dir) = marker.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&marker, "");
    unsafe {
        if missing.contains(&"Accessibility") {
            let (keys, values) = ([kAXTrustedCheckOptionPrompt], [kCFBooleanTrue]);
            let options = CFDictionaryCreate(std::ptr::null(), keys.as_ptr(), values.as_ptr(), 1, &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks);
            AXIsProcessTrustedWithOptions(options);
            CFRelease(options);
        }
        if missing.contains(&"Screen Recording") {
            CGRequestScreenCaptureAccess();
        }
    }
}

fn marker(home: &Path, app: &Path) -> PathBuf {
    let name = app.file_stem().map_or("Hyper".into(), |n| n.to_string_lossy().into_owned());
    home.join("Library/Application Support/Hyper").join(format!("asked-access-{name}"))
}

/// The `.app` this runs from. A bare build runs with its terminal's permissions, so needs none.
fn bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let app = exe.parent()?.parent()?.parent()?;
    (app.extension()? == "app").then(|| app.to_owned())
}
