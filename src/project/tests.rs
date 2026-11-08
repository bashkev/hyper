use super::*;
use rune_kit::diff::DiffMode;
use rune::{Env, Runtime};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Repo(PathBuf);
impl Repo {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("hyper-project-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&path).unwrap();
        let repo = Self(path);
        repo.git(&["init", "-q"]);
        repo.git(&["config", "user.email", "test@example.invalid"]);
        repo.git(&["config", "user.name", "Test"]);
        repo
    }
    fn git(&self, args: &[&str]) {
        let out = std::process::Command::new("git").arg("-C").arg(&self.0).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    fn write(&self, name: &str, value: &str) {
        let path = self.0.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, value).unwrap();
    }
    fn context(&self) -> Context {
        data::discover(&self.0).unwrap()
    }
    fn commit(&self) {
        self.git(&["add", "."]);
        self.git(&["commit", "-qm", "Initial"]);
    }
}
impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn changed<'a>(status: &'a Status, path: &str, area: Area) -> &'a Change {
    status.changes.iter().find(|c| c.path == Path::new(path) && c.area == area).unwrap()
}
fn patch(ctx: &Context, c: &Change) -> Vec<rune_kit::diff::FileDiff> {
    match data::diff(ctx, c).unwrap() {
        Preview::Diff(files) => files.into_iter().map(data::Patch::into_diff).collect(),
        _ => panic!("Expected a diff"),
    }
}

#[test]
fn git_keeps_staged_and_unstaged_versions_separate_and_handles_unusual_paths() {
    let repo = Repo::new();
    repo.write("src/file.rs", "original\n");
    repo.write("old name\n.rs", "rename me\n");
    repo.write("delete.rs", "gone\n");
    repo.commit();
    repo.write("src/file.rs", "staged\n");
    repo.git(&["add", "src/file.rs"]);
    repo.write("src/file.rs", "working\n");
    repo.git(&["mv", "--", "old name\n.rs", "-new name\n.rs"]);
    repo.write("-new name\n.rs", "working rename\n");
    std::fs::remove_file(repo.0.join("delete.rs")).unwrap();
    repo.write("new [*]\n.rs", "new file\n");
    let ctx = repo.context();
    let status = data::status(&ctx).unwrap();
    assert_eq!(status.changes.len(), 6);
    let staged = patch(&ctx, changed(&status, "src/file.rs", Area::Staged));
    let unstaged = patch(&ctx, changed(&status, "src/file.rs", Area::Unstaged));
    assert_eq!(staged[0].hunks[0].lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["original", "staged"]);
    assert_eq!(unstaged[0].hunks[0].lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>(), ["staged", "working"]);
    let rename = changed(&status, "-new name\n.rs", Area::Staged);
    assert_eq!(rename.old.as_deref(), Some(Path::new("old name\n.rs")));
    let renamed = patch(&ctx, rename);
    assert_eq!(renamed[0].status, rune_kit::diff::FileStatus::Renamed);
    let working_rename = patch(&ctx, changed(&status, "-new name\n.rs", Area::Unstaged));
    assert_eq!(working_rename[0].old_path.as_deref(), Some("-new name\n.rs"));
    assert_eq!(working_rename[0].hunks[0].lines.last().unwrap().text, "working rename");
    let new = patch(&ctx, changed(&status, "new [*]\n.rs", Area::Untracked));
    assert_eq!(new[0].additions(), 1);
    assert_eq!(new[0].path(), "new [*]\n.rs");
    let deleted = patch(&ctx, changed(&status, "delete.rs", Area::Unstaged));
    assert_eq!(deleted[0].deletions(), 1);
}

#[test]
fn explorer_obeys_git_ignores_without_hiding_tracked_files_or_dotfiles() {
    let repo = Repo::new();
    repo.write(".gitignore", "build/\n*.log\n");
    repo.write(".env.example", "MODE=dev\n");
    repo.write("build/ignored.rs", "hidden\n");
    repo.write("tracked.log", "visible\n");
    repo.git(&["add", "-f", "tracked.log"]);
    std::os::unix::fs::symlink(&repo.0, repo.0.join("loop")).unwrap();
    let ctx = repo.context();
    let entries = data::directory(&ctx, Path::new("")).unwrap();
    assert!(entries.iter().any(|e| e.name == "build" && e.ignored && e.dir));
    assert!(entries.iter().any(|e| e.name == "tracked.log" && !e.ignored));
    assert!(entries.iter().any(|e| e.name == ".env.example" && !e.ignored));
    assert!(entries.iter().any(|e| e.name == "loop" && !e.dir));
    assert!(data::filenames(&ctx, "ignored", false).unwrap().is_empty());
    assert_eq!(data::filenames(&ctx, "ignored", true).unwrap(), [PathBuf::from("build/ignored.rs")]);
}

