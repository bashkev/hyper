//! Reloading the app in place, for a new build: ⌃⌘R builds one from the app's source first, and
//! `kill -USR1 $HYPER_PID` from a terminal takes the one on disk.
//!
//! Each terminal saves its screen and leaves its PTY open, and the app `exec`s the binary on
//! disk. `exec` keeps the pid, so the programs stay this process's children, and the new build
//! takes over their PTYs. They never notice: what they print meanwhile waits in the PTY.

use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rune::{El, Tx, Waker, WindowHandle, row, text};
use rune_kit::{Button, ButtonVariant, Dialog, Kit, Spinner, Toast, ToastRegion};
use serde_json::{Value, json};

use crate::{App, View};

/// Names the folder the build before this one handed its terminals over in.
const ADOPT: &str = "HYPER_ADOPT";
/// Asks a build only whether it starts.
const CHECK: &str = "HYPER_RELOAD_CHECK";
const STATE: &str = "state.json";
/// How long a new build has to show it starts before the reload is called off.
const CHECK_WAIT: Duration = Duration::from_secs(10);
/// Where this build was built from.
const SOURCE: &str = env!("CARGO_MANIFEST_DIR");
/// How much of a failed build's output is shown.
const LOG_LINES: usize = 24;

/// What a build that failed says, and the part of its output that explains why.
pub type BuildError = (String, Option<String>);

static REQUESTED: AtomicBool = AtomicBool::new(false);
/// What the build before this one handed over: its folder and state.
static HANDED: Mutex<Option<(PathBuf, Value)>> = Mutex::new(None);
/// Where the build before left the window. Kept apart, since `setup` adopts the rest before the
/// window exists.
static FRAME: Mutex<Option<String>> = Mutex::new(None);

thread_local! {
    static WINDOW: std::cell::RefCell<Option<WindowHandle>> = const { std::cell::RefCell::new(None) };
}

