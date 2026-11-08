//! Starting a program on a new PTY, with the environment a terminal opened from Finder would give it.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};

use super::{Launch, Size};

pub(super) fn pipe() -> Option<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return None;
    }
    for fd in fds {
        set_flags(fd);
    }
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    Some((write, read))
}

/// Non-blocking, and closed in any program a terminal starts.
pub(super) fn set_flags(fd: RawFd) {
    unsafe {
        libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK);
    }
}

pub(super) fn winsize(size: Size) -> libc::winsize {
    libc::winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: size.cols * size.cell_w,
        ws_ypixel: size.rows * size.cell_h,
    }
}

/// Starts the program on a new PTY through the login shell, since an app opened from Finder
/// doesn't have the user's PATH. It goes as arguments, so nothing in it is read as shell syntax.
pub(super) fn spawn(launch: &Launch, size: Size) -> std::io::Result<(OwnedFd, std::process::Child)> {
    let (mut master, mut slave) = (-1, -1);
    let mut ws = winsize(size);
    if unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut ws) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    set_flags(master.as_raw_fd());
    let mut command = shell_command(&login_shell(), &launch.argv, &launch.cwd);
    command
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    // The PTY becomes this process's controlling terminal, so Ctrl-C and hangups reach it.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok((master, command.spawn()?))
}

/// The program, run by `shell` as a login shell. It starts from what macOS gives an app opened
/// from Finder. Opened from a shell, the app carries that shell's environment: from inside an
/// agent that means `NO_COLOR`, a `C` locale and the agent's own markers, so CLIs lost their
/// colours or took themselves for child sessions. The login shell adds everything the user set.
pub fn shell_command(shell: &str, argv: &[String], cwd: &Path) -> Command {
    const KEPT: [&str; 7] = ["HOME", "USER", "LOGNAME", "SHELL", "TMPDIR", "SSH_AUTH_SOCK", "__CF_USER_TEXT_ENCODING"];
    let mut command = Command::new(shell);
    command.env_clear();
    for name in KEPT {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        // Finder sets no locale, which leaves shells in `C`, so take the system's, as Terminal does.
        .env("LANG", posix_locale(system_locale().as_deref()))
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .env("TERM_PROGRAM", "ghostty")
        // `kill -USR1 $HYPER_PID` reloads the app these programs run in, and they keep running.
        .env("HYPER_PID", std::process::id().to_string())
        .args(["-l", "-i", "-c", r#"exec "$@""#, "sh"])
        .args(argv)
        .current_dir(cwd);
    command
}

/// The system locale's identifier, such as `en_US` or `zh-Hans_CN@rg=gbzzzz`.
fn system_locale() -> Option<String> {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    objc2::rc::autoreleasepool(|_| unsafe {
        let locale: *mut AnyObject = msg_send![objc2::class!(NSLocale), currentLocale];
        let id: *mut AnyObject = msg_send![locale, localeIdentifier];
        let utf8: *const std::ffi::c_char = msg_send![id, UTF8String];
        (!utf8.is_null()).then(|| std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned())
    })
}

/// A UTF-8 locale the system has for a locale identifier. Many language and region pairs, such as
/// `en_DE`, have none, and a `LANG` naming one leaves shells in `C`, so those fall back to `en_US`.
fn posix_locale(id: Option<&str>) -> String {
    // Options, as in `@rg=gbzzzz`, and scripts, as in `zh-Hans_CN`, aren't part of POSIX names.
    let id = id.and_then(|id| id.split('@').next()).unwrap_or_default();
    let language = id.split(['-', '_']).next().unwrap_or_default();
    let region = id.rsplit_once('_').map(|(_, r)| r).unwrap_or_default();
    [id.to_owned(), format!("{language}_{region}")]
        .into_iter()
        .map(|name| format!("{name}.UTF-8"))
        .find(|name| !name.starts_with('.') && std::path::Path::new("/usr/share/locale").join(name).is_dir())
        .unwrap_or_else(|| "en_US.UTF-8".into())
}

pub fn login_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn programs_dont_inherit_the_environment_of_whatever_opened_the_app() {
        // Cargo gives tests this, as an agent's shell gives the app `NO_COLOR`.
        assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
        let out = shell_command("/bin/sh", &["/usr/bin/env".into()], &std::env::temp_dir()).stdin(Stdio::null()).output().unwrap();
        let env = String::from_utf8_lossy(&out.stdout);
        let names: Vec<&str> = env.lines().filter_map(|l| l.split_once('=').map(|(k, _)| k)).collect();
        assert!(!names.contains(&"CARGO_MANIFEST_DIR"), "{env}");
        assert!(names.contains(&"HOME") && env.lines().any(|l| l == "TERM=xterm-256color"), "{env}");
    }

    #[test]
    fn a_locale_the_system_lacks_falls_back_to_one_it_has() {
        assert_eq!(posix_locale(Some("de_DE@rg=gbzzzz")), "de_DE.UTF-8");
        assert_eq!(posix_locale(Some("zh-Hans_CN")), "zh_CN.UTF-8");
        assert_eq!(posix_locale(Some("en_DE")), "en_US.UTF-8");
        assert_eq!(posix_locale(Some("en_150")), "en_US.UTF-8");
        assert_eq!(posix_locale(None), "en_US.UTF-8");
    }
}