#[test]
fn worktrees_and_unborn_repositories_use_their_own_index_and_root() {
    let repo = Repo::new();
    repo.write("src/file.rs", "first\n");
    repo.git(&["add", "."]);
    let ctx = repo.context();
    let status = data::status(&ctx).unwrap();
    assert_eq!(patch(&ctx, changed(&status, "src/file.rs", Area::Staged))[0].additions(), 1);
    repo.git(&["commit", "-qm", "Initial"]);
    let tree = repo.0.join("tree");
    repo.git(&["worktree", "add", "-qb", "other", tree.to_str().unwrap()]);
    std::fs::write(tree.join("src/file.rs"), "in worktree\n").unwrap();
    let ctx = data::discover(&tree.join("src")).unwrap();
    assert_eq!(ctx.root, tree.canonicalize().unwrap());
    let (dir, common) = ctx.git.as_ref().unwrap();
    assert_ne!(dir, common);
    let status = data::status(&ctx).unwrap();
    assert_eq!(status.branch, "other");
    assert_eq!(patch(&ctx, changed(&status, "src/file.rs", Area::Unstaged))[0].hunks[0].lines.last().unwrap().text, "in worktree");
    assert!(!data::status(&repo.context()).unwrap().changes.iter().any(|c| c.path == Path::new("src/file.rs")));
}

#[test]
fn previews_are_bounded_and_search_includes_untracked_text_at_the_right_line() {
    let repo = Repo::new();
    repo.write("src/ü.rs", "// café\r\n\tlet value = 42;\r\nneedle\n");
    let ctx = repo.context();
    let Preview::Code(code) = data::file(&ctx, Path::new("src/ü.rs")).unwrap() else { panic!() };
    assert_eq!(code.lines[1].text, "    let value = 42;");
    assert!(code.lines[1].highlights.iter().any(|(r, _)| &code.lines[1].text[r.clone()] == "let"));
    let hits = data::search(&ctx, "?needle", false).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].path, Path::new("src/ü.rs"));
    assert_eq!(hits[0].line, Some(3));
    std::fs::write(repo.0.join("binary"), [0, 1, 2]).unwrap();
    assert!(matches!(data::file(&ctx, Path::new("binary")).unwrap(), Preview::Note(s) if s == "Binary file"));
    let large = std::fs::File::create(repo.0.join("large")).unwrap();
    large.set_len(3 * 1024 * 1024).unwrap();
    assert!(matches!(data::file(&ctx, Path::new("large")).unwrap(), Preview::Note(s) if s.contains("exceeds")));
    let fifo = std::ffi::CString::new(repo.0.join("fifo").as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(matches!(data::file(&ctx, Path::new("fifo")).unwrap(), Preview::Note(s) if s.contains("regular")));
}

fn runtime(repo: &Repo) -> Runtime<App> {
    runtime_command(repo, vec!["/bin/sleep".into(), "120".into()])
}
fn runtime_command(repo: &Repo, argv: Vec<String>) -> Runtime<App> {
    let home = repo.0.join("home");
    let mut app = App::at(crate::data::Roots::under(&home), home);
    app.index_job.cancel();
    app.index = Load::Ready(Arc::new(Vec::new()));
    app.view = View::Workspace;
    app.settings.remember_project = false;
    app.launch_for_test(crate::workspace::Tool::Shell, repo.0.to_str().unwrap(), argv);
    let mut rt = Runtime::new(app, crate::window::view, (1600.0, 900.0), Env::new(true, 2.0));
    rt.poll = Some(crate::poll);
    rt.on_key = Some(crate::on_key);
    rt.frame(0.0);
    rt
}
fn until(rt: &mut Runtime<App>, done: impl Fn(&App) -> bool) {
    let start = std::time::Instant::now();
    while !done(&rt.state) {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "Timed out: context {:?}, watcher {}, error {:?}, directories {}, status {:?}",
            rt.state.project.ctx,
            rt.state.project.watcher.is_some(),
            rt.state.project.watch_error,
            rt.state.project.directories.len(),
            match &rt.state.project.status {
                Load::Ready(s) => format!("{} changes", s.changes.len()),
                Load::Failed(e) => e.clone(),
                _ => "waiting".into(),
            }
        );
        rt.frame(1.0 / 60.0);
        std::thread::sleep(Duration::from_millis(3));
    }
    for _ in 0..50 {
        rt.frame(1.0 / 60.0);
    }
}
fn click(rt: &mut Runtime<App>, key: &str) {
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("Missing {key}"));
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(1.0 / 60.0);
}

