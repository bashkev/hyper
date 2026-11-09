//! Whether typing goes to the terminal on screen or to something over it, and letting its
//! program know.

use rune::{KeyEvent, Tx};

use crate::term::Term;
use crate::{App, View};

impl App {
    /// Whether something over the terminal has the keyboard: the quit question, the launcher
    /// (even while its field is disabled) or the find bar. Keys its field passes on, such as ⌃C,
    /// mustn't reach the program.
    fn terminal_covered(&self) -> bool {
        self.view != View::Workspace || self.ws.confirming_quit || self.ws.palette || self.ws.find.focused
    }

    /// The terminal the keyboard belongs to.
    pub fn typing_term(&self) -> Option<&Term> {
        if self.terminal_covered() {
            return None;
        }
        self.current().filter(|l| l.running()).map(|l| &l.term)
    }

    pub fn terminal_key(&self, ev: &KeyEvent, tx: &mut Tx) -> bool {
        if self.terminal_covered() {
            return false;
        }
        if ev.is_command('c') && let Some(live) = self.current() {
            live.term.copy();
            return true;
        }
        self.typing_term().is_some_and(|t| t.key(ev, tx))
    }

    /// Lets the terminal on screen know whether it has the keyboard, and the one before it that
    /// it no longer does. Programs use this to decide whether to notify. Each poll calls it too,
    /// after Rune has moved focus on its own, such as back to the find bar when the launcher
    /// closes, and only a change is sent.
    pub fn tell_focus(&mut self) {
        // The launcher or the find bar over it has the keyboard.
        let typing = self.ws.window_focused && self.view == View::Workspace && !self.ws.palette && !self.ws.find.focused;
        let now = self.current().map(|l| (l.id, typing));
        if now == self.ws.told {
            return;
        }
        if let Some((before, true)) = self.ws.told
            && now.map(|(id, _)| id) != Some(before)
            && let Some(live) = self.live(before)
        {
            live.term.focus(false);
        }
        if let Some((id, focused)) = now
            && let Some(live) = self.live(id)
        {
            live.term.focus(focused);
        }
        self.ws.told = now;
    }
}
