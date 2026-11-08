//! Hyper's Settings pane. Every control saves as it changes, as macOS settings do.

use std::collections::BTreeMap;
use rune::{El, PopupMenu, ScrollSpec, Shortcut, ShortcutError, Tx, col, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, SegmentedControl, Select, SelectAppearance, ShortcutRecorder, ShortcutRecorderState, Switch, shortcut_error_message};
use crate::{App, commands::{self, Action}, fonts, settings::{self, Appearance, Links, Range, Settings}, workspace::Tool};

pub struct Preferences {
    pub tab: usize,
    agent: usize,
    recorders: BTreeMap<Action, ShortcutRecorderState>,
}

impl Preferences {
    pub fn new(settings: &Settings) -> Self {
        let mut p = Self { tab: 0, agent: 0, recorders: BTreeMap::new() };
        p.sync(settings);
        p
    }

    /// Shows what's saved, as when the pane opens or the defaults come back.
    pub fn sync(&mut self, settings: &Settings) {
        self.recorders = Action::ALL.iter().map(|&a| (a, ShortcutRecorderState::new(a.binding(settings)))).collect();
    }

    pub fn recording(&self) -> bool {
        self.recorders.values().any(ShortcutRecorderState::is_recording)
    }
}

impl App {
    /// Settings are only reflected in controls after the durable write succeeds.
    pub fn persist_settings(&mut self, changed: Settings) -> bool {
        match changed.save(&self.settings_path) {
            Ok(()) => {
                self.settings_due = None;
                let menus = changed.shortcuts != self.settings.shortcuts;
                self.settings = changed;
                self.settings_error = None;
                self.rebadge();
                self.refresh_hyper_config();
                if menus && let Some(window) = &self.window && let Err(e) = window.set_menu(commands::menu(&self.settings)) {
                    self.settings_error = Some(format!("Couldn't update menus: {e}"));
                }
                true
            }
            Err(e) => { self.settings_error = Some(format!("Couldn't save settings: {e}")); false }
        }
    }

    /// Changes a setting only once it's saved, so the switch never shows what the next launch
    /// won't do.
    pub fn set_open_maximized(&mut self, on: bool) {
        let mut changed = self.settings.clone();
        changed.open_maximized = on;
        self.persist_settings(changed);
    }

    /// Config lists Hyper's settings as they are in effect, so it reads them again while it shows.
    fn refresh_hyper_config(&self) {
        if self.view == crate::View::Browse && self.mode == crate::Mode::Config {
            self.request_config();
        }
    }

    /// Uses `changed` now and writes it once the key making it stops repeating.
    pub fn save_settings_soon(&mut self, changed: Settings, tx: &mut Tx) {
        const SETTLE: std::time::Duration = std::time::Duration::from_millis(400);
        self.settings = changed;
        self.refresh_hyper_config();
        self.settings_due = Some(std::time::Instant::now() + SETTLE);
        tx.set_font_families(&self.settings.appearance.interface_font, &self.settings.appearance.monospace_font);
        tx.instant();
        tx.wake_after(SETTLE);
    }

    /// Writes settings that so far changed only in memory, once they're due.
    pub fn flush_settings(&mut self, tx: &mut Tx) {
        let Some(due) = self.settings_due else { return };
        let now = std::time::Instant::now();
        if now < due {
            return tx.wake_after(due - now);
        }
        self.settings_due = None;
        if let Err(e) = self.settings.save(&self.settings_path) {
            self.settings_error = Some(format!("Couldn't save settings: {e}"));
        }
    }

    pub fn save_settings(&mut self, changed: Settings, tx: &mut Tx) -> bool {
        if !self.persist_settings(changed) { return false; }
        tx.set_font_families(&self.settings.appearance.interface_font, &self.settings.appearance.monospace_font);
        tx.instant();
        true
    }

    fn change_appearance(&mut self, tx: &mut Tx, change: impl FnOnce(&mut Appearance)) {
        let mut changed = self.settings.clone();
        change(&mut changed.appearance);
        if changed != self.settings { self.save_settings(changed, tx); }
    }
}

