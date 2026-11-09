//! ⌘F in a terminal: the find bar over its top corner, and going from match to match.
//!
//! The bar floats over the terminal rather than taking a row from it, so opening it doesn't
//! resize the program, which would redraw.

use rune::{El, Key, Tx, row, text};
use rune_kit::{Kit, TextInput, glyphs};

use super::{Live, Main};
use crate::App;
use crate::term::{Found, Toward};
use crate::ui::{keys, pane_button};

/// How wide the bar's field is.
const FIELD: f32 = 220.0;

impl App {
    /// ⌘F with a terminal on screen: the find bar, with what was last looked for selected, and
    /// its matches marked again.
    pub fn terminal_find(&mut self) {
        let Main::Terminal(id) = self.ws.main else { return };
        self.ws.finding = Some(id);
        let edit = &mut self.ws.find;
        edit.focused = true;
        edit.set_selection(0, edit.text.len());
        self.terminal_find_changed();
    }

    fn finding(&self) -> Option<&Live> {
        self.current().filter(|l| self.ws.finding == Some(l.id))
    }

    fn terminal_find_changed(&self) {
        if let Some(live) = self.finding() {
            live.term.find(&self.ws.find.text);
        }
    }

    fn terminal_find_next(&self, toward: Toward) {
        if let Some(live) = self.finding() {
            live.term.find_next(toward);
        }
    }

    /// Escape: the bar goes, with its marks, and typing goes back to the terminal.
    fn close_terminal_find(&mut self, tx: &mut Tx) {
        self.end_terminal_find();
        tx.blur();
    }

    /// The bar goes from the terminal it was over, as another takes the pane.
    pub(super) fn end_terminal_find(&mut self) {
        if let Some(id) = self.ws.finding.take()
            && let Some(live) = self.live(id)
        {
            live.term.find("");
        }
        self.ws.find.focused = false;
    }
}

/// "3 of 12", or that there's nothing, once the scrollback has been searched.
fn count(found: Found) -> Option<String> {
    match found {
        Found { total: 0, complete: true, .. } => Some("No results".to_owned()),
        Found { total: 0, .. } => None,
        Found { total, selected: Some(at), .. } => Some(format!("{} of {total}", at + 1)),
        Found { total, selected: None, .. } => Some(total.to_string()),
    }
}

/// The find bar, over the terminal it finds in.
pub(super) fn find_bar(s: &App, k: &Kit) -> Option<El> {
    let live = s.finding()?;
    let edit = &s.ws.find;
    let found = live.term.frame.as_ref().and_then(|f| f.found).filter(|_| !edit.text.is_empty());
    let field = TextInput::new(edit)
        .width(Some(FIELD))
        .placeholder("Find")
        .label("Find in terminal")
        .leading_icon(&glyphs::SEARCH)
        .invalid(found.is_some_and(|f| f.total == 0 && f.complete))
        .on_edit(|s: &mut App| &mut s.ws.find)
        .on_change(|s: &mut App, _| s.terminal_find_changed())
        .on_key(|s: &mut App, ev, tx| match ev.key {
            Key::Escape => {
                s.close_terminal_find(tx);
                true
            }
            Key::Enter if ev.mods.shift => {
                s.terminal_find_next(Toward::Newer);
                true
            }
            _ => false,
        })
        .on_submit(|s: &mut App, _| s.terminal_find_next(Toward::Older))
        .build(k)
        .key("terminal-find");
    let said = found.and_then(count).map(|said| text(said).font(k.ty.small.features("'tnum' 1")).color(k.c.text_faint).truncate());
    // Up is older, as the matches are counted from the newest.
    let mut bar = row()
        .key("terminal-find-bar")
        .pin(None, Some(8.0), Some(12.0), None)
        .padding(4.0)
        .gap(4.0)
        .center_y()
        .fill(k.c.layer_01)
        .border(1.0, k.c.border_base)
        .radius(k.radius.md)
        .child(field)
        .children(said)
        .child(pane_button(&glyphs::CHEVRON_UP, "Next match", keys(edit.focused, "enter"), "terminal-find-next", true, k, |s, _| s.terminal_find_next(Toward::Older)))
        .child(pane_button(&glyphs::CHEVRON_DOWN, "Previous match", keys(edit.focused, "shift+enter"), "terminal-find-previous", true, k, |s, _| s.terminal_find_next(Toward::Newer)))
        .child(pane_button(&glyphs::CLOSE, "Close find", keys(edit.focused, "escape"), "terminal-find-close", true, k, App::close_terminal_find));
    for shadow in k.shadow_layers(&k.elevation.floating) {
        bar = bar.box_shadow(shadow);
    }
    Some(bar)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_count_waits_for_the_whole_scrollback_before_saying_there_is_nothing() {
        let said = [
            Found { total: 0, selected: None, complete: false },
            Found { total: 0, selected: None, complete: true },
            Found { total: 12, selected: Some(2), complete: true },
            Found { total: 4, selected: None, complete: false },
        ]
        .map(count);
        assert_eq!(said, [None, Some("No results".to_owned()), Some("3 of 12".to_owned()), Some("4".to_owned())]);
    }
}
