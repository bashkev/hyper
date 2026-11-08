//! Event names and per-agent delivery policy, independent of terminal and macOS plumbing.

use std::collections::BTreeMap;
use serde_json::{Map, Value};
use crate::workspace::Tool;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Event { TurnCompleted, ApprovalRequested, InputRequested, Idle, Notification, Bell, Exited, Failed }

impl Event {
    pub const ALL: [Self; 8] = [Self::TurnCompleted, Self::ApprovalRequested, Self::InputRequested, Self::Idle, Self::Notification, Self::Bell, Self::Exited, Self::Failed];
    pub fn id(self) -> &'static str {
        match self { Self::TurnCompleted => "turn_completed", Self::ApprovalRequested => "approval_requested", Self::InputRequested => "input_requested", Self::Idle => "idle", Self::Notification => "notification", Self::Bell => "bell", Self::Exited => "exited", Self::Failed => "failed" }
    }
    pub fn label(self) -> &'static str {
        match self { Self::TurnCompleted => "Turn completed", Self::ApprovalRequested => "Approval requested", Self::InputRequested => "Input requested", Self::Idle => "Idle reminder", Self::Notification => "Other agent notifications", Self::Bell => "Terminal bell", Self::Exited => "Session exited", Self::Failed => "Session failed" }
    }
    /// The key of its control in Settings.
    pub fn setting_key(self) -> &'static str {
        match self { Self::TurnCompleted => "notification-turn_completed", Self::ApprovalRequested => "notification-approval_requested", Self::InputRequested => "notification-input_requested", Self::Idle => "notification-idle", Self::Notification => "notification-notification", Self::Bell => "notification-bell", Self::Exited => "notification-exited", Self::Failed => "notification-failed" }
    }
    pub fn supported(self, tool: Tool) -> bool {
        match (self, tool) {
            (Self::TurnCompleted | Self::ApprovalRequested | Self::InputRequested | Self::Idle, Tool::Agent(source)) => source.agent().reports(self),
            (Self::TurnCompleted | Self::ApprovalRequested | Self::InputRequested | Self::Idle, Tool::Shell) => false,
            _ => true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery { Off, Silent, Sound }
impl Delivery {
    pub fn id(self) -> &'static str { match self { Self::Off => "off", Self::Silent => "silent", Self::Sound => "sound" } }
    pub fn label(self) -> &'static str { match self { Self::Off => "Off", Self::Silent => "Silent", Self::Sound => "Sound" } }
    fn parse(value: &str) -> Option<Self> { match value { "off" => Some(Self::Off), "silent" => Some(Self::Silent), "sound" => Some(Self::Sound), _ => None } }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When { Background, OtherSession, Always }
impl When {
    pub fn id(self) -> &'static str { match self { Self::Background => "background", Self::OtherSession => "other_session", Self::Always => "always" } }
    pub fn label(self) -> &'static str { match self { Self::Background => "App in background", Self::OtherSession => "Not looking at it", Self::Always => "Always" } }
    pub fn parse(value: &str) -> Option<Self> { [Self::Background, Self::OtherSession, Self::Always].into_iter().find(|w| w.id() == value) }
    pub fn allows(self, app_focused: bool, looking: bool) -> bool {
        match self { Self::Background => !app_focused, Self::OtherSession => !looking, Self::Always => true }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Agent {
    pub when: When,
    pub dock_badge: bool,
    pub events: BTreeMap<String, String>,
}
impl Default for Agent {
    fn default() -> Self { Self { when: When::Background, dock_badge: true, events: BTreeMap::new() } }
}
impl Agent {
    pub fn delivery(&self, event: Event) -> Delivery {
        self.events.get(event.id()).and_then(|s| Delivery::parse(s)).unwrap_or(if event == Event::Exited { Delivery::Off } else { Delivery::Sound })
    }
    pub fn load(map: &Map<String, Value>) -> Self {
        let when = map.get("when").and_then(Value::as_str).and_then(When::parse).unwrap_or(When::Background);
        let dock_badge = map.get("dock_badge").and_then(Value::as_bool).unwrap_or(true);
        let events = map.get("events").and_then(Value::as_object).map(|events| events.iter().filter_map(|(key, value)| value.as_str().map(|v| (key.clone(), v.into()))).collect()).unwrap_or_default();
        Self { when, dock_badge, events }
    }
    pub fn merge(&self, mut map: Map<String, Value>) -> Value {
        map.insert("when".into(), self.when.id().into());
        map.insert("dock_badge".into(), self.dock_badge.into());
        let mut events = map.get("events").and_then(Value::as_object).cloned().unwrap_or_default();
        events.extend(self.events.iter().map(|(id, mode)| (id.clone(), Value::String(mode.clone()))));
        map.insert("events".into(), events.into());
        map.into()
    }
}

/// OSC 777 titles from Hyper's launch-scoped hooks and extensions are explicit event
/// identifiers. Otherwise the agent says what its own words mean; anything else's stay
/// unclassified.
pub fn classify(tool: Tool, title: &str, body: &str) -> Event {
    if let Some(id) = title.strip_prefix("Hyper:") && let Some(event) = Event::ALL.into_iter().find(|e| e.id() == id) { return event; }
    let message = if body.is_empty() { title } else { body };
    match tool {
        Tool::Agent(source) => source.agent().notice(message),
        Tool::Shell => Event::Notification,
    }
}

/// The OSC 777 titles an agent's Hyper extension says its session, and that it's working, under.
pub const SESSION: &str = "Hyper:session";
pub const WORKING: &str = "Hyper:working";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delivery_obeys_each_event_and_foreground_choice() {
        let mut agent = Agent::default();
        agent.events.insert("turn_completed".into(), "off".into());
        agent.events.insert("approval_requested".into(), "silent".into());
        assert_eq!(agent.delivery(Event::TurnCompleted), Delivery::Off);
        assert_eq!(agent.delivery(Event::ApprovalRequested), Delivery::Silent);
        assert_eq!(agent.delivery(Event::Bell), Delivery::Sound);
        assert!(!When::Background.allows(true, false));
        assert!(When::OtherSession.allows(true, false));
        assert!(!When::OtherSession.allows(true, true));
        assert!(When::Always.allows(true, true));
    }
}