#[test]
fn file_preview_follows_content_size_and_spacing_live() {
    let repo = Repo::new();
    repo.write("a.rs", "let alpha = 1;\nlet beta = 2;\n");
    let mut rt = runtime(&repo);
    click(&mut rt, "files-toggle");
    until(&mut rt, |s| s.project.files.get(&data::key(Path::new("a.rs"))).is_some());
    click(&mut rt, &data::key(Path::new("a.rs")));
    until(&mut rt, |s| s.project.code.is_some());
    rt.update(|s, tx| {
        s.settings.appearance.content_size = 22.0;
        s.settings.appearance.line_spacing = 1.5;
        tx.instant();
    });
    for _ in 0..10 { rt.frame(1.0 / 60.0); }
    let rows = rt.elements();
    let line = rows.iter().find(|e| e.key.as_deref() == Some("code-line-0")).unwrap();
    assert_eq!(line.rect[3], 33.0, "file rows follow content size × line spacing");
    let text = rows.iter().find(|e| e.key.as_deref() == Some("code-text-0")).unwrap();
    assert_eq!(text.rect[3], 33.0, "the text itself, not just its row, resizes");
}

#[test]
fn files_are_lazy_tabs_stay_open_and_reopening_a_file_reuses_its_tab() {
    let repo = Repo::new();
    repo.write("src/a.rs", "let alpha = 1;\n");
    repo.write("src/b.rs", "let beta = 2;\n");
    let mut rt = runtime(&repo);
    let id = rt.state.current().unwrap().id;
    click(&mut rt, "files-toggle");
    until(&mut rt, |s| s.project.ctx.is_some());
    assert!(rt.find("terminal").is_some());
    assert_eq!(rt.state.current().unwrap().id, id);
    assert!(!rt.state.project.directories.contains_key(Path::new("src")), "Opening the pane must not walk subfolders");
    click(&mut rt, &data::key(Path::new("src")));
    until(&mut rt, |s| s.project.directories.contains_key(Path::new("src")));
    click(&mut rt, &data::key(Path::new("src/a.rs")));
    until(&mut rt, |s| s.project.code.is_some());
    assert_eq!(rt.state.project.code.as_ref().unwrap().lines[0].text, "let alpha = 1;");
    click(&mut rt, &data::key(Path::new("src/b.rs")));
    until(&mut rt, |s| s.project.code.is_some());
    assert_eq!(rt.state.project.tabs.len(), 2, "Opening another file preserves the first tab");
    assert_eq!(rt.state.project.code.as_ref().unwrap().lines[0].text, "let beta = 2;");
    click(&mut rt, &data::key(Path::new("src/a.rs")));
    until(&mut rt, |s| s.project.code.is_some());
    assert_eq!(rt.state.project.tabs.len(), 2, "Reopening a file selects its existing tab");
    click(&mut rt, "files-toggle");
    rt.frame(1.0 / 60.0);
    assert!(rt.state.project.watcher.is_none());
    assert!(rt.find("terminal").is_some());
    repo.write("src/a.rs", "changed while closed\n");
    click(&mut rt, "files-toggle");
    until(&mut rt, |s| s.project.code.as_ref().is_some_and(|c| c.lines[0].text == "changed while closed"));
    assert!(rt.state.project.files.is_expanded(&data::key(Path::new("src"))));
}

#[test]
fn diff_text_gutters_and_spacing_follow_content_settings_in_both_modes() {
    let repo = Repo::new();
    repo.write("a.rs", "let alpha = 1;\nlet beta = 2;\n");
    repo.commit();
    repo.write("a.rs", "let alpha = 3;\nlet beta = 2;\n");
    let mut rt = runtime(&repo);
    click(&mut rt, "changes-toggle");
    until(&mut rt, |s| matches!(&s.project.status, Load::Ready(status) if !status.changes.is_empty()));
    let key = match &rt.state.project.status {
        Load::Ready(status) => changed(status, "a.rs", Area::Unstaged).key(),
        _ => unreachable!(),
    };
    click(&mut rt, &key);
    until(&mut rt, |s| !s.project.diff.files().is_empty());
    click(&mut rt, "project-expand");
    for _ in 0..90 { rt.frame(1.0 / 60.0); }
    // Rebuild already-memoised rows, including a spacing-only change at the same font size.
    for (mode, size, spacing) in [(DiffMode::Unified, 22.0, 1.5), (DiffMode::Unified, 22.0, 2.0), (DiffMode::Split, 26.0, 1.5)] {
        rt.update(move |s, tx| {
            s.settings.appearance.content_size = size;
            s.settings.appearance.line_spacing = spacing;
            s.project.diff.set_mode(mode);
            tx.instant();
        });
        for _ in 0..10 { rt.frame(1.0 / 60.0); }
        let rows = rt.elements();
        let text = rows.iter().find(|e| e.path.contains("project-diff") && e.text.as_deref() == Some("let alpha = 3;"))
            .expect("the added code is rendered");
        assert_eq!(text.rect[3], size * spacing, "code follows the content settings in {mode:?}");
        let number = rows.iter().find(|e| e.path.contains("project-diff") && e.text.as_deref() == Some("1"))
            .expect("line numbers are rendered");
        assert_eq!(number.rect[3], text.rect[3], "numbers have the code's line height");
        let header = rows.iter().find(|e| e.path.contains("project-diff") && e.text.as_deref() == Some("a.rs"))
            .expect("the diff has a filename header");
        assert_eq!(header.rect[3], rt.state.settings.appearance.kit(true).ty.ui_medium.line_height, "headers follow interface typography");
    }
}

