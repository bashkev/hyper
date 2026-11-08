//! Back and Forward: the places the person went, one history for the window across the
//! Workspace, Browse and Settings, as in Finder or Xcode.
//!
//! What's on screen is recorded as the view is built, so every way of getting somewhere counts:
//! the sidebar, the numbered shortcuts, the launcher, a notification. A place that's gone since,
//! like a closed terminal, is passed over.
//!
//! The ‹ › buttons sit past the sidebar's toggle; holding or right-clicking one lists where it
//! goes. ⌘[ and ⌘], the mouse's side buttons and a sideways two-finger swipe go too.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use rune::{El, GesturePhase, Phase, PointerButton, PopupAnchor, PopupItem, PopupMenu, Tx, WheelEvent, el, icon, row};
use rune_kit::{ControlSize, IconButton, IconButtonVariant, Kit};
use serde_json::{Value, json};

use crate::browse::Mode;
use crate::browser::{BACK, FORWARD};
use crate::commands::Action;
use crate::workspace::{self, Main};
use crate::{App, View};

/// Places kept to go back to.
const KEEP: usize = 50;
/// How long a button is held before it lists where it goes.
const HOLD: Duration = Duration::from_millis(400);
/// Points a sideways swipe travels to go.
const SWIPE: f32 = 100.0;

/// Somewhere the person can be.
#[derive(Clone, Debug, PartialEq)]
pub enum Place {
    Launcher,
    Terminal(u64),
    Session(String),
    Config(String),
    Settings,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Way {
    Back,
    Forward,
}

/// The places behind and ahead of where the person is.
#[derive(Default)]
pub struct Trail {
    back: VecDeque<Place>,
    forward: VecDeque<Place>,
    /// Where the person is, as of the last look.
    here: Option<Place>,
    /// Whether they stepped there with the keys, through a list.
    stepped: bool,
}

impl Trail {
    /// The person is at `place`. Somewhere new is a step: what was ahead goes. Arrowing through
    /// a list is one step, from where it started to where it stops, as `stepping` says.
    pub fn arrive(&mut self, place: Place, stepping: bool) {
        if self.here.as_ref() == Some(&place) {
            return;
        }
        let passing = std::mem::replace(&mut self.stepped, stepping) && stepping;
        if let Some(left) = self.here.replace(place)
            && !passing
        {
            self.back.push_back(left);
            if self.back.len() > KEEP {
                self.back.pop_front();
            }
            self.forward.clear();
        }
    }

    /// The places that way that still exist, nearest first.
    pub fn places(&self, way: Way, exists: impl Fn(&Place) -> bool) -> Vec<Place> {
        self.stack(way).iter().rev().filter(|p| exists(p)).cloned().collect()
    }

    /// Whether any place that way exists.
    pub fn any(&self, way: Way, exists: impl Fn(&Place) -> bool) -> bool {
        self.stack(way).iter().any(exists)
    }

    fn stack(&self, way: Way) -> &VecDeque<Place> {
        match way {
            Way::Back => &self.back,
            Way::Forward => &self.forward,
        }
    }

    /// Goes past `skip` places that way that exist, to the next, and says where that is. Places
    /// that don't exist are stepped over, not forgotten: one that isn't known yet, like a session
    /// before the list is read, can be gone back to once it is.
    pub fn go(&mut self, way: Way, skip: usize, exists: impl Fn(&Place) -> bool) -> Option<Place> {
        if self.places(way, &exists).len() <= skip {
            return None;
        }
        let (from, to) = match way {
            Way::Back => (&mut self.back, &mut self.forward),
            Way::Forward => (&mut self.forward, &mut self.back),
        };
        let mut left = skip + 1;
        // Stepped over since the last place, nearest it first.
        let mut over = VecDeque::new();
        while left > 0 {
            let place = from.pop_back()?;
            if !exists(&place) {
                over.push_back(place);
                continue;
            }
            if let Some(here) = self.here.replace(place) {
                to.push_back(here);
            }
            self.stepped = false;
            to.append(&mut over);
            left -= 1;
        }
        self.here.clone()
    }

