//! The window: the sidebar, the main pane with its toolbar, and the inspector, which shows the
//! open session's details or the Files and Changes panes.

use std::time::Duration;

use rune::{Cx, El, Icon, PointerButton, Spring, col, el, row, text};
use rune_kit::shell::{self, PaneMotion, SidebarPeek, SidebarToggle, SplitView, Toolbar};
use rune_kit::{ControlSize, IconButton, IconButtonVariant, Kit};

use crate::browse::{Mode, config_details, config_pane};
use crate::commands::Action;
use crate::reader::{details, tools_toggle, transcript_pane};
use crate::sidebar::sidebar;
use crate::ui::FOLDER;
use crate::{App, Load, View, browser, history, preferences, project, reload, workspace};

impl App {
    /// Goes to a view, leaving the one on screen as the person would find it again.
    pub fn set_view(&mut self, view: View) {
        if self.view == view {
            return;
        }
        self.view = view;
        if view != View::Workspace {
            self.close_palette();
        }
        if view == View::Browse && self.mode == Mode::Config {
            self.request_config();
        }
        // Leaving Settings stops any recording; coming back shows what's saved.
        self.preferences.sync(&self.settings);
        self.tell_focus();
        self.see_current();
        self.relist();
        if view == View::Browse && self.mode == Mode::Sessions && self.reader.selected.is_none() {
            self.select_first();
        }
    }

    /// Whether there are details to show for what's on screen: only an agent's session has them.
    pub fn details_available(&self) -> bool {
        match self.view {
            View::Workspace => self.current().is_some_and(|live| {
                matches!(live.tool, workspace::Tool::Agent(_)) && live.session.as_deref().and_then(|id| self.session(id)).is_some()
            }),
            View::Browse => true,
            View::Settings => false,
        }
    }
}

pub fn view(s: &App, cx: &mut Cx<App>) -> El {
    let k = s.settings.appearance.kit(cx.system_dark());
    s.dark.set(cx.system_dark());
    s.look();
    if let Load::Failed(e) = &s.index && s.view == View::Browse {
        return col().w_full().h_full().fill(k.c.bg_base).center().child(text(e.clone()).font(k.ty.ui).color(k.c.danger.fg));
    }
    let working = s.view == View::Workspace;
    let setting = s.view == View::Settings;
    let has_details = s.details_available();
    let inspector_shown = s.inspector.is_open() && (s.project.mode != project::Mode::Details || has_details);
    let project_controls_width = if has_details { 102.0 } else { 76.0 };
    let config = !working && !setting && s.mode == Mode::Config;
    let transcript = !config && !working && !setting;
    let title = if setting {
        "Settings".to_owned()
    } else if working {
        workspace::title(s)
    } else if config {
        s.config.selected_item().map_or(String::new(), |x| x.title.clone())
    } else {
        s.selected_session().map_or(String::new(), |x| x.title.clone())
    };
    let shown = s.selected_session();
    let toolbar = Toolbar::new()
        .title(title)
        .trailing(
            row()
                .gap(2.0)
                .center_y()
                .children(shown.filter(|_| transcript).and_then(|session| workspace::resume_button(s, session, &k)))
                .children(transcript.then(|| tools_toggle(s, &k)))
                // Leave the fixed project controls' slots clear while the toolbar reaches them.
                .children((!setting && !inspector_shown).then(|| el().size(project_controls_width, 24.0).a11y_hidden())),
        );
    let mut layer = Vec::new();
    let (pane, inspector) = if setting {
        (preferences::pane(s, &k), None)
    } else if working {
        (workspace::pane(s, &k), (has_details || s.project.mode != project::Mode::Details).then(|| details(s, &k)))
    } else if config {
        (config_pane(s, &k), Some(config_details(s, &k)))
    } else {
        let reading = s.settings.appearance.reading_kit(cx.system_dark());
        (transcript_pane(s, &reading, &mut layer), Some(details(s, &k)))
    };
    let below = layer.len();
    workspace::quit_dialog(s, &k, &mut layer);
    workspace::tidy_dialog(s, &k, &mut layer);
    reload::dialog(s, &k, &mut layer);
    // The page is native, over everything Rune draws: it steps aside for a dialog or a toast.
    let covered = layer.len() > below;
    let full_project = s.project_uses_full_window();
    let project_top = if full_project { 8.0 } else { 0.0 };
    // Past the traffic lights: the sidebar's toggle, then Back and Forward.
    let lights = if cx.titlebar_inset() > 0.0 { cx.titlebar_inset() + 16.0 } else { 18.0 };
    let history_left = lights + 28.0 + 4.0;
    let project_tabs_left = if full_project { history_left + history::BUTTONS_WIDTH + 4.0 } else { 10.0 };
    let mut base = SplitView::new(pane).sidebar(&s.sidebar, |s: &mut App| &mut s.sidebar, sidebar(s, &k));
    if let Some(inspector) = inspector {
        let inspector = col().pt(shell::TOOLBAR_HEIGHT + project_top).grow(1.0).min_h(0.0)
            .child(match s.project.mode {
                project::Mode::Details => inspector,
                project::Mode::Browser => browser::pane(s, &k),
                _ => project::pane(s, &k),
            })
            .children((s.project.mode != project::Mode::Details).then(|| {
                let top = if s.project.mode == project::Mode::Browser { browser::tab_bar(s, &k) } else { project::tabs(s, &k) };
                // Centred on the panel toggles: they're 24pt tall at 20pt from the window's top, and
                // this 32pt row sits inside the window frame's inset.
                top.pin(Some(project_tabs_left), Some(8.0 + project_top), Some(project_controls_width + 28.0), None)
            }));
        base = base.inspector(&s.inspector, |s: &mut App| &mut s.inspector, inspector);
    }
    let base = base
        .toolbar(toolbar)
        .titlebar_inset(cx.titlebar_inset())
        .frame_inset(if full_project { 0.0 } else { 8.0 })
        .toolbar_in_panel()
        .sidebar_padding(18.0, 10.0)
        .sidebar_toggle(SidebarToggle::new().shortcut(Action::ToggleSidebar.hint(&s.settings)).size(ControlSize::Large).past_lights(16.0).edge(18.0).gap(4.0 + history::BUTTONS_WIDTH + 4.0))
        .pane_motion(PaneMotion::new().spring(Spring::new(0.3, 0.0)).scale_with_width(480.0, 0.15, 0.3).fade_tail(0.15))
        .sidebar_peek(SidebarPeek::new().edge(16.0, ms(40)).toggle_delay(ms(80)).below(56.0).hold(15.0, ms(100)).spring(Spring::new(0.18, 0.0)))
        .reduced_motion(cx.is_reduced_motion())
        .build(&k)
        .on_layout_tx(|s: &mut App, [_, _, w, _], tx| { if s.project.window_width == w { tx.live_only(); } else { s.project.window_width = w; s.fit_project_pane(); } })
        .children((!setting).then(|| panel_toggles(s, has_details, &k)))
        // Level with the sidebar's toggle: 28pt tall in the 8pt frame and the toolbar's band.
        .child(history::buttons(s, &k).absolute(history_left, 8.0 + (shell::TOOLBAR_HEIGHT - 28.0) / 2.0))
        .children(history::swipe_arrow(s, &k))
        .pointer_buttons([PointerButton::Back, PointerButton::Forward])
        .on_press_tx(|s: &mut App, tx| s.history_button(tx))
        .on_wheel(|s: &mut App, ev, tx| s.swipe(ev, tx));
    s.browser_sync(covered);
    rune_kit::root(base, layer).on_paste(|s: &mut App, text, tx| {
        if !s.project_has_keyboard(tx) && !s.browser_has_keyboard() && let Some(t) = s.typing_term() {
            t.paste(text);
        }
    })
}

