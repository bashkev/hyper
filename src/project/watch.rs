//! Native events for the visible project, including worktree Git metadata outside its root.

use super::data::Context;
use notify::{RecommendedWatcher, RecursiveMode, Watcher as _};
use rune::Waker;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct Events {
    pub paths: HashSet<PathBuf>,
    pub rescan: bool,
}

pub struct Watch {
    _watcher: RecommendedWatcher,
    events: Arc<Mutex<Events>>,
    pub ignored: Arc<Mutex<Vec<PathBuf>>>,
}

impl Watch {
    pub fn start(ctx: &Context, wake: Waker) -> Result<Self, String> {
        let events = Arc::new(Mutex::new(Events::default()));
        let ignored = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
        let (noted, hidden) = (events.clone(), ignored.clone());
        let root = ctx.root.clone();
        let git = ctx.git.clone();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let Ok(event) = event else {
                noted.lock().unwrap().rescan = true;
                wake();
                return;
            };
            if matches!(event.kind, notify::EventKind::Access(_)) {
                return;
            }
            let ignored = hidden.lock().unwrap();
            let mut events = noted.lock().unwrap();
            let was_empty = events.paths.is_empty() && !events.rescan;
            events.rescan |= event.need_rescan();
            for path in event.paths {
                // Git's object database and lock/temp files aren't workspace changes.
                let metadata =
                    git.as_ref().and_then(|(dir, common)| path.strip_prefix(dir).ok().or_else(|| path.strip_prefix(common).ok()));
                if let Some(p) = metadata {
                    if !(p.as_os_str().is_empty()
                        || p == std::path::Path::new("index")
                        || p == std::path::Path::new("HEAD")
                        || p == std::path::Path::new("packed-refs")
                        || p.starts_with("refs")
                        || p.starts_with("info"))
                    {
                        continue;
                    }
                } else if path.starts_with(root.join(".git")) || ignored.iter().any(|p| path.starts_with(p)) {
                    continue;
                }
                if events.paths.len() < 4096 {
                    events.paths.insert(path);
                } else {
                    events.rescan = true;
                }
            }
            if was_empty && (!events.paths.is_empty() || events.rescan) {
                wake();
            }
        })
        .map_err(|e| e.to_string())?;
        watcher.watch(&ctx.root, RecursiveMode::Recursive).map_err(|e| e.to_string())?;
        if let Some((dir, common)) = &ctx.git {
            for path in [dir, common] {
                if !path.starts_with(&ctx.root) {
                    watcher.watch(path, RecursiveMode::NonRecursive).map_err(|e| e.to_string())?;
                }
            }
            let refs = common.join("refs");
            if !refs.starts_with(&ctx.root) && refs.is_dir() {
                watcher.watch(&refs, RecursiveMode::Recursive).map_err(|e| e.to_string())?;
            }
        }
        Ok(Self { _watcher: watcher, events, ignored })
    }

    pub fn take(&self) -> Events {
        std::mem::take(&mut *self.events.lock().unwrap())
    }
}
