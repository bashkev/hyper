//! The Workspace's rows in the sidebar, its pane, a terminal or the launcher, and its dialogs.

use rune::{Anim, Cursor, El, Tx, col, el, icon, row, text};
use rune_kit::shell::{self, RowStatus, SidebarRow};
use rune_kit::{Button, ButtonVariant, Dialog, Kit};

use super::launcher::{palette, palette_overlay};
use super::{Live, Main, Tool, folder_project};
use crate::commands::Action;
use crate::data::Session;
use crate::sidebar::{COMPACT_ROW, ROW_INSET, Worktree, folded_mark, group_header};
use crate::ui::{BRANCH, PLUS, SORT, TOOLS, live_dot, pane_button};
use crate::{App, logos, term};

/// What a terminal is called: its session's title, or its tool's name until it has one.
pub fn live_title(s: &App, live: &Live) -> String {
    let session = live.session.as_deref().and_then(|id| s.session(id));
    session.map_or_else(|| live.tool.label().to_owned(), |x| x.title.clone())
}

/// What the toolbar names, for whatever the Workspace is showing.
pub fn title(s: &App) -> String {
    match s.ws.main {
        Main::Launcher => String::new(),
        Main::Terminal(_) => s.current().map_or_else(String::new, |l| live_title(s, l)),
    }
}

pub fn live_row(s: &App, id: u64, k: &Kit) -> El {
    let Some(live) = s.live(id) else { return el() };
    let title = live_title(s, live);
    let avatar = match live.tool {
        Tool::Agent(source) => logos::logo(source, 16.0, k),
        Tool::Shell => icon(&TOOLS, 16.0).color(k.c.icon_muted).a11y_hidden(),
    };
    let mut r = SidebarRow::new(title.clone())
        .depth(if live.worktree.is_some() { 2 } else { 1 })
        .height(COMPACT_ROW)
        .avatar(avatar)
        .selected(s.ws.main == Main::Terminal(id))
        .on_select(move |s: &mut App, tx| {
            s.stepping(tx);
            s.show(Main::Terminal(id));
        });
    let working = s.live_working(live);
    match live.failed {
        Some(_) => r = r.on_close(move |s: &mut App, _| s.close_live(id)),
        _ if live.needs_you => r = r.status(RowStatus::Attention),
        None if working => r = r.status(RowStatus::Busy),
        _ => r = r.status(RowStatus::Other(live_dot(k))),
    }
    // The keyboard stays with the terminal when a row is clicked.
    r.build(k).label(format!("{title}, {}", live.tool.label())).focusable(false).preserve_focus_on_press(true).key(format!("live-{id}"))
}

/// Starts the launcher, for the top of the sidebar.
pub fn new_button(s: &App, k: &Kit) -> El {
    let press = |s: &mut App, tx: &mut Tx| {
        s.new_session(None, Some(tx));
    };
    let new = Button::new("New").help("Command palette").shortcut(Action::CommandPalette.hint(&s.settings)).variant(ButtonVariant::Neutral).on_press_tx(press).build(k).key("new-session").grow(1.0).min_w(0.0).preserve_focus_on_press(true);
    let sort = pane_button(&SORT, "Sort", "", "sidebar-order", true, k, |s: &mut App, tx| s.order_menu(tx));
    row().key("workspace-header").w_full().gap(4.0).center_y().child(new).child(sort)
}

/// Starts the default tool in a project at once. Right-click offers the others.
pub fn plus(s: &App, name: &str, k: &Kit) -> El {
    let (project, menu_project) = (name.to_owned(), name.to_owned());
    let tool = s.default_tool();
    plus_button(name, k)
        .help(format!("New {}", tool.label()))
        .on_press_tx(move |s: &mut App, tx| match s.project_folder(&project) {
            Some(folder) => s.start_now(project.clone(), folder, tool, s.settings.default_worktree),
            None => s.new_session(Some(project.clone()), Some(tx)),
        })
        .on_secondary_press(move |s: &mut App, at, tx| {
            if let Some(folder) = s.project_folder(&menu_project) {
                s.start_menu(menu_project.clone(), folder, true, at.into(), tx);
            }
        })
}

fn plus_button(name: &str, k: &Kit) -> El {
    let button = row()
        .key("new")
        .role("button")
        .label(format!("New in {name}"))
        .size(20.0, 20.0)
        .center()
        .radius(k.radius.sm)
        .cursor(Cursor::Pointer)
        .child(icon(&PLUS, 14.0).color(k.c.icon_muted).a11y_hidden());
    k.interactive(button).hover_motion(Anim::INSTANT, Anim::INSTANT).focusable(false).preserve_focus_on_press(true)
}