#[test]
fn native_events_update_files_and_git_without_a_refresh_loop() {
    let repo = Repo::new();
    repo.write("a.rs", "before\n");
    repo.commit();
    let mut rt = runtime(&repo);
    click(&mut rt, "changes-toggle");
    until(&mut rt, |s| s.project.watcher.is_some());
    assert!(rt.state.project.changes.is_empty());
    repo.write("a.rs", "after\n");
    until(&mut rt, |s| matches!(&s.project.status, Load::Ready(status) if status.changes.iter().any(|c| c.path == Path::new("a.rs"))));
    let key = changed(
        match &rt.state.project.status {
            Load::Ready(s) => s,
            _ => panic!(),
        },
        "a.rs",
        Area::Unstaged,
    )
    .key();
    click(&mut rt, &key);
    until(&mut rt, |s| !s.project.diff.files().is_empty());
    assert_eq!(rt.state.project.diff.files()[0].additions(), 1);
    repo.git(&["add", "a.rs"]);
    until(&mut rt, |s| matches!(&s.project.status, Load::Ready(status) if status.changes.iter().any(|c| c.area == Area::Staged)));
    assert!(!rt.state.project.status_job.is_pending());
}

#[test]
fn command_f_in_the_file_filter_opens_file_find_without_leaving_the_workspace() {
    let repo = Repo::new();
    repo.write("a.rs", "let alpha = 1;\n");
    let mut rt = runtime(&repo);
    click(&mut rt, "files-toggle");
    until(&mut rt, |s| s.project.ctx.is_some());
    click(&mut rt, &data::key(Path::new("a.rs")));
    until(&mut rt, |s| s.project.code.is_some());
    click(&mut rt, "project-filter");
    rt.key(KeyEvent { key: Key::Command('f'), mods: rune::Mods { cmd: true, ..Default::default() } });
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.view, View::Workspace, "The global session search must not take the file shortcut");
    assert!(rt.state.project.finding);
    assert!(rt.find("project-find").is_some());
    rt.key(KeyEvent { key: Key::Text("alpha".into()), mods: Default::default() });
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.project.matches.len(), 1);
    assert_eq!(rt.state.project.code.as_ref().unwrap().lines[0].text[rt.state.project.matches[0].1.clone()], *"alpha");
}

#[test]
fn switching_projects_cannot_install_an_older_projects_files_or_preview() {
    let first = Repo::new();
    first.write("first.rs", "first\n");
    let second = Repo::new();
    second.write("second.rs", "second\n");
    let mut rt = runtime(&first);
    click(&mut rt, "files-toggle");
    rt.update(|s, _| s.launch_for_test(crate::workspace::Tool::Shell, second.0.to_str().unwrap(), vec!["/bin/sleep".into(), "120".into()]));
    until(&mut rt, |s| s.project.ctx.as_ref().is_some_and(|ctx| ctx.root == second.0.canonicalize().unwrap()));
    assert!(rt.state.project.files.contains(&data::key(Path::new("second.rs"))));
    assert!(!rt.state.project.files.contains(&data::key(Path::new("first.rs"))));
    click(&mut rt, &data::key(Path::new("second.rs")));
    until(&mut rt, |s| s.project.code.is_some());
    assert_eq!(rt.state.project.code.as_ref().unwrap().lines[0].text, "second");
}

#[test]
fn the_native_close_menu_closes_the_preview_when_the_file_pane_has_the_keyboard() {
    let repo = Repo::new();
    repo.write("a.rs", "alpha\n");
    let mut rt = runtime(&repo);
    click(&mut rt, "files-toggle");
    until(&mut rt, |s| s.project.ctx.is_some());
    click(&mut rt, &data::key(Path::new("a.rs")));
    until(&mut rt, |s| s.project.code.is_some());
    rt.update(|s, tx| s.dispatch(crate::commands::Action::CloseSession, tx));
    assert_eq!(rt.state.ws.live.len(), 1, "The native menu must not close the terminal under the preview");
    assert!(rt.state.project.tabs.is_empty());
}