    pub fn hand_off(&self) -> Value {
        json!({ "back": self.back.iter().map(place_json).collect::<Vec<_>>(), "forward": self.forward.iter().map(place_json).collect::<Vec<_>>() })
    }

    pub fn adopt(state: &Value) -> Trail {
        let places = |key: &str| state[key].as_array().into_iter().flatten().filter_map(place_from_json).collect();
        Trail { back: places("back"), forward: places("forward"), here: None, stepped: false }
    }
}

fn place_json(place: &Place) -> Value {
    match place {
        Place::Launcher => json!({ "launcher": true }),
        Place::Terminal(id) => json!({ "terminal": id }),
        Place::Session(id) => json!({ "session": id }),
        Place::Config(id) => json!({ "config": id }),
        Place::Settings => json!({ "settings": true }),
    }
}

fn place_from_json(v: &Value) -> Option<Place> {
    let text = |key: &str| v[key].as_str().map(str::to_owned);
    if v["launcher"].as_bool() == Some(true) {
        Some(Place::Launcher)
    } else if v["settings"].as_bool() == Some(true) {
        Some(Place::Settings)
    } else if let Some(id) = v["terminal"].as_u64() {
        Some(Place::Terminal(id))
    } else if let Some(id) = text("session") {
        Some(Place::Session(id))
    } else {
        text("config").map(Place::Config)
    }
}

/// A sideways swipe over the window, until the fingers lift.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Swipe {
    #[default]
    Idle,
    /// The gesture is scrolling, not going anywhere.
    Scrolling,
    /// Going `way`, this far along: at 1 the lift goes.
    Going { way: Way, travel: f32 },
    /// It went, or stopped short, and its momentum follows.
    Done,
}

#[derive(Default)]
pub struct History {
    /// In a `RefCell`: the view records where the person is.
    trail: RefCell<Trail>,
    /// The selection just moved by a key, not a click: see [`App::stepping`].
    stepping: Cell<bool>,
    /// A button being held, and when its list opens.
    hold: Option<(Way, Instant)>,
    /// The list opened from a hold, so letting go doesn't go too.
    held: bool,
    swipe: Swipe,
    /// Where the person was when the swipe started going. Somewhere else, the view under the
    /// fingers may have closed and taken the gesture's end with it, so the swipe is off.
    swipe_from: Option<Place>,
}

impl App {
    /// Where the person is.
    pub fn place(&self) -> Option<Place> {
        match self.view {
            View::Settings => Some(Place::Settings),
            View::Workspace => Some(match self.ws.main {
                Main::Launcher => Place::Launcher,
                Main::Terminal(id) => Place::Terminal(id),
            }),
            View::Browse => match self.mode {
                Mode::Sessions => self.reader.selected.clone().map(Place::Session),
                Mode::Config => self.config.selected.clone().map(Place::Config),
            },
        }
    }

    /// Records where the person is now. The view calls it, so it sees every way they got there.
    pub fn look(&self) {
        let stepping = self.history.stepping.take();
        if let Some(place) = self.place() {
            self.history.trail.borrow_mut().arrive(place, stepping);
        }
    }

    /// A sidebar row's `on_select`: whether a key moved the selection there, as when arrowing
    /// down a list. Rune runs that press inside the key's routing, where a focused row is
    /// known; a click's press has none.
    pub fn stepping(&self, tx: &Tx) {
        self.history.stepping.set(tx.focused().is_some());
    }

    fn exists(&self, place: &Place) -> bool {
        match place {
            Place::Launcher | Place::Settings => true,
            Place::Terminal(id) => self.live(*id).is_some(),
            Place::Session(id) => self.session(id).is_some(),
            Place::Config(id) => self.config.item(id).is_some(),
        }
    }