/// Opens and closes the inspector's panes, at the window's top right whatever's under them.
fn panel_toggles(s: &App, has_details: bool, k: &Kit) -> El {
    let details = s.inspector.is_open() && s.project.mode == project::Mode::Details;
    let details_label = if details { "Hide details" } else { "Show details" };
    row().key("panel-toggles").gap(2.0).center_y()
        .child(panel_toggle(s, &FOLDER, project::Mode::Files, "Files", Action::ToggleFiles, "files-toggle", k))
        .child(panel_toggle(s, &project::CHANGES, project::Mode::Changes, "Changes", Action::ToggleChanges, "changes-toggle", k))
        .child(panel_toggle(s, &browser::GLOBE, project::Mode::Browser, "Browser", Action::ToggleBrowser, "browser-toggle", k))
        .children(has_details.then(|| panel_toggle(s, &shell::INSPECTOR, project::Mode::Details, details_label, Action::ToggleDetails, "inspector-toggle", k)))
        // Match its closed position: 8pt window frame + 12pt toolbar padding.
        .pin(None, Some(20.0), Some(20.0), None)
}

/// The keyboard stays where it was when one is clicked.
fn panel_toggle(s: &App, glyph: &'static Icon, mode: project::Mode, label: &str, action: Action, key: &'static str, k: &Kit) -> El {
    IconButton::new(glyph)
        .variant(IconButtonVariant::GhostMuted)
        .expanded(s.inspector.is_open() && s.project.mode == mode)
        .label(label)
        .help(label)
        .shortcut(action.hint(&s.settings))
        .on_press(move |s: &mut App| if mode == project::Mode::Browser { s.toggle_browser() } else { s.toggle_project_panel(mode) })
        .build(k)
        .key(key)
        .focusable(false)
        .preserve_focus_on_press(true)
}

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}