/// Heads a worktree's terminals, one level into its project.
pub fn worktree_row(s: &App, worktree: &Worktree, k: &Kit) -> El {
    let Worktree { name, key, count, open, attention, folder } = worktree;
    let (open, attention) = (*open, *attention);
    let toggled = key.clone();
    let (folder, menu_folder) = (folder.clone(), folder.clone());
    let project = s.ws.live.iter().find(|l| l.cwd == folder).map_or_else(|| folder_project(&folder), |l| l.project.clone());
    let menu_project = project.clone();
    let tool = s.default_tool();
    let plus = plus_button(name, k)
        .help(format!("New {}", tool.label()))
        .on_press(move |s: &mut App| s.start_now(project.clone(), folder.clone(), tool, false))
        .on_secondary_press(move |s: &mut App, at, tx| s.start_menu(menu_project.clone(), menu_folder.clone(), false, at.into(), tx));
    let base = row()
        .label(format!("Worktree {name}"))
        .description(format!("{count} items"))
        .w_full()
        .h(COMPACT_ROW)
        .pl(ROW_INSET + shell::INDENT)
        .pr(ROW_INSET)
        .on_press_tx(move |s: &mut App, _| s.toggle_worktree(&toggled));
    group_header(base, name, open, (&BRANCH, "branch"), folded_mark(!open, attention, k), plus, k)
}

/// Why a terminal stopped, over what it printed.
fn failure_bar(code: Option<i32>, id: u64, k: &Kit) -> El {
    let said = code.map_or_else(|| "Stopped".to_owned(), |code| format!("Exited with {code}"));
    row()
        .key("failed")
        .w_full()
        .px(12.0)
        .py(8.0)
        .gap(12.0)
        .center_y()
        .fill(k.c.layer_02)
        .child(text(said).font(k.ty.ui).color(k.c.danger.fg).grow(1.0))
        .child(Button::new("Close").variant(ButtonVariant::Neutral).on_press(move |s: &mut App| s.close_live(id)).build(k).key("close-failed"))
}

fn terminal(live: &Live, k: &Kit) -> El {
    let style = term::style(k);
    let cell = term::cell(style);
    let bg = live.term.frame.as_ref().map_or(k.c.bg_base, |f| f.bg.color());
    // One element for every terminal, so the keyboard stays put when the sidebar switches them.
    let screen = col()
        .key("terminal")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .fill(bg)
        .focusable(true)
        .no_focus_ring()
        .on_key(|s: &mut App, ev, tx| s.command_key(ev, tx) || s.terminal_key(ev, tx))
        .on_drag(move |s: &mut App, drag, tx| {
            // A click in the terminal takes the keyboard back from the file pane.
            if drag.phase == rune::Phase::Down {
                s.project.keyboard = false;
                // ⌘-click opens a link, as in other terminals.
                if tx.mods().cmd && let Some(url) = s.current().and_then(|live| live.term.link_at(drag.x, drag.y, cell)) {
                    return s.open_link(&url, tx);
                }
            }
            if let Some(live) = s.current() {
                live.term.select(drag, cell, tx.mods().alt);
                tx.instant();
            }
        })
        .on_wheel(move |s: &mut App, ev, tx| {
            if s.swipe(ev, tx) {
                return true;
            }
            if let Some(live) = s.current_mut() {
                live.term.wheel(ev, cell);
            }
            true
        })
        // Keep keyboard focus on the outer terminal. A new metrics node reports layout
        // when the font changes even if the pane's outer rectangle hasn't moved.
        .child(col().key(format!("terminal-grid-{}-{}", cell.0.to_bits(), cell.1.to_bits())).w_full().grow(1.0).min_h(0.0)
            .on_layout(move |s: &mut App, [_, _, w, h]| s.set_pane(w, h, cell))
            .child(term::screen(live.term.frame.as_ref(), style, cell, live.running(), k.selection())));
    let bar = live.failed.map(|code| failure_bar(code, live.id, k));
    col().w_full().h_full().children(bar).child(screen)
}

pub fn pane(s: &App, k: &Kit) -> El {
    match s.ws.main {
        // A little above the middle, and fixed there.
        Main::Launcher => col().w_full().h_full().px(24.0).items_center().py(16.0).child(el().grow(1.0)).child(palette(s, k, true).shrink().min_h(0.0)).child(el().grow(1.6)),
        Main::Terminal(_) => col().w_full().h_full().children(s.current().map(|live| terminal(live, k))).children(palette_overlay(s, k)),
    }
}

/// Resume, for the session on screen.
pub fn resume_button(s: &App, session: &Session, k: &Kit) -> Option<El> {
    if !s.can_resume(session) {
        return None;
    }
    let here = s.live_for(&session.id).is_some();
    let id = session.id.clone();
    Some(Button::new(if here { "Open" } else { "Resume" }).variant(ButtonVariant::Neutral).on_press(move |s: &mut App| s.resume(&id)).build(k).key("resume"))
}

pub fn quit_dialog(s: &App, k: &Kit, layer: &mut Vec<El>) {
    let count = s.running_agents();
    let title = if count == 1 { "Stop 1 agent?".to_owned() } else { format!("Stop {count} agents?") };
    let actions = row()
        .w_full()
        .padding(16.0)
        .gap(8.0)
        .justify_end()
        .child(Button::new("Cancel").variant(ButtonVariant::Ghost).on_press(|s: &mut App| s.ws.confirming_quit = false).build(k).key("quit-cancel"))
        .child(Button::new("Quit").variant(ButtonVariant::Danger).on_press_tx(|_: &mut App, tx| tx.quit()).build(k).key("quit-confirm"));
    Dialog::new("quit", title).width(360.0).fit().open(s.ws.confirming_quit).on_close(|s: &mut App| s.ws.confirming_quit = false).child(actions).build(k, layer);
}