    #[cfg(test)]
    pub fn history_places(&self, way: Way) -> Vec<Place> {
        self.history.trail.borrow().places(way, |p| self.exists(p))
    }

    pub fn can_go(&self, way: Way) -> bool {
        self.history.trail.borrow().any(way, |p| self.exists(p))
    }

    /// Goes `way` to the `nth` time `place` comes that way, if it's still there.
    fn go_to(&mut self, way: Way, place: &Place, nth: usize, tx: &mut Tx) {
        self.look();
        let places = self.history.trail.borrow().places(way, |p| self.exists(p));
        if let Some(skip) = places.iter().enumerate().filter(|(_, p)| *p == place).map(|(i, _)| i).nth(nth) {
            self.go(way, skip, tx);
        }
    }

    /// Goes `way` past `skip` places.
    pub fn go(&mut self, way: Way, skip: usize, tx: &mut Tx) {
        self.look();
        let target = self.history.trail.borrow_mut().go(way, skip, |p| self.exists(p));
        if let Some(place) = target {
            self.visit(&place, tx);
        }
    }

    fn visit(&mut self, place: &Place, tx: &mut Tx) {
        match place {
            Place::Launcher => {
                self.set_view(View::Workspace);
                self.show(Main::Launcher);
            }
            Place::Terminal(id) => {
                self.set_view(View::Workspace);
                self.show(Main::Terminal(*id));
                tx.scroll_into_view(format!("live-{id}"));
            }
            Place::Session(id) => {
                if self.mode != Mode::Sessions {
                    self.set_mode(Mode::Sessions);
                }
                self.set_view(View::Browse);
                // Its row shows, under its project.
                if let Some(project) = self.session(id).map(|s| s.project.clone()) {
                    self.closed_projects.remove(&project);
                    self.open_projects.insert(project);
                }
                self.select(id);
                self.relist();
                tx.scroll_into_view(id.clone());
            }
            Place::Config(id) => {
                self.set_view(View::Browse);
                if self.mode != Mode::Config {
                    self.set_mode(Mode::Config);
                }
                self.config.selected = Some(id.clone());
                self.relist();
                tx.scroll_into_view(id.clone());
            }
            Place::Settings => self.set_view(View::Settings),
        }
        // As the sidebar's own shortcuts do: the terminal on screen takes the keys.
        tx.blur();
    }

    /// What a place is called in the button's list.
    fn place_title(&self, place: &Place) -> String {
        match place {
            Place::Launcher => "New Session".into(),
            Place::Settings => "Settings".into(),
            Place::Terminal(id) => self.live(*id).map(|l| workspace::live_title(self, l)).unwrap_or_default(),
            Place::Session(id) => self.session(id).map(|s| s.title.clone()).unwrap_or_default(),
            Place::Config(id) => self.config.item(id).map(|c| c.title.clone()).unwrap_or_default(),
        }
    }

    /// Lists the places that way, to go to one.
    fn history_menu(&mut self, way: Way, anchor: PopupAnchor, tx: &mut Tx) {
        let places = self.history.trail.borrow().places(way, |p| self.exists(p));
        if places.is_empty() {
            return;
        }
        let items = places.iter().enumerate().map(|(i, place)| {
            let symbol = match place {
                Place::Launcher => "plus",
                Place::Terminal(_) => "terminal",
                Place::Session(_) => "text.bubble",
                Place::Config(_) => "doc.text",
                Place::Settings => "gearshape",
            };
            PopupItem::new(i.to_string(), self.place_title(place)).symbol(symbol)
        });
        // A place may close while the list is open: what's picked is a place, not a count.
        tx.popup_menu(PopupMenu::new().items(items), anchor, move |s: &mut App, picked, tx| {
            if let Some(i) = picked.and_then(|p| p.parse::<usize>().ok()).filter(|i| *i < places.len()) {
                let nth = places[..i].iter().filter(|p| **p == places[i]).count();
                s.go_to(way, &places[i], nth, tx);
            }
        });
    }

