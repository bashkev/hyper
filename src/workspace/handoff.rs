//! A reload hands every terminal to the next build, which takes it over: its program keeps
//! running, and its screen and scrollback come back from a snapshot. See `reload.rs`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{Live, Main, Tool};
use crate::term::{self, Term};
use crate::App;

impl App {
    /// Gives every terminal up for the next build, which takes them over with
    /// [`App::adopt_workspace`]. Their screens are saved in `dir`; their programs keep running.
    pub fn hand_off_workspace(&mut self, dir: &Path) -> Value {
        let mut handed = Vec::new();
        let asked: Vec<_> = std::mem::take(&mut self.ws.live)
            .into_iter()
            .map(|mut live| {
                let snapshot = format!("{}.snap", live.id);
                let handoff = live.term.handoff(&dir.join(&snapshot));
                (live, snapshot, handoff)
            })
            .collect();
        let deadline = std::time::Instant::now() + term::HANDOFF_WAIT;
        for (mut live, snapshot, handoff) in asked {
            let term = match handoff.wait(deadline) {
                // It didn't answer, so it's lost.
                None => continue,
                Some(term::Handed::Unstarted { argv }) => json!({ "argv": argv }),
                Some(term::Handed::Running { fd, pid, size, saved, pending }) => {
                    let pending = pending.then(|| term::pending_path(Path::new(&snapshot)).display().to_string());
                    json!({ "fd": fd, "pid": pid, "size": size, "snapshot": saved.then_some(&snapshot), "pending": pending })
                }
            };
            // An end the UI hadn't heard of yet: the terminal it goes to says it again.
            let exited = live.term.take_events().into_iter().find_map(|e| match e {
                term::Event::Exited(code) => Some(json!({ "code": code })),
                _ => None,
            });
            handed.push(json!({
                "id": live.id,
                "tool": live.tool.id(),
                "project": live.project,
                "cwd": live.cwd,
                "worktree": live.worktree,
                "session": live.session,
                "known": live.known,
                "failed": live.failed.map(|code| json!({ "code": code })),
                "needs_you": live.needs_you,
                "working": live.working,
                "said": live.said,
                "seen": live.seen,
                "posted": live.posted,
                "term": term,
                "exited": exited,
            }));
        }
        let main = match self.ws.main { Main::Terminal(id) => Some(id), Main::Launcher => None };
        json!({ "live": handed, "main": main, "next": self.ws.next })
    }

    /// Takes over the terminals [`App::hand_off_workspace`] gave up, in this build or the next.
    pub fn adopt_workspace(&mut self, handed: &Value, dir: &Path, dark: bool) {
        let text = |v: &Value| v.as_str().map(str::to_owned);
        for live in handed["live"].as_array().into_iter().flatten() {
            let (Some(id), Some(tool), Some(cwd)) = (live["id"].as_u64(), live["tool"].as_str().and_then(Tool::from_id), live["cwd"].as_str()) else { continue };
            let cwd = PathBuf::from(cwd);
            let wake = self.wake();
            let handed = &live["term"];
            let term = match handed["argv"].as_array() {
                Some(argv) => Term::new(argv.iter().filter_map(text).collect(), cwd.clone(), dark, wake),
                None => {
                    let size = handed["size"].as_array().map(|s| s.iter().filter_map(serde_json::Value::as_u64).map(|n| n as u16).collect::<Vec<_>>()).unwrap_or_default();
                    let adopt = term::Adopt {
                        fd: handed["fd"].as_i64().map(|fd| fd as i32),
                        pid: handed["pid"].as_i64().map(|pid| pid as i32),
                        size: size.try_into().unwrap_or([80, 24, 8, 17]),
                        snapshot: handed["snapshot"].as_str().and_then(|name| std::fs::read(dir.join(name)).ok()),
                        pending: handed["pending"].as_str().and_then(|name| std::fs::read(dir.join(name)).ok()).unwrap_or_default(),
                    };
                    Term::adopt(adopt, cwd.clone(), dark, wake)
                }
            };
            if let Some(exited) = live["exited"].as_object() {
                term.exited(exit_code(exited));
            }
            let project = text(&live["project"]).unwrap_or_default();
            self.open_projects.insert(project.clone());
            self.ws.next = self.ws.next.max(id + 1);
            self.ws.live.push(Live {
                id,
                tool,
                project,
                cwd,
                worktree: text(&live["worktree"]),
                session: text(&live["session"]),
                known: live["known"].as_array().into_iter().flatten().filter_map(text).collect(),
                term,
                failed: live["failed"].as_object().map(exit_code),
                needs_you: live["needs_you"].as_bool().unwrap_or(false),
                working: live["working"].as_bool().unwrap_or(false),
                said: live["said"].as_bool(),
                seen: live["seen"].as_bool().unwrap_or(true),
                posted: live["posted"].as_bool().unwrap_or(false),
                posted_event: None,
            });
        }
        self.ws.next = self.ws.next.max(handed["next"].as_u64().unwrap_or(1));
        let main = handed["main"].as_u64().filter(|id| self.ws.live.iter().any(|l| l.id == *id));
        self.rebadge();
        self.show(main.map_or(Main::Launcher, Main::Terminal));
    }
}

/// The code an `{ "code": ... }` says a program exited with, if it had one.
fn exit_code(exit: &serde_json::Map<String, Value>) -> Option<i32> {
    exit.get("code").and_then(Value::as_i64).map(|code| code as i32)
}