#[test]
fn expanded_project_fills_the_workspace_and_restores_its_previous_width() {
    let repo = Repo::new();
    repo.write("a.rs", "alpha\n");
    let mut rt = runtime(&repo);
    click(&mut rt, "files-toggle");
    until(&mut rt, |s| s.project.ctx.is_some());
    click(&mut rt, &data::key(Path::new("a.rs")));
    until(&mut rt, |s| s.project.code.is_some());
    let previous = rt.state.inspector.width();
    let settle = |rt: &mut Runtime<App>| {
        for _ in 0..180 {
            rt.frame(1.0 / 60.0);
        }
    };
    let fills = |rt: &Runtime<App>, left: f32, right: f32| {
        let ([x, _, w, _], _) = rt.presented("inspector-pane").unwrap();
        assert!((x - left).abs() < 0.5 && (x + w - right).abs() < 0.5, "Expected pane from {left} to {right}, got {x} to {}", x + w);
        assert!(rt.find("terminal").is_some(), "The terminal stays mounted while covered");
    };
    click(&mut rt, "project-expand");
    settle(&mut rt);
    fills(&rt, 300.0, 1600.0);
    click(&mut rt, "sidebar-toggle");
    settle(&mut rt);
    fills(&rt, 0.0, 1600.0);
    let ([toggle_x, _, toggle_w, _], _) = rt.presented("sidebar-toggle").unwrap();
    let ([tab_x, _, _, _], _) = rt.presented("project-tab-0").unwrap();
    assert!(tab_x >= toggle_x + toggle_w, "The file tab must clear the title-bar controls");
    rt.set_viewport((2560.0, 900.0), 2.0);
    settle(&mut rt);
    fills(&rt, 0.0, 2560.0);
    click(&mut rt, "sidebar-toggle");
    settle(&mut rt);
    fills(&rt, 300.0, 2560.0);
    click(&mut rt, "project-expand");
    settle(&mut rt);
    assert_eq!(rt.state.inspector.width(), previous);
}

#[test]
fn maximizing_the_file_pane_preserves_the_hidden_terminals_real_pty_size() {
    let repo = Repo::new();
    let size = repo.0.join("terminal-size");
    let script = format!("stty size > '{0}'; while IFS= read -r line; do stty size > '{0}'; done", size.display());
    let mut rt = runtime_command(&repo, vec!["/bin/sh".into(), "-c".into(), script]);
    until(&mut rt, |_| std::fs::read_to_string(&size).is_ok_and(|s| s.split_whitespace().count() == 2));
    click(&mut rt, "files-toggle");
    until(&mut rt, |s| s.project.ctx.is_some());
    let measure = |rt: &mut Runtime<App>| {
        std::fs::remove_file(&size).unwrap();
        rt.state.current().unwrap().term.text("measure\n");
        until(rt, |_| std::fs::read_to_string(&size).is_ok_and(|s| s.split_whitespace().count() == 2));
        std::fs::read_to_string(&size).unwrap()
    };
    let before = measure(&mut rt);
    click(&mut rt, "project-expand");
    for _ in 0..180 {
        rt.frame(1.0 / 60.0);
    }
    assert_eq!(measure(&mut rt), before, "A hidden terminal must retain its usable PTY dimensions");
}


fn cmd(c: char) -> KeyEvent {
    KeyEvent { key: Key::Command(c), mods: rune::Mods { cmd: true, ..Default::default() } }
}
/// Clicks a tab on its name, clear of its close button.
fn click_tab(rt: &mut Runtime<App>, i: usize) {
    let ([x, y, _, h], _) = rt.presented(&format!("project-tab-{i}")).unwrap();
    rt.pointer_down(x + 12.0, y + h / 2.0);
    rt.pointer_up(x + 12.0, y + h / 2.0);
    rt.frame(1.0 / 60.0);
}
fn open_file(rt: &mut Runtime<App>, path: &str) {
    click(rt, &data::key(Path::new(path)));
    until(rt, |s| s.project.code.is_some() && s.project.tab().is_some_and(|t| t.path == Path::new(path)));
}

#[test]
fn clicking_a_file_tab_after_the_terminal_gives_the_pane_the_keyboard() {
    let repo = Repo::new();
    repo.write("a.rs", "alpha\n");
    let mut rt = runtime(&repo);
    click(&mut rt, "files-toggle");
    until(&mut rt, |s| s.project.ctx.is_some());
    open_file(&mut rt, "a.rs");
    click(&mut rt, "terminal");
    let mut owned = true;
    rt.update(|s, tx| owned = s.project_has_keyboard(tx));
    assert!(!owned, "The terminal has the keyboard");
    let ([tab_x, _, tab_w, _], _) = rt.presented("project-tab-0").unwrap();
    let ([close_x, _, close_w, _], _) = rt.presented("project-tab-close-0").unwrap();
    assert!(tab_w < 100.0, "A short name's tab fits its name, not the strip: {tab_w}");
    assert!((tab_x + tab_w) - (close_x + close_w) < 4.0, "The close button ends the tab");
    click_tab(&mut rt, 0);
    assert_eq!(rt.state.project.tabs.len(), 1, "Clicking a tab's name must not close it");
    rt.key(cmd('f'));
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.view, View::Workspace, "⌘F finds in the file, not in sessions");
    assert!(rt.state.project.finding);
    rt.key(KeyEvent { key: Key::Escape, mods: Default::default() });
    click_tab(&mut rt, 0);
    rt.key(cmd('w'));
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.ws.live.len(), 1, "⌘W must not close the terminal");
    assert!(rt.state.project.tabs.is_empty(), "⌘W closes the clicked tab");
}