    /// Opens a held button's list once it's been held long enough. Says whether it did.
    pub fn take_hold(&mut self, tx: &mut Tx) -> bool {
        let Some((way, due)) = self.history.hold else { return false };
        let now = Instant::now();
        if now < due {
            tx.wake_after(due - now);
            return false;
        }
        self.history.hold = None;
        self.history.held = true;
        self.history_menu(way, button_key(way).into(), tx);
        true
    }

    /// The mouse's side buttons.
    pub fn history_button(&mut self, tx: &mut Tx) {
        match tx.button() {
            PointerButton::Back => self.go(Way::Back, 0, tx),
            PointerButton::Forward => self.go(Way::Forward, 0, tx),
            _ => {}
        }
    }

    /// A trackpad scroll over a view that doesn't scroll sideways: one that starts sideways is a
    /// swipe back or forward, as Safari's is, when System Settings allows it. Says whether it took
    /// the scroll.
    pub fn swipe(&mut self, ev: WheelEvent, tx: &mut Tx) -> bool {
        if matches!(self.history.swipe, Swipe::Going { .. }) && self.place() != self.history.swipe_from {
            self.history.swipe = Swipe::Done;
        }
        let swipe = self.history.swipe;
        if ev.phase == GesturePhase::Began && !ev.momentum {
            self.history.swipe = Swipe::Idle;
        }
        // A wheel's notches are never part of one.
        if !ev.precise {
            return false;
        }
        // A gesture's coasting never starts one, and follows one that went.
        if ev.momentum {
            return matches!(swipe, Swipe::Going { .. } | Swipe::Done);
        }
        match self.history.swipe {
            Swipe::Idle if ev.dx != 0.0 || ev.dy != 0.0 => {
                // Fingers moving right bring back what's to the left, as a page turns.
                let way = if ev.dx > 0.0 { Way::Back } else { Way::Forward };
                if ev.dx.abs() <= ev.dy.abs() || !swipes_navigate() || !self.can_go(way) {
                    self.history.swipe = Swipe::Scrolling;
                    return false;
                }
                self.history.swipe = Swipe::Going { way, travel: 0.0 };
                self.history.swipe_from = self.place();
                self.swipe_moved(ev, tx)
            }
            Swipe::Idle | Swipe::Scrolling => false,
            Swipe::Going { .. } => self.swipe_moved(ev, tx),
            Swipe::Done => true,
        }
    }

    /// The window lost focus: a gesture going on won't report its end here, so it's off.
    pub fn swipe_lost(&mut self) {
        if matches!(self.history.swipe, Swipe::Going { .. }) {
            self.history.swipe = Swipe::Done;
        }
    }

    fn swipe_moved(&mut self, ev: WheelEvent, tx: &mut Tx) -> bool {
        let Swipe::Going { way, travel } = self.history.swipe else { return false };
        let along = if way == Way::Back { ev.dx } else { -ev.dx };
        let travel = (travel + along / SWIPE).clamp(0.0, 1.0);
        tx.instant();
        if ev.phase == GesturePhase::Ended {
            self.history.swipe = Swipe::Done;
            if travel >= 1.0 {
                self.go(way, 0, tx);
            }
        } else {
            self.history.swipe = Swipe::Going { way, travel };
        }
        true
    }
}

fn button_key(way: Way) -> &'static str {
    match way {
        Way::Back => "history-back",
        Way::Forward => "history-forward",
    }
}

/// Whether System Settings has two-finger swipes go between pages.
fn swipes_navigate() -> bool {
    #[cfg(not(test))]
    {
        use objc2::{class, msg_send};
        // SAFETY: a class method of NSEvent with no arguments.
        unsafe { msg_send![class!(NSEvent), isSwipeTrackingFromScrollEventsEnabled] }
    }
    #[cfg(test)]
    true
}

/// Lets a sideways swipe over a view that scrolls up and down go back or forward. A view inside
/// it that scrolls sideways, like a wide code block, keeps its swipes.
pub fn swipeable(scroll: El) -> El {
    scroll.on_wheel(|s: &mut App, ev, tx| s.swipe(ev, tx))
}

