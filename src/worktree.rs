//! New git worktrees, made where Claude Code makes its own: `<repo>/.claude/worktrees/<name>`,
//! on branch `worktree-<name>`. Every tool starts in them the same way.

use std::path::{Path, PathBuf};
use std::process::Command;

const ADJECTIVES: [&str; 16] = ["brave", "calm", "eager", "fond", "gentle", "keen", "lively", "merry", "nimble", "proud", "quick", "quiet", "sunny", "swift", "tidy", "witty"];
const ANIMALS: [&str; 16] = ["otter", "heron", "lynx", "panda", "robin", "badger", "falcon", "gecko", "koala", "marten", "newt", "owl", "puffin", "seal", "wren", "yak"];

/// The worktree a folder is in, by the tools' own layouts: `~/src/app/.claude/worktrees/x/sub`
/// is `x`, and Codex's `~/.codex/worktrees/1a2b/app` is `1a2b`.
pub fn name_of(cwd: &Path) -> Option<String> {
    let cwd = cwd.to_str()?;
    let rest = match cwd.split_once("/.claude/worktrees/") {
        Some((_, rest)) => rest,
        None => cwd.split_once("/.codex/worktrees/")?.1,
    };
    rest.split('/').next().filter(|name| !name.is_empty()).map(str::to_owned)
}

/// The repository a folder in one of its `.claude/worktrees` belongs to.
pub fn repo_of(cwd: &Path) -> Option<PathBuf> {
    let (repo, _) = cwd.to_str()?.split_once("/.claude/worktrees/")?;
    Some(PathBuf::from(repo))
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| format!("Could not run git: {e}"))?;
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_owned());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    // The reason is on the `fatal:` or `error:` line; hints and advice can follow it.
    let line = said
        .lines()
        .find(|l| l.starts_with("fatal: ") || l.starts_with("error: "))
        .or_else(|| said.lines().rev().find(|l| !l.trim().is_empty()))
        .unwrap_or("git failed");
    Err(line.trim().trim_start_matches("fatal: ").trim_start_matches("error: ").to_owned())
}

/// A name that's safe as a folder and in a branch: letters, digits, `.`, `_` and `-`.
fn clean(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            out.push(c);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    while out.contains("..") {
        out = out.replace("..", ".");
    }
    out.trim_matches(|c| c == '-' || c == '.').to_owned()
}

fn random_name() -> String {
    let mut b = [0u8; 2];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = std::io::Read::read_exact(&mut f, &mut b);
    }
    format!("{}-{}", ADJECTIVES[b[0] as usize % 16], ANIMALS[b[1] as usize % 16])
}