#[test]
fn shift_command_e_and_g_open_files_and_changes_by_default() {
    let repo = Repo::new();
    let mut rt = runtime(&repo);
    let press = |rt: &mut Runtime<App>, c: char| {
        rt.key(KeyEvent { key: Key::Command(c), mods: rune::Mods { cmd: true, shift: true, ..Default::default() } });
        rt.frame(1.0 / 60.0);
    };
    press(&mut rt, 'g');
    assert_eq!((rt.state.project.mode, rt.state.inspector.is_open()), (Mode::Changes, true), "⇧⌘G opens Changes");
    press(&mut rt, 'e');
    assert_eq!((rt.state.project.mode, rt.state.inspector.is_open()), (Mode::Files, true), "⇧⌘E switches to Files");
    press(&mut rt, 'e');
    assert!(!rt.state.inspector.is_open(), "⇧⌘E again closes it");
}

#[test]
fn remapped_shortcuts_are_honored_while_the_file_pane_has_the_keyboard() {
    let repo = Repo::new();
    repo.write("a.rs", "alpha\n");
    let mut rt = runtime(&repo);
    rt.update(|s, _| {
        s.settings.shortcuts.insert("search".into(), "cmd+shift+f".into());
        s.settings.shortcuts.insert("toggle_sidebar".into(), "cmd+f".into());
    });
    click(&mut rt, "files-toggle");
    until(&mut rt, |s| s.project.ctx.is_some());
    open_file(&mut rt, "a.rs");
    let sidebar = rt.state.sidebar.is_open();
    rt.key(cmd('f'));
    rt.frame(1.0 / 60.0);
    assert!(!rt.state.project.finding, "⌘F is Toggle Sidebar now");
    assert_ne!(rt.state.sidebar.is_open(), sidebar);
    rt.key(KeyEvent { key: Key::Command('f'), mods: rune::Mods { cmd: true, shift: true, ..Default::default() } });
    rt.frame(1.0 / 60.0);
    assert!(rt.state.project.finding, "⇧⌘F is Search, which finds in the file here");
}

#[test]
fn expanding_context_never_shows_lines_the_diff_does_not_have() {
    let repo = Repo::new();
    let lines: String = (1..=40).map(|n| format!("line {n}\n")).collect();
    repo.write("a.rs", &lines);
    repo.commit();
    repo.write("a.rs", &lines.replace("line 20\n", "changed\n"));
    let ctx = repo.context();
    let status = data::status(&ctx).unwrap();
    let files = patch(&ctx, changed(&status, "a.rs", Area::Unstaged));
    assert!(!files[0].expandable(), "A parsed patch has only its hunks' lines");
}

#[test]
fn submodule_folders_list_their_files() {
    let sub = Repo::new();
    sub.write("a.rs", "in sub\n");
    sub.write(".gitignore", "*.log\n");
    sub.commit();
    let repo = Repo::new();
    repo.write("top.rs", "top\n");
    repo.git(&["-c", "protocol.file.allow=always", "submodule", "add", "-q", sub.0.to_str().unwrap(), "sub"]);
    repo.commit();
    repo.write("sub/x.log", "ignored in sub\n");
    let ctx = repo.context();
    let top = data::directory(&ctx, Path::new("")).unwrap();
    assert!(top.iter().any(|e| e.name == "sub" && e.dir && !e.ignored));
    let inside = data::directory(&ctx, Path::new("sub")).expect("A submodule lists with its own Git context");
    assert!(inside.iter().any(|e| e.path == Path::new("sub/a.rs") && !e.ignored));
    assert!(inside.iter().any(|e| e.path == Path::new("sub/x.log") && e.ignored), "The submodule's own ignores apply");
}

#[test]
fn reopening_files_reapplies_the_filter_to_what_changed_while_closed() {
    let repo = Repo::new();
    repo.write("alpha.rs", "a\n");
    let mut rt = runtime(&repo);
    click(&mut rt, "files-toggle");
    until(&mut rt, |s| s.project.watcher.is_some());
    click(&mut rt, "project-filter");
    rt.key(KeyEvent { key: Key::Text("beta".into()), mods: Default::default() });
    until(&mut rt, |s| matches!(&s.project.search_paths, Some(Ok(p)) if p.is_empty()) && !s.project.search.is_pending());
    click(&mut rt, "files-toggle");
    repo.write("beta.rs", "b\n");
    click(&mut rt, "files-toggle");
    until(&mut rt, |s| s.project.files.contains(&format!("{}:0", data::key(Path::new("beta.rs")))));
}