/// First thing in `main`. A build being checked has started, so it can stop. Otherwise this picks
/// up what the build before it handed over, if it was started by one.
pub fn start() {
    if std::env::var_os(CHECK).is_some() {
        std::process::exit(0);
    }
    let Some(dir) = std::env::var_os(ADOPT).map(PathBuf::from) else { return };
    // SAFETY: no other thread has started yet.
    unsafe { std::env::remove_var(ADOPT) };
    let state = std::fs::read(dir.join(STATE)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok());
    match state {
        Some(state) => *HANDED.lock().unwrap() = Some((dir, state)),
        None => {
            eprintln!("hyper: nothing to take over in {}", dir.display());
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Where a running app that reloads on SIGUSR1 says so, by its pid. To a build from before
/// reloading existed, the signal is fatal, so `scripts/bundle.sh --reload` checks here first.
fn markers() -> PathBuf {
    std::env::temp_dir().join("hyper-reloads")
}

/// Catches SIGUSR1, doing nothing: kqueue is what hears it.
fn hear() {
    extern "C" fn heard(_: libc::c_int) {}
    unsafe { libc::signal(libc::SIGUSR1, heard as *const () as libc::sighandler_t) };
}

/// Listens for SIGUSR1, which asks for a reload.
pub fn listen(wake: Waker) {
    // kqueue hears it whatever its disposition. A handler, unlike ignoring it, keeps it from
    // ending the app without passing on to the programs and helpers the app starts.
    hear();
    let markers = markers();
    if std::fs::create_dir_all(&markers).is_ok() {
        // Those of apps that have quit go.
        for entry in std::fs::read_dir(&markers).into_iter().flatten().flatten() {
            let pid = entry.file_name().to_str().and_then(|n| n.parse::<libc::pid_t>().ok());
            if pid.is_none_or(|pid| unsafe { libc::kill(pid, 0) } != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        let _ = std::fs::write(markers.join(std::process::id().to_string()), "");
    }
    let queue = unsafe { libc::kqueue() };
    if queue < 0 {
        return;
    }
    let change = libc::kevent { ident: libc::SIGUSR1 as usize, filter: libc::EVFILT_SIGNAL, flags: libc::EV_ADD, fflags: 0, data: 0, udata: std::ptr::null_mut() };
    if unsafe { libc::kevent(queue, &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) } != 0 {
        return;
    }
    let _ = std::thread::Builder::new().name("reload-signal".into()).spawn(move || {
        // SAFETY: a zeroed kevent is valid, and it's only written to.
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        loop {
            if unsafe { libc::kevent(queue, std::ptr::null(), 0, &mut event, 1, std::ptr::null()) } > 0 {
                REQUESTED.store(true, Ordering::Release);
                wake();
            }
        }
    });
}

/// Whether a reload was asked for since the last look.
pub fn requested() -> bool {
    REQUESTED.swap(false, Ordering::AcqRel)
}

/// Keeps the window to save its frame, and puts it where the build before left it. Says whether
/// it did.
pub fn on_window(handle: &WindowHandle) -> bool {
    WINDOW.with_borrow_mut(|w| *w = Some(handle.clone()));
    let frame = FRAME.lock().unwrap().take();
    frame.is_some_and(|frame| set_frame(handle, &frame))
}

impl App {
    /// Takes over the terminals the build before this one handed over, once wakers exist.
    pub fn adopt_handed(&mut self) {
        let Some((dir, state)) = HANDED.lock().unwrap().take() else { return };
        *FRAME.lock().unwrap() = state["frame"].as_str().map(str::to_owned);
        self.adopt(&dir, &state);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn adopt(&mut self, dir: &Path, state: &Value) {
        let dark = state["dark"].as_bool().unwrap_or(self.dark.get());
        self.adopt_workspace(&state["workspace"], dir, dark);
        self.adopt_browser(&state["browser"]);
        self.set_view(match state["view"].as_str() {
            Some("browse") => View::Browse,
            Some("settings") => View::Settings,
            _ => View::Workspace,
        });
        self.adopt_history(&state["history"]);
    }

    /// ⌃⌘R: builds the app from its source, then reloads into the new build. A copy that isn't
    /// the bundle `scripts/bundle.sh` makes, such as one moved out of `target/`, reloads the
    /// binary on disk as it is.
    pub fn build_and_reload(&mut self, tx: &mut Tx) {
        if self.build_job.is_pending() {
            return;
        }
        let source = Path::new(SOURCE);
        let Some(flags) = std::env::current_exe().ok().and_then(|exe| bundle_flags(source, &exe)) else { return self.reload(tx) };
        self.build_job.request((), move |()| build(source, flags));
    }

    /// The build ⌃⌘R asked for finished.
    pub fn built(&mut self, result: Result<(), BuildError>, tx: &mut Tx) {
        match result {
            Ok(()) => self.reload(tx),
            Err((message, log)) => {
                self.reload_error = Some(message);
                self.reload_log = log;
            }
        }
    }

    /// Hands the terminals to the binary on disk and becomes it. Comes back only if it fails,
    /// with the terminals taken back.
    pub fn reload(&mut self, tx: &mut Tx) {
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => return self.reload_error = Some(format!("Couldn't find the app: {e}")),
        };
        if let Err(e) = check(&exe) {
            return self.reload_error = Some(e);
        }
        self.settings_due = Some(Instant::now());
        self.flush_settings(tx);
        let dir = std::env::temp_dir().join(format!("hyper-reload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return self.reload_error = Some(format!("Couldn't save the terminals: {e}"));
        }
        let view = match self.view {
            View::Browse => "browse",
            View::Settings => "settings",
            View::Workspace => "workspace",
        };
        let frame = WINDOW.with_borrow(|w| w.as_ref().and_then(frame));
        let state = json!({ "view": view, "dark": self.dark.get(), "frame": frame, "browser": self.browser_hand_off(), "history": self.history_hand_off(), "workspace": self.hand_off_workspace(&dir) });
        let error = match std::fs::write(dir.join(STATE), state.to_string()) {
            Ok(()) => {
                // `exec` resets the handler, and the new build listens only once it's set up:
                // until then, ignoring the signal keeps a second one from ending it.
                unsafe { libc::signal(libc::SIGUSR1, libc::SIG_IGN) };
                let error = Command::new(&exe).args(std::env::args_os().skip(1)).env(ADOPT, &dir).exec();
                hear();
                error
            }
            Err(e) => e,
        };
        // Still here, so the new build never started.
        self.adopt(&dir, &state);
        let _ = std::fs::remove_dir_all(&dir);
        self.reload_error = Some(format!("Couldn't start the new build: {error}"));
    }
}

/// The `scripts/bundle.sh` flags that rebuild `exe`, when it's one of the bundles the script
/// makes from `source`.
fn bundle_flags(source: &Path, exe: &Path) -> Option<&'static [&'static str]> {
    if !source.join("scripts/bundle.sh").is_file() {
        return None;
    }
    let exe = exe.canonicalize().ok()?;
    let bundles: [(&str, &'static [&'static str]); 2] = [("Hyper", &[]), ("Hyper Dev", &["--dev"])];
    bundles
        .into_iter()
        .find(|(name, _)| source.join(format!("target/{name}.app/Contents/MacOS/Hyper")).canonicalize().is_ok_and(|bundled| bundled == exe))
        .map(|(_, flags)| flags)
}

/// Runs `scripts/bundle.sh` in a login shell, as a terminal would, so it finds `cargo` and `zig`.
fn build(source: &Path, flags: &[&str]) -> Result<(), BuildError> {
    let mut argv = vec![source.join("scripts/bundle.sh").to_string_lossy().into_owned()];
    argv.extend(flags.iter().map(|&f| f.to_owned()));
    let out = crate::term::shell_command(&crate::term::login_shell(), &argv, source)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| (format!("Couldn't start the build: {e}"), None))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(("The build failed.".into(), Some(failure(&String::from_utf8_lossy(&out.stderr)))))
    }
}

/// The part of a failed build's output that says why: from cargo's first error, or else its end.
fn failure(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let start = lines.iter().position(|l| l.starts_with("error")).unwrap_or(lines.len().saturating_sub(LOG_LINES));
    lines[start..].iter().take(LOG_LINES).copied().collect::<Vec<_>>().join("\n")
}

/// Whether `exe` is a build that can take terminals over, and starts.
fn check(exe: &Path) -> Result<(), String> {
    let bytes = std::fs::read(exe).map_err(|e| format!("Couldn't read {}: {e}", exe.display()))?;
    // A build from before reloading existed would start without the terminals, and stop them.
    if memchr::memmem::find(&bytes, ADOPT.as_bytes()).is_none() {
        return Err("That build can't take over running terminals.".into());
    }
    let mut child = Command::new(exe)
        .env(CHECK, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Couldn't start the new build: {e}"))?;
    let deadline = Instant::now() + CHECK_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("The new build didn't start ({status}).")),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("The new build didn't start.".into());
            }
        }
    }
}

