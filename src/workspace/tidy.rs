//! Worktrees tidy up after themselves: when the last terminal in one closes, it's removed if
//! nothing would be lost, and the person is asked first if something would.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rune::{El, row, text};
use rune_kit::{Button, ButtonVariant, Dialog, Kit};

use crate::{App, worktree};

/// News of a worktree whose last terminal closed.
pub(super) enum Tidy {
    /// What removing it would lose, or that git couldn't say.
    Checked(PathBuf, Result<worktree::Unsaved, String>),
    Removed(PathBuf, Result<(), String>),
    /// Something is working in it, such as an agent started elsewhere, so it stays.
    Busy(PathBuf),
}

impl App {
    /// Whether a terminal is running in this worktree.
    pub(super) fn in_use(&self, tree: &Path) -> bool {
        self.ws.live.iter().any(|l| worktree::top_of(&l.cwd).as_deref() == Some(tree))
    }

    /// Once a worktree's last terminal closes, finds out what removing it would lose.
    pub(super) fn tidy_after(&mut self, cwd: &Path) {
        let Some(tree) = worktree::top_of(cwd) else { return };
        if self.in_use(&tree) || !tree.is_dir() || self.ws.tidying.contains(&tree) || self.ws.asking.iter().any(|(t, _)| *t == tree) {
            return;
        }
        self.ws.tidying.insert(tree.clone());
        self.in_background(move || {
            // The terminal just closed is still being stopped; give it a moment to go.
            let mut waits = 0;
            while worktree::busy(&tree) {
                if waits == 20 {
                    return Tidy::Busy(tree);
                }
                waits += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            let found = worktree::unsaved(&tree);
            Tidy::Checked(tree, found)
        });
    }

    fn remove_worktree(&mut self, tree: PathBuf, force: bool) {
        if !self.ws.tidying.insert(tree.clone()) {
            return;
        }
        self.ws.removing.insert(tree.clone());
        // It's no longer offered in the launcher.
        self.relist();
        self.in_background(move || {
            // Something may have started in it since it was checked.
            if worktree::busy(&tree) {
                Tidy::Busy(tree)
            } else {
                let done = worktree::remove(&tree, force);
                Tidy::Removed(tree, done)
            }
        });
    }

    /// Works on a worktree away from the interface, which `take_tidy` hears from.
    fn in_background(&self, work: impl FnOnce() -> Tidy + Send + 'static) {
        let (send, wake) = (self.ws.tidy.0.clone(), self.waker.clone());
        std::thread::spawn(move || {
            let _ = send.send(work());
            if let Some(wake) = wake {
                wake();
            }
        });
    }

    /// Acts on worktrees that have been checked or removed. Says whether anything shown changed.
    pub fn take_tidy(&mut self) -> bool {
        let mut changed = false;
        while let Ok(news) = self.ws.tidy.1.try_recv() {
            let (Tidy::Checked(tree, _) | Tidy::Removed(tree, _) | Tidy::Busy(tree)) = &news;
            self.ws.tidying.remove(tree);
            if self.ws.removing.remove(tree) {
                self.relist();
                changed = true;
            }
            match news {
                // Something was started in it again meanwhile, or it's already gone.
                Tidy::Checked(tree, _) if self.in_use(&tree) || !tree.is_dir() => {}
                Tidy::Checked(tree, Ok(unsaved)) if unsaved.is_empty() => self.remove_worktree(tree, false),
                Tidy::Checked(tree, Ok(unsaved)) => {
                    self.ws.asking.push((tree, unsaved));
                    changed = true;
                }
                // When git can't say what's in it, or something is working in it, it stays.
                Tidy::Checked(_, Err(_)) | Tidy::Busy(_) => {}
                Tidy::Removed(tree, done) => {
                    // The launcher may be in a folder inside it.
                    if self.ws.folder.as_deref().and_then(worktree::top_of).as_ref() == Some(&tree) {
                        self.ws.folder = worktree::repo_of(&tree);
                    }
                    if let Err(error) = done {
                        self.ws.error = Some(error);
                    }
                    self.relist();
                    changed = true;
                }
            }
        }
        changed
    }

    /// Answers the question about the first worktree with work in it.
    pub fn answer_tidy(&mut self, remove: bool) {
        if self.ws.asking.is_empty() {
            return;
        }
        let (tree, _) = self.ws.asking.remove(0);
        if remove && !self.in_use(&tree) {
            self.remove_worktree(tree, true);
        }
    }

    /// Whether the worktree a folder is in is being removed.
    pub fn removing(&self, folder: &Path) -> bool {
        worktree::top_of(folder).is_some_and(|tree| self.ws.removing.contains(&tree))
    }

    #[cfg(test)]
    pub fn settle_tidy(&mut self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            self.take_tidy();
            if self.ws.tidying.is_empty() {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "worktrees were never checked");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
}

/// Asks before removing a worktree that has work in it.
pub fn tidy_dialog(s: &App, k: &Kit, layer: &mut Vec<El>) {
    let asked = s.ws.asking.first();
    let name = asked.and_then(|(tree, _)| tree.file_name()).map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let lost = asked.map_or_else(String::new, |(_, unsaved)| format!("It has {}.", unsaved.describe()));
    let actions = row()
        .w_full()
        .padding(16.0)
        .gap(8.0)
        .justify_end()
        .child(Button::new("Keep").variant(ButtonVariant::Ghost).on_press(|s: &mut App| s.answer_tidy(false)).build(k).key("tidy-keep"))
        .child(Button::new("Remove").variant(ButtonVariant::Danger).on_press(|s: &mut App| s.answer_tidy(true)).build(k).key("tidy-remove"));
    // The kit's description is one line; this one needs to wrap.
    let detail = text(lost).font(k.ty.ui.lh(20.0)).color(k.c.text_muted).wrap().w_full().px(16.0).key("tidy-detail");
    Dialog::new("tidy", format!("Remove {name}?"))
        .width(360.0)
        .fit()
        .open(asked.is_some() && !s.ws.confirming_quit)
        .on_close(|s: &mut App| s.answer_tidy(false))
        .child(detail)
        .child(actions)
        .build(k, layer);
}