#[test]
fn restoring_an_expanded_pane_lets_it_widen_with_the_window_again() {
    let repo = Repo::new();
    repo.write("a.rs", "alpha\n");
    let mut rt = runtime(&repo);
    rt.set_viewport((720.0, 800.0), 2.0);
    rt.frame(1.0 / 60.0);
    click(&mut rt, "files-toggle");
    until(&mut rt, |s| s.project.ctx.is_some());
    click(&mut rt, "project-expand");
    for _ in 0..60 { rt.frame(1.0 / 60.0); }
    click(&mut rt, "project-expand");
    rt.set_viewport((1600.0, 900.0), 2.0);
    for _ in 0..60 { rt.frame(1.0 / 60.0); }
    rt.update(|s, _| s.inspector.set_width(800.0));
    assert_eq!(rt.state.inspector.width(), 800.0, "Capped at {}", rt.state.inspector.max_width());
}

#[test]
fn closing_all_changes_keeps_open_files_and_live_edits_do_not_blank_the_preview() {
    let repo = Repo::new();
    repo.write("a.rs", "alpha\n");
    repo.write("b.rs", "beta\n");
    repo.commit();
    repo.write("b.rs", "beta 2\n");
    let mut rt = runtime(&repo);
    click(&mut rt, "files-toggle");
    until(&mut rt, |s| s.project.watcher.is_some());
    open_file(&mut rt, "a.rs");
    repo.write("a.rs", "alpha 2\n");
    let start = std::time::Instant::now();
    while !rt.state.project.code.as_ref().is_some_and(|c| c.lines[0].text == "alpha 2") {
        assert!(rt.state.project.code.is_some(), "A live edit must keep the old text until the new text arrives");
        assert!(start.elapsed() < Duration::from_secs(10));
        rt.frame(1.0 / 60.0);
        std::thread::sleep(Duration::from_millis(3));
    }
    click_tab(&mut rt, 0);
    assert!(rt.state.project.code.is_some(), "Clicking the active tab keeps its preview");
    click(&mut rt, "changes-toggle");
    until(&mut rt, |s| matches!(s.project.status, Load::Ready(_)) && s.project.changes.len() > 1);
    let key = rt.state.project.changes.visible_id(1).unwrap().to_owned();
    click(&mut rt, &key);
    until(&mut rt, |s| !s.project.diff.files().is_empty());
    rt.update(|s, tx| s.project_tab_action("all", 0, tx));
    assert_eq!(rt.state.project.tabs.len(), 1, "Close All in Changes keeps the Files tab");
    assert_eq!(rt.state.project.tabs[0].path, Path::new("a.rs"));
}

#[test]
fn the_browser_takes_the_keyboard_from_the_terminal_and_survives_a_reload() {
    let repo = Repo::new();
    let mut rt = runtime(&repo);
    click(&mut rt, "browser-toggle");
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.project.mode, Mode::Browser);
    assert!(rt.state.browser.address.focused, "An empty browser opens ready for an address");
    assert!(rt.state.browser_has_keyboard());
    let mut files = true;
    rt.update(|s, tx| files = s.project_has_keyboard(tx));
    assert!(!files, "The browser isn't the file pane");

    rt.key(KeyEvent { key: Key::Text("typo".into()), mods: Default::default() });
    rt.key(KeyEvent { key: Key::Escape, mods: Default::default() });
    rt.frame(1.0 / 60.0);
    assert!(rt.state.browser.address.text.is_empty() && !rt.state.browser.address.focused, "Escape drops what was typed");
    rt.key(cmd('l'));
    rt.key(KeyEvent { key: Key::Text("3000".into()), mods: Default::default() });
    rt.key(KeyEvent { key: Key::Enter, mods: Default::default() });
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.browser.url(), "http://localhost:3000", "A bare port is a local server");

    rt.key(cmd('l'));
    rt.key(cmd('f'));
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.view, View::Workspace, "⌘F finds in the page, not in sessions");
    assert!(rt.state.browser_has_keyboard() && !rt.state.browser.address.focused, "The find bar has the keyboard");
    assert!(rt.find("browser-find").is_some());
    rt.key(KeyEvent { key: Key::Escape, mods: Default::default() });
    rt.frame(1.0 / 60.0);
    assert!(rt.find("browser-find").is_none(), "Escape closes the find bar");
    assert!(!rt.state.browser_has_keyboard());

    click(&mut rt, "terminal");
    assert!(!rt.state.browser_has_keyboard(), "Clicking the terminal takes the keyboard back");
    let shift_cmd_l = KeyEvent { key: Key::Command('l'), mods: rune::Mods { cmd: true, shift: true, ..Default::default() } };
    rt.key(shift_cmd_l.clone());
    rt.frame(1.0 / 60.0);
    assert!(!rt.state.inspector.is_open(), "⇧⌘L hides the page");
    rt.key(shift_cmd_l.clone());
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.browser.url(), "http://localhost:3000", "⇧⌘L shows the same page again");
    assert!(!rt.state.browser_has_keyboard(), "A page to show doesn't take the keyboard");

    let handed = rt.state.browser_hand_off();
    let mut next = runtime(&repo);
    next.update(|s, _| s.adopt_browser(&handed));
    next.frame(1.0 / 60.0);
    assert_eq!(next.state.browser.url(), "http://localhost:3000");
    assert!(next.state.browser_open(), "A reload keeps the browser open");

    rt.key(cmd('l'));
    rt.frame(1.0 / 60.0);
    assert!(rt.state.browser.address.focused, "⌘L goes to the address");
    rt.key(cmd('w'));
    rt.frame(1.0 / 60.0);
    assert!(!rt.state.inspector.is_open(), "⌘W in the browser closes it");
    assert!(rt.state.browser.url().is_empty(), "⌘W closes the page too");
    assert_eq!(rt.state.ws.live.len(), 1, "⌘W in the browser must not close the terminal");
    rt.key(shift_cmd_l);
    rt.frame(1.0 / 60.0);
    assert!(rt.state.browser_open() && rt.state.browser.address.focused, "The next one starts empty, ready for an address");
}

