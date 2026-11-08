//! Waits for programs to end, by pid. A program's waiter outlives its terminal thread, so a
//! terminal handed back to this same process, when a reload fails, still hears how it ended.

use std::collections::HashMap;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::{LazyLock, Mutex};

#[derive(Default)]
struct Watch {
    exit: Option<Option<i32>>,
    /// Written to when it ends: the terminal thread's poke pipe.
    notify: Option<OwnedFd>,
    /// Handed to the next build, which reaps it.
    released: bool,
    /// A thread is waiting for it to end.
    waiting: bool,
}

static WATCHED: LazyLock<Mutex<HashMap<libc::pid_t, Watch>>> = LazyLock::new(Mutex::default);

fn poke(fd: &OwnedFd) {
    unsafe { libc::write(fd.as_raw_fd(), [0u8].as_ptr().cast(), 1) };
}

pub fn watch(pid: libc::pid_t, notify: OwnedFd) {
    let mut watched = WATCHED.lock().unwrap();
    let watch = watched.entry(pid).or_default();
    watch.released = false;
    if watch.exit.is_some() {
        poke(&notify);
    }
    watch.notify = Some(notify);
    if watch.exit.is_none() && !watch.waiting {
        watch.waiting = true;
        let _ = std::thread::Builder::new().name("terminal-reaper".into()).spawn(move || wait(pid));
    }
}

fn wait(pid: libc::pid_t) {
    // Waits without reaping, so a program handed over in the meantime is left for the build
    // taking it, which would otherwise never learn how it ended.
    let ended = loop {
        // SAFETY: a zeroed siginfo_t is valid, and it's only written to.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let found = unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOWAIT) };
        if found == 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            break found == 0;
        }
    };
    let mut watched = WATCHED.lock().unwrap();
    if let Some(watch) = watched.get_mut(&pid).filter(|w| w.released) {
        watch.waiting = false;
        return;
    }
    let mut status = 0;
    let reaped = ended && unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) } == pid;
    // Gone already, reaped by a build before this one: how it ended is lost.
    let code = (reaped && libc::WIFEXITED(status)).then(|| libc::WEXITSTATUS(status));
    if let Some(watch) = watched.get_mut(&pid) {
        watch.waiting = false;
        watch.exit = Some(code);
        if let Some(notify) = &watch.notify {
            poke(notify);
        }
    }
}

pub fn exit(pid: libc::pid_t) -> Option<Option<i32>> {
    WATCHED.lock().unwrap().get(&pid).and_then(|w| w.exit)
}

/// Its terminal thread is gone; one taking it over will watch it again, and reap it.
pub fn release(pid: libc::pid_t) {
    if let Some(watch) = WATCHED.lock().unwrap().get_mut(&pid) {
        watch.notify = None;
        watch.released = true;
    }
}

/// Its terminal closed: the program is still reaped, but nobody asks how it ended.
pub fn forget(pid: libc::pid_t) {
    let Some(watch) = WATCHED.lock().unwrap().remove(&pid) else { return };
    // Released and already ended, so no thread is left to reap it.
    if !watch.waiting && watch.exit.is_none() {
        unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Command, Stdio};
    use std::time::Duration;

    use crate::term::pty::pipe;
    use crate::term::reap;

    /// A program handed to the next build is left for it to reap, so it learns how it ended.
    #[test]
    fn released_program_is_left_to_reap() {
        let mut child = Command::new("/bin/sh").args(["-c", "read _; exit 3"]).stdin(Stdio::piped()).spawn().unwrap();
        let pid = child.id() as libc::pid_t;
        let stdin = child.stdin.take();
        // The test reaps it with `waitpid` below, as the next build would.
        drop(child);
        let (_poked, poke) = pipe().unwrap();
        reap::watch(pid, poke);
        reap::release(pid);
        // It ends only once released, as a program does between handing over and `exec`.
        drop(stdin);
        let mut status = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let found = loop {
            let found = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            if found != 0 || std::time::Instant::now() > deadline {
                break found;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(found, pid);
        assert!(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 3);
        assert_eq!(reap::exit(pid), None);
    }
}
