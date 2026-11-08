//! Glyphs and small pieces of interface shared by the sidebar, the panes and the inspector.

use rune::{El, Icon, Tx, col, el, row, text};
use rune_kit::{IconButton, IconButtonVariant, Kit};

use crate::App;

pub static TOOLS: Icon = Icon {
    name: "sb-tools",
    view: 16.0,
    body: r#"<path d="M3.5 5L6.5 8L3.5 11M8.5 11.5H12.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};

pub static GEAR: Icon = Icon {
    name: "sb-gear",
    view: 24.0,
    body: r#"<path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" fill="none"/><circle cx="12" cy="12" r="3" stroke="currentColor" stroke-width="1.8" fill="none"/>"#,
};

pub static FOLDER: Icon = Icon {
    name: "sb-folder",
    view: 16.0,
    body: r#"<path d="M2 4.5C2 3.67 2.67 3 3.5 3H6.1L7.6 4.75H12.5C13.33 4.75 14 5.42 14 6.25V11.5C14 12.33 13.33 13 12.5 13H3.5C2.67 13 2 12.33 2 11.5V4.5Z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/>"#,
};

pub static PLUS: Icon = Icon {
    name: "sb-plus",
    view: 16.0,
    body: r#"<path d="M8 3.5V12.5M3.5 8H12.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>"#,
};

pub static SORT: Icon = Icon {
    name: "sb-sort",
    view: 16.0,
    body: r#"<path d="M3 4.5H13M5 8H11M7 11.5H9" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/>"#,
};

pub static BRANCH: Icon = Icon {
    name: "sb-branch",
    view: 16.0,
    body: r#"<circle cx="5" cy="3.5" r="1.5" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="5" cy="12.5" r="1.5" stroke="currentColor" stroke-width="1.2" fill="none"/><circle cx="11" cy="5" r="1.5" stroke="currentColor" stroke-width="1.2" fill="none"/><path d="M5 5V11M11 6.5C11 9 8.5 9 5 11" stroke="currentColor" stroke-width="1.2" fill="none" stroke-linecap="round"/>"#,
};

pub static EXTERNAL: Icon = Icon {
    name: "sb-external",
    view: 16.0,
    body: r#"<path d="M9 2H14V7 M14 2L7 9 M6 3H2V14H13V10" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" fill="none"/>"#,
};

/// An icon button in a pane's bar, its tooltip ending in `shortcut` (`""` for none). Rune can
/// only focus text, so clicking one leaves the keyboard where it was.
pub fn pane_button(glyph: &'static Icon, label: &str, shortcut: &str, key: &str, enabled: bool, k: &Kit, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
    IconButton::new(glyph)
        .variant(IconButtonVariant::GhostMuted)
        .label(label)
        .help(label)
        .shortcut(shortcut)
        .disabled(!enabled)
        .on_press_tx(f)
        .build(k)
        .key(key)
        .focusable(false)
        .preserve_focus_on_press(true)
}

/// `shortcut` while the field that takes it has the keyboard, for a find bar's buttons.
pub fn keys(field: bool, shortcut: &'static str) -> &'static str {
    if field { shortcut } else { "" }
}

/// How wide reading text runs.
const COLUMN: f32 = 760.0;

/// A column of reading width in the middle of its pane.
pub fn centered(body: El) -> El {
    row().w_full().justify_center().px(28.0).child(col().w_full().max_w(COLUMN).child(body))
}

/// Fills its pane and puts what it says in the middle.
pub fn center_message(body: El) -> El {
    col().w_full().h_full().center().child(body)
}

/// Something is running there.
pub fn live_dot(k: &Kit) -> El {
    dot(k.c.success.icon)
}

/// The kit's mark for a row that needs attention, for a folded group holding one.
pub fn attention_dot(k: &Kit) -> El {
    dot(k.c.warning.icon)
}

fn dot(color: rune::Color) -> El {
    el().size(7.0, 7.0).radius(3.5).fill(color).a11y_hidden()
}

/// A labelled value in a details pane.
pub fn field(label: &str, value: String, k: &Kit) -> El {
    col()
        .w_full()
        .gap(2.0)
        .child(text(label.to_owned()).font(k.ty.small).color(k.c.text_faint))
        .child(text(value).font(k.ty.ui).color(k.c.text_base).wrap_anywhere().selectable(true))
}