#[test]
fn command_clicking_a_link_in_a_terminal_opens_it_where_settings_say() {
    let repo = Repo::new();
    let argv = ["/bin/sh", "-c", "printf 'see https://example.com/docs now'; sleep 60"].map(String::from).to_vec();
    let mut rt = runtime_command(&repo, argv);
    let shows = |s: &App| s.current().and_then(|l| l.term.frame.as_ref()).is_some_and(|f| f.lines.iter().any(|l| l.runs.iter().any(|r| r.text.contains("example"))));
    until(&mut rt, shows);
    let (cell_w, cell_h) = crate::term::cell(crate::term::style(&rt.state.settings.appearance.kit(true)));
    let ([x, y, _, _], _) = rt.presented("terminal").unwrap();
    // Column 8 is inside the URL, which starts at column 4.
    let (at_x, at_y) = (x + crate::term::INSET + cell_w * 8.5, y + crate::term::INSET + cell_h * 0.5);
    let click = |rt: &mut Runtime<App>, cmd: bool| {
        rt.physical_input(rune::PhysicalInput::Modifiers(rune::Mods { cmd, ..Default::default() }));
        rt.pointer_down(at_x, at_y);
        rt.pointer_up(at_x, at_y);
        rt.frame(1.0 / 60.0);
    };

    click(&mut rt, false);
    assert!(rt.take_url_requests().is_empty() && !rt.state.inspector.is_open(), "A plain click selects, it doesn't open");

    rt.state.settings.links = crate::settings::Links::Browser;
    click(&mut rt, true);
    assert_eq!(rt.take_url_requests(), ["https://example.com/docs"]);
    assert!(!rt.state.inspector.is_open(), "The default browser has it, not the pane");

    rt.state.settings.links = crate::settings::Links::Hyper;
    click(&mut rt, true);
    assert!(rt.take_url_requests().is_empty());
    assert!(rt.state.inspector.is_open() && rt.state.project.mode == Mode::Browser);
    assert_eq!(rt.state.browser.url(), "https://example.com/docs");
}

#[test]
fn showing_a_session_takes_the_keyboard_back_from_the_find_bar() {
    let repo = Repo::new();
    let mut rt = runtime(&repo);
    rt.key(cmd('l'));
    rt.key(cmd('f'));
    rt.frame(1.0 / 60.0);
    assert!(rt.state.browser_has_keyboard());
    let id = rt.state.ws.live[0].id;
    click(&mut rt, &format!("live-{id}"));
    assert!(!rt.state.browser_has_keyboard(), "Picking the session in the sidebar gives it the keyboard");
}

#[test]
fn a_reload_keeps_the_keyboard_where_it_was_with_an_empty_browser_open() {
    let repo = Repo::new();
    let mut rt = runtime(&repo);
    rt.update(|s, _| s.adopt_browser(&serde_json::json!({ "url": "", "open": true })));
    rt.frame(1.0 / 60.0);
    assert!(rt.state.browser_open());
    assert!(!rt.state.browser_has_keyboard(), "The terminal keeps the keyboard across a reload");
}
