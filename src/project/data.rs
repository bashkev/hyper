//! Bounded, read-only filesystem and Git work. Called on Rune's worker pool.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use rune_kit::{
    diff::{self, FileDiff, FileStatus, Hunk},
    syntax::{self, Highlight},
};

const FILE_LIMIT: usize = 2 * 1024 * 1024;
const GIT_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Context {
    pub root: PathBuf,
    pub git: Option<(PathBuf, PathBuf)>,
}

fn command(root: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("--no-optional-locks")
        .arg("-C")
        .arg(root)
        .env_remove("GIT_LITERAL_PATHSPECS")
        .env_remove("GIT_GLOB_PATHSPECS")
        .env_remove("GIT_NOGLOB_PATHSPECS")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    c
}

/// Read pipes concurrently: large path lists cannot deadlock against check-ignore's output.
fn run(mut c: Command, input: Option<Vec<u8>>) -> Result<(ExitStatus, Vec<u8>, String), String> {
    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    c.stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = c.spawn().map_err(|e| format!("Could not run Git: {e}"))?;
    let writer = input.map(|input| {
        let mut stdin = child.stdin.take().unwrap();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&input);
        })
    });
    let mut stderr = child.stderr.take().unwrap();
    let errors = std::thread::spawn(move || {
        let mut first = Vec::new();
        let _ = stderr.by_ref().take(8192).read_to_end(&mut first);
        let _ = std::io::copy(&mut stderr, &mut std::io::sink());
        String::from_utf8_lossy(&first).trim().to_owned()
    });
    let mut bytes = Vec::new();
    let read = child.stdout.take().unwrap().take(GIT_LIMIT as u64 + 1).read_to_end(&mut bytes);
    if bytes.len() > GIT_LIMIT || read.is_err() {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if let Some(writer) = writer {
        let _ = writer.join();
    }
    let error = errors.join().unwrap_or_default();
    read.map_err(|e| e.to_string())?;
    if bytes.len() > GIT_LIMIT {
        return Err("Too large to preview. Open in Editor.".into());
    }
    Ok((status, bytes, error))
}

fn git(root: &Path, args: &[&OsStr]) -> Result<Vec<u8>, String> {
    let mut c = command(root);
    c.args(args);
    let (status, bytes, error) = run(c, None)?;
    if status.success() { Ok(bytes) } else { Err(if error.is_empty() { "Git could not read this repository".into() } else { error }) }
}

fn path_line(bytes: Vec<u8>) -> PathBuf {
    let mut bytes = bytes;
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    PathBuf::from(OsString::from_vec(bytes))
}

pub fn discover(cwd: &Path) -> Result<Context, String> {
    if !cwd.is_dir() {
        return Err("Project folder is unavailable".into());
    }
    let Ok(root) = git(cwd, &[OsStr::new("rev-parse"), OsStr::new("--show-toplevel")]) else {
        return Ok(Context { root: cwd.canonicalize().unwrap_or_else(|_| cwd.into()), git: None });
    };
    let root = path_line(root);
    let dir = path_line(git(&root, &[OsStr::new("rev-parse"), OsStr::new("--absolute-git-dir")])?);
    let common = path_line(git(&root, &[OsStr::new("rev-parse"), OsStr::new("--path-format=absolute"), OsStr::new("--git-common-dir")])?);
    Ok(Context { root, git: Some((dir, common)) })
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub dir: bool,
    pub ignored: bool,
}