fn branch_exists(repo: &Path, branch: &str) -> bool {
    git(repo, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok()
}

/// Makes a worktree of the repository `folder` is in, from `folder`'s current commit, and
/// returns where in it to start: the same place `folder` is in its own checkout. An empty
/// `name` gets a random one. A name already in use is picked up: its worktree if it has one,
/// or its branch checked out into a new one.
pub fn add(folder: &Path, name: &str) -> Result<PathBuf, String> {
    git(folder, &["rev-parse", "--is-inside-work-tree"]).map_err(|_| "Not a git repository".to_owned())?;
    let common = PathBuf::from(git(folder, &["rev-parse", "--path-format=absolute", "--git-common-dir"])?);
    // The main checkout, even when `folder` is itself in a worktree.
    let repo = match common.file_name() {
        Some(n) if n == ".git" => common.parent().map(Path::to_path_buf).ok_or("No repository folder")?,
        _ => return Err("A bare repository has no checkout to put worktrees in".to_owned()),
    };
    let prefix = git(folder, &["rev-parse", "--show-prefix"])?;
    let trees = repo.join(".claude/worktrees");

    let typed = !name.trim().is_empty();
    let mut name = if typed { clean(name) } else { random_name() };
    if name.is_empty() {
        return Err("Use letters, digits, . _ or - in the name".to_owned());
    }
    if !typed {
        let mut n = 2;
        let base = name.clone();
        while trees.join(&name).exists() || branch_exists(&repo, &format!("worktree-{name}")) {
            name = format!("{base}-{n}");
            n += 1;
        }
    }
    let path = trees.join(&name);
    if path.exists() {
        // Only a checkout of this repository is picked up, not some other folder of that name.
        let found = git(&path, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).map(PathBuf::from);
        let top = git(&path, &["rev-parse", "--show-toplevel"]).map(PathBuf::from);
        return match top {
            // On a case-insensitive disk `fix` finds `Fix`; start in it as it's spelled.
            Ok(top) if found.is_ok_and(|c| same(&c, &common)) && same_folder(&top, &path) => Ok(start_in(&top, &prefix)),
            _ => Err(format!("{} already exists", path.display())),
        };
    }

    ignore(&repo, &common)?;
    // Forget worktrees whose folders were deleted, or their names can't be used again.
    git(&repo, &["worktree", "prune"])?;
    let branch = format!("worktree-{name}");
    let at = path.to_string_lossy();
    if branch_exists(&repo, &branch) {
        git(folder, &["worktree", "add", &at, &branch])?;
    } else {
        git(folder, &["worktree", "add", "-b", &branch, &at, "HEAD"])?;
    }
    Ok(start_in(&path, &prefix))
}

/// Where `prefix` is in a worktree, or its top when that folder isn't checked in.
fn start_in(top: &Path, prefix: &str) -> PathBuf {
    let start = top.join(prefix);
    if start.is_dir() { start } else { top.to_path_buf() }
}

fn same(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The same folder, even spelled in another case on a disk that ignores it.
fn same_folder(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase(),
        _ => a == b,
    }
}

/// Keeps the worktrees out of the repository's own changes, as Claude Code does, without
/// touching a file that's checked in.
fn ignore(repo: &Path, common: &Path) -> Result<(), String> {
    if git(repo, &["check-ignore", "-q", ".claude/worktrees/"]).is_ok() {
        return Ok(());
    }
    let exclude = common.join("info/exclude");
    let had = std::fs::read_to_string(&exclude).unwrap_or_default();
    let mut text = had.clone();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(".claude/worktrees/\n");
    std::fs::create_dir_all(common.join("info")).and_then(|_| std::fs::write(&exclude, text)).map_err(|e| format!("Could not update {}: {e}", exclude.display()))
}

/// The top of the `.claude/worktrees` worktree a folder is in.
pub fn top_of(cwd: &Path) -> Option<PathBuf> {
    Some(repo_of(cwd)?.join(".claude/worktrees").join(name_of(cwd)?))
}

/// The worktrees kept in a repository's `.claude/worktrees`, by name.
pub fn list(repo: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(repo.join(".claude/worktrees")) else { return Vec::new() };
    // A checkout has a `.git` file pointing back at the repository; anything else isn't one.
    let mut trees: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.join(".git").is_file()).collect();
    trees.sort();
    trees
}

/// What removing a worktree would lose.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Unsaved {
    /// Changed and untracked files.
    pub files: usize,
    /// Commits on no other branch and not pushed.
    pub commits: usize,
}

impl Unsaved {
    pub fn is_empty(self) -> bool {
        self.files == 0 && self.commits == 0
    }

    pub fn describe(self) -> String {
        let count = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let parts: Vec<String> = [
            (self.files > 0).then(|| count(self.files, "uncommitted file", "uncommitted files")),
            (self.commits > 0).then(|| count(self.commits, "commit on no other branch", "commits on no other branch")),
        ]
        .into_iter()
        .flatten()
        .collect();
        parts.join(" and ")
    }
}

/// What removing a worktree would lose: its changes, and commits that only its own branch, or
/// a detached HEAD, has. Any other branch checked out in it stays.
pub fn unsaved(tree: &Path) -> Result<Unsaved, String> {
    let status = git(tree, &["status", "--porcelain", "--untracked-files=all"])?;
    let files = status.lines().filter(|l| !l.trim().is_empty()).count();
    let own = own_branch(tree);
    let own_ref = format!("refs/heads/{own}");
    let mut going: Vec<&str> = Vec::new();
    if branch_exists(tree, &own) {
        going.push(&own_ref);
    }
    if git(tree, &["branch", "--show-current"]).unwrap_or_default().is_empty() {
        going.push("HEAD");
    }
    if going.is_empty() {
        return Ok(Unsaved { files, commits: 0 });
    }
    let not_own = format!("--exclude={own}");
    let mut args = vec!["rev-list", "--count"];
    args.extend(going);
    args.extend(["--not", &not_own, "--branches", "--remotes"]);
    let commits = git(tree, &args)?.trim().parse().unwrap_or(0);
    Ok(Unsaved { files, commits })
}

