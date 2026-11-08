//! Hyper's own settings, kept in `~/Library/Application Support/Hyper/settings.json`.

use std::path::{Path, PathBuf};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};

const OPEN_MAXIMIZED: &str = "open_maximized";

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    /// Open the window filling the screen, as Ghostty's `maximize = true` does.
    pub open_maximized: bool,
    pub appearance: Appearance,
    pub default_tool: String,
    pub default_worktree: bool,
    pub remember_project: bool,
    pub links: Links,
    /// Where a newly started terminal goes among its project's in the Workspace sidebar.
    pub sidebar_order: Order,
    pub last_folder: Option<PathBuf>,
    /// Missing entries use the action catalog's default; an empty string disables a binding.
    pub shortcuts: BTreeMap<String, String>,
    pub notifications: BTreeMap<String, crate::notifications::Agent>,
}

/// Where a link clicked in a terminal opens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Links {
    /// The browser pane, for web pages; other links still go to macOS.
    #[default]
    Hyper,
    Browser,
}

impl Links {
    pub const ALL: [Links; 2] = [Links::Hyper, Links::Browser];

    fn id(self) -> &'static str {
        match self {
            Links::Hyper => "hyper",
            Links::Browser => "browser",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Links::Hyper => "Hyper",
            Links::Browser => "Default Browser",
        }
    }
}

/// The order of a project's terminals in the Workspace sidebar, by when each was started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Order {
    #[default]
    Oldest,
    Newest,
}

impl Order {
    pub const ALL: [Order; 2] = [Order::Newest, Order::Oldest];