/// One level only. Symlink directories remain leaves, so loops and other projects aren't walked.
pub fn directory(ctx: &Context, relative: &Path) -> Result<Vec<Entry>, String> {
    let mut entries = Vec::new();
    for e in std::fs::read_dir(ctx.root.join(relative)).map_err(|e| e.to_string())? {
        let e = e.map_err(|e| e.to_string())?;
        let kind = e.file_type().map_err(|e| e.to_string())?;
        entries.push(Entry {
            path: relative.join(e.file_name()),
            name: e.file_name().to_string_lossy().into_owned(),
            dir: kind.is_dir(),
            ignored: false,
        });
    }
    if ctx.git.is_some() && !entries.is_empty() {
        // Inside a submodule or nested repository, its own Git answers: the outer one refuses
        // paths in a submodule, and its ignore rules don't apply there.
        let repo = relative
            .ancestors()
            .filter(|a| !a.as_os_str().is_empty())
            .find(|a| ctx.root.join(a).join(".git").exists())
            .unwrap_or(Path::new(""));
        let mut input = Vec::new();
        for e in &entries {
            input.extend_from_slice(e.path.strip_prefix(repo).unwrap_or(&e.path).as_os_str().as_bytes());
            if e.dir {
                input.push(b'/');
            }
            input.push(0);
        }
        let mut c = command(&ctx.root.join(repo));
        c.args(["check-ignore", "--stdin", "-z"]);
        let (status, output, error) = run(c, Some(input))?;
        if !status.success() && status.code() != Some(1) {
            return Err(error);
        }
        let ignored: HashSet<PathBuf> = output
            .split(|&b| b == 0)
            .filter(|b| !b.is_empty())
            .map(|b| repo.join(OsStr::from_bytes(b)))
            .map(|p| p.to_str().and_then(|s| s.strip_suffix('/')).map(PathBuf::from).unwrap_or(p))
            .collect();
        for e in &mut entries {
            e.ignored = ignored.contains(&e.path) || e.name == ".git";
        }
    }
    entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| rune_kit::natural_cmp(&a.name, &b.name)));
    Ok(entries)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Area {
    Staged,
    Unstaged,
    Untracked,
}