/// Back and Forward, side by side.
pub fn buttons(s: &App, k: &Kit) -> El {
    row().key("history").gap(2.0).center_y().child(button(s, Way::Back, k)).child(button(s, Way::Forward, k))
}

/// The width [`buttons`] take.
pub const BUTTONS_WIDTH: f32 = 28.0 * 2.0 + 2.0;

fn button(s: &App, way: Way, k: &Kit) -> El {
    let (glyph, label, action) = match way {
        Way::Back => (&BACK, "Back", Action::GoBack),
        Way::Forward => (&FORWARD, "Forward", Action::GoForward),
    };
    let key = button_key(way);
    let enabled = s.can_go(way);
    let mut b = IconButton::new(glyph)
        .variant(IconButtonVariant::GhostMuted)
        .control_size(ControlSize::Large)
        .label(label)
        .help(label)
        .shortcut(action.hint(&s.settings))
        .disabled(!enabled)
        .on_press_tx(move |s: &mut App, tx| {
            // Letting go after the list opened from holding.
            if !std::mem::take(&mut s.history.held) {
                s.go(way, 0, tx);
            }
        })
        .build(k)
        .key(key)
        .focusable(false)
        .preserve_focus_on_press(true);
    if enabled {
        b = b
            .on_drag(move |s: &mut App, d, tx| match d.phase {
                Phase::Down => {
                    s.history.held = false;
                    s.history.hold = Some((way, Instant::now() + HOLD));
                    tx.wake_after(HOLD);
                }
                Phase::Up => s.history.hold = None,
                Phase::Move => {}
            })
            .on_secondary_press(move |s: &mut App, _, tx| s.history_menu(way, key.into(), tx));
    }
    b
}

/// While a swipe is going, an arrow comes in from the side it goes to.
pub fn swipe_arrow(s: &App, k: &Kit) -> Option<El> {
    let Swipe::Going { way, travel } = s.history.swipe else { return None };
    if s.place() != s.history.swipe_from {
        return None;
    }
    let size = 36.0;
    let x = 8.0 + 24.0 * travel;
    let glyph = if way == Way::Back { &BACK } else { &FORWARD };
    let ready = travel >= 1.0;
    let arrow = el()
        .key("swipe-arrow")
        .size(size, size)
        .radius(size / 2.0)
        .center()
        .fill(if ready { k.c.bg_accent } else { k.c.surface_float })
        .border(1.0, k.c.border_muted)
        .opacity(travel)
        .child(icon(glyph, 18.0).color(if ready { k.c.white } else { k.c.icon_base }))
        .a11y_hidden();
    // Halfway down the window, on the side it comes from.
    let wrap = el().key("swipe-arrow-row").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).center_y();
    Some(match way {
        Way::Back => wrap.child(arrow.ml(x)),
        Way::Forward => wrap.justify_end().child(arrow.mr(x)),
    })
}

impl App {
    pub fn history_hand_off(&self) -> Value {
        self.look();
        self.history.trail.borrow().hand_off()
    }

