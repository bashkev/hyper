//! What the terminals say, and telling the person when one needs them: a mark in the sidebar,
//! the Dock badge and a notification, as each agent's settings ask.

use rune::Tx;

use super::{Main, Tool, view::live_title};
use crate::notifications::{self, Delivery, Event as Notice, When};
use crate::sidebar::SIDEBAR_SCROLL;
use crate::term::Event;
use crate::{App, View, attention};

impl App {
    /// Takes in what the terminals did. Says whether anything shown changed.
    pub fn poll_terminals(&mut self, tx: &mut Tx) -> bool {
        if std::mem::take(&mut self.ws.scroll_up) {
            tx.scroll_to_y(SIDEBAR_SCROLL, 0.0);
        }
        let mut exited = Vec::new();
        let mut asked = Vec::new();
        let mut relink = false;
        let mut changed = false;
        for live in &mut self.ws.live {
            for event in live.term.take_events() {
                match event {
                    Event::Exited(code) => exited.push((live.id, code)),
                    Event::Copy(text) => tx.copy(text),
                    Event::Bell => asked.push((live.id, None, Notice::Bell)),
                    Event::Notification { title, body } if title == notifications::SESSION && matches!(live.tool, Tool::Agent(_)) => {
                        let session = body.trim();
                        if !session.is_empty() && live.session.as_deref() != Some(session) {
                            live.session = Some(session.to_owned());
                            relink = true;
                        }
                    }
                    // `attend` takes it from here, as it would a turn its file says has started.
                    Event::Notification { title, .. } if title == notifications::WORKING && matches!(live.tool, Tool::Agent(_)) => {
                        changed |= live.said != Some(true);
                        live.said = Some(true);
                    }
                    Event::Notification { title, body } => {
                        let event = notifications::classify(live.tool, &title, &body);
                        let over = matches!(event, Notice::TurnCompleted | Notice::ApprovalRequested | Notice::InputRequested);
                        if over && live.said.is_some() && title.starts_with("Hyper:") {
                            // This is the news of it, so `attend` mustn't take it for a turn ending too.
                            live.said = Some(false);
                            live.working = false;
                        }
                        asked.push((live.id, Some(if body.trim().is_empty() { title } else { body }), event));
                    }
                }
            }
        }
        if relink {
            self.relist();
        }
        changed |= relink || !exited.is_empty() || !asked.is_empty();
        for (id, message, event) in asked {
            changed |= self.alert(id, message, event, true);
        }
        for (id, code) in exited {
            let shell = self.live(id).is_some_and(|l| l.tool == Tool::Shell);
            let event = if code == Some(0) || (shell && code.is_some()) { Notice::Exited } else { Notice::Failed };
            // A shell exits with its last command's status, which says nothing about the shell.
            if event == Notice::Exited {
                // Gone once closed, so this notice is the only trace, and stays to be read.
                let notice = self.live(id).map(|live| {
                    (self.settings.notifications_for(live.tool), live_title(self, live), self.looking(id))
                });
                self.close_live(id);
                if let Some((policy, title, looking)) = notice {
                    let delivery = policy.delivery(event);
                    if delivery != Delivery::Off && policy.when.allows(attention::app_focused(), looking) {
                        attention::post(id, &title, event.label(), delivery == Delivery::Sound);
                    }
                }
            } else if let Some(live) = self.live_mut(id) {
                live.failed = Some(code);
                self.relist();
                let body = code.map_or_else(|| event.label().to_owned(), |code| format!("Session failed (exit {code})"));
                self.alert(id, Some(body), event, true);
            }
        }
        if self.ws.confirming_quit && self.running_agents() == 0 {
            self.ws.confirming_quit = false;
            changed = true;
        }
        if self.view == View::Workspace && self.current_mut().is_some_and(|l| l.term.take_frame()) {
            changed = true;
        }
        if changed {
            // A commit from `poll` animates, which would fade and slide the cells.
            tx.instant();
        }
        changed
    }

    /// Whether the person is looking at this terminal.
    fn looking(&self, id: u64) -> bool {
        self.view == View::Workspace && self.ws.window_focused && self.ws.main == Main::Terminal(id)
    }