fn setting(label: &str, k: &Kit, control: El) -> El {
    row().w_full().gap(20.0).center_y().justify_between()
        .child(text(label.to_owned()).font(k.ty.ui).color(k.c.text_base).wrap().min_w(0.0)).child(control)
}

/// A pop-up button: the current choice, opening a native menu of the others.
fn popup(k: &Kit, key: &'static str, label: &str, options: Vec<(String, String)>, current: &str, width: f32, pick: impl Fn(&mut App, &str, &mut Tx) + Clone + 'static) -> El {
    let shown = options.iter().find(|(id, _)| id == current).map_or(current.to_owned(), |(_, title)| title.clone());
    let current = current.to_owned();
    Select::new(key, &[shown.as_str()]).current(Some(&shown)).label(label).appearance(SelectAppearance::Field).width(width).trigger(k)
        .on_press_tx(move |_: &mut App, tx: &mut Tx| {
            let pick = pick.clone();
            let menu = PopupMenu::picker(options.clone(), &current);
            tx.popup_menu(menu, format!("{key}-trigger"), move |s: &mut App, picked, tx| {
                if let Some(id) = picked { pick(s, id, tx); }
            });
        })
}

fn general(s: &App, k: &Kit) -> El {
    let toggle = |label: &'static str, key: &'static str, on: bool, flip: fn(&mut Settings)| {
        setting(label, k, Switch::new(on).label(label).hide_label().on_toggle(move |s: &mut App| {
            let mut changed = s.settings.clone(); flip(&mut changed); s.persist_settings(changed);
        }).build(k).key(key))
    };
    let maximized = setting("Open maximized", k, Switch::new(s.settings.open_maximized).label("Open maximized").hide_label()
        .on_toggle(|s: &mut App| s.set_open_maximized(!s.settings.open_maximized)).build(k).key("open-maximized"));
    let tool = SegmentedControl::new(&Tool::ALL.map(Tool::label), Tool::from_id(&s.settings.default_tool).map(Tool::index))
        .label("Default tool").width(None).on_pick(|s: &mut App, i, _| {
            let mut changed = s.settings.clone(); changed.default_tool = Tool::ALL[i].id().into(); s.persist_settings(changed);
        }).build(k).key("default-tool");
    let links = SegmentedControl::new(&Links::ALL.map(Links::label), Links::ALL.iter().position(|l| *l == s.settings.links))
        .label("Open links in").width(None).on_pick(|s: &mut App, i, _| {
            let mut changed = s.settings.clone(); changed.links = Links::ALL[i]; s.persist_settings(changed);
        }).build(k).key("open-links");
    col().w_full().gap(16.0).child(maximized)
        .child(toggle("Remember last project", "remember-project", s.settings.remember_project, |c| c.remember_project = !c.remember_project))
        .child(toggle("Start in a new worktree", "default-worktree", s.settings.default_worktree, |c| c.default_worktree = !c.default_worktree))
        .child(col().w_full().gap(8.0).child(text("Default tool").font(k.ty.ui).color(k.c.text_base)).child(tool))
        .child(col().w_full().gap(8.0).child(text("Open links in").font(k.ty.ui).color(k.c.text_base)).child(links))
}

fn font_picker(s: &App, k: &Kit, mono: bool) -> El {
    let (key, label, current) = if mono {
        ("monospace-font", "Monospace font", &s.settings.appearance.monospace_font)
    } else {
        ("interface-font", "Interface font", &s.settings.appearance.interface_font)
    };
    let default = if mono { Appearance::default().monospace_font } else { Appearance::default().interface_font };
    let fonts = if mono { fonts::session() } else { fonts::INTERFACE.to_vec() };
    let options = fonts.into_iter()
        .map(|(family, name)| (family.to_owned(), if family == default { format!("{name} (Default)") } else { name.to_owned() }))
        .collect();
    let control = popup(k, key, label, options, current, 240.0, move |s: &mut App, family, tx| {
        if !fonts::offered(family, mono) { return; }
        let family = family.to_owned();
        s.change_appearance(tx, |a| if mono { a.monospace_font = family } else { a.interface_font = family });
    });
    setting(label, k, control)
}