    /// Takes back what [`App::history_hand_off`] gave, once where the person is has come back.
    pub fn adopt_history(&mut self, state: &Value) {
        *self.history.trail.get_mut() = Trail::adopt(state);
        self.look();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walk(places: &[Place]) -> Trail {
        let mut trail = Trail::default();
        for place in places {
            trail.arrive(place.clone(), false);
        }
        trail
    }

    #[test]
    fn back_and_forward_retrace_the_steps_and_a_new_one_drops_what_was_ahead() {
        let a = Place::Terminal(1);
        let b = Place::Session("b".into());
        let c = Place::Settings;
        let mut trail = walk(&[a.clone(), b.clone(), c.clone()]);
        let all = |_: &Place| true;
        assert_eq!(trail.places(Way::Back, all), [b.clone(), a.clone()]);
        assert_eq!(trail.go(Way::Back, 0, all), Some(b.clone()));
        assert_eq!(trail.go(Way::Back, 0, all), Some(a.clone()));
        assert_eq!(trail.go(Way::Back, 0, all), None, "nothing before the first place");
        assert_eq!(trail.places(Way::Forward, all), [b.clone(), c.clone()]);
        assert_eq!(trail.go(Way::Forward, 1, all), Some(c.clone()), "the list can skip ahead");
        assert_eq!(trail.places(Way::Back, all), [b.clone(), a.clone()]);

        trail.go(Way::Back, 0, all);
        trail.arrive(Place::Launcher, false);
        assert!(trail.places(Way::Forward, all).is_empty(), "going somewhere new drops what was ahead");
        assert_eq!(trail.places(Way::Back, all), [b, a]);
    }

    #[test]
    fn staying_put_is_no_step_and_places_that_are_gone_are_passed_over() {
        let mut trail = walk(&[Place::Terminal(1), Place::Terminal(2), Place::Terminal(2), Place::Terminal(3)]);
        let open = |p: &Place| *p != Place::Terminal(2);
        assert_eq!(trail.places(Way::Back, |_| true), [Place::Terminal(2), Place::Terminal(1)]);
        assert_eq!(trail.go(Way::Back, 0, open), Some(Place::Terminal(1)), "the closed terminal is passed over");
        assert_eq!(trail.places(Way::Forward, open), [Place::Terminal(3)]);
        assert_eq!(trail.go(Way::Back, 0, open), None);
    }

    #[test]
    fn a_place_not_known_yet_is_stepped_over_and_found_again_once_it_is() {
        let session = Place::Session("s".into());
        let mut trail = walk(&[Place::Launcher, session.clone(), Place::Settings]);
        let loading = |p: &Place| *p != session;
        assert_eq!(trail.go(Way::Back, 0, loading), Some(Place::Launcher));
        assert_eq!(trail.go(Way::Forward, 0, |_| true), Some(session), "the session is there once the list is read");
    }

    #[test]
    fn arrowing_through_a_list_is_one_step_from_where_it_started() {
        let mut trail = walk(&[Place::Settings, Place::Session("a".into())]);
        for id in ["b", "c", "d"] {
            trail.arrive(Place::Session(id.into()), true);
        }
        let all = |_: &Place| true;
        assert_eq!(trail.places(Way::Back, all), [Place::Session("a".into()), Place::Settings]);
        assert_eq!(trail.go(Way::Back, 0, all), Some(Place::Session("a".into())));
        trail.arrive(Place::Session("e".into()), true);
        trail.arrive(Place::Session("f".into()), true);
        assert_eq!(trail.places(Way::Back, all), [Place::Session("a".into()), Place::Settings], "going back ends the run");
        trail.arrive(Place::Launcher, false);
        assert_eq!(trail.places(Way::Back, all)[0], Place::Session("f".into()), "where the arrows stopped is a place");
    }

    #[test]
    fn only_the_last_places_are_kept() {
        let places: Vec<_> = (0..KEEP as u64 + 10).map(Place::Terminal).collect();
        let trail = walk(&places);
        let back = trail.places(Way::Back, |_| true);
        assert_eq!(back.len(), KEEP);
        assert_eq!(back.last(), Some(&Place::Terminal(9)));
    }

    #[test]
    fn the_trail_survives_a_hand_off() {
        let mut trail = walk(&[Place::Launcher, Place::Terminal(4), Place::Session("s".into()), Place::Config("c".into()), Place::Settings]);
        trail.go(Way::Back, 1, |_| true);
        let adopted = Trail::adopt(&trail.hand_off());
        assert_eq!(adopted.places(Way::Back, |_| true), trail.places(Way::Back, |_| true));
        assert_eq!(adopted.places(Way::Forward, |_| true), trail.places(Way::Forward, |_| true));
    }
}