/// The window's frame, as AppKit writes it down.
fn frame(handle: &WindowHandle) -> Option<String> {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    crate::settings::with_ns_window(handle, |window| unsafe {
        let saved: *mut AnyObject = msg_send![window, stringWithSavedFrame];
        let utf8: *const std::ffi::c_char = if saved.is_null() { std::ptr::null() } else { msg_send![saved, UTF8String] };
        (!utf8.is_null()).then(|| std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned())
    })
    .flatten()
}

fn set_frame(handle: &WindowHandle, frame: &str) -> bool {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    let Ok(frame) = std::ffi::CString::new(frame) else { return false };
    crate::settings::with_ns_window(handle, |window| unsafe {
        let string: *mut AnyObject = msg_send![objc2::class!(NSString), stringWithUTF8String: frame.as_ptr()];
        if string.is_null() {
            return false;
        }
        let _: () = msg_send![window, setFrameFromString: string];
        true
    })
    .unwrap_or(false)
}

/// Shows a build ⌃⌘R started, and says why a reload didn't happen.
pub fn dialog(s: &App, k: &Kit, layer: &mut Vec<El>) {
    if s.build_job.is_pending() {
        let toast = Toast::new("building", "Building Hyper").lead(Spinner::new().size(14.0).build(k)).build(k);
        layer.push(ToastRegion::new().toast(toast).build(k));
    }
    fn dismiss(s: &mut App) {
        s.reload_error = None;
        s.reload_log = None;
    }
    let message = s.reload_error.clone().unwrap_or_default();
    let detail = text(message).font(k.ty.ui.lh(20.0)).color(k.c.text_muted).wrap().w_full().px(16.0).key("reload-error");
    let log = s.reload_log.clone().map(|log| text(log).font(k.ty.mono).color(k.c.text_base).wrap().w_full().px(16.0).pt(12.0).key("reload-log"));
    let actions = row()
        .w_full()
        .padding(16.0)
        .justify_end()
        .child(Button::new("OK").variant(ButtonVariant::Neutral).on_press(dismiss).build(k).key("reload-ok"));
    let mut dialog = Dialog::new("reload", "Couldn't reload")
        .width(if log.is_some() { 640.0 } else { 360.0 })
        .fit()
        .open(s.reload_error.is_some())
        .on_close(dismiss)
        .child(detail);
    if let Some(log) = log {
        dialog = dialog.child(log);
    }
    dialog.child(actions).build(k, layer);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_bundles_the_script_makes_are_rebuilt() {
        let source = std::env::temp_dir().join(format!("hyper-bundles-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&source);
        let exe = |name: &str| source.join(format!("{name}/Contents/MacOS/Hyper"));
        for name in ["target/Hyper.app", "target/Hyper Dev.app", "moved/Hyper.app"] {
            std::fs::create_dir_all(exe(name).parent().unwrap()).unwrap();
            std::fs::write(exe(name), "").unwrap();
        }
        assert_eq!(bundle_flags(&source, &exe("target/Hyper.app")), None, "no script, nothing to build with");
        std::fs::create_dir_all(source.join("scripts")).unwrap();
        std::fs::write(source.join("scripts/bundle.sh"), "").unwrap();
        assert_eq!(bundle_flags(&source, &exe("target/Hyper.app")), Some(&[][..]));
        assert_eq!(bundle_flags(&source, &exe("target/Hyper Dev.app")), Some(&["--dev"][..]));
        assert_eq!(bundle_flags(&source, &exe("moved/Hyper.app")), None, "a copy elsewhere isn't what the script writes");
        std::fs::remove_dir_all(&source).unwrap();
    }

    #[test]
    fn a_failed_build_shows_from_its_first_error() {
        let output = "   Compiling hyper v0.1.0\nerror[E0425]: cannot find value `x`\n --> src/main.rs:1:1\n\nerror: could not compile `hyper`";
        assert_eq!(failure(output), "error[E0425]: cannot find value `x`\n --> src/main.rs:1:1\n\nerror: could not compile `hyper`");
        let long: String = (0..100).map(|i| format!("line {i}\n")).collect();
        assert_eq!(failure(&long).lines().next(), Some("line 76"), "without an error, its end");
    }
}