fn size_picker(s: &App, k: &Kit, key: &'static str, label: &'static str, range: &'static Range, get: fn(&mut Appearance) -> &mut f32) -> El {
    let mut a = s.settings.appearance.clone();
    let current = *get(&mut a);
    let title = |v: f32| if range.step < 1.0 { format!("{v:.2}") } else { format!("{v:.0} pt") };
    let mut steps = range.steps();
    if !steps.iter().any(|v| (v - current).abs() < 0.001) {
        steps.push(current);
        steps.sort_by(f32::total_cmp);
    }
    let options = steps.into_iter().map(|v| (v.to_string(), title(v))).collect();
    let control = popup(k, key, label, options, &current.to_string(), 110.0, move |s: &mut App, id, tx| {
        if let Ok(v) = id.parse::<f32>().map(|v| range.clamp(v)) { s.change_appearance(tx, |a| *get(a) = v); }
    });
    setting(label, k, control)
}

fn appearance(s: &App, k: &Kit) -> El {
    col().w_full().gap(16.0)
        .child(font_picker(s, k, false))
        .child(size_picker(s, k, "interface-size", "Interface size", &settings::INTERFACE_SIZE, |a| &mut a.interface_size))
        .child(font_picker(s, k, true))
        .child(size_picker(s, k, "content-size", "Content size", &settings::CONTENT_SIZE, |a| &mut a.content_size))
        .child(size_picker(s, k, "line-spacing", "Line spacing", &settings::LINE_SPACING, |a| &mut a.line_spacing))
        .child(row().w_full().justify_end().pt(8.0)
            .child(Button::new("Restore Defaults").variant(ButtonVariant::Neutral).on_press_tx(|s: &mut App, tx| {
                s.change_appearance(tx, |a| *a = Appearance::default());
            }).build(k).key("reset-appearance")))
}

fn recorder(s: &App, k: &Kit, action: Action) -> El {
    let control = ShortcutRecorder::new(format!("record-{}", action.id()), &s.preferences.recorders[&action], move |s: &mut App| s.preferences.recorders.get_mut(&action).unwrap())
        .label(action.title()).width(150.0)
        .on_change(move |s: &mut App, next: Option<&Shortcut>, tx| rebind(s, action, next, tx))
        .error_message(|error, shortcut| match error { ShortcutError::Backend(why) => why.clone(), _ => shortcut_error_message(error, shortcut) })
        .build(k);
    // The slot stays when there's nothing to clear, so the recorders keep one column.
    let mut clear = row().size(24.0, 24.0).mt(4.0);
    if s.preferences.recorders[&action].shortcut().is_some() {
        clear = clear.child(IconButton::new(k.glyph.clear).variant(IconButtonVariant::GhostMuted).control_size(ControlSize::Regular)
            .label(format!("Clear {} shortcut", action.title())).help("Clear shortcut")
            .on_press_tx(move |s: &mut App, tx| {
                if rebind(s, action, None, tx).is_ok() { s.preferences.recorders.insert(action, ShortcutRecorderState::new(None)); }
            })
            .build(k).key(format!("clear-{}", action.id())));
    }
    row().key(action.id()).w_full().gap(12.0).py(2.0)
        .child(text(action.title()).font(k.ty.ui).color(k.c.text_base).grow(1.0).min_w(0.0).wrap().pt(7.0))
        .child(row().gap(4.0).child(control).child(clear))
}

/// Saves `action` bound to `next` (None turns it off) unless another action has it.
fn rebind(s: &mut App, action: Action, next: Option<&Shortcut>, tx: &mut Tx) -> Result<(), ShortcutError> {
    if let Some(shortcut) = next { commands::check(&s.settings, action, shortcut).map_err(ShortcutError::Backend)?; }
    let mut changed = s.settings.clone();
    commands::bind(&mut changed, action, next);
    if !s.save_settings(changed, tx) { return Err(ShortcutError::Backend(s.settings_error.clone().unwrap_or_default())); }
    // Releasing a saved binding can restore another command's default. Keep the active
    // recorder intact so it still swallows repeats until the captured key is released.
    for (&other, state) in &mut s.preferences.recorders {
        if other != action {
            let binding = other.binding(&s.settings);
            if state.shortcut() != binding.as_ref() {
                *state = ShortcutRecorderState::new(binding);
            }
        }
    }
    Ok(())
}