    /// The terminal on screen has been seen, if the person is looking at it.
    pub fn see_current(&mut self) {
        let Main::Terminal(id) = self.ws.main else { return };
        if !self.looking(id) {
            return;
        }
        let Some(live) = self.current_mut() else { return };
        live.seen = true;
        live.posted_event = None;
        if std::mem::take(&mut live.posted) {
            attention::remove(id);
        }
        if std::mem::take(&mut live.needs_you) {
            self.rebadge();
            self.relist();
        }
    }

    pub fn rebadge(&mut self) {
        let count = self.ws.live.iter().filter(|l| l.needs_you && self.settings.notifications_for(l.tool).dock_badge).count();
        if count != self.ws.badged {
            self.ws.badged = count;
            attention::badge(count);
        }
    }

    /// A terminal finished or asked for something: if the person is elsewhere, mark it, and
    /// tell them as its agent's notification settings say. `said` is whether the program itself
    /// spoke up rather than its turn being seen to end.
    fn alert(&mut self, id: u64, message: Option<String>, event: Notice, said: bool) -> bool {
        let looking = self.looking(id);
        let Some(tool) = self.live(id).map(|l| l.tool) else { return false };
        let policy = self.settings.notifications_for(tool);
        let Some(live) = self.live_mut(id) else { return false };
        if looking {
            live.seen |= !live.working;
            // Only someone who asked to hear even about what's in front of them goes on.
            if policy.when != When::Always {
                return false;
            }
        }
        // An agent that repeats itself (Claude) reminds that it's idle, rings, or says its turn
        // is over, none of which is news once it's been seen. A request for approval or input
        // is. The others only speak up when a turn ends or wants something, which counts even
        // if the turn was too quick to be seen working.
        let request = matches!(event, Notice::ApprovalRequested | Notice::InputRequested);
        let repeats = matches!(live.tool, Tool::Agent(source) if source.agent().repeats_itself());
        if said && !looking && !request && event != Notice::Failed && repeats && !live.working && live.seen {
            return false;
        }
        // An event turned off in Settings doesn't count at all: no mark, badge or notice.
        let delivery = policy.delivery(event);
        if delivery == Delivery::Off {
            return false;
        }
        let first = !live.needs_you && !looking;
        live.needs_you |= !looking;
        let Some(live) = self.live(id) else { return false };
        let title = live_title(self, live);
        let body = message.filter(|m| !m.trim().is_empty());
        let in_front = attention::app_focused();
        let Some(live) = self.live_mut(id) else { return false };
        if !looking {
            live.seen = false;
        }
        // The same news can come as the turn ending and as the program saying so, even as two
        // kinds of event: it sounds once until it's seen, and the program's words replace the
        // plain ones. Only a new request, or a failure, sounds again.
        let fresh = live.posted_event != Some(event);
        if policy.when.allows(in_front, looking) && (first || fresh || body.is_some()) {
            attention::post(id, &title, body.as_deref().unwrap_or(event.label()), delivery == Delivery::Sound && (!live.posted || (fresh && (request || event == Notice::Failed))));
            live.posted = true;
            live.posted_event = Some(event);
        }
        if first {
            self.rebadge();
            self.relist();
        }
        first
    }

    /// Marks agents whose turn has ended since the last look. Says whether anything shown changed.
    pub fn attend(&mut self) -> bool {
        let mut ended = Vec::new();
        let mut changed = false;
        for at in 0..self.ws.live.len() {
            let live = &self.ws.live[at];
            if !live.running() || !matches!(live.tool, Tool::Agent(_)) {
                continue;
            }
            let working = self.live_working(live);
            let live = &mut self.ws.live[at];
            if working == live.working {
                continue;
            }
            live.working = working;
            if !working {
                ended.push(live.id);
            } else {
                // It's moved on by itself, so what it was waiting for has been dealt with.
                live.seen = false;
                changed |= std::mem::take(&mut live.needs_you);
                if std::mem::take(&mut live.posted) {
                    attention::remove(live.id);
                }
                live.posted_event = None;
            }
        }
        if changed {
            self.rebadge();
            self.relist();
        }
        for id in ended {
            changed |= self.alert(id, None, Notice::TurnCompleted, false);
        }
        if let Some(id) = attention::clicked() {
            attention::bring_to_front();
            if self.live(id).is_some() {
                self.set_view(View::Workspace);
                self.show(Main::Terminal(id));
            }
            changed = true;
        }
        changed
    }
}