fn own_branch(tree: &Path) -> String {
    format!("worktree-{}", tree.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
}

/// Whether any process, such as an agent started outside this app, is working in a folder in
/// the worktree.
pub fn busy(tree: &Path) -> bool {
    let tree = tree.canonicalize().unwrap_or_else(|_| tree.to_path_buf());
    let mut pids = vec![0 as libc::pid_t; 8192];
    let bytes = (pids.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int;
    // SAFETY: the buffer is as big as it says.
    let count = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    let me = std::process::id() as libc::pid_t;
    pids.iter().take(count.max(0) as usize).filter(|&&pid| pid > 0 && pid != me).any(|&pid| cwd_of(pid).is_some_and(|cwd| cwd.starts_with(&tree)))
}

fn cwd_of(pid: libc::pid_t) -> Option<PathBuf> {
    // SAFETY: a zeroed proc_vnodepathinfo is valid, and it's only written to.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    let read = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size) };
    if read != size {
        return None;
    }
    let path = &info.pvi_cdir.vip_path;
    let bytes: Vec<u8> = path.iter().flatten().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
    (!bytes.is_empty()).then(|| PathBuf::from(String::from_utf8_lossy(&bytes).into_owned()))
}

/// Removes a worktree and its `worktree-<name>` branch. Without `force`, git refuses if it has
/// changes.
pub fn remove(tree: &Path, force: bool) -> Result<(), String> {
    let repo = repo_of(tree).ok_or("Not a worktree")?;
    let at = tree.to_string_lossy();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&at);
    // A branch checked out in another worktree can't be deleted, so it's left; the folder
    // still goes.
    let branch = own_branch(tree);
    let checked_out = format!("branch refs/heads/{branch}");
    let list = git(&repo, &["worktree", "list", "--porcelain"])?;
    let elsewhere = list.split("\n\n").any(|entry| {
        let at = entry.lines().find_map(|l| l.strip_prefix("worktree ")).map(Path::new);
        entry.lines().any(|l| l == checked_out) && at.is_some_and(|at| !same(at, tree))
    });
    git(&repo, &args)?;
    if branch_exists(&repo, &branch) && !elsewhere {
        git(&repo, &["branch", "-D", &branch])?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) -> String {
        git(dir, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    }

    /// A real repository with one commit and a subfolder.
    pub(crate) fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sessions-worktree-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/file"), "x").unwrap();
        run(&dir, &["init", "-q", "-b", "main"]);
        run(&dir, &["add", "."]);
        run(&dir, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "first"]);
        dir.canonicalize().unwrap()
    }

    #[test]
    fn a_named_worktree_is_made_on_its_own_branch_and_kept_out_of_the_repos_changes() {
        let dir = repo("named");
        let start = add(&dir, "Fix login!").unwrap();
        let path = dir.join(".claude/worktrees/Fix-login");
        assert_eq!(start, path);
        assert_eq!(run(&path, &["branch", "--show-current"]), "worktree-Fix-login");
        assert!(run(&dir, &["worktree", "list"]).contains(".claude/worktrees/Fix-login"));
        assert_eq!(run(&dir, &["status", "--porcelain"]), "", "the worktree isn't an untracked change in the repo");
        // Asking again picks the same one up rather than failing or making another.
        assert_eq!(add(&dir, "Fix-login").unwrap(), path);
        assert_eq!(std::fs::read_to_string(dir.join(".git/info/exclude")).unwrap().matches(".claude/worktrees/").count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_empty_name_gets_a_fresh_random_one() {
        let dir = repo("random");
        let a = add(&dir, "").unwrap();
        let b = add(&dir, " ").unwrap();
        assert_ne!(a, b);
        for path in [&a, &b] {
            let name = name_of(path).unwrap();
            assert_eq!(run(path, &["branch", "--show-current"]), format!("worktree-{name}"));
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn it_starts_where_the_folder_is_in_its_checkout_and_nests_under_the_main_repo() {
        let dir = repo("sub");
        let start = add(&dir.join("sub"), "a").unwrap();
        assert_eq!(start, dir.join(".claude/worktrees/a/sub"));
        assert!(start.join("file").is_file());
        // From inside a worktree, the next one still goes in the main checkout.
        assert_eq!(add(&start, "b").unwrap(), dir.join(".claude/worktrees/b/sub"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_existing_branch_is_checked_out_rather_than_made_again() {
        let dir = repo("branch");
        run(&dir, &["branch", "worktree-old"]);
        let start = add(&dir, "old").unwrap();
        assert_eq!(run(&start, &["branch", "--show-current"]), "worktree-old");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_folder_thats_not_a_worktree_of_this_repo_is_refused() {
        let dir = repo("taken");
        std::fs::create_dir_all(dir.join(".claude/worktrees/taken")).unwrap();
        assert!(add(&dir, "taken").unwrap_err().contains("already exists"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_deleted_worktrees_name_can_be_used_again() {
        let dir = repo("deleted");
        let path = add(&dir, "gone").unwrap();
        std::fs::remove_dir_all(&path).unwrap();
        assert_eq!(add(&dir, "gone").unwrap(), path);
        assert!(path.join("sub/file").is_file());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn from_a_folder_thats_not_checked_in_it_starts_at_the_top() {
        let dir = repo("untracked");
        std::fs::create_dir_all(dir.join("build")).unwrap();
        assert_eq!(add(&dir.join("build"), "u").unwrap(), dir.join(".claude/worktrees/u"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn gits_reason_is_reported_not_its_advice() {
        let dir = repo("reason");
        run(&dir, &["worktree", "add", "-q", "-b", "other", "elsewhere", "HEAD"]);
        std::fs::remove_dir_all(dir.join("elsewhere")).unwrap();
        // git says why on one line and what to do about it on the next.
        let error = git(&dir, &["worktree", "add", "elsewhere", "other"]).unwrap_err();
        assert!(error.contains("missing but already registered"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn outside_a_repository_it_says_so() {
        let dir = std::env::temp_dir().join(format!("sessions-worktree-{}-none", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(add(&dir, "x").unwrap_err(), "Not a git repository");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn worktrees_are_named_by_the_tools_layouts() {
        assert_eq!(name_of(Path::new("/Users/me/src/app/.claude/worktrees/x/sub")).as_deref(), Some("x"));
        assert_eq!(name_of(Path::new("/Users/me/.codex/worktrees/1a2b/app")).as_deref(), Some("1a2b"));
        assert_eq!(name_of(Path::new("/Users/me/src/app")), None);
        assert_eq!(repo_of(Path::new("/Users/me/src/app/.claude/worktrees/x")), Some(PathBuf::from("/Users/me/src/app")));
    }

    #[test]
    fn a_fresh_worktree_has_nothing_to_lose_until_it_changes_or_commits() {
        let dir = repo("unsaved");
        let tree = add(&dir, "w").unwrap();
        assert!(unsaved(&tree).unwrap().is_empty());
        std::fs::write(tree.join("new"), "x").unwrap();
        std::fs::write(tree.join("sub/file"), "changed").unwrap();
        assert_eq!(unsaved(&tree).unwrap(), Unsaved { files: 2, commits: 0 });
        run(&tree, &["add", "."]);
        run(&tree, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "work"]);
        assert_eq!(unsaved(&tree).unwrap(), Unsaved { files: 0, commits: 1 });
        // Once another branch has the commit, removing the worktree loses nothing.
        run(&dir, &["branch", "keep", "worktree-w"]);
        assert!(unsaved(&tree).unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn commits_on_a_branch_that_stays_arent_lost_but_detached_ones_are() {
        let dir = repo("switched");
        let tree = add(&dir, "w").unwrap();
        run(&tree, &["switch", "-q", "-c", "other"]);
        run(&tree, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "work"]);
        assert_eq!(unsaved(&tree).unwrap().commits, 0, "removing the worktree keeps `other`");
        run(&tree, &["switch", "-q", "--detach"]);
        run(&tree, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty", "-m", "loose"]);
        assert_eq!(unsaved(&tree).unwrap().commits, 1, "a detached commit has no branch to keep it");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn its_branch_checked_out_in_another_worktree_stays_and_the_folder_still_goes() {
        let dir = repo("elsewhere");
        let a = add(&dir, "a").unwrap();
        run(&a, &["switch", "-q", "-c", "moved"]);
        let b = add(&dir, "b").unwrap();
        run(&b, &["switch", "-q", "worktree-a"]);
        remove(&a, false).unwrap();
        assert!(!a.exists());
        assert!(branch_exists(&dir, "worktree-a"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn removing_takes_the_folder_and_its_branch_and_refuses_changes_unless_forced() {
        let dir = repo("remove");
        let tree = add(&dir, "w").unwrap();
        std::fs::write(tree.join("new"), "x").unwrap();
        assert!(remove(&tree, false).is_err());
        assert!(tree.is_dir());
        remove(&tree, true).unwrap();
        assert!(!tree.exists());
        assert!(!branch_exists(&dir, "worktree-w"));
        assert!(!run(&dir, &["worktree", "list"]).contains("worktree-w"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_checkouts_are_listed_as_worktrees() {
        let dir = repo("list");
        let tree = add(&dir, "w").unwrap();
        std::fs::create_dir_all(dir.join(".claude/worktrees/stray")).unwrap();
        assert_eq!(list(&dir), std::slice::from_ref(&tree));
        assert_eq!(top_of(&tree.join("sub")), Some(tree));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_process_working_in_a_worktree_makes_it_busy() {
        let dir = repo("busy");
        let tree = add(&dir, "w").unwrap();
        assert!(!busy(&tree));
        let mut inside = std::process::Command::new("/bin/sleep").arg("30").current_dir(tree.join("sub")).spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(busy(&tree));
        assert!(!busy(&dir.join(".claude/worktrees/other")), "only folders in it count");
        inside.kill().unwrap();
        inside.wait().unwrap();
        assert!(!busy(&tree));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn what_would_be_lost_reads_plainly() {
        assert_eq!(Unsaved { files: 1, commits: 0 }.describe(), "1 uncommitted file");
        assert_eq!(Unsaved { files: 2, commits: 3 }.describe(), "2 uncommitted files and 3 commits on no other branch");
    }
}