fn shortcuts(s: &App, k: &Kit) -> El {
    let groups = Action::menus().into_iter().map(|menu| {
        col().key(format!("group-{menu}")).w_full().gap(4.0)
            .child(text(menu).font(k.ty.small_medium).color(k.c.text_weak).pb(4.0))
            .children(Action::ALL.iter().filter(|a| a.menu() == menu).map(|&a| recorder(s, k, a)))
    });
    col().w_full().gap(20.0).children(groups)
        .child(row().w_full().justify_end()
            .child(Button::new("Restore Defaults").variant(ButtonVariant::Neutral).on_press_tx(|s: &mut App, tx| {
                let mut changed = s.settings.clone();
                changed.shortcuts.clear();
                if s.save_settings(changed, tx) { s.preferences.sync(&s.settings); }
            }).build(k).key("reset-shortcuts")))
}

fn notification_settings(s: &App, k: &Kit) -> El {
    use crate::notifications::{self, Delivery, Event, When};
    let tool = Tool::ALL[s.preferences.agent];
    let policy = s.settings.notifications_for(tool);
    let change = move |s: &mut App, edit: &dyn Fn(&mut notifications::Agent)| {
        let mut changed = s.settings.clone();
        edit(changed.notifications.entry(tool.id().into()).or_default());
        s.persist_settings(changed);
    };
    let agents = SegmentedControl::new(&Tool::ALL.map(Tool::label), Some(s.preferences.agent)).label("Agent").width(None)
        .on_pick(|s: &mut App, i, _| s.preferences.agent = i).build(k).key("notification-agent");
    let whens = [When::Background, When::OtherSession, When::Always].map(|w| (w.id().to_owned(), w.label().to_owned())).to_vec();
    let when = popup(k, "notification-when", "Notify when", whens, policy.when.id(), 220.0, move |s, id, _| {
        if let Some(w) = When::parse(id) { change(s, &|a| a.when = w); }
    });
    let badge = setting("Dock badge", k, Switch::new(policy.dock_badge).label("Dock badge").hide_label()
        .on_toggle(move |s: &mut App| change(s, &|a| a.dock_badge = !a.dock_badge)).build(k).key("notification-badge"));
    let deliveries = [Delivery::Off, Delivery::Silent, Delivery::Sound].map(|d| (d.id().to_owned(), d.label().to_owned())).to_vec();
    let events = Event::ALL.into_iter().filter(|event| event.supported(tool)).map(|event| {
        let key: &'static str = event.setting_key();
        let control = popup(k, key, event.label(), deliveries.clone(), policy.delivery(event).id(), 110.0, move |s, id, _| {
            change(s, &|a| { a.events.insert(event.id().into(), id.into()); });
        });
        setting(event.label(), k, control)
    });
    col().w_full().gap(16.0).child(agents).child(setting("Notify when", k, when)).child(badge).children(events)
}

pub fn pane(s: &App, k: &Kit) -> El {
    let tabs = SegmentedControl::new(&crate::config::SETTINGS_TABS, Some(s.preferences.tab)).label("Settings section").width(None)
        .on_pick(|s: &mut App, i, _| s.preferences.tab = i).build(k).key("settings-tabs");
    let page = match s.preferences.tab { 1 => appearance(s, k), 2 => shortcuts(s, k), 3 => notification_settings(s, k), _ => general(s, k) };
    let error = s.settings_error.as_ref().map(|e| text(e.clone()).font(k.ty.ui).color(k.c.danger.fg).wrap().key("settings-error"));
    // The tabs and a save error stay in view while a long section scrolls under them.
    let header = row().w_full().justify_center().px(28.0).pt(24.0)
        .child(col().w_full().max_w(580.0).gap(16.0).child(tabs).children(error));
    let body = col().key("settings-scroll").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false })
        .child(row().w_full().justify_center().px(28.0).child(col().w_full().max_w(580.0).pb(24.0).child(page)));
    let body = crate::history::swipeable(body);
    col().key("settings").w_full().h_full().min_h(0.0).gap(24.0).child(header).child(body)
}