impl Area {
    pub fn label(self) -> &'static str {
        match self {
            Self::Staged => "Staged",
            Self::Unstaged => "Changes",
            Self::Untracked => "Untracked",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    pub path: PathBuf,
    pub old: Option<PathBuf>,
    pub area: Area,
    pub status: char,
}

impl Change {
    pub fn key(&self) -> String {
        format!("{:?}:{}", self.area, key(&self.path))
    }
}

/// Raw path bytes make UI identities lossless even for filenames that aren't UTF-8.
pub fn key(path: &Path) -> String {
    use std::fmt::Write;
    let mut key = String::with_capacity(path.as_os_str().len() * 2);
    for byte in path.as_os_str().as_bytes() {
        let _ = write!(key, "{byte:02x}");
    }
    key
}

#[derive(Default)]
pub struct Status {
    pub branch: String,
    pub changes: Vec<Change>,
}

pub fn status(ctx: &Context) -> Result<Status, String> {
    let bytes = git(
        &ctx.root,
        &[
            OsStr::new("status"),
            OsStr::new("--porcelain=v2"),
            OsStr::new("--branch"),
            OsStr::new("-z"),
            OsStr::new("--untracked-files=all"),
        ],
    )?;
    parse_status(&bytes)
}

fn parse_status(bytes: &[u8]) -> Result<Status, String> {
    let mut out = Status::default();
    let mut records = bytes.split(|&b| b == 0).filter(|r| !r.is_empty());
    while let Some(r) = records.next() {
        if let Some(branch) = r.strip_prefix(b"# branch.head ") {
            out.branch = String::from_utf8_lossy(branch).into_owned();
            continue;
        }
        if r[0] == b'#' {
            continue;
        }
        if r.starts_with(b"? ") {
            out.changes.push(Change {
                path: PathBuf::from(OsString::from_vec(r[2..].to_vec())),
                old: None,
                area: Area::Untracked,
                status: '?',
            });
            continue;
        }
        let n = match r[0] {
            b'1' => 9,
            b'2' => 10,
            b'u' => 11,
            _ => continue,
        };
        let fields: Vec<_> = r.splitn(n, |&b| b == b' ').collect();
        if fields.len() != n || fields[1].len() != 2 {
            return Err("Could not read Git status".into());
        }
        let path = PathBuf::from(OsString::from_vec(fields[n - 1].to_vec()));
        let old = if r[0] == b'2' {
            Some(PathBuf::from(OsString::from_vec(records.next().ok_or("Incomplete Git rename")?.to_vec())))
        } else {
            None
        };
        if r[0] == b'u' {
            out.changes.push(Change { path, old, area: Area::Unstaged, status: 'U' });
            continue;
        }
        for (i, area) in [Area::Staged, Area::Unstaged].into_iter().enumerate() {
            let status = fields[1][i];
            if status != b'.' {
                out.changes.push(Change { path: path.clone(), old: old.clone(), area, status: char::from(status) });
            }
        }
    }
    out.changes.sort_by(|a, b| rune_kit::natural_cmp(&a.path.to_string_lossy(), &b.path.to_string_lossy()));
    Ok(out)
}

pub struct CodeLine {
    pub text: String,
    pub highlights: Vec<Highlight>,
}
pub struct Code {
    pub lines: Vec<CodeLine>,
    pub columns: usize,
}

// FileDiff contains Rc text storage. Transfer its patch data, then attach the UI state on the UI thread.
pub struct Patch {
    pub old: Option<String>,
    pub new: Option<String>,
    pub status: FileStatus,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    pub similarity: Option<u8>,
}
impl Patch {
    pub fn into_diff(self) -> FileDiff {
        // Built like a parsed patch, without whole source texts, so the view only offers the
        // lines the patch has. `from_texts` would claim complete (empty) files instead.
        let mut f = diff::parse("diff --git a/_ b/_\n").pop().expect("a header parses to a file");
        f.old_path = self.old;
        f.new_path = self.new;
        f.status = self.status;
        f.binary = self.binary;
        f.hunks = self.hunks;
        f.old_mode = self.old_mode;
        f.new_mode = self.new_mode;
        f.similarity = self.similarity;
        f
    }
}

pub enum Preview {
    Code(Code),
    Diff(Vec<Patch>),
    Note(String),
}

pub fn file(ctx: &Context, path: &Path) -> Result<Preview, String> {
    let f = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(ctx.root.join(path)).map_err(|e| e.to_string())?;
    if !f.metadata().map_err(|e| e.to_string())?.is_file() {
        return Ok(Preview::Note("Not a regular file".into()));
    }
    let mut bytes = Vec::new();
    f.take(FILE_LIMIT as u64 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > FILE_LIMIT {
        return Ok(Preview::Note("File exceeds 2 MB. Open in Editor.".into()));
    }
    if bytes.contains(&0) {
        return Ok(Preview::Note("Binary file".into()));
    }
    let Ok(source) = String::from_utf8(bytes) else {
        return Ok(Preview::Note("Binary or non-UTF-8 file".into()));
    };
    let source = source.replace('\t', "    ");
    if source.lines().any(|l| l.len() > 16_384) {
        return Ok(Preview::Note("Line too long to preview. Open in Editor.".into()));
    }
    let lang = path.extension().and_then(OsStr::to_str).or_else(|| path.file_name().and_then(OsStr::to_str));
    let hl = syntax::builtin(lang, &source);
    let mut lines = Vec::new();
    let (mut offset, mut cursor, mut columns) = (0, 0, 0);
    for line in source.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let end = offset + line.len();
        while cursor < hl.len() && hl[cursor].0.end <= offset {
            cursor += 1;
        }
        let highlights = hl[cursor..]
            .iter()
            .take_while(|(r, _)| r.start < end)
            .map(|(r, p)| (r.start.max(offset) - offset..r.end.min(end) - offset, *p))
            .collect();
        columns = columns.max(line.chars().count());
        lines.push(CodeLine { text: line.into(), highlights });
        offset = end + if source.as_bytes().get(end) == Some(&b'\r') { 2 } else { 1 };
    }
    Ok(Preview::Code(Code { lines, columns }))
}

pub fn diff(ctx: &Context, change: &Change) -> Result<Preview, String> {
    let mut c = command(&ctx.root);
    c.env("GIT_LITERAL_PATHSPECS", "1");
    if change.area == Area::Untracked {
        let meta = std::fs::symlink_metadata(ctx.root.join(&change.path)).map_err(|e| e.to_string())?;
        if meta.len() > FILE_LIMIT as u64 {
            return Ok(Preview::Note("File exceeds 2 MB. Open in Editor.".into()));
        }
        if !meta.is_file() && !meta.file_type().is_symlink() {
            return Ok(Preview::Note("Not a regular file".into()));
        }
    }
    c.args(["diff", "--no-ext-diff", "--no-textconv", "--no-color", "--no-relative", "--src-prefix=a/", "--dst-prefix=b/", "--unified=3"]);
    match change.area {
        Area::Staged => {
            c.args(["--cached", "--find-renames"]);
        }
        Area::Untracked => {
            c.arg("--no-index");
        }
        Area::Unstaged if change.status == 'U' => {
            c.arg("--base");
        }
        _ => {}
    }
    c.arg("--");
    if change.area == Area::Untracked {
        c.arg("/dev/null");
    }
    if let Some(old) = &change.old {
        c.arg(old);
    }
    c.arg(&change.path);
    let (status, bytes, error) = run(c, None)?;
    if !status.success() && !(change.area == Area::Untracked && status.code() == Some(1)) {
        return Err(error);
    }
    let patch = String::from_utf8_lossy(&bytes);
    if patch.lines().any(|line| line.len() > 16_384) {
        return Ok(Preview::Note("Line too long to preview. Open in Editor.".into()));
    }
    let mut files = diff::parse(&patch);
    if files.is_empty() {
        return Ok(Preview::Note(if change.status == 'U' { "Unmerged file" } else { "No text changes" }.into()));
    }
    let files = files
        .drain(..)
        .map(|f| Patch {
            old: f.old_path.map(|_| {
                let path = if change.area == Area::Staged { change.old.as_ref().unwrap_or(&change.path) } else { &change.path };
                path.to_string_lossy().into_owned()
            }),
            new: f.new_path.map(|_| change.path.to_string_lossy().into_owned()),
            status: f.status,
            binary: f.binary,
            hunks: f.hunks,
            old_mode: f.old_mode,
            new_mode: f.new_mode,
            similarity: f.similarity,
        })
        .collect();
    Ok(Preview::Diff(files))
}

pub fn filenames(ctx: &Context, query: &str, ignored: bool) -> Result<Vec<PathBuf>, String> {
    let mut paths = if ctx.git.is_some() {
        let mut c = command(&ctx.root);
        c.args(["ls-files", "--cached", "--others", "-z"]);
        if !ignored {
            c.arg("--exclude-standard");
        }
        let (status, bytes, error) = run(c, None)?;
        if !status.success() {
            return Err(error);
        }
        bytes.split(|&b| b == 0).filter(|b| !b.is_empty()).map(|b| PathBuf::from(OsString::from_vec(b.to_vec()))).collect::<Vec<_>>()
    } else {
        let mut out = Vec::new();
        let mut stack = vec![PathBuf::new()];
        while let Some(dir) = stack.pop() {
            for e in directory(ctx, &dir)? {
                if e.dir {
                    stack.push(e.path);
                } else {
                    out.push(e.path);
                }
                if out.len() > 100_000 {
                    return Err("Too many files. Narrow the project folder.".into());
                }
            }
        }
        out
    };
    let query = query.to_lowercase();
    paths.retain(|p| p.to_string_lossy().to_lowercase().contains(&query));
    paths.sort_by(|a, b| rune_kit::natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
    paths.dedup();
    paths.truncate(1000);
    Ok(paths)
}

#[derive(Clone)]
pub struct SearchHit {
    pub path: PathBuf,
    pub line: Option<usize>,
}

pub fn search(ctx: &Context, query: &str, ignored: bool) -> Result<Vec<SearchHit>, String> {
    let Some(query) = query.strip_prefix('?') else {
        return filenames(ctx, query, ignored).map(|paths| paths.into_iter().map(|path| SearchHit { path, line: None }).collect());
    };
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut c = command(&ctx.root);
    c.args(["grep", "--no-color", "-n", "-z", "-I", "-i", "-F", "--max-count=20"]);
    if ctx.git.is_some() {
        c.arg("--untracked");
        if ignored {
            c.arg("--no-exclude-standard");
        }
    } else {
        c.arg("--no-index");
    }
    c.arg("-e").arg(query).arg("--").arg(".");
    let (status, bytes, error) = run(c, None)?;
    if !status.success() && status.code() != Some(1) {
        return Err(error);
    }
    let mut rest = bytes.as_slice();
    let mut hits = Vec::new();
    while !rest.is_empty() && hits.len() < 1000 {
        let Some(end) = rest.iter().position(|&b| b == 0) else { break };
        let path = PathBuf::from(OsString::from_vec(rest[..end].to_vec()));
        rest = &rest[end + 1..];
        let Some(end) = rest.iter().position(|&b| b == 0) else { break };
        let line = std::str::from_utf8(&rest[..end]).ok().and_then(|s| s.parse::<usize>().ok());
        rest = &rest[end + 1..];
        let Some(end) = rest.iter().position(|&b| b == b'\n') else { break };
        rest = &rest[end + 1..];
        hits.push(SearchHit { path: path.strip_prefix("./").unwrap_or(&path).to_owned(), line });
    }
    Ok(hits)
}