    pub fn id(self) -> &'static str {
        match self {
            Order::Oldest => "oldest",
            Order::Newest => "newest",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Order::Oldest => "Oldest First",
            Order::Newest => "Newest First",
        }
    }

    pub fn from_id(id: &str) -> Option<Order> {
        Order::ALL.into_iter().find(|o| o.id() == id)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Appearance {
    pub interface_font: String,
    pub interface_size: f32,
    pub monospace_font: String,
    pub content_size: f32,
    pub line_spacing: f32,
}

impl Default for Appearance {
    fn default() -> Self {
        Self { interface_font: "Inter".into(), interface_size: 13.0, monospace_font: "ui-monospace".into(), content_size: 13.0, line_spacing: 17.0 / 13.0 }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self { open_maximized: false, appearance: Appearance::default(), default_tool: "claude".into(), default_worktree: false, remember_project: true, links: Links::default(), sidebar_order: Order::default(), last_folder: None, shortcuts: BTreeMap::new(), notifications: BTreeMap::new() }
    }
}

/// A size's bounds and the steps its picker offers. Loading, validation, the pickers and the
/// zoom keys all use these, so they can't disagree.
pub struct Range { pub min: f32, pub max: f32, pub step: f32 }

impl Range {
    pub fn contains(&self, value: f32) -> bool { value.is_finite() && (self.min..=self.max).contains(&value) }
    pub fn clamp(&self, value: f32) -> f32 { value.clamp(self.min, self.max) }
    /// Every step from `min` to `max`, as the picker lists them.
    pub fn steps(&self) -> Vec<f32> {
        let n = ((self.max - self.min) / self.step).round() as usize;
        (0..=n).map(|i| ((self.min + self.step * i as f32) * 100.0).round() / 100.0).collect()
    }
}

pub const INTERFACE_SIZE: Range = Range { min: 10.0, max: 20.0, step: 1.0 };
pub const CONTENT_SIZE: Range = Range { min: 8.0, max: 36.0, step: 1.0 };
pub const LINE_SPACING: Range = Range { min: 1.0, max: 2.0, step: 0.05 };

impl Appearance {
    /// Each size with its saved key, its label and its bounds.
    pub fn sizes_mut(&mut self) -> [(&'static str, &'static str, &'static Range, &mut f32); 3] {
        [
            ("interface_size", "Interface size", &INTERFACE_SIZE, &mut self.interface_size),
            ("content_size", "Content size", &CONTENT_SIZE, &mut self.content_size),
            ("line_spacing", "Line spacing", &LINE_SPACING, &mut self.line_spacing),
        ]
    }

    pub fn fonts_mut(&mut self) -> [(&'static str, &mut String); 2] {
        [("interface_font", &mut self.interface_font), ("monospace_font", &mut self.monospace_font)]
    }

    pub fn kit(&self, dark: bool) -> rune_kit::Kit {
        let mut k = rune_kit::Kit::neutral(dark);
        scale_type(&mut k, self.interface_size / 13.0);
        // Every code surface uses the same content metrics, independently of the interface.
        k.ty.mono.size = self.content_size;
        k.ty.mono.line_height = self.content_size * self.line_spacing;
        k
    }

    /// Transcript prose and code share content size and spacing; navigation stays in `kit`.
    pub fn reading_kit(&self, dark: bool) -> rune_kit::Kit {
        let mut k = self.kit(dark);
        scale_type(&mut k, self.content_size / self.interface_size);
        k.ty.body.size = self.content_size;
        k.ty.body.line_height = self.content_size * self.line_spacing;
        k.ty.ui.line_height = k.ty.body.line_height;
        k.ty.ui_medium.line_height = k.ty.body.line_height;
        k
    }
}

pub fn scale_type(k: &mut rune_kit::Kit, scale: f32) {
    for style in [&mut k.ty.ui, &mut k.ty.ui_medium, &mut k.ty.ui_compact, &mut k.ty.small, &mut k.ty.small_medium, &mut k.ty.body, &mut k.ty.body_medium, &mut k.ty.text_small, &mut k.ty.text_small_medium, &mut k.ty.text_xsmall, &mut k.ty.text_xsmall_medium] {
        style.size *= scale;
        style.line_height *= scale;
        style.tracking *= scale;
    }
}

impl Settings {
    pub fn notifications_for(&self, tool: crate::workspace::Tool) -> crate::notifications::Agent {
        self.notifications.get(tool.id()).cloned().unwrap_or_default()
    }
    pub fn path(home: &Path) -> PathBuf {
        home.join("Library/Application Support/Hyper/settings.json")
    }

    /// What's saved at `path`, with defaults for anything missing or unreadable.
    pub fn load(path: &Path) -> Settings {
        let saved = read(path);
        let mut settings = Settings {
            open_maximized: saved.get(OPEN_MAXIMIZED).and_then(Value::as_bool).unwrap_or(false),
            default_worktree: saved.get("default_worktree").and_then(Value::as_bool).unwrap_or(false),
            remember_project: saved.get("remember_project").and_then(Value::as_bool).unwrap_or(true),
            last_folder: saved.get("last_folder").and_then(Value::as_str).map(PathBuf::from),
            ..Settings::default()
        };
        if let Some(tool) = saved.get("default_tool").and_then(Value::as_str).and_then(crate::workspace::Tool::from_id) {
            settings.default_tool = tool.id().into();
        }
        if let Some(links) = saved.get("open_links").and_then(Value::as_str).and_then(|id| Links::ALL.into_iter().find(|l| l.id() == id)) {
            settings.links = links;
        }
        if let Some(order) = saved.get("sidebar_order").and_then(Value::as_str).and_then(Order::from_id) {
            settings.sidebar_order = order;
        }
        if let Some(appearance) = saved.get("appearance").and_then(Value::as_object) {
            // Prefer the unified keys. Preserve the old terminal size when upgrading; use the
            // old transcript size only when no session size was saved.
            let mut appearance = appearance.clone();
            for (key, old) in [("monospace_font", "session_font"), ("content_size", "session_size")] {
                if !appearance.contains_key(key) && let Some(value) = appearance.get(old).cloned() {
                    appearance.insert(key.into(), value);
                }
            }
            if !appearance.contains_key("content_size") && let Some(value) = appearance.get("transcript_size").cloned() {
                appearance.insert("content_size".into(), value);
            }
            for (key, value) in settings.appearance.fonts_mut() {
                if let Some(font) = appearance.get(key).and_then(Value::as_str).filter(|f| crate::fonts::offered(f, key == "monospace_font")) {
                    *value = font.into();
                }
            }
            for (key, _, range, value) in settings.appearance.sizes_mut() {
                if let Some(number) = appearance.get(key).and_then(Value::as_f64).map(|n| n as f32).filter(|n| range.contains(*n)) {
                    *value = number;
                }
            }
        }
        // Only what differs from the catalog is kept, so a later default reaches anyone who
        // never changed that shortcut.
        if let Some(shortcuts) = saved.get("shortcuts").and_then(Value::as_object) {
            settings.shortcuts = shortcuts.iter()
                .filter_map(|(id, value)| value.as_str().map(|value| (id.clone(), value.to_owned())))
                .filter(|(id, value)| !crate::commands::is_default(id, value))
                .collect();
        }
        if let Some(notifications) = saved.get("notifications").and_then(Value::as_object) {
            settings.notifications = notifications.iter().filter_map(|(id, value)| value.as_object().map(|value| (id.clone(), crate::notifications::Agent::load(value)))).collect();
        }
        settings
    }

    /// Writes these settings, keeping any keys this version doesn't know.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        // Refuse to destroy a malformed or unreadable file. Defaults are safe to load, but
        // only a valid object (or a missing file) is safe to merge into.
        let mut saved = match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| v.as_object().cloned()).ok_or_else(|| std::io::Error::other("Settings file is not a JSON object"))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::new(),
            Err(e) => return Err(e),
        };
        saved.insert(OPEN_MAXIMIZED.into(), self.open_maximized.into());
        saved.insert("default_tool".into(), self.default_tool.clone().into());
        saved.insert("default_worktree".into(), self.default_worktree.into());
        saved.insert("remember_project".into(), self.remember_project.into());
        saved.insert("open_links".into(), self.links.id().into());
        saved.insert("sidebar_order".into(), self.sidebar_order.id().into());
        saved.insert("last_folder".into(), self.last_folder.as_ref().map_or(Value::Null, |p| p.to_string_lossy().into_owned().into()));
        let mut appearance = saved.get("appearance").and_then(Value::as_object).cloned().unwrap_or_default();
        for old in ["session_font", "session_size", "transcript_size"] {
            appearance.remove(old);
        }
        let mut a = self.appearance.clone();
        for (key, value) in a.fonts_mut() {
            appearance.insert(key.into(), value.clone().into());
        }
        for (key, _, _, value) in a.sizes_mut() {
            appearance.insert(key.into(), (*value).into());
        }
        saved.insert("appearance".into(), appearance.into());
        let shortcuts: Map<String, Value> = self.shortcuts.iter().map(|(id, value)| (id.clone(), Value::String(value.clone()))).collect();
        saved.insert("shortcuts".into(), shortcuts.into());
        let mut notifications = saved.get("notifications").and_then(Value::as_object).cloned().unwrap_or_default();
        for (id, agent) in &self.notifications {
            let previous = notifications.get(id).and_then(Value::as_object).cloned().unwrap_or_default();
            notifications.insert(id.clone(), agent.merge(previous));
        }
        saved.insert("notifications".into(), notifications.into());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(&Value::Object(saved)).map_err(std::io::Error::other)?;
        // A failed/partial write must leave the last saved preferences intact.
        use std::io::Write;
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temp = path.with_file_name(format!(".settings-{}-{}.tmp", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let result = (|| {
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&temp)?;
            file.write_all((json + "\n").as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&temp, path)
        })();
        if result.is_err() { let _ = std::fs::remove_file(&temp); }
        result
    }
}

fn read(path: &Path) -> Map<String, Value> {
    match std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

/// Zooms the window to fill its screen, leaving the menu bar and Dock, as the green button's
/// Option-click does. A zoom keeps the frame it started from, so zooming again goes back to it;
/// setting the frame outright would leave Zoom nothing to return to.
pub fn maximize(window: &rune::WindowHandle) {
    #[cfg(target_os = "macos")]
    with_ns_window(window, |window| {
        use objc2::msg_send;
        use objc2::runtime::{AnyObject, Bool};
        // SAFETY: `window` is a live NSWindow, on the UI thread.
        unsafe {
            let zoomed: Bool = msg_send![window, isZoomed];
            if !zoomed.as_bool() {
                let _: () = msg_send![window, zoom: std::ptr::null::<AnyObject>()];
            }
        }
    });
    #[cfg(not(target_os = "macos"))]
    let _ = window;
}

/// Runs `f` with the window's NSWindow. None if it has none yet.
#[cfg(target_os = "macos")]
pub fn with_ns_window<T>(window: &rune::WindowHandle, f: impl FnOnce(*mut objc2::runtime::AnyObject) -> T) -> Option<T> {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use rune::raw_window_handle::RawWindowHandle;
    window
        .with_raw_handle(|handle| {
            let RawWindowHandle::AppKit(handle) = handle.as_raw() else { return None };
            // SAFETY: on the UI thread, inside the callback that keeps the view alive.
            let window: *mut AnyObject = unsafe { msg_send![handle.ns_view.as_ptr().cast::<AnyObject>(), window] };
            (!window.is_null()).then(|| f(window))
        })
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hyper-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("settings.json")
    }

    #[test]
    fn where_links_open_is_saved() {
        let path = temp("links");
        assert_eq!(Settings::load(&path).links, Links::Hyper, "the browser pane by default");
        Settings { links: Links::Browser, ..Settings::default() }.save(&path).unwrap();
        assert_eq!(Settings::load(&path).links, Links::Browser);
    }

    #[test]
    fn a_saved_setting_comes_back() {
        let path = temp("round-trip");
        assert!(!Settings::load(&path).open_maximized, "nothing saved opens at the default size");
        Settings { open_maximized: true, ..Settings::default() }.save(&path).unwrap();
        assert!(Settings::load(&path).open_maximized);
        Settings { open_maximized: false, ..Settings::default() }.save(&path).unwrap();
        assert!(!Settings::load(&path).open_maximized);
    }

    #[test]
    fn saving_keeps_keys_it_doesnt_know_and_a_broken_file_reads_as_defaults() {
        let path = temp("unknown");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"later": 3}"#).unwrap();
        Settings { open_maximized: true, ..Settings::default() }.save(&path).unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["later"], 3);
        assert_eq!(saved["open_maximized"], true);
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
    }

    #[test]
    fn interface_and_content_sizes_are_independent_and_all_content_shares_metrics() {
        let a = Appearance { interface_size: 18.0, content_size: 24.0, line_spacing: 1.5, ..Appearance::default() };
        let k = a.kit(true);
        let reading = a.reading_kit(true);
        assert_eq!(k.ty.ui.size, 18.0);
        for style in [k.ty.mono, reading.ty.mono, reading.ty.body, reading.ty.ui] {
            assert_eq!(style.size, 24.0);
        }
        for style in [k.ty.mono, reading.ty.mono, reading.ty.body] {
            assert_eq!(style.line_height, 36.0);
        }
        let bigger_interface = Appearance { interface_size: 20.0, ..a.clone() };
        assert_eq!(k.ty.mono, bigger_interface.kit(true).ty.mono);
        assert_eq!(reading.ty.body, bigger_interface.reading_kit(true).ty.body);
        let bigger_content = Appearance { content_size: 30.0, ..a };
        assert_eq!(k.ty.ui, bigger_content.kit(true).ty.ui);
        assert_eq!(bigger_content.reading_kit(true).ty.body.size, 30.0);
    }

    #[test]
    fn legacy_appearance_migrates_without_changing_the_terminal_and_removes_obsolete_keys() {
        let path = temp("appearance-migration");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"appearance":{"session_font":"Menlo","session_size":16,"transcript_size":14,"future":true}}"#).unwrap();
        let settings = Settings::load(&path);
        assert_eq!(settings.appearance.content_size, 16.0);
        assert_eq!(settings.appearance.monospace_font, "Menlo");
        settings.save(&path).unwrap();
        let saved = read(&path)["appearance"].as_object().unwrap().clone();
        assert_eq!(saved["content_size"].as_f64(), Some(16.0));
        assert_eq!(saved["monospace_font"], "Menlo");
        assert_eq!(saved["future"], true);
        for old in ["session_font", "session_size", "transcript_size"] {
            assert!(!saved.contains_key(old), "{old} should not survive migration");
        }
        assert_eq!(Settings::load(&path), settings);
    }

    #[test]
    fn unified_keys_win_and_transcript_only_settings_have_a_fallback() {
        let path = temp("appearance-precedence");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"appearance":{"content_size":20,"session_size":16,"monospace_font":"Menlo","session_font":"Monaco"}}"#).unwrap();
        let a = Settings::load(&path).appearance;
        assert_eq!(a.content_size, 20.0);
        assert_eq!(a.monospace_font, "Menlo");
        std::fs::write(&path, r#"{"appearance":{"transcript_size":18}}"#).unwrap();
        assert_eq!(Settings::load(&path).appearance.content_size, 18.0);
        for invalid in ["7", "37", "null", "\"large\""] {
            std::fs::write(&path, format!(r#"{{"appearance":{{"content_size":{invalid}}}}}"#)).unwrap();
            assert_eq!(Settings::load(&path).appearance.content_size, Appearance::default().content_size);
        }
    }
}
