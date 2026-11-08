//! The app driven as a person would: through its window, keys and the files agents write.

use super::*;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use rune::{Cx, El, Env, Key, Mods, Runtime, TextEdit, col};
use serde_json::{Value, json};

use crate::config::Owner;
use crate::format::now;
use crate::index::QUIET;
use crate::live::{Activity, Changes};
use crate::reader::content_markdown;
use crate::window::view;

fn line(v: Value) -> String {
    format!("{v}\n")
}

fn user(uuid: &str, text: &str) -> String {
    line(json!({"type": "user", "uuid": uuid, "cwd": "/src/demo", "gitBranch": "main", "timestamp": "2026-01-02T03:04:05Z", "message": {"content": text}}))
}

fn reply(uuid: &str, text: &str) -> String {
    line(json!({"type": "assistant", "uuid": uuid, "message": {"model": "claude-x", "content": [{"type": "text", "text": text}]}}))
}

fn fixture(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("hyper-ui-{name}-{}", std::process::id()));
    let project = root.join("claude/-src-demo");
    std::fs::create_dir_all(&project).unwrap();
    let tool = line(json!({"type": "assistant", "uuid": "tool", "message": {"content": [
        {"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls", "description": "List files"}}
    ]}}));
    let result = line(json!({"type": "user", "uuid": "res", "message": {"content": [
        {"type": "tool_result", "tool_use_id": "t1", "content": "main.rs"}
    ]}}));
    let mut long = user("a0", "Fix the login page") + &tool + &result;
    for i in 0..200 {
        long += &reply(&format!("r{i}"), &format!("Reply number {i} with enough words to fill a line."));
    }
    long += &reply("target", "The zebrafish test passes now.");
    for i in 200..400 {
        long += &reply(&format!("r{i}"), &format!("Reply number {i} with enough words to fill a line."));
    }
    std::fs::write(project.join("aaa.jsonl"), long).unwrap();
    std::fs::write(project.join("bbb.jsonl"), user("b0", "Another session") + &reply("b1", "Done.")).unwrap();

    let day = root.join("codex/sessions/2026/01/02");
    std::fs::create_dir_all(&day).unwrap();
    let meta = line(json!({"type": "session_meta", "payload": {"id": "ccc", "cwd": "/src/other", "source": "cli"}}));
    let prompt = line(json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "A codex prompt"}]}}));
    std::fs::write(day.join("rollout-ccc.jsonl"), meta + &prompt).unwrap();

    let folder = root.join("pi/--src-demo--");
    std::fs::create_dir_all(&folder).unwrap();
    let header = line(json!({"type": "session", "id": "ppp", "cwd": "/src/demo"}));
    let message = line(json!({"type": "message", "id": "m", "parentId": null, "message": {"role": "user", "content": [{"type": "text", "text": "A pi prompt"}]}}));
    std::fs::write(folder.join("ppp.jsonl"), header + &message).unwrap();

    let claude = root.join("home/.claude");
    std::fs::create_dir_all(claude.join("skills/draw")).unwrap();
    std::fs::write(claude.join("CLAUDE.md"), "Use zebrafish names.").unwrap();
    std::fs::write(claude.join("skills/draw/SKILL.md"), "---\nname: draw\ndescription: Draws.\n---\nBody").unwrap();
    let pi = root.join("home/.pi/agent");
    std::fs::create_dir_all(&pi).unwrap();
    std::fs::write(pi.join("mcp.json"), "{\"mcpServers\":{\"docs\":{\"url\":\"https://a.dev\"}}}").unwrap();
    root
}

fn runtime(root: PathBuf) -> Runtime<App> {
    let env = Env { system_dark: true, scale: 2.0 };
    let roots = Roots::under(&root);
    let mut rt = Runtime::new(App::at(roots, root.join("home")), view, (1240.0, 800.0), env);
    rt.poll = Some(poll);
    rt.frame(0.0);
    rt
}

/// Runs frames until `done`: the workers run in real time, whatever the virtual clock says.
fn until(rt: &mut Runtime<App>, what: &str, done: impl Fn(&Runtime<App>) -> bool) {
    let started = Instant::now();
    while !done(rt) {
        assert!(started.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        rt.frame(1.0 / 60.0);
        std::thread::sleep(Duration::from_millis(2));
    }
    for _ in 0..90 {
        rt.frame(1.0 / 60.0);
    }
}

fn click(rt: &mut Runtime<App>, key: &str) {
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("no element {key}"));
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(1.0 / 60.0);
}

#[test]
fn sidebar_toggles_stay_fixed_while_panes_animate() {
    for (width, inset) in [(1240.0, Chrome::DEFAULT.titlebar_inset()), (720.0, 0.0)] {
        let mut app = workspace_app(vec![]);
        app.view = View::Browse;
        app.index_job.cancel();
        app.sidebar.set_open(false);
        let mut rt = Runtime::new(app, view, (width, 800.0), Env::new(true, 2.0));
        rt.set_titlebar_inset(inset);
        rt.frame(0.0);
        let left = rt.find("sidebar-toggle").unwrap();
        let right = rt.find("inspector-toggle").unwrap();
        let back = rt.find("history-back").unwrap();
        assert!(back.0 > left.0 + 28.0, "Back comes after the sidebar's toggle");
        for (key, sidebar, inspector) in [
            ("sidebar-toggle", true, false), ("inspector-toggle", true, true),
            ("sidebar-toggle", false, true), ("inspector-toggle", false, false),
        ] {
            click(&mut rt, key);
            for _ in 0..120 {
                rt.frame(1.0 / 60.0);
                for (key, expected) in [("sidebar-toggle", left), ("inspector-toggle", right), ("history-back", back)] {
                    let actual = rt.find(key).unwrap();
                    assert!((actual.0 - expected.0).abs() < 0.1 && (actual.1 - expected.1).abs() < 0.1,
                        "{key} moved from {expected:?} to {actual:?}");
                }
            }
            assert_eq!(rt.state.sidebar.is_open(), sidebar);
            assert_eq!(rt.state.inspector.is_open(), inspector);
        }
        assert!(!rt.state.sidebar.is_open() && !rt.state.inspector.is_open(), "both toggled open and closed");
    }
}

#[test]
fn workspace_details_follow_agent_sessions_and_never_open_for_shells() {
    let root = fixture("workspace-details");
    let mut rt = runtime(root.clone());
    until(&mut rt, "the index", loaded);
    rt.update(|s, _| s.set_view(View::Workspace));
    rt.frame(0.0);
    assert!(rt.find("inspector-toggle").is_none(), "The empty launcher has no session details");
    rt.update(|s, _| s.launch_for_test(workspace::Tool::Shell, root.to_str().unwrap(), vec!["/bin/sleep".into(), "120".into()]));
    rt.frame(0.0);
    let shell = rt.state.current().unwrap().id;
    assert!(rt.find("inspector-toggle").is_none());
    for (source, session) in [(Source::Claude, "aaa"), (Source::Codex, "ccc"), (Source::Pi, "ppp")] {
        rt.update(|s, _| s.launch_for_test(workspace::Tool::Agent(source), root.to_str().unwrap(), vec!["/bin/sleep".into(), "120".into()]));
        rt.frame(0.0);
        assert!(rt.find("inspector-toggle").is_none(), "Details wait until the agent has a session");
        let agent = rt.state.current().unwrap().id;
        rt.update(|s, _| {
            s.ws.live.last_mut().unwrap().session = Some(session.to_owned());
            s.show(workspace::Main::Terminal(agent));
        });
        rt.frame(0.0);
        assert!(rt.find("inspector-toggle").is_some(), "{source:?} has session details");
        if !rt.state.inspector.is_open() {
            click(&mut rt, "inspector-toggle");
        }
        assert!(rt.find("details").is_some());
        rt.update(|s, _| s.show(workspace::Main::Terminal(shell)));
        rt.frame(0.0);
        assert!(rt.find("inspector-toggle").is_none());
        assert!(rt.find("inspector-pane").is_none(), "Switching to a shell hides an already-open details pane");
        rt.update(|s, tx| s.dispatch(commands::Action::ToggleDetails, tx));
        rt.frame(0.0);
        assert!(rt.state.inspector.is_open(), "The shell shortcut preserves the agent's pane preference");
        assert!(rt.find("inspector-pane").is_none());
        rt.update(|s, _| s.show(workspace::Main::Terminal(agent)));
        rt.frame(0.0);
        assert!(rt.find("details").is_some(), "Returning to the agent restores its details");
        rt.update(|s, _| s.show(workspace::Main::Terminal(shell)));
        rt.frame(0.0);
    }
    click(&mut rt, "files-toggle");
    assert!(rt.find("inspector-pane").is_some(), "Shells still have the Files pane");
    click(&mut rt, "changes-toggle");
    assert!(rt.find("inspector-pane").is_some(), "Shells still have the Changes pane");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn tooltips_name_the_shortcut_that_runs_the_button_now() {
    let root = fixture("tooltip-shortcuts");
    let _ = std::fs::remove_dir_all(root.join("home/Library"));
    let mut rt = runtime(root.clone());
    let help = |rt: &Runtime<App>, key: &str| rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).and_then(|e| e.help);
    assert_eq!(help(&rt, "sidebar-toggle").as_deref(), Some("Hide sidebar  ⌘B"));
    assert_eq!(help(&rt, "files-toggle").as_deref(), Some("Files  ⇧⌘E"));
    rt.update(|s, _| {
        commands::bind(&mut s.settings, commands::Action::ToggleFiles, Some(&"cmd+alt+f".parse().unwrap()));
        commands::bind(&mut s.settings, commands::Action::ToggleSidebar, None);
    });
    rt.frame(0.0);
    assert_eq!(help(&rt, "files-toggle").as_deref(), Some("Files  ⌥⌘F"), "a rebound shortcut");
    assert_eq!(help(&rt, "sidebar-toggle").as_deref(), Some("Hide sidebar"), "a cleared one");
    std::fs::remove_dir_all(root).unwrap();
}

/// Sessions showing in the sidebar, headings aside.
fn sessions_listed(rt: &Runtime<App>) -> usize {
    rt.state.listed.iter().filter(|i| matches!(i, Item::Session(_))).count()
}

fn projects(rt: &Runtime<App>) -> Vec<(String, usize)> {
    let heading = |item: &Item| match item {
        Item::Project { name, count, .. } => Some((name.clone(), *count)),
        _ => None,
    };
    rt.state.listed.iter().filter_map(heading).collect()
}

fn tools(rt: &Runtime<App>) -> Vec<Owner> {
    let heading = |item: &Item| match item {
        Item::Tool(source) => Some(*source),
        _ => None,
    };
    rt.state.listed.iter().filter_map(heading).collect()
}

fn loaded(rt: &Runtime<App>) -> bool {
    matches!(rt.state.reader.transcript, Load::Ready(_))
}

fn type_text(rt: &mut Runtime<App>, text: &str) {
    click(rt, "search/value/input");
    for c in text.chars() {
        rt.key(KeyEvent { key: Key::Text(c.to_string()), mods: Mods::default() });
        rt.frame(1.0 / 60.0);
    }
}

#[test]
fn settings_opens_from_the_sidebar_and_open_maximized_is_saved() {
    let root = fixture("settings");
    let _ = std::fs::remove_dir_all(root.join("home/Library"));
    let mut rt = runtime(root.clone());
    assert!(rt.find("open-maximized").is_none());
    click(&mut rt, "settings-row");
    assert_eq!(rt.state.view, View::Settings);
    // The pane slides in.
    for _ in 0..90 {
        rt.frame(1.0 / 60.0);
    }
    assert!(rt.state.listed.is_empty(), "the sidebar lists nothing under Settings");
    click(&mut rt, "open-maximized");
    let saved = settings::Settings::path(&root.join("home"));
    assert!(settings::Settings::load(&saved).open_maximized, "checking it saves it");
    let reopened = App::at(rt.state.roots.clone(), root.join("home"));
    assert!(reopened.settings.open_maximized, "the next launch sees it");
    click(&mut rt, "open-maximized");
    assert!(!settings::Settings::load(&saved).open_maximized, "unchecking it saves that too");
    click(&mut rt, "view-filter/segment1");
    assert_eq!(rt.state.view, View::Browse);
    assert!(rt.find("open-maximized").is_none());
}

#[test]
fn a_setting_that_cant_be_saved_stays_as_it_was_and_says_why() {
    let root = fixture("settings-unsaved");
    let home = root.join("home");
    let _ = std::fs::remove_dir_all(home.join("Library"));
    // A file where the settings folder belongs, so it can't be made.
    std::fs::create_dir_all(home.join("Library/Application Support")).unwrap();
    std::fs::write(home.join("Library/Application Support/Hyper"), "").unwrap();
    let mut rt = runtime(root.clone());
    click(&mut rt, "settings-row");
    for _ in 0..90 {
        rt.frame(1.0 / 60.0);
    }
    click(&mut rt, "open-maximized");
    assert!(!rt.state.settings.open_maximized, "the switch doesn't claim what wasn't saved");
    assert!(rt.find("settings-error").is_some(), "and the page says it couldn't save");
}

/// Opens Settings on a tab, in a window short enough that the shortcut list scrolls.
fn settings_tab(name: &str, tab: &str) -> (PathBuf, Runtime<App>) {
    let root = fixture(name);
    let _ = std::fs::remove_dir_all(root.join("home/Library"));
    let roots = Roots::under(&root);
    let mut rt = Runtime::new(App::at(roots, root.join("home")), view, (1240.0, 520.0), Env { system_dark: true, scale: 2.0 });
    rt.poll = Some(poll);
    rt.on_key = Some(on_key);
    rt.frame(0.0);
    click(&mut rt, "settings-row");
    for _ in 0..90 { rt.frame(1.0 / 60.0); }
    click(&mut rt, &format!("settings-tabs/{tab}"));
    (root, rt)
}

fn chord(rt: &mut Runtime<App>, key: rune::PhysicalKeyCode, mods: Mods) {
    rt.physical_input(rune::PhysicalInput::Modifiers(mods));
    rt.physical_input(rune::PhysicalInput::Key { key, mods, pressed: true, repeat: false });
    rt.physical_input(rune::PhysicalInput::Key { key, mods, pressed: false, repeat: false });
    rt.physical_input(rune::PhysicalInput::Modifiers(Mods::default()));
    for _ in 0..10 { rt.frame(1.0 / 60.0); }
}

#[test]
fn recording_a_taken_shortcut_says_who_has_it_and_a_free_one_saves_at_once() {
    let (root, mut rt) = settings_tab("settings-record", "segment2");
    let cmd = Mods { cmd: true, ..Default::default() };
    // The last row, scrolled to, so its error shows where the list has scrolled.
    let (x, y) = rt.find("record-new_session").unwrap();
    rt.wheel(x, y, -2000.0);
    for _ in 0..30 { rt.frame(1.0 / 60.0); }
    click(&mut rt, "record-last_session");
    assert!(rt.state.preferences.recording());
    chord(&mut rt, rune::PhysicalKeyCode::KeyN, cmd);
    let error = rt.elements().into_iter().find(|e| e.text.as_deref().is_some_and(|t| t.contains("⌘N is used by New Session.")))
        .expect("the refusal names the other action");
    assert!(error.rect[1] >= 0.0 && error.rect[1] + error.rect[3] <= 520.0, "and shows by the row, on screen: {:?}", error.rect);
    assert_eq!(rt.state.view, View::Settings, "⌘N was recorded, not run");
    let saved = settings::Settings::path(&root.join("home"));
    chord(&mut rt, rune::PhysicalKeyCode::KeyJ, Mods { alt: true, ..cmd });
    assert!(!rt.state.preferences.recording());
    assert_eq!(settings::Settings::load(&saved).shortcuts.get("last_session").map(String::as_str), Some("alt+super+j"), "saved without an Apply");
    let file: serde_json::Value = serde_json::from_slice(&std::fs::read(&saved).unwrap()).unwrap();
    assert_eq!(file["shortcuts"].as_object().unwrap().len(), 1, "only the change is written: {}", file["shortcuts"]);
    click(&mut rt, "reset-shortcuts");
    assert!(settings::Settings::load(&saved).shortcuts.is_empty());
    assert_eq!(commands::Action::LastSession.hint(&rt.state.settings), "⌘9");
}

#[test]
fn a_shortcut_cleared_with_its_button_stays_off_until_the_defaults_come_back() {
    let (root, mut rt) = settings_tab("settings-clear", "segment2");
    assert_eq!(commands::Action::LastSession.hint(&rt.state.settings), "⌘9");
    let (x, y) = rt.find("record-new_session").unwrap();
    rt.wheel(x, y, -2000.0);
    for _ in 0..30 { rt.frame(1.0 / 60.0); }
    click(&mut rt, "clear-last_session");
    for _ in 0..10 { rt.frame(1.0 / 60.0); }
    assert!(!rt.state.preferences.recording(), "clearing doesn't start a recording");
    let saved = settings::Settings::path(&root.join("home"));
    assert_eq!(settings::Settings::load(&saved).shortcuts.get("last_session").map(String::as_str), Some(""), "saved as off");
    assert_eq!(commands::Action::LastSession.hint(&rt.state.settings), "", "and gone from the menu");
    assert!(rt.find("clear-last_session").is_none(), "nothing left to clear");
    assert!(rt.find("clear-new_session").is_some(), "other shortcuts keep theirs");
    click(&mut rt, "reset-shortcuts");
    for _ in 0..10 { rt.frame(1.0 / 60.0); }
    assert_eq!(commands::Action::LastSession.hint(&rt.state.settings), "⌘9");
    assert!(rt.find("clear-last_session").is_some());
}

#[test]
fn the_tabs_and_a_save_error_stay_in_view_while_a_section_scrolls() {
    let (_, mut rt) = settings_tab("settings-scrolls", "segment2");
    rt.update(|s, _| s.settings_error = Some("Couldn't save settings: test".into()));
    rt.frame(0.0);
    let (x, y) = rt.find("record-new_session").unwrap();
    rt.wheel(x, y, -2000.0);
    for _ in 0..30 { rt.frame(1.0 / 60.0); }
    assert!(rt.find("reset-shortcuts").is_some_and(|(_, y)| y < 520.0), "the list scrolled to its end");
    for key in ["settings-error", "settings-tabs"] {
        let ([_, top, _, h], _) = rt.presented(key).unwrap();
        assert!(top >= 0.0 && top + h <= 520.0, "{key} is on screen after scrolling, at {top}");
        let (cx, cy) = rt.find(key).unwrap();
        let picked = rt.element_at(cx, cy).map(|p| p.path().to_string()).unwrap_or_default();
        assert!(picked.contains(key), "{key} isn't covered or clipped: {picked}");
    }
}

#[test]
fn a_font_chosen_from_its_menu_is_saved_and_used() {
    let (root, mut rt) = settings_tab("settings-font", "segment1");
    click(&mut rt, "monospace-font-trigger");
    let menu = rt.popup_menu().expect("a native menu opens");
    assert!(menu.find("ui-monospace").is_some_and(rune::PopupItem::is_checked), "the current font is checked");
    assert!(menu.find("Helvetica").is_none(), "only curated monospace families are offered");
    assert!(menu.find("Menlo").is_some());
    rt.resolve_popup(Some("Menlo")).unwrap();
    for _ in 0..5 { rt.frame(1.0 / 60.0); }
    assert_eq!(rt.state.settings.appearance.monospace_font, "Menlo");
    assert_eq!(settings::Settings::load(&settings::Settings::path(&root.join("home"))).appearance.monospace_font, "Menlo");
    assert_eq!(rt.font_families().1, "Menlo", "the terminal font changes at once");
    click(&mut rt, "monospace-font-trigger");
    assert!(rt.popup_menu().unwrap().find("Menlo").is_some_and(rune::PopupItem::is_checked));
    rt.resolve_popup(None).unwrap();
    assert_eq!(rt.state.settings.appearance.monospace_font, "Menlo", "dismissing keeps the current font");

    click(&mut rt, "interface-font-trigger");
    let menu = rt.popup_menu().expect("the interface font also uses a native menu");
    assert!(menu.find("Inter").is_some_and(rune::PopupItem::is_checked));
    assert!(menu.find("System Font").is_some() && menu.find("Helvetica").is_none());
    rt.resolve_popup(Some("System Font")).unwrap();
    for _ in 0..5 { rt.frame(1.0 / 60.0); }
    assert_eq!(rt.font_families(), ("System Font", "Menlo"), "both choices apply without restarting");
    assert_eq!(settings::Settings::load(&settings::Settings::path(&root.join("home"))).appearance.interface_font, "System Font");
}

#[test]
fn a_size_picked_from_its_menu_is_saved_and_used() {
    let (root, mut rt) = settings_tab("settings-size", "segment1");
    assert!(rt.find("transcript-size-trigger").is_none(), "there are only two sizes");
    click(&mut rt, "content-size-trigger");
    let menu = rt.popup_menu().expect("a native menu opens").clone();
    assert!(menu.find("13").is_some_and(rune::PopupItem::is_checked), "the current size is checked");
    assert!(menu.find("37").is_none() && menu.find("8").is_some(), "the menu is the size's range");
    rt.resolve_popup(Some("18")).unwrap();
    for _ in 0..5 { rt.frame(1.0 / 60.0); }
    assert_eq!(rt.state.settings.appearance.content_size, 18.0);
    assert_eq!(settings::Settings::load(&settings::Settings::path(&root.join("home"))).appearance.content_size, 18.0);
    assert_eq!(rt.state.settings.appearance.kit(true).ty.mono.size, 18.0);
}

#[test]
fn changing_content_metrics_rebuilds_an_already_visible_transcript() {
    let mut rt = runtime(fixture("transcript-content-zoom"));
    until(&mut rt, "the index", loaded);
    rt.update(|s, tx| { s.set_view(View::Browse); tx.instant(); });
    rt.frame(0.0);
    click(&mut rt, "bbb");
    until(&mut rt, "the transcript", |rt| rt.state.reader.selected.as_deref() == Some("bbb") && loaded(rt));
    for (size, spacing) in [(22.0, 1.5), (22.0, 2.0)] {
        rt.update(move |s, tx| {
            let mut changed = s.settings.clone();
            changed.appearance.content_size = size;
            changed.appearance.line_spacing = spacing;
            s.save_settings(changed, tx);
        });
        for _ in 0..10 { rt.frame(1.0 / 60.0); }
        let elements = rt.elements();
        let reply = elements.iter().find(|e| e.text.as_deref() == Some("Done.")).expect("the reply is visible");
        assert_eq!(reply.rect[3], size * spacing, "memoised transcript rows must pick up new content metrics");
    }
}

#[test]
fn transcript_prose_and_code_share_content_size_and_line_spacing() {
    fn reading(a: &settings::Appearance, _: &mut Cx<settings::Appearance>) -> El {
        let k = a.reading_kit(true);
        col().w_full().child(content_markdown("Readable prose.\n\n```rust\nlet answer = 42;\n```", &k).build(&k))
    }
    let mut rt = Runtime::new(settings::Appearance::default(), reading, (900.0, 600.0), Env::new(true, 2.0));
    rt.frame(0.0);
    for (size, spacing) in [(22.0, 1.5), (22.0, 2.0), (16.0, 1.25)] {
        rt.update(move |a, tx| { a.content_size = size; a.line_spacing = spacing; tx.instant(); });
        for _ in 0..5 { rt.frame(1.0 / 60.0); }
        let elements = rt.elements();
        for value in ["Readable prose.", "let answer = 42;"] {
            let text = elements.iter().find(|e| e.text.as_deref() == Some(value)).unwrap_or_else(|| panic!("missing {value}"));
            assert_eq!(text.rect[3], size * spacing, "{value} shares content metrics");
        }
    }
}

#[test]
fn the_details_shortcut_does_nothing_in_settings() {
    let mut rt = runtime(fixture("settings-details"));
    let details = |rt: &mut Runtime<App>| {
        let ev = KeyEvent { key: Key::Command('i'), mods: Mods { cmd: true, alt: true, ..Mods::default() } };
        rt.update(move |s: &mut App, tx: &mut Tx| {
            on_key(s, &ev, tx);
        });
    };
    let open = rt.state.inspector.is_open();
    details(&mut rt);
    assert_ne!(rt.state.inspector.is_open(), open, "in Browse it toggles them");
    details(&mut rt);
    click(&mut rt, "settings-row");
    details(&mut rt);
    click(&mut rt, "view-filter/segment1");
    assert_eq!(rt.state.inspector.is_open(), open, "details come back as they were");
}

#[test]
fn a_row_shows_its_tools_logo_until_a_filter_makes_it_redundant() {
    let root = fixture("logos");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    assert!(rt.find("aaa/avatar/logo/sb-claude").is_some(), "under All, a Claude row leads with Claude's logo");
    assert!(rt.find("aaa/avatar/logo/sb-codex").is_none(), "and only that");
    click(&mut rt, "source-filter/segment1");
    assert_eq!(rt.state.source, Some(Source::Claude));
    assert!(rt.find("aaa").is_some(), "the row is still there");
    assert!(rt.find("aaa/avatar/logo/sb-claude").is_none(), "under Claude every row would repeat it");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn clicking_a_session_opens_it() {
    let root = fixture("open");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    click(&mut rt, "bbb");
    until(&mut rt, "second transcript", |rt| rt.state.reader.selected.as_deref() == Some("bbb") && loaded(rt));
    let Load::Ready(t) = &rt.state.reader.transcript else { panic!() };
    assert_eq!(t.rows.len(), 2);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn typing_finds_a_reply_and_opening_the_hit_scrolls_to_it() {
    let root = fixture("search");
    let mut rt = runtime(root.clone());
    until(&mut rt, "index", loaded);
    click(&mut rt, "bbb");
    until(&mut rt, "other session", |rt| rt.state.reader.selected.as_deref() == Some("bbb") && loaded(rt));
    type_text(&mut rt, "zebrafish");
    assert_eq!(rt.state.search.field.text, "zebrafish");
    until(&mut rt, "hits", |rt| rt.state.search.hits.contains_key("aaa"));
    assert_eq!(sessions_listed(&rt), 1, "only the session that mentions it stays listed");
    click(&mut rt, "aaa");
    until(&mut rt, "hit's transcript", |rt| rt.state.reader.selected.as_deref() == Some("aaa") && loaded(rt) && rt.state.reader.reveal.is_none());
    let (rect, _) = rt.presented("target:0").expect("the matching row is mounted");
    assert!(rect[1] > 0.0 && rect[1] + rect[3] < 800.0, "and on screen: {rect:?}");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn escape_clears_the_search_and_restores_the_list() {
    let root = fixture("escape");
    let mut rt = runtime(root.clone());
    until(&mut rt, "index", loaded);
    type_text(&mut rt, "zebrafish");
    until(&mut rt, "hits", |rt| sessions_listed(rt) == 1);
    rt.key(KeyEvent { key: Key::Escape, mods: Mods::default() });
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.search.field.text, "");
    assert_eq!(sessions_listed(&rt), 4);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn sessions_group_by_project_across_tools_and_the_filter_narrows_them() {
    let root = fixture("group");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    let mut all = projects(&rt);
    all.sort();
    assert_eq!(all, [("demo".to_owned(), 3), ("other".to_owned(), 1)], "Claude and Pi share demo");

    click(&mut rt, "source-filter/segment2");
    assert_eq!(rt.state.source, Some(Source::Codex));
    assert_eq!(projects(&rt), [("other".to_owned(), 1)]);
    assert_eq!(sessions_listed(&rt), 1, "and its project is open");

    click(&mut rt, "source-filter/segment3");
    assert_eq!(rt.state.source, Some(Source::Pi));
    assert_eq!(projects(&rt), [("demo".to_owned(), 1)]);

    click(&mut rt, "source-filter/segment0");
    assert_eq!(rt.state.source, None);
    assert_eq!(sessions_listed(&rt), 4);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_project_collapses_and_a_search_opens_it_without_forgetting() {
    let root = fixture("collapse");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    click(&mut rt, "project-demo");
    assert_eq!(sessions_listed(&rt), 1, "demo's three sessions fold away");
    type_text(&mut rt, "zebrafish");
    until(&mut rt, "hits", |rt| rt.state.search.hits.contains_key("aaa"));
    assert_eq!(sessions_listed(&rt), 1, "the hit shows inside the closed project");
    assert!(rt.state.listed.iter().any(|i| matches!(i, Item::Session(_))));
    rt.key(KeyEvent { key: Key::Escape, mods: Mods::default() });
    rt.frame(1.0 / 60.0);
    assert!(!rt.state.open_projects.contains("demo"), "and it's closed again after");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn config_is_read_again_each_time_it_comes_into_view() {
    let root = fixture("config-reread");
    let mut rt = runtime(root.clone());
    until(&mut rt, "config", |rt| !rt.state.config.items.is_empty());
    rt.update(|s: &mut App, _: &mut Tx| s.set_view(View::Browse));
    click(&mut rt, "mode-filter/segment1");
    let has = |rt: &Runtime<App>, name: &str| rt.state.config.items.iter().any(|i| i.title == name);
    assert!(!has(&rt, "paint"));

    let skills = root.join("home/.claude/skills");
    std::fs::create_dir_all(skills.join("paint")).unwrap();
    std::fs::write(skills.join("paint/SKILL.md"), "---\nname: paint\ndescription: Paints.\n---\nBody").unwrap();
    click(&mut rt, "mode-filter/segment0");
    click(&mut rt, "mode-filter/segment1");
    until(&mut rt, "the new skill, back from Sessions", |rt| has(rt, "paint"));

    let paint = rt.state.config.items.iter().find(|i| i.title == "paint").unwrap().id.clone();
    rt.update(|s: &mut App, _: &mut Tx| s.config.selected = Some(paint.clone()));
    std::fs::remove_dir_all(skills.join("paint")).unwrap();
    click(&mut rt, "view-filter/segment0");
    click(&mut rt, "view-filter/segment1");
    assert_eq!(rt.state.mode, Mode::Config);
    until(&mut rt, "the skill gone, back from Workspace", |rt| !has(rt, "paint"));
    let selected = rt.state.config.selected.as_deref().and_then(|id| rt.state.config.item(id));
    assert!(selected.is_some(), "a removed item's selection moves to one that's there");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_config_view_lists_each_tool_filters_and_opens_an_item() {
    let root = fixture("config");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    until(&mut rt, "config", |rt| !rt.state.config.items.is_empty());
    click(&mut rt, "mode-filter/segment1");
    assert_eq!(rt.state.mode, Mode::Config);
    assert_eq!(rt.state.config.selected.as_deref(), Some("Hyper/Settings/General"));
    assert_eq!(tools(&rt), [Owner::Hyper, Owner::Tool(Source::Claude), Owner::Tool(Source::Pi)], "All heads Hyper and each tool that has config, once");
    let names: Vec<String> = projects(&rt).into_iter().map(|(name, _)| name).collect();
    assert!(names.contains(&"Skills".to_owned()) && names.contains(&"MCP servers".to_owned()), "{names:?}");

    click(&mut rt, "source-filter/segment3");
    assert_eq!(projects(&rt), [("MCP servers".to_owned(), 1)]);
    assert!(tools(&rt).is_empty(), "one tool needs no heading");
    // The row slides up from where it sat under the longer list.
    for _ in 0..30 { rt.frame(1.0 / 60.0); }
    click(&mut rt, "Pi/MCP servers/docs");
    assert_eq!(rt.state.config.selected.as_deref(), Some("Pi/MCP servers/docs"));
    rt.frame(1.0 / 60.0);
    assert!(rt.find("config-path").is_some());
    assert!(rt.find("tools-toggle").is_none(), "tool calls are a sessions thing");

    click(&mut rt, "mode-filter/segment0");
    assert_eq!(rt.state.mode, Mode::Sessions);
    assert_eq!(projects(&rt), [("demo".to_owned(), 1)], "the filter carries over");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn hyper_settings_show_what_is_in_effect_and_open_their_section_in_settings() {
    let root = fixture("config-hyper");
    let mut rt = runtime(root.clone());
    until(&mut rt, "config", |rt| !rt.state.config.items.is_empty());
    rt.update(|s: &mut App, _: &mut Tx| s.set_view(View::Browse));
    click(&mut rt, "mode-filter/segment1");
    let markdown = |rt: &Runtime<App>, id: &str| rt.state.config.item(id).map(|i| i.markdown.clone()).unwrap_or_default();
    assert!(markdown(&rt, "Hyper/Settings/General").contains("| Default tool | Claude |"), "{}", markdown(&rt, "Hyper/Settings/General"));
    assert!(markdown(&rt, "Hyper/Settings/Shortcuts").contains("| New Session | ⌘N |"), "{}", markdown(&rt, "Hyper/Settings/Shortcuts"));

    rt.update(|s: &mut App, _: &mut Tx| {
        let mut changed = s.settings.clone();
        changed.default_tool = "codex".into();
        s.persist_settings(changed);
    });
    click(&mut rt, "mode-filter/segment0");
    click(&mut rt, "mode-filter/segment1");
    until(&mut rt, "the new default", |rt| markdown(rt, "Hyper/Settings/General").contains("| Default tool | Codex |"));
    assert!(rt.state.config.item("Hyper/Settings/settings.json").is_some_and(|i| i.markdown.contains("\"codex\"")), "the saved file is listed too");

    click(&mut rt, "source-filter/segment1");
    assert!(!tools(&rt).contains(&Owner::Hyper) && rt.state.listed.iter().all(|i| !matches!(i, Item::Config(n) if rt.state.config.items[*n].source == Owner::Hyper)), "a tool's filter leaves Hyper out");
    click(&mut rt, "source-filter/segment0");

    rt.update(|s: &mut App, _: &mut Tx| s.config.selected = Some("Hyper/Settings/Shortcuts".into()));
    if !rt.state.inspector.is_open() {
        click(&mut rt, "inspector-toggle");
    }
    for _ in 0..30 { rt.frame(1.0 / 60.0); }
    click(&mut rt, "open-settings");
    assert_eq!(rt.state.view, View::Settings);
    assert_eq!(rt.state.preferences.tab, 2);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_config_group_closes_for_its_own_tool_and_stays_closed_under_a_filter() {
    let root = fixture("config-collapse");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    until(&mut rt, "config", |rt| !rt.state.config.items.is_empty());
    click(&mut rt, "mode-filter/segment1");
    let listed = |rt: &Runtime<App>, id: &str| rt.state.listed.iter().any(|i| matches!(i, Item::Config(n) if rt.state.config.items[*n].id == id));
    assert!(listed(&rt, "Pi/MCP servers/docs"));
    click(&mut rt, "config-Pi-MCP servers");
    assert!(!listed(&rt, "Pi/MCP servers/docs"), "Pi's group folds away");
    assert!(rt.state.config.items.iter().filter(|i| i.source == Source::Claude && i.group == "MCP servers").all(|i| listed(&rt, &i.id)), "Claude's is its own");
    click(&mut rt, "source-filter/segment3");
    assert!(!listed(&rt, "Pi/MCP servers/docs"), "and it's still closed under Pi");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn clicking_a_heading_during_a_search_leaves_it_as_it_was() {
    let root = fixture("toggle-searching");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    until(&mut rt, "config", |rt| !rt.state.config.items.is_empty());
    click(&mut rt, "mode-filter/segment1");
    type_text(&mut rt, "mcp");
    click(&mut rt, "config-Pi-MCP servers");
    rt.key(KeyEvent { key: Key::Escape, mods: Mods::default() });
    rt.frame(1.0 / 60.0);
    assert!(rt.state.config.closed.is_empty(), "a search shows every group open, so there was nothing to close");
    std::fs::remove_dir_all(root).unwrap();
}

fn fake(id: &str, source: Source, project: &str, updated: i64) -> Session {
    Session {
        id: id.into(),
        source,
        path: PathBuf::new(),
        project: project.into(),
        cwd: String::new(),
        title: id.into(),
        branch: String::new(),
        updated,
        size: 0,
        turn: None,
    }
}

fn workspace_app(sessions: Vec<Session>) -> App {
    let mut app = App::at(Roots::under(Path::new("/none")), "/none".into());
    app.view = View::Workspace;
    app.index = Load::Ready(Arc::new(sessions));
    app.relist();
    app
}

fn listing(app: &App) -> Vec<String> {
    app.listed
        .iter()
        .map(|item| match item {
            Item::Project { name, .. } => format!("# {name}"),
            Item::Session(n) => app.sessions()[*n].id.clone(),
            Item::Live(id) => format!("live {id}"),
            Item::Worktree(worktree) => format!("## {}", worktree.name),
            _ => "?".to_owned(),
        })
        .collect()
}

fn in_folder(mut session: Session, cwd: &str) -> Session {
    session.cwd = cwd.into();
    session
}

#[test]
fn the_sidebar_lists_only_what_is_running_and_past_sessions_stay_out_of_it() {
    let mut app = workspace_app(vec![in_folder(fake("a", Source::Claude, "alpha", 2), "/"), in_folder(fake("b", Source::Claude, "beta", 1), "/")]);
    assert!(listing(&app).is_empty());
    app.resume("b");
    assert_eq!(listing(&app), ["# beta", "live 1"]);
}

#[test]
fn the_launcher_offers_the_latest_sessions_except_whats_open() {
    let mut app = workspace_app(vec![in_folder(fake("a", Source::Claude, "alpha", 3), "/"), in_folder(fake("b", Source::Claude, "alpha", 2), "/"), in_folder(fake("c", Source::Claude, "beta", 1), "/")]);
    app.running = Some(Running { trusted: true, ..Running::default() });
    assert_eq!(app.recent_ids(), ["a", "b", "c"], "every project's, newest first");
    app.resume("a");
    assert_eq!(app.recent_ids(), ["b", "c"]);
}

#[test]
fn the_folder_picker_starts_the_tool_picked_without_any_session_history_and_cancel_starts_nothing() {
    let root = fixture("choose-folder");
    let folder = root.join("brand new folder");
    std::fs::create_dir(&folder).unwrap();
    let roots = Roots::under(&root.join("none"));
    let mut app = App::at(roots, root.join("home"));
    app.view = View::Workspace;
    app.ws.tool = 3;
    let mut rt = Runtime::new(app, view, (1240.0, 800.0), Env { system_dark: true, scale: 2.0 });
    rt.poll = Some(poll);
    until(&mut rt, "empty history", |rt| matches!(rt.state.index, Load::Ready(_)));
    assert_eq!(rt.state.choices(), [workspace::Choice::ChooseFolder]);
    click(&mut rt, "choice-0");
    assert_eq!(rt.file_dialog().unwrap().0, rune::FileDialogKind::Folder);
    rt.resolve_file_dialog(Some(folder.clone())).unwrap();
    rt.frame(0.0);
    assert_eq!(rt.state.projects(), &[("brand new folder".into(), folder.clone())]);
    assert_eq!(rt.state.current().unwrap().tool, workspace::Tool::Shell, "picking the folder started it");
    rt.update(|s: &mut App, _: &mut Tx| s.current().unwrap().term.text("pwd\n"));
    until(&mut rt, "pwd in the chosen folder", |rt| printed(rt).contains(folder.to_str().unwrap()));

    // Over the terminal, cancelling the folder picker starts nothing.
    rt.update(|s: &mut App, tx: &mut Tx| s.new_session(None, Some(tx)));
    until(&mut rt, "the launcher over the terminal", |rt| rt.find("launcher").is_some());
    let last = rt.state.choices().len() - 1;
    click(&mut rt, &format!("choice-{last}"));
    rt.resolve_file_dialog(None).unwrap();
    rt.frame(0.0);
    assert_eq!(rt.state.ws.live.len(), 1);
    drop(rt);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn resuming_a_session_that_is_already_open_goes_to_it() {
    let mut app = workspace_app(vec![in_folder(fake("a", Source::Claude, "alpha", 1), "/")]);
    app.resume("a");
    app.show(workspace::Main::Launcher);
    app.resume("a");
    assert_eq!(app.ws.live.len(), 1);
    assert_eq!(app.ws.main, workspace::Main::Terminal(1));
}

#[test]
fn a_codex_session_joins_the_terminal_that_started_it_in_that_folder() {
    let started = now();
    let mut app = workspace_app(vec![in_folder(fake("old", Source::Codex, "alpha", started - 3600), "/"), in_folder(fake("other", Source::Codex, "alpha", started), "/tmp")]);
    app.ws.tool = 1;
    app.start_from_launcher();
    assert_eq!(app.current().and_then(|l| l.session.clone()), None, "neither a stale session nor another folder's");

    let mut sessions = app.sessions().to_vec();
    sessions.insert(0, in_folder(fake("new", Source::Codex, "alpha", started), "/"));
    app.index = Load::Ready(Arc::new(sessions));
    app.relist();
    assert_eq!(app.current().and_then(|l| l.session.clone()).as_deref(), Some("new"));
}

#[test]
fn only_agents_make_quitting_ask() {
    let mut app = workspace_app(vec![in_folder(fake("a", Source::Claude, "alpha", 1), "/")]);
    app.ws.tool = workspace::Tool::ALL.iter().position(|t| *t == workspace::Tool::Shell).unwrap();
    app.start_from_launcher();
    assert_eq!(app.running_agents(), 0);
    app.ws.tool = 0;
    app.start_from_launcher();
    assert_eq!(app.running_agents(), 1);
}

#[test]
fn the_quit_question_keeps_the_keyboard_from_the_terminal() {
    let mut app = workspace_app(vec![in_folder(fake("a", Source::Claude, "alpha", 1), "/")]);
    app.start_from_launcher();
    assert!(app.typing_term().is_some());
    app.ws.confirming_quit = true;
    assert!(app.typing_term().is_none());
    app.ws.confirming_quit = false;
    app.set_view(View::Browse);
    assert!(app.typing_term().is_none(), "Browse has no terminal on screen");
}

#[test]
fn command_equals_command_plus_and_command_minus_zoom_all_content() {
    let root = fixture("zoom-keys");
    let mut app = workspace_app(vec![in_folder(fake("a", Source::Claude, "alpha", 1), "/")]);
    app.index_job.cancel();
    app.settings_path = root.join("settings.json");
    let mut rt = Runtime::new(app, view, (1240.0, 800.0), Env::new(true, 2.0));
    rt.on_key = Some(on_key);
    rt.frame(0.0);
    let start = rt.state.settings.appearance.content_size;
    let cmd = Mods { cmd: true, ..Default::default() };
    rt.key(KeyEvent { key: Key::Command('='), mods: cmd });
    rt.frame(0.0);
    assert_eq!(rt.state.settings.appearance.content_size, start + 1.0, "⌘=");
    // ⌘+ is ⌘⇧= on US layouts; macOS apps treat it as the same zoom.
    rt.key(KeyEvent { key: Key::Command('+'), mods: Mods { shift: true, ..cmd } });
    rt.frame(0.0);
    assert_eq!(rt.state.settings.appearance.content_size, start + 2.0, "⌘+");
    rt.key(KeyEvent { key: Key::Command('-'), mods: cmd });
    rt.frame(0.0);
    assert_eq!(rt.state.settings.appearance.content_size, start + 1.0, "⌘-");
    let saved = |rt: &Runtime<App>| settings::Settings::load(&rt.state.settings_path).appearance.content_size;
    assert_eq!(saved(&rt), start + 1.0, "a press saves at once");
    // Held down, it grows as it repeats and is written once it stops.
    rt.poll = Some(poll);
    for _ in 0..5 { rt.repeat_key(KeyEvent { key: Key::Command('='), mods: cmd }); rt.frame(0.0); }
    assert_eq!(rt.state.settings.appearance.content_size, start + 6.0);
    assert_eq!(saved(&rt), start + 1.0, "not written on every repeat");
    let started = Instant::now();
    while saved(&rt) != start + 6.0 {
        assert!(started.elapsed() < Duration::from_secs(3), "the size held to was never saved");
        rt.frame(1.0 / 60.0);
        std::thread::sleep(Duration::from_millis(10));
    }
    for view in [View::Browse, View::Settings, View::Workspace] {
        rt.update(move |s, tx| { s.set_view(view); tx.instant(); });
        let before = rt.state.settings.appearance.content_size;
        let interface = rt.state.settings.appearance.interface_size;
        rt.key(KeyEvent { key: Key::Command('='), mods: cmd });
        rt.frame(0.0);
        assert_eq!(rt.state.settings.appearance.content_size, before + 1.0, "shared zoom in {view:?}");
        assert_eq!(rt.state.settings.appearance.interface_size, interface);
        rt.key(KeyEvent { key: Key::Command('-'), mods: Mods { alt: true, ..cmd } });
        rt.frame(0.0);
        assert_eq!(rt.state.settings.appearance.interface_size, interface - 1.0);
        assert_eq!(rt.state.settings.appearance.content_size, before + 1.0);
        rt.key(KeyEvent { key: Key::Command('0'), mods: cmd });
        rt.frame(0.0);
        assert_eq!(rt.state.settings.appearance.content_size, settings::Appearance::default().content_size);
    }
}

#[test]
fn a_saved_palette_shortcut_still_toggles_the_sidebar() {
    let root = fixture("saved-palette-shortcut");
    let home = root.join("home");
    let path = settings::Settings::path(&home);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, r#"{"shortcuts":{"toggle_sidebar":"cmd+shift+n"}}"#).unwrap();
    let mut app = workspace_app(vec![]);
    app.settings = settings::Settings::load(&path);
    app.settings_path = path;
    app.index_job.cancel();
    app.launch_in_project_for_test("alpha", "/", vec!["/bin/sleep".into(), "30".into()]);
    let mut rt = Runtime::new(app, view, (1240.0, 1400.0), Env::new(true, 2.0));
    rt.on_key = Some(on_key);
    rt.frame(0.0);
    let open = rt.state.sidebar.is_open();

    key(&mut rt, Key::Command('n'), Mods { cmd: true, shift: true, ..Default::default() });

    assert_eq!(rt.state.sidebar.is_open(), !open, "the saved action wins over the new palette default");
    assert!(!rt.state.ws.palette, "the palette did not take the shortcut");
    assert_eq!(commands::Action::CommandPalette.binding(&rt.state.settings), None, "menus and Settings must not advertise the claimed shortcut");
    assert!(commands::check(&rt.state.settings, commands::Action::ToggleSidebar, &"cmd+shift+n".parse().unwrap()).is_ok());

    key(&mut rt, Key::Command(','), Mods { cmd: true, ..Default::default() });
    for _ in 0..90 { rt.frame(1.0 / 60.0); }
    assert_eq!(rt.state.view, View::Settings);
    click(&mut rt, "settings-tabs/segment2");
    for _ in 0..90 { rt.frame(1.0 / 60.0); }
    assert!(rt.find("record-command_palette").is_some());
    assert!(rt.find("clear-command_palette").is_none(), "the claimed palette shortcut is shown as unbound");
    click(&mut rt, "record-toggle_sidebar");
    assert!(rt.state.preferences.recording(), "the sidebar shortcut recorder is active");
    chord(&mut rt, rune::PhysicalKeyCode::KeyB, Mods { cmd: true, ..Default::default() });
    assert!(!rt.state.settings.shortcuts.contains_key("toggle_sidebar"), "the sidebar's default was saved: {:?}", rt.state.settings.shortcuts);
    assert_eq!(commands::Action::CommandPalette.hint(&rt.state.settings), "⇧⌘N");
    assert!(rt.find("clear-command_palette").is_some(), "the recorder updates when the palette's default becomes available again");
    key(&mut rt, Key::Command('n'), Mods { cmd: true, shift: true, ..Default::default() });
    assert!(rt.state.ws.palette, "the palette shortcut works again after the saved binding moves away");
    drop(rt);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn command_n_starts_the_current_tool_in_the_same_worktree_without_return() {
    let root = fixture("new-session-here");
    let folder = root.join("project/.claude/worktrees/topic");
    std::fs::create_dir_all(&folder).unwrap();
    let mut app = workspace_app(vec![]);
    app.index_job.cancel();
    app.settings_path = root.join("settings.json");
    app.settings.default_tool = "codex".into();
    app.settings.default_worktree = true;
    app.launch_in_project_for_test("alpha", folder.to_str().unwrap(), vec!["/bin/sleep".into(), "30".into()]);
    let mut rt = Runtime::new(app, view, (1240.0, 800.0), Env::new(true, 2.0));
    rt.on_key = Some(on_key);
    rt.poll = Some(poll);
    rt.frame(0.0);

    key(&mut rt, Key::Command('n'), Mods { cmd: true, ..Default::default() });

    assert_eq!(rt.state.ws.live.len(), 2, "⌘N starts immediately");
    let current = rt.state.current().unwrap();
    assert_eq!(current.tool, workspace::Tool::Shell, "inherits the current tool, not the default");
    assert_eq!(current.project, "alpha");
    assert_eq!(current.cwd(), folder.as_path(), "stays in the current worktree");
    assert_eq!(current.worktree.as_deref(), Some("topic"));
    assert!(!rt.state.ws.palette && rt.find("launcher").is_none());
    key(&mut rt, Key::Text("pwd".into()), Mods::default());
    key(&mut rt, Key::Enter, Mods::default());
    until(&mut rt, "a shell in the same directory", |rt| {
        rt.state.current().and_then(|l| l.term.frame.as_ref()).is_some_and(|frame| {
            let output: String = frame.lines.iter().flat_map(|l| &l.runs).map(|r| r.text.as_str()).collect();
            output.contains(folder.to_str().unwrap())
        })
    });
    drop(rt);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn command_n_without_a_current_session_opens_the_launcher() {
    let app = workspace_app(vec![]);
    app.index_job.cancel();
    let mut rt = Runtime::new(app, view, (1240.0, 800.0), Env::new(true, 2.0));
    rt.on_key = Some(on_key);
    rt.frame(0.0);
    key(&mut rt, Key::Command('n'), Mods { cmd: true, ..Default::default() });
    assert!(rt.state.ws.live.is_empty());
    assert!(rt.find("launcher").is_some());
    key(&mut rt, Key::Text("project".into()), Mods::default());
    assert_eq!(rt.state.ws.query.text, "project", "the launcher receives typing");
}

#[test]
fn command_shift_n_then_return_starts_in_the_project_without_a_mouse() {
    let root = fixture("keyboard-launcher");
    let folder = root.join("project");
    std::fs::create_dir_all(&folder).unwrap();
    let mut app = workspace_app(vec![in_folder(fake("a", Source::Claude, "alpha", 1), folder.to_str().unwrap())]);
    app.index_job.cancel();
    app.settings_path = root.join("settings.json");
    app.ws.argv = Some(vec!["/bin/sh".into(), "-c".into(), "printf keyboard-started; sleep 30".into()]);
    let mut rt = Runtime::new(app, view, (1240.0, 800.0), Env::new(true, 2.0));
    rt.on_key = Some(on_key);
    rt.poll = Some(poll);
    rt.frame(0.0);
    rt.key(KeyEvent { key: Key::Command('n'), mods: Mods { cmd: true, shift: true, ..Default::default() } });
    rt.frame(0.0);
    assert!(rt.state.ws.live.is_empty() && rt.find("launcher").is_some(), "⇧⌘N opens the launcher without starting a session");
    rt.key(KeyEvent { key: Key::Enter, mods: Mods::default() });
    rt.frame(0.0);
    until(&mut rt, "keyboard-started", |rt| printed(rt).contains("keyboard-started"));
    assert_eq!(rt.state.ws.live.len(), 1);
    assert_eq!(rt.state.current().unwrap().cwd(), folder.as_path());
}

/// A window with a terminal running in `folder`, and the launcher up over it.
fn palette_over_terminal(sessions: Vec<Session>) -> Runtime<App> {
    let mut app = workspace_app(sessions);
    app.index_job.cancel();
    app.settings_path = std::env::temp_dir().join(format!("hyper-palette-{}.json", std::process::id()));
    app.running = Some(Running { trusted: true, ..Running::default() });
    app.ws.argv = Some(vec!["/bin/sleep".into(), "30".into()]);
    app.start_from_launcher();
    let mut rt = Runtime::new(app, view, (1240.0, 800.0), Env::new(true, 2.0));
    rt.on_key = Some(on_key);
    rt.frame(0.0);
    rt.key(KeyEvent { key: Key::Command('n'), mods: Mods { cmd: true, shift: true, ..Default::default() } });
    rt.frame(0.0);
    assert!(rt.state.ws.palette && rt.find("palette-search").is_some(), "⇧⌘N puts the launcher over the terminal");
    rt
}

fn key(rt: &mut Runtime<App>, key: Key, mods: Mods) {
    rt.key(KeyEvent { key, mods });
    rt.frame(1.0 / 60.0);
}

#[test]
fn the_launcher_over_a_terminal_types_picks_a_tool_and_escape_leaves_the_terminal_running() {
    let mut rt = palette_over_terminal(vec![in_folder(fake("a", Source::Claude, "alpha", 2), "/"), in_folder(fake("b", Source::Claude, "beta", 1), "/tmp")]);
    let none = Mods::default();
    key(&mut rt, Key::Tab, none);
    assert_eq!(workspace::Tool::ALL[rt.state.ws.tool], workspace::Tool::Agent(Source::Codex), "Tab picks the next tool");
    key(&mut rt, Key::Tab, Mods { shift: true, ..none });
    key(&mut rt, Key::Tab, Mods { shift: true, ..none });
    assert_eq!(workspace::Tool::ALL[rt.state.ws.tool], workspace::Tool::Shell, "⇧Tab wraps");
    for c in ["b", "e", "t"] {
        key(&mut rt, Key::Text(c.into()), none);
    }
    assert_eq!(rt.state.ws.query.text, "bet", "typing goes to the search, not the terminal");
    assert_eq!(rt.state.choices()[0], workspace::Choice::Folder("beta".into(), "/tmp".into()));
    key(&mut rt, Key::Escape, none);
    assert!(!rt.state.ws.palette);
    rt.key(KeyEvent { key: Key::Command('n'), mods: Mods { cmd: true, shift: true, ..none } });
    key(&mut rt, Key::Command('w'), Mods { cmd: true, ..none });
    assert!(!rt.state.ws.palette, "⌘W closes the launcher");
    assert_eq!(rt.state.ws.live.len(), 1, "and leaves the terminal behind it");
    assert_eq!(rt.state.ws.main, workspace::Main::Terminal(1));
}

#[test]
fn the_launcher_offers_the_folder_on_screen_first_and_return_on_a_recent_session_resumes_it() {
    let mut rt = palette_over_terminal(vec![in_folder(fake("a", Source::Claude, "alpha", 3), "/"), in_folder(fake("b", Source::Claude, "beta", 2), "/tmp"), in_folder(fake("c", Source::Claude, "alpha", 1), "/")]);
    // The terminal runs in alpha's folder; beta is newer to the list but alpha comes first.
    rt.update(|s: &mut App, tx: &mut Tx| { s.close_palette(); s.ws.live[0].project = "beta".into(); s.new_session(None, Some(tx)); });
    rt.frame(0.0);
    let choices = rt.state.choices();
    assert_eq!(choices[0], workspace::Choice::Folder("alpha".into(), "/".into()));
    let resume = choices.iter().position(|c| *c == workspace::Choice::Resume("b".into())).unwrap();
    for _ in 0..resume {
        key(&mut rt, Key::Down, Mods::default());
    }
    key(&mut rt, Key::Enter, Mods::default());
    assert_eq!(rt.state.ws.live.len(), 2);
    assert_eq!(rt.state.current().unwrap().session.as_deref(), Some("b"));
    assert!(!rt.state.ws.palette, "starting closes the launcher");
}

#[test]
fn return_before_the_history_has_loaded_starts_nothing() {
    let mut app = workspace_app(vec![in_folder(fake("a", Source::Claude, "alpha", 1), "/")]);
    app.index = Load::Waiting;
    app.ws.argv = Some(vec!["/bin/sleep".into(), "30".into()]);
    let mut rt = Runtime::new(app, view, (1240.0, 800.0), Env::new(true, 2.0));
    rt.on_key = Some(on_key);
    rt.frame(0.0);
    key(&mut rt, Key::Down, Mods::default());
    key(&mut rt, Key::Enter, Mods::default());
    assert!(rt.state.ws.live.is_empty(), "nothing was shown to start");
    assert!(rt.file_dialog().is_none());
}

#[test]
fn command_return_on_choose_folder_makes_a_worktree_there() {
    let root = fixture("choose-folder-worktree");
    let plain = root.join("plain");
    std::fs::create_dir(&plain).unwrap();
    let roots = Roots::under(&root.join("none"));
    let mut app = App::at(roots, root.join("home"));
    app.view = View::Workspace;
    app.ws.argv = Some(vec!["/bin/sleep".into(), "30".into()]);
    let mut rt = Runtime::new(app, view, (1240.0, 800.0), Env::new(true, 2.0));
    rt.on_key = Some(on_key);
    rt.poll = Some(poll);
    until(&mut rt, "empty history", |rt| matches!(rt.state.index, Load::Ready(_)));
    assert_eq!(rt.state.choices(), [workspace::Choice::ChooseFolder]);
    key(&mut rt, Key::Enter, Mods { cmd: true, ..Default::default() });
    rt.resolve_file_dialog(Some(plain)).unwrap();
    until(&mut rt, "the error", |rt| rt.find("error").is_some());
    assert!(rt.accessibility_json().to_string().contains("Not a git repository"), "it tried a worktree, not the folder as it is");
    assert!(rt.state.ws.live.is_empty());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn keys_never_reach_the_terminal_under_the_launcher_even_while_a_worktree_is_made() {
    let script = r#"stty raw -echo; printf ready; cat -v"#;
    let mut rt = run_in(AGENT, "/", &["/bin/sh", "-c", script]);
    rt.on_key = Some(on_key);
    until(&mut rt, "the program", |rt| printed(rt).contains("ready"));
    rt.update(|s: &mut App, tx: &mut Tx| { s.new_session(None, Some(tx)); s.ws.creating = true; });
    rt.frame(1.0 / 60.0);
    key(&mut rt, Key::Text("x".into()), Mods::default());
    key(&mut rt, Key::Enter, Mods::default());
    std::thread::sleep(Duration::from_millis(200));
    for _ in 0..10 { rt.frame(1.0 / 60.0); }
    assert!(!printed(&rt).contains('x'), "the terminal under the launcher got the key");
}

#[test]
fn the_launcher_fits_a_small_window_with_its_footer_in_view() {
    for over_terminal in [false, true] {
        let mut app = workspace_app((0..12).map(|i| in_folder(fake(&format!("s{i}"), Source::Claude, &format!("p{i}"), i as i64), "/")).collect());
        app.running = Some(Running { trusted: true, ..Running::default() });
        app.ws.argv = Some(vec!["/bin/sleep".into(), "30".into()]);
        if over_terminal {
            app.start_from_launcher();
            app.new_session(None, None);
        }
        let mut rt = Runtime::new(app, view, (720.0, 480.0), Env::new(true, 2.0));
        for _ in 0..30 { rt.frame(1.0 / 60.0); }
        let (_, y) = rt.find("footer").expect("a footer");
        assert!(y < 470.0, "the footer is at {y} in a 480 point window (over a terminal: {over_terminal})");
    }
}

#[test]
fn command_o_in_the_launcher_keeps_the_tool_picked_there() {
    let root = fixture("command-o-tool");
    let folder = root.join("there");
    std::fs::create_dir(&folder).unwrap();
    let mut app = workspace_app(vec![in_folder(fake("a", Source::Claude, "alpha", 1), "/")]);
    app.ws.argv = Some(vec!["/bin/sleep".into(), "30".into()]);
    let mut rt = Runtime::new(app, view, (1240.0, 800.0), Env::new(true, 2.0));
    rt.on_key = Some(on_key);
    rt.frame(0.0);
    key(&mut rt, Key::Tab, Mods { shift: true, ..Default::default() });
    assert_eq!(workspace::Tool::ALL[rt.state.ws.tool], workspace::Tool::Shell);
    key(&mut rt, Key::Command('o'), Mods { cmd: true, ..Default::default() });
    rt.resolve_file_dialog(Some(folder.clone())).unwrap();
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.current().map(|l| l.tool), Some(workspace::Tool::Shell));
    drop(rt);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_launchers_own_return_chords_cant_be_given_to_a_command() {
    let settings = settings::Settings::default();
    for chord in ["cmd+enter", "cmd+shift+enter"] {
        let shortcut: rune::Shortcut = chord.parse().unwrap();
        assert!(commands::check(&settings, commands::Action::Browse, &shortcut).is_err(), "{chord} was accepted");
    }
}

#[test]
fn typing_at_launch_goes_to_the_launchers_search() {
    let mut rt = Runtime::new(workspace_app(vec![in_folder(fake("a", Source::Claude, "alpha", 1), "/")]), view, (1240.0, 800.0), Env::new(true, 2.0));
    rt.on_key = Some(on_key);
    rt.frame(0.0);
    key(&mut rt, Key::Text("a".into()), Mods::default());
    assert_eq!(rt.state.ws.query.text, "a");
}

#[test]
fn plus_starts_the_default_tool_at_once_and_its_menu_starts_the_others() {
    let mut app = workspace_app(vec![in_folder(fake("a", Source::Claude, "alpha", 1), "/")]);
    app.index_job.cancel();
    app.settings_path = std::env::temp_dir().join(format!("hyper-plus-{}.json", std::process::id()));
    app.ws.argv = Some(vec!["/bin/sleep".into(), "30".into()]);
    app.start_from_launcher();
    let mut rt = Runtime::new(app, view, (1240.0, 800.0), Env::new(true, 2.0));
    rt.frame(0.0);
    click(&mut rt, "project-alpha/new");
    assert_eq!(rt.state.ws.live.len(), 2, "no launcher in between");
    assert!(!rt.state.ws.palette);
    assert_eq!(rt.state.current().unwrap().tool, workspace::Tool::Agent(Source::Claude));

    let (x, y) = rt.find("project-alpha/new").unwrap();
    rt.secondary_down(x, y);
    rt.secondary_up(x, y);
    rt.frame(1.0 / 60.0);
    assert!(rt.popup_menu().is_some_and(|menu| menu.find("tree-codex").is_some()), "it offers each tool in a new worktree too");
    rt.resolve_popup(Some("here-shell")).unwrap();
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.ws.live.len(), 3);
    assert_eq!(rt.state.current().unwrap().tool, workspace::Tool::Shell);
}

#[test]
fn closing_the_terminal_on_screen_moves_to_its_neighbour_then_the_launcher() {
    let mut app = workspace_app(vec![in_folder(fake("a", Source::Claude, "alpha", 1), "/")]);
    app.start_from_launcher();
    app.start_from_launcher();
    app.close_live(2);
    assert_eq!(app.ws.main, workspace::Main::Terminal(1));
    app.close_live(1);
    assert_eq!(app.ws.main, workspace::Main::Launcher);
}

const AGENT: workspace::Tool = workspace::Tool::Agent(Source::Claude);

/// Starts a real program in a real terminal.
fn run_in(tool: workspace::Tool, cwd: &'static str, argv: &[&str]) -> Runtime<App> {
    let root = fixture("exits");
    let mut rt = runtime(root);
    rt.update(|s: &mut App, _: &mut Tx| {
        s.view = View::Workspace;
        s.index = Load::Ready(Arc::new(vec![in_folder(fake("a", Source::Claude, "alpha", 1), "/")]));
    });
    let argv: Vec<String> = argv.iter().map(|a| (*a).to_owned()).collect();
    rt.update(move |s: &mut App, _: &mut Tx| {
        s.ws.main = workspace::Main::Launcher;
        s.launch_for_test(tool, cwd, argv.clone());
        s.set_pane(640.0, 340.0, (8.0, 17.0));
    });
    rt
}

fn printed(rt: &Runtime<App>) -> String {
    let frame = rt.state.ws.live.first().and_then(|l| l.term.frame.as_ref());
    frame.map(|f| f.lines.iter().flat_map(|l| &l.runs).map(|r| r.text.as_str()).collect()).unwrap_or_default()
}

fn stopped(rt: &Runtime<App>) -> bool {
    rt.state.ws.live.first().is_some_and(|l| !l.running())
}

#[test]
fn space_reaches_the_terminal_after_switching_to_browse_and_back() {
    // What the program read goes to a file: the screen isn't redrawn while Browse shows.
    let out = std::env::temp_dir().join(format!("hyper-typed-{}", std::process::id()));
    let _ = std::fs::remove_file(&out);
    let script = format!("stty -echo -icanon; printf typing-ready; dd bs=1 count=3 2>/dev/null | od -An -tx1 > '{}'; sleep 30", out.display());
    let mut rt = run_in(AGENT, "/", &["/bin/sh", "-c", &script]);
    rt.on_key = Some(on_key);
    until(&mut rt, "the program waiting for keys", |rt| printed(rt).contains("typing-ready"));
    click(&mut rt, "terminal");
    click(&mut rt, "view-filter/segment1");
    assert_eq!(rt.state.view, View::Browse);
    click(&mut rt, "view-filter/segment0");
    assert_eq!(rt.state.view, View::Workspace);
    // One more than it reads, so a dropped key shows as the wrong bytes rather than a wait.
    for c in ["a", " ", "b", "c"] {
        rt.key(KeyEvent { key: Key::Text(c.into()), mods: Mods::default() });
        rt.frame(1.0 / 60.0);
    }
    let read = |out: &Path| std::fs::read_to_string(out).unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ");
    let started = Instant::now();
    while read(&out).is_empty() && started.elapsed() < Duration::from_secs(5) {
        rt.frame(1.0 / 60.0);
        std::thread::sleep(Duration::from_millis(5));
    }
    let typed = read(&out);
    let _ = std::fs::remove_file(&out);
    assert_eq!(typed, "61 20 62", "a, space and b, in order");
}

#[test]
fn trackpad_scrolling_moves_the_live_terminal_through_its_history() {
    let mut rt = run_in(AGENT, "/", &["/bin/sh", "-c", "i=1; while [ $i -le 100 ]; do printf 'scroll-row-%03d\\n' $i; i=$((i+1)); done; sleep 30"]);
    until(&mut rt, "terminal output", |rt| printed(rt).contains("scroll-row-100"));
    let (x, y) = rt.find("terminal").unwrap();
    rt.wheel_event(rune::WheelEvent::new(0.0, 600.0).at(x, y).precise(true).phase(rune::GesturePhase::Began));
    until(&mut rt, "older terminal output after scrolling up", |rt| !printed(rt).contains("scroll-row-100") && printed(rt).contains("scroll-row-050"));
    rt.wheel_event(rune::WheelEvent::new(0.0, -600.0).at(x, y).precise(true).phase(rune::GesturePhase::Began));
    until(&mut rt, "latest terminal output after scrolling down", |rt| printed(rt).contains("scroll-row-100"));
}

/// What a reload does, minus the `exec`: a failed one takes its terminals back the same way.
#[test]
fn a_handed_over_terminal_keeps_its_program_screen_and_scrollback() {
    let script = "i=1; while [ $i -le 100 ]; do printf 'old-row-%03d\\n' $i; i=$((i+1)); done; printf 'before-reload\\n'; sleep 1; printf 'while-away\\n'; \
            while read l; do [ \"$l\" = quit ] && exit 3; echo \"got:$l\"; done";
    let mut rt = run_in(AGENT, "/", &["/bin/sh", "-c", script]);
    until(&mut rt, "the program to start", |rt| printed(rt).contains("before-reload"));
    let dir = std::env::temp_dir().join(format!("hyper-handoff-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let handed = std::rc::Rc::new(std::cell::RefCell::new(Value::Null));
    let (into, from) = (handed.clone(), dir.clone());
    rt.update(move |s: &mut App, _: &mut Tx| *into.borrow_mut() = s.hand_off_workspace(&from));
    assert!(rt.state.ws.live.is_empty(), "the terminals were given up");
    let term = &handed.borrow()["live"][0]["term"];
    assert!(term["fd"].as_i64().is_some() && term["snapshot"].is_string(), "{term}");
    // It prints while nobody reads its PTY, as it would while the new build starts.
    std::thread::sleep(Duration::from_millis(1500));
    let (handed, from) = (handed.borrow().clone(), dir.clone());
    rt.update(move |s: &mut App, _: &mut Tx| s.adopt_workspace(&handed, &from, true));
    until(&mut rt, "what it printed while away", |rt| printed(rt).contains("before-reload") && printed(rt).contains("while-away"));
    rt.update(|s: &mut App, _: &mut Tx| s.ws.live[0].term.text("hello\r"));
    until(&mut rt, "the same program to answer", |rt| printed(rt).contains("got:hello"));
    let (x, y) = rt.find("terminal").unwrap();
    rt.wheel_event(rune::WheelEvent::new(0.0, 3000.0).at(x, y).precise(true).phase(rune::GesturePhase::Began));
    until(&mut rt, "the scrollback from before", |rt| printed(rt).contains("old-row-001"));
    rt.update(|s: &mut App, _: &mut Tx| s.ws.live[0].term.text("quit\r"));
    until(&mut rt, "its exit to be heard", |rt| rt.state.ws.live.first().is_some_and(|l| l.failed == Some(Some(3))));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn trackpad_scrolling_reports_the_pointer_position_to_mouse_tracking_programs() {
    let mut rt = run_in(AGENT, "/", &["/bin/sh", "-c", r"stty -echo -icanon; printf '\033[?1000h\033[?1006hmouse-ready'; dd bs=1 count=12 2>/dev/null | od -An -tx1; sleep 30"]);
    until(&mut rt, "mouse tracking", |rt| printed(rt).contains("mouse-ready"));
    let rect = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("terminal")).unwrap().rect;
    let cell = term::cell(term::style(&rt.state.settings.appearance.kit(true)));
    let (x, y) = (rect[0] + term::INSET + 20.5 * cell.0, rect[1] + term::INSET + 10.5 * cell.1);
    rt.wheel_event(rune::WheelEvent::new(0.0, cell.1 * 2.0).at(x, y).precise(true).phase(rune::GesturePhase::Began));
    until(&mut rt, "mouse report", |rt| printed(rt).split_whitespace().collect::<Vec<_>>().join(" ").contains("1b 5b 3c"));
    let report = printed(&rt).split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(report.contains("1b 5b 3c 36 34 3b 32 31 3b 31 31 4d"), "wheel up at column 21, row 11: {report}");
}

#[test]
fn trackpad_scrolling_sends_arrow_keys_to_full_screen_programs() {
    let mut rt = run_in(AGENT, "/", &["/bin/sh", "-c", r"stty -echo -icanon; printf '\033[?1049hpager-ready'; dd bs=1 count=6 2>/dev/null | od -An -tx1; sleep 30"]);
    until(&mut rt, "alternate screen", |rt| printed(rt).contains("pager-ready"));
    let (x, y) = rt.find("terminal").unwrap();
    let cell = term::cell(term::style(&rt.state.settings.appearance.kit(true)));
    rt.wheel_event(rune::WheelEvent::new(0.0, cell.1 * 2.0).at(x, y).precise(true).phase(rune::GesturePhase::Began));
    until(&mut rt, "two up arrows", |rt| printed(rt).split_whitespace().collect::<Vec<_>>().join(" ").contains("1b 5b 41 1b 5b 41"));
}

#[test]
fn command_u_and_j_page_a_full_screen_program_and_shift_jumps_to_its_ends() {
    let mut rt = run_in(AGENT, "/", &["/bin/sh", "-c", r"stty -echo -icanon; printf '\033[?1049hpager-ready'; dd bs=1 count=20 2>/dev/null | od -An -tx1; sleep 30"]);
    until(&mut rt, "the program", |rt| printed(rt).contains("pager-ready"));
    let cmd = Mods { cmd: true, ..Default::default() };
    for (c, mods) in [('u', cmd), ('j', cmd), ('u', Mods { shift: true, ..cmd }), ('j', Mods { shift: true, ..cmd })] {
        let ev = KeyEvent { key: Key::Command(c), mods };
        rt.update(move |s: &mut App, tx: &mut Tx| { on_key(s, &ev, tx); });
    }
    // ESC[5~ ESC[6~ ESC[1;5H ESC[1;5F
    let sent = "1b 5b 35 7e 1b 5b 36 7e 1b 5b 31 3b 35 48 1b 5b 31 3b 35 46";
    until(&mut rt, "page up, page down, ctrl-home, ctrl-end", |rt| printed(rt).split_whitespace().collect::<Vec<_>>().join(" ").contains(sent));
}

#[test]
fn a_clean_exit_closes_the_terminal() {
    let mut rt = run_in(AGENT, "/", &["/usr/bin/true"]);
    until(&mut rt, "the terminal to close", |rt| rt.state.ws.live.is_empty());
    assert_eq!(rt.state.ws.main, workspace::Main::Launcher);
}

#[test]
fn a_failed_agent_stays_with_its_exit_code() {
    let mut rt = run_in(AGENT, "/", &["/bin/sh", "-c", "echo gone wrong; exit 3"]);
    until(&mut rt, "the terminal to fail", stopped);
    assert_eq!(rt.state.ws.live[0].failed, Some(Some(3)));
    assert!(printed(&rt).contains("gone wrong"), "what it printed stays to read");
}

#[test]
fn dragging_failed_output_and_command_c_copies_it_through_the_engine() {
    let mut rt = run_in(AGENT, "/", &["/bin/sh", "-c", r"printf '\033[2J\033[Hcopy alpha beta'; exit 3"]);
    until(&mut rt, "the terminal to fail", stopped);
    assert!(printed(&rt).contains("copy alpha beta"), "{}", printed(&rt));
    let clipboard = rune::clipboard::memory();
    clipboard.set_text("unchanged");
    rt.use_clipboard(&clipboard);
    let ([x, y, _, _], _) = rt.presented("terminal").unwrap();
    let cell = term::cell(term::style(&rt.state.settings.appearance.kit(true)));
    let (x, y) = (x + term::INSET, y + term::INSET + cell.1 / 2.0);
    rt.pointer_down(x + 5.1 * cell.0, y);
    rt.pointer_move(x + 9.9 * cell.0, y);
    rt.pointer_up(x + 9.9 * cell.0, y);
    rt.frame(0.0);
    until(&mut rt, "selection highlight", |rt| rt.state.current().unwrap().term.frame.as_ref().is_some_and(|f| f.lines[0].selected.is_some()));
    let (rect, opacity) = rt.presented("terminal-selection").expect("selection is drawn");
    assert!(rect[2] >= cell.0 * 4.0 && rect[3] >= cell.1 && opacity > 0.0, "{rect:?}, {opacity}");
    rt.key(KeyEvent { key: Key::Command('c'), mods: Mods { cmd: true, ..Mods::default() } });
    rt.frame(0.0);
    until(&mut rt, "copied text", |_| clipboard.text().is_some_and(|text| *text != "unchanged"));
    assert_eq!(clipboard.text().as_deref().map(String::as_str), Some("alpha"));
}

#[test]
fn a_program_s_image_covers_the_cells_it_asked_for() {
    // A 3×2 PNG over 4 columns and 2 rows from row 2, column 3, as `kitty icat` sends one.
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAMAAAACCAYAAACddGYaAAAAEUlEQVR4nGP4z8DwH4YZkDkAm34L9XKwuTwAAAAASUVORK5CYII=";
    let script = format!(r"printf '\033[2J\033[2;3H\033_Ga=T,f=100,c=4,r=2,q=2;{png}\033\\'; sleep 30");
    let mut rt = run_in(AGENT, "/", &["/bin/sh", "-c", &script]);
    until(&mut rt, "the image", |rt| rt.presented("terminal-image-0").is_some_and(|(_, opacity)| opacity > 0.0));
    let ([x, y, _, _], _) = rt.presented("terminal").unwrap();
    let (w, h) = term::cell(term::style(&rt.state.settings.appearance.kit(true)));
    let ([ix, iy, iw, ih], _) = rt.presented("terminal-image-0").unwrap();
    let near = |a: f32, b: f32| (a - b).abs() < 0.01;
    assert!(near(ix, x + term::INSET + 2.0 * w) && near(iy, y + term::INSET + h), "at {ix},{iy}");
    assert!(near(iw, 4.0 * w) && near(ih, 2.0 * h), "{iw}×{ih}");
}

#[test]
fn a_shell_closes_whatever_its_last_command_returned() {
    let mut rt = run_in(workspace::Tool::Shell, "/", &["/bin/sh", "-c", "exit 1"]);
    until(&mut rt, "the shell to close", |rt| rt.state.ws.live.is_empty());
}

#[test]
fn a_program_ending_is_noticed_even_with_a_background_job_holding_the_terminal() {
    let mut rt = run_in(AGENT, "/", &["/bin/sh", "-c", "sleep 30 & exit 0"]);
    until(&mut rt, "the terminal to close", |rt| rt.state.ws.live.is_empty());
}

#[test]
fn a_noisy_background_job_cant_keep_a_finished_program_from_being_noticed() {
    let mut rt = run_in(AGENT, "/", &["/bin/sh", "-c", "trap '' HUP; yes & exit 3"]);
    until(&mut rt, "the terminal to fail", stopped);
}

#[test]
fn a_folder_that_cant_be_entered_says_why() {
    let mut rt = run_in(AGENT, "/no/such/folder", &["/usr/bin/true"]);
    until(&mut rt, "the terminal to fail", stopped);
    until(&mut rt, "the reason", |rt| printed(rt).contains("No such file"));
}

#[test]
fn leaving_for_browse_tells_the_program_it_lost_focus() {
    let script = r#"stty raw -echo; printf '\033[?1004h'; printf ready; cat -v"#;
    let mut rt = run_in(AGENT, "/", &["/bin/sh", "-c", script]);
    until(&mut rt, "the program", |rt| printed(rt).contains("ready"));
    rt.update(|s: &mut App, _: &mut Tx| s.set_view(View::Browse));
    std::thread::sleep(Duration::from_millis(100));
    rt.update(|s: &mut App, _: &mut Tx| s.set_view(View::Workspace));
    until(&mut rt, "focus out", |rt| printed(rt).contains("^[[O"));
}

#[test]
fn workspace_headers_fold_whatever_browse_is_doing() {
    let mut app = workspace_app(vec![in_folder(fake("a", Source::Claude, "alpha", 1), "/")]);
    app.start_from_launcher();
    app.search.field.text = "hidden".into();
    app.mode = Mode::Config;
    app.relist();
    assert_eq!(listing(&app), ["# alpha", "live 1"]);
    app.toggle_project("alpha", "project-alpha");
    assert_eq!(listing(&app), ["# alpha"]);
}

#[test]
fn a_terminal_ending_behind_browse_leaves_the_transcript_alone() {
    let mut app = workspace_app(vec![in_folder(fake("a", Source::Claude, "alpha", 2), "/"), in_folder(fake("x", Source::Claude, "alpha", 1), "/")]);
    app.resume("a");
    app.ws.tool = 0;
    app.start_from_launcher();
    app.set_view(View::Browse);
    app.select("x");
    let on_screen = app.ws.live.last().map(|l| l.id).unwrap();
    app.close_live(on_screen);
    assert_eq!(app.reader.selected.as_deref(), Some("x"));
}

#[test]
fn a_session_already_going_isnt_taken_for_a_new_terminals() {
    let started = now();
    let mut app = workspace_app(vec![in_folder(fake("busy", Source::Codex, "alpha", started), "/")]);
    app.ws.tool = 1;
    app.start_from_launcher();
    assert_eq!(app.current().and_then(|l| l.session.clone()), None);
}

fn folder(name: &str) -> String {
    let path = std::env::temp_dir().join(format!("sessions-{}-{name}", std::process::id()));
    std::fs::create_dir_all(path.join(".claude/worktrees/x")).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn a_project_starts_in_its_repo_not_a_worktree_that_ran_last() {
    let repo = folder("repo");
    let worktree = format!("{repo}/.claude/worktrees/x");
    let app = workspace_app(vec![in_folder(fake("new", Source::Claude, "app", 2), &worktree), in_folder(fake("old", Source::Claude, "app", 1), &repo)]);
    assert_eq!(app.projects(), [("app".to_owned(), PathBuf::from(repo))]);
}

#[test]
fn a_project_with_sessions_only_in_worktrees_starts_in_its_repo() {
    let repo = folder("trees-only");
    let worktree = format!("{repo}/.claude/worktrees/x");
    let app = workspace_app(vec![in_folder(fake("new", Source::Claude, "app", 2), &worktree)]);
    assert_eq!(app.projects(), [("app".to_owned(), PathBuf::from(repo))]);
}

#[test]
fn a_new_worktree_starts_the_tool_in_it_and_lists_it_under_its_project() {
    let repo = worktree::tests::repo("launch-worktree");
    let mut app = workspace_app(vec![in_folder(fake("old", Source::Claude, "app", 1), &repo.to_string_lossy())]);
    app.ws.argv = Some(vec!["/bin/sleep".into(), "30".into()]);
    app.start_from_launcher();
    app.show(workspace::Main::Launcher);
    app.ws.worktree = true;
    app.ws.worktree_name = TextEdit::new("feature");
    app.start_from_launcher();
    assert!(app.ws.creating && app.ws.live.len() == 1, "nothing starts until the worktree is there");
    app.wait_for_worktree();

    let tree = repo.join(".claude/worktrees/feature");
    assert!(tree.join("sub/file").is_file());
    let live = app.current().unwrap();
    assert_eq!((live.project.as_str(), live.worktree.as_deref()), ("app", Some("feature")));
    assert_eq!(listing(&app), ["# app", "live 1", "## feature", "live 2"]);
    assert!(!app.ws.worktree && app.ws.worktree_name.text.is_empty(), "the next Start is a plain one again");

    // Closing it goes back to the project's own folder, not the worktree.
    app.close_live(2);
    assert_eq!(listing(&app), ["# app", "live 1"]);
    app.show(workspace::Main::Launcher);
    assert_eq!(app.launcher_folder(), Some(repo.clone()));
    app.settle_tidy();
    std::fs::remove_dir_all(repo).unwrap();
}

/// An app on a real repository, running a stand-in program in a new worktree of it.
fn in_new_worktree(name: &str) -> (App, PathBuf, PathBuf) {
    let repo = worktree::tests::repo(name);
    let mut app = workspace_app(vec![in_folder(fake("old", Source::Claude, "app", 1), &repo.to_string_lossy())]);
    app.ws.argv = Some(vec!["/bin/sleep".into(), "30".into()]);
    app.ws.worktree = true;
    app.ws.worktree_name = TextEdit::new("w");
    app.start_from_launcher();
    app.wait_for_worktree();
    let tree = repo.join(".claude/worktrees/w");
    assert!(tree.is_dir());
    (app, repo, tree)
}

fn branches(repo: &Path) -> String {
    let out = std::process::Command::new("git").arg("-C").arg(repo).args(["branch", "--list"]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_worktree_with_nothing_in_it_goes_when_its_last_terminal_closes() {
    let (mut app, repo, tree) = in_new_worktree("tidy-clean");
    let first = app.current().unwrap().id;
    app.launch_in(tree.clone());
    app.start_from_launcher();
    app.close_live(first);
    app.settle_tidy();
    assert!(tree.is_dir(), "another terminal is still in it");
    let second = app.current().unwrap().id;
    app.close_live(second);
    app.settle_tidy();
    assert!(!tree.exists());
    assert!(!branches(&repo).contains("worktree-w"));
    assert!(app.ws.asking.is_empty(), "nothing would be lost, so nothing was asked");
    assert!(!app.projects().iter().any(|(_, p)| p == &tree));
    std::fs::remove_dir_all(repo).unwrap();
}

#[test]
fn a_worktree_closed_twice_in_quick_succession_is_asked_about_once() {
    let (mut app, repo, tree) = in_new_worktree("tidy-twice");
    std::fs::write(tree.join("draft"), "x").unwrap();
    let id = app.current().unwrap().id;
    app.close_live(id);
    app.launch_in(tree.clone());
    app.start_from_launcher();
    let id = app.current().unwrap().id;
    app.close_live(id);
    app.settle_tidy();
    assert_eq!(app.ws.asking.len(), 1);
    std::fs::remove_dir_all(repo).unwrap();
}

#[test]
fn a_worktree_something_outside_the_app_is_running_in_is_left_alone() {
    let (mut app, repo, tree) = in_new_worktree("tidy-busy");
    let mut outside = std::process::Command::new("/bin/sleep").arg("30").current_dir(tree.join("sub")).spawn().unwrap();
    let id = app.current().unwrap().id;
    app.close_live(id);
    app.settle_tidy();
    outside.kill().unwrap();
    outside.wait().unwrap();
    assert!(tree.is_dir() && app.ws.asking.is_empty(), "it's in use, so it's neither removed nor asked about");
    std::fs::remove_dir_all(repo).unwrap();
}

#[test]
fn nothing_starts_in_a_worktree_while_its_being_removed() {
    let (mut app, repo, tree) = in_new_worktree("tidy-race");
    let id = app.current().unwrap().id;
    app.launch_in(tree.clone());
    app.close_live(id);
    // Its check finds nothing, and removal begins.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while app.ws.live.is_empty() && !app.removing(&tree) {
        app.take_tidy();
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(!app.projects().iter().any(|(_, p)| p == &tree), "it's no longer offered");
    app.start_from_launcher();
    assert!(app.ws.live.is_empty(), "nothing started in a folder about to be deleted");
    app.settle_tidy();
    assert!(!tree.exists());
    std::fs::remove_dir_all(repo).unwrap();
}

#[test]
fn a_worktree_with_work_in_it_is_asked_about_kept_listed_and_removed_on_request() {
    let (mut app, repo, tree) = in_new_worktree("tidy-dirty");
    std::fs::write(tree.join("draft"), "x").unwrap();
    let id = app.current().unwrap().id;
    app.close_live(id);
    app.settle_tidy();
    assert_eq!(app.ws.asking, [(tree.clone(), worktree::Unsaved { files: 1, commits: 0 })]);

    let mut rt = Runtime::new(app, view, (1240.0, 800.0), Env { system_dark: true, scale: 2.0 });
    rt.frame(1.0 / 60.0);
    assert!(rt.accessibility_json().to_string().contains("It has 1 uncommitted file."));
    click(&mut rt, "tidy-keep");
    let mut app = rt.state;
    assert!(app.ws.asking.is_empty() && tree.join("draft").is_file());
    // Kept, it can be picked to start in again.
    assert!(app.projects().contains(&("app".to_owned(), tree.clone())));

    app.launch_in(tree.clone());
    app.start_from_launcher();
    let id = app.current().unwrap().id;
    app.close_live(id);
    app.settle_tidy();
    app.answer_tidy(true);
    app.settle_tidy();
    assert!(!tree.exists());
    assert!(!branches(&repo).contains("worktree-w"));
    std::fs::remove_dir_all(repo).unwrap();
}

#[test]
fn a_worktree_that_cant_be_made_says_why_and_starts_nothing() {
    let root = fixture("worktree-error");
    let plain = root.join("plain");
    std::fs::create_dir(&plain).unwrap();
    let roots = Roots::under(&root.join("none"));
    let mut app = App::at(roots, root.join("home"));
    app.view = View::Workspace;
    app.ws.argv = Some(vec!["/bin/sleep".into(), "30".into()]);
    app.launch_in(plain);
    let mut rt = Runtime::new(app, view, (1240.0, 800.0), Env { system_dark: true, scale: 2.0 });
    rt.on_key = Some(on_key);
    rt.poll = Some(poll);
    until(&mut rt, "empty history", |rt| matches!(rt.state.index, Load::Ready(_)));
    // ⌘Return: the same folder, in a new worktree.
    key(&mut rt, Key::Enter, Mods { cmd: true, ..Default::default() });
    until(&mut rt, "the error", |rt| rt.find("error").is_some());
    assert!(rt.accessibility_json().to_string().contains("Not a git repository"));
    assert!(rt.state.ws.live.is_empty() && !rt.state.ws.creating);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_worktree_from_the_sidebar_that_cant_be_made_says_why_over_the_terminal() {
    let root = fixture("worktree-error-sidebar");
    let plain = root.join("plain");
    std::fs::create_dir(&plain).unwrap();
    let roots = Roots::under(&root.join("none"));
    let mut app = App::at(roots, root.join("home"));
    app.ws.argv = Some(vec!["/bin/sleep".into(), "30".into()]);
    app.start_now("plain".into(), plain.clone(), workspace::Tool::Shell, false);
    app.start_now("plain".into(), plain, workspace::Tool::Shell, true);
    app.wait_for_worktree();
    assert!(app.ws.palette, "the launcher comes up over the terminal");
    assert_eq!(app.ws.live.len(), 1);
    let mut rt = Runtime::new(app, view, (1240.0, 800.0), Env::new(true, 2.0));
    rt.frame(0.0);
    assert!(rt.find("error").is_some());
    assert!(rt.accessibility_json().to_string().contains("Not a git repository"), "with git's reason");
    drop(rt);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_project_whose_folder_is_gone_cant_be_started_as_another() {
    let mut app = workspace_app(vec![in_folder(fake("here", Source::Claude, "here", 2), "/"), in_folder(fake("gone", Source::Claude, "gone", 1), "/no/such/folder")]);
    app.new_session(Some("gone".to_owned()), None);
    app.start_from_launcher();
    assert!(app.ws.live.is_empty());
}

#[test]
fn a_session_whose_folder_is_gone_offers_no_resume() {
    let mut app = workspace_app(vec![in_folder(fake("gone", Source::Claude, "gone", 1), "/no/such/folder")]);
    app.running = Some(Running { trusted: true, ..Running::default() });
    let session = app.sessions()[0].clone();
    assert!(!app.can_resume(&session));
}

fn press(rt: &mut Runtime<App>, key: rune::Key, cmd: bool) {
    let ev = KeyEvent { key, mods: rune::Mods { cmd, ..Default::default() } };
    rt.update(move |s: &mut App, tx: &mut Tx| {
        on_key(s, &ev, tx);
    });
}

#[test]
fn command_digits_go_to_running_sessions_in_sidebar_order() {
    let root = fixture("digits");
    let tree = root.join("alpha/.claude/worktrees/feature");
    std::fs::create_dir_all(&tree).unwrap();
    let alpha = root.join("alpha").to_str().unwrap().to_owned();
    let beta = root.to_str().unwrap().to_owned();
    let tree = tree.to_str().unwrap().to_owned();
    let mut rt = runtime(root.clone());
    rt.update(move |s: &mut App, _: &mut Tx| {
        s.view = View::Workspace;
        // Started out of sidebar order: alpha's worktree, beta, alpha itself, beta again.
        for (project, cwd) in [("alpha", &tree), ("beta", &beta), ("alpha", &alpha), ("beta", &beta)] {
            s.launch_in_project_for_test(project, cwd, vec!["/bin/sleep".into(), "30".into()]);
        }
        s.relist_workspace();
    });
    let sidebar: Vec<u64> = rt.state.listed.iter().filter_map(|i| match i { Item::Live(id) => Some(*id), _ => None }).collect();
    assert_eq!(sidebar, [3, 1, 2, 4], "alpha's own terminal, its worktree's, then beta's");
    let shown = |rt: &Runtime<App>| match rt.state.ws.main { workspace::Main::Terminal(id) => Some(id), _ => None };
    let digit = |rt: &mut Runtime<App>, c: char, ctrl: bool| {
        let ev = KeyEvent { key: rune::Key::Command(c), mods: rune::Mods { cmd: true, ctrl, ..Default::default() } };
        rt.update(move |s: &mut App, tx: &mut Tx| { on_key(s, &ev, tx); });
    };

    rt.update(|s: &mut App, _: &mut Tx| s.set_view(View::Browse));
    digit(&mut rt, '1', false);
    assert_eq!((rt.state.view, shown(&rt)), (View::Workspace, Some(3)), "⌘1 comes back to the Workspace");
    digit(&mut rt, '2', false);
    assert_eq!(shown(&rt), Some(1));
    digit(&mut rt, '9', false);
    assert_eq!(shown(&rt), Some(4), "⌘9 is the last, however many");
    digit(&mut rt, '5', false);
    assert_eq!(shown(&rt), Some(4), "past the end does nothing");
    digit(&mut rt, '2', false);
    let ev = KeyEvent { key: rune::Key::Command('['), mods: rune::Mods { cmd: true, shift: true, ..Default::default() } };
    rt.update(move |s: &mut App, tx: &mut Tx| { on_key(s, &ev, tx); });
    assert_eq!(shown(&rt), Some(3), "previous follows the sidebar too");

    digit(&mut rt, '2', true);
    assert_eq!(rt.state.view, View::Browse, "⌃⌘2 is Browse");
    digit(&mut rt, '1', true);
    assert_eq!(rt.state.view, View::Workspace, "⌃⌘1 is the Workspace");
    drop(rt);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_sort_menu_puts_each_projects_newest_terminals_first_and_digits_follow() {
    let root = fixture("sidebar-order");
    let feature = root.join("alpha/.claude/worktrees/feature");
    let fix = root.join("alpha/.claude/worktrees/fix");
    std::fs::create_dir_all(&feature).unwrap();
    std::fs::create_dir_all(&fix).unwrap();
    let [alpha, beta, feature, fix] = [root.join("alpha"), root.clone(), feature, fix].map(|p| p.to_str().unwrap().to_owned());
    let mut rt = runtime(root.clone());
    rt.update(move |s: &mut App, _: &mut Tx| {
        s.view = View::Workspace;
        for (project, cwd) in [("alpha", &alpha), ("alpha", &feature), ("alpha", &alpha), ("alpha", &fix), ("alpha", &feature), ("beta", &beta)] {
            s.launch_in_project_for_test(project, cwd, vec!["/bin/sleep".into(), "30".into()]);
        }
        s.relist_workspace();
    });
    rt.frame(1.0 / 60.0);
    let sidebar = |rt: &Runtime<App>| -> Vec<u64> { rt.state.listed.iter().filter_map(|i| match i { Item::Live(id) => Some(*id), _ => None }).collect() };
    assert_eq!(sidebar(&rt), [1, 3, 2, 5, 4, 6], "oldest first until asked");

    click(&mut rt, "sidebar-order");
    let menu = rt.popup_menu().expect("a native menu opens").clone();
    assert!(menu.find("oldest").is_some_and(rune::PopupItem::is_checked) && menu.find("newest").is_some_and(|i| !i.is_checked()));
    rt.resolve_popup(Some("newest")).unwrap();
    rt.frame(1.0 / 60.0);
    assert_eq!(sidebar(&rt), [3, 1, 5, 2, 4, 6], "alpha's own newest first, then its most recent worktree's, and beta stays after alpha");
    assert_eq!(settings::Settings::load(&settings::Settings::path(&root.join("home"))).sidebar_order, settings::Order::Newest, "the pick is saved");

    let shown = |rt: &Runtime<App>| match rt.state.ws.main { workspace::Main::Terminal(id) => Some(id), _ => None };
    for (c, id) in [('1', 3), ('3', 5), ('9', 6)] {
        let ev = KeyEvent { key: rune::Key::Command(c), mods: rune::Mods { cmd: true, ..Default::default() } };
        rt.update(move |s: &mut App, tx: &mut Tx| { on_key(s, &ev, tx); });
        assert_eq!(shown(&rt), Some(id), "⌘{c} follows the sidebar");
    }

    click(&mut rt, "sidebar-order");
    assert!(rt.popup_menu().unwrap().find("newest").is_some_and(rune::PopupItem::is_checked));
    rt.resolve_popup(Some("oldest")).unwrap();
    rt.frame(1.0 / 60.0);
    assert_eq!(sidebar(&rt), [1, 3, 2, 5, 4, 6]);
    drop(rt);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn shortcuts_wait_behind_the_quit_question() {
    let mut rt = run_in(AGENT, "/", &["/bin/sleep", "30"]);
    rt.update(|s: &mut App, _: &mut Tx| s.ws.confirming_quit = true);
    let shown = rt.state.ws.main;
    press(&mut rt, rune::Key::Command('n'), true);
    assert_eq!(rt.state.ws.main, shown);
    press(&mut rt, rune::Key::Command('f'), true);
    assert_eq!(rt.state.view, View::Workspace);
}

#[test]
fn the_quit_question_goes_away_with_the_last_agent() {
    let mut rt = run_in(AGENT, "/", &["/usr/bin/true"]);
    rt.update(|s: &mut App, _: &mut Tx| s.ws.confirming_quit = true);
    until(&mut rt, "the question to go", |rt| !rt.state.ws.confirming_quit);
}

#[test]
fn switching_tool_opens_that_tools_newest_projects() {
    let root = fixture("newest");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    let mut sessions = Vec::new();
    for i in 0..40 {
        sessions.push(fake(&format!("c{i}"), Source::Claude, "shared", 1000 - i as i64));
    }
    for i in 0..40 {
        sessions.push(fake(&format!("x{i}"), Source::Codex, ["p", "q"][i / 20], 900 - i as i64));
    }
    for i in 0..40 {
        sessions.push(fake(&format!("y{i}"), Source::Codex, "shared", 800 - i as i64));
    }
    rt.state.open_projects.clear();
    rt.state.index = Load::Ready(Arc::new(sessions));
    rt.state.open_newest();
    rt.state.relist();
    rt.frame(1.0 / 60.0);
    click(&mut rt, "source-filter/segment2");
    for _ in 0..30 {
        rt.frame(1.0 / 60.0);
    }
    assert_eq!(sessions_listed(&rt), 80, "the three newest are open");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_search_box_filters_config_by_content() {
    let root = fixture("config-search");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    until(&mut rt, "config", |rt| !rt.state.config.items.is_empty());
    click(&mut rt, "mode-filter/segment1");
    type_text(&mut rt, "zebrafish");
    let items: Vec<&Item> = rt.state.listed.iter().filter(|i| matches!(i, Item::Config(_))).collect();
    assert_eq!(items.len(), 1, "only the rules file says it");
    for _ in 0..9 {
        rt.key(KeyEvent { key: Key::Backspace, mods: Mods::default() });
    }
    rt.frame(1.0 / 60.0);
    type_text(&mut rt, "mcp");
    let names: Vec<String> = projects(&rt).into_iter().map(|(name, _)| name).collect();
    assert!(names.contains(&"MCP servers".to_owned()), "a group's own name matches: {names:?}");
    assert!(tools(&rt).contains(&Owner::Tool(Source::Pi)), "and it stays under its tool");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn opening_a_codex_session_shows_its_transcript() {
    let root = fixture("codex");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    click(&mut rt, "source-filter/segment2");
    click(&mut rt, "ccc");
    until(&mut rt, "codex transcript", |rt| rt.state.reader.selected.as_deref() == Some("ccc") && loaded(rt));
    let Load::Ready(t) = &rt.state.reader.transcript else { panic!() };
    assert_eq!(t.rows[0].text, "A codex prompt");
    std::fs::remove_dir_all(root).unwrap();
}

fn prompt_fixture(name: &str) -> PathBuf {
    let root = fixture(name);
    let mut body = user("first", "First prompt");
    for turn in 0..2 {
        for i in 0..80 {
            body += &reply(&format!("reply-{turn}-{i}"), "Enough transcript content to put the next prompt offscreen.");
        }
        body += &user(if turn == 0 { "middle" } else { "last" }, if turn == 0 { "Middle prompt" } else { "Last prompt" });
    }
    body += &reply("end", "Final short reply.");
    std::fs::write(root.join("claude/-src-demo/aaa.jsonl"), body).unwrap();
    root
}

#[test]
fn prompt_outline_previews_and_jumps_to_unmounted_prompts_at_the_top() {
    let root = prompt_fixture("prompt-outline");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    assert!(rt.presented("middle:0").is_none(), "the target starts outside the virtual window");
    let (x, y) = rt.find("prompt-mark-1").unwrap();
    rt.pointer_move(x, y);
    rt.frame(1.0 / 60.0);
    assert!(rt.find("prompt-preview-1-tooltip").is_some(), "hovering previews the prompt");
    rt.pointer_move(900.0, 400.0);
    rt.frame(1.0 / 60.0);
    until(&mut rt, "preview closed", |rt| rt.find("prompt-preview-1-tooltip").is_none());

    click(&mut rt, "prompt-mark-1");
    until(&mut rt, "middle prompt at top", |rt| {
        let Some((r, _)) = rt.presented("middle:0") else { return false };
        let (v, _) = rt.presented("transcript-aaa").unwrap();
        (r[1] - v[1]).abs() < 1.0
    });
    assert_eq!(rt.state.reader.prompt, 1);
    click(&mut rt, "previous-prompt");
    until(&mut rt, "first prompt", |rt| rt.state.reader.prompt == 0 && showing(rt, "First prompt"));
    click(&mut rt, "next-prompt");
    until(&mut rt, "next prompt", |rt| rt.state.reader.prompt == 1 && showing(rt, "Middle prompt"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn prompt_navigation_clamps_at_the_end_and_tracks_manual_scrolling() {
    let root = prompt_fixture("prompt-end");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    click(&mut rt, "prompt-mark-2");
    until(&mut rt, "last prompt", |rt| showing(rt, "Last prompt") && rt.scroll_at_bottom("transcript-aaa") == Some(true));
    let (r, _) = rt.presented("last:0").unwrap();
    let (v, _) = rt.presented("transcript-aaa").unwrap();
    assert!(r[1] > v[1] + 100.0, "a short final turn stays near the bottom: {r:?}, {v:?}");
    assert_eq!(rt.state.reader.prompt, 2);
    click(&mut rt, "next-prompt");
    assert_eq!(rt.state.reader.prompt, 2, "Next stops at the last prompt");
    let (x, y) = rt.find("transcript-aaa").unwrap();
    rt.wheel(x, y, 1600.0);
    until(&mut rt, "manual scroll back into the middle turn", |rt| rt.state.reader.prompt == 1);
    click(&mut rt, "previous-prompt");
    until(&mut rt, "first prompt", |rt| rt.state.reader.prompt == 0 && showing(rt, "First prompt"));
    click(&mut rt, "previous-prompt");
    assert_eq!(rt.state.reader.prompt, 0, "Previous stops at the first prompt");
    click(&mut rt, "bbb");
    until(&mut rt, "another session", |rt| rt.state.reader.selected.as_deref() == Some("bbb") && loaded(rt));
    assert!(rt.find("prompt-mark-1").is_none(), "the outline belongs to the selected session");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_tool_call_expands_and_the_toolbar_hides_tools() {
    let root = fixture("tools");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    click(&mut rt, "aaa");
    until(&mut rt, "long transcript", |rt| rt.state.reader.selected.as_deref() == Some("aaa") && loaded(rt));
    click(&mut rt, "row-toggle");
    assert!(rt.state.reader.expanded.contains("tool:0"));
    let total = rt.state.reader.shown.len();
    click(&mut rt, "tools-toggle");
    assert_eq!(rt.state.reader.shown.len(), total - 1);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_clear_button_empties_the_search_and_restores_the_list() {
    let root = fixture("clear");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    type_text(&mut rt, "zebrafish");
    until(&mut rt, "hits", |rt| sessions_listed(rt) == 1 && !rt.state.search.job.is_pending());
    click(&mut rt, "search/action");
    assert_eq!(rt.state.search.field.text, "");
    assert_eq!(sessions_listed(&rt), 4);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn text_typed_in_config_searches_content_on_returning_to_sessions() {
    let root = fixture("mode-search");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    until(&mut rt, "config", |rt| !rt.state.config.items.is_empty());
    click(&mut rt, "mode-filter/segment1");
    type_text(&mut rt, "zebrafish");
    click(&mut rt, "mode-filter/segment0");
    until(&mut rt, "hits", |rt| rt.state.search.hits.contains_key("aaa"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_second_codex_session_shows_its_own_rows() {
    let root = fixture("codex-rows");
    let day = root.join("codex/sessions/2026/01/02");
    let meta = line(json!({"type": "session_meta", "payload": {"id": "ddd", "cwd": "/src/other", "source": "cli"}}));
    let prompt = line(json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Second codex prompt"}]}}));
    std::fs::write(day.join("rollout-ddd.jsonl"), meta + &prompt).unwrap();
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    click(&mut rt, "source-filter/segment2");
    click(&mut rt, "ccc");
    until(&mut rt, "first codex session", |rt| rt.state.reader.selected.as_deref() == Some("ccc") && loaded(rt));
    click(&mut rt, "ddd");
    until(&mut rt, "second codex session", |rt| rt.state.reader.selected.as_deref() == Some("ddd") && loaded(rt));
    let texts: Vec<String> = rt.elements().into_iter().filter_map(|e| e.text).collect();
    assert!(texts.iter().any(|t| t == "Second codex prompt"), "{texts:?}");
    let first = texts.iter().filter(|t| *t == "A codex prompt").count();
    assert_eq!(first, 1, "only its sidebar row: {texts:?}");
    std::fs::remove_dir_all(root).unwrap();
}

fn append(path: &Path, text: &str) {
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(text.as_bytes()).unwrap();
}

/// A runtime that sees file events and process exits as the window would.
fn watching(root: PathBuf) -> Runtime<App> {
    let mut rt = runtime(root);
    rt.state.watch(Arc::new(|| {}));
    rt
}

fn first_history(name: &str) -> (PathBuf, Runtime<App>) {
    let root = std::env::temp_dir().join(format!("hyper-first-history-{name}-{}", std::process::id()));
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let roots = Roots::at_home(&home);
    let mut app = App::at(roots, home);
    app.view = View::Workspace;
    let mut rt = Runtime::new(app, view, (1240.0, 800.0), Env { system_dark: true, scale: 2.0 });
    rt.poll = Some(poll);
    until(&mut rt, "initial index", |rt| !matches!(rt.state.index, Load::Waiting));
    (root, rt)
}

fn write_first_session(rt: &Runtime<App>, cwd: &Path) -> PathBuf {
    let path = rt.state.roots.get(Source::Codex).join("sessions/2026/01/02/rollout-first.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let meta = line(json!({"type": "session_meta", "payload": {"id": "first", "cwd": cwd, "source": "cli"}}));
    let prompt = line(json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "My first session"}]}}));
    std::fs::write(&path, meta + &prompt).unwrap();
    path
}

#[test]
fn a_delivered_first_session_change_is_indexed_without_reloading() {
    let (root, mut rt) = first_history("delivered");
    let path = write_first_session(&rt, &root);
    rt.state.absorb(Changes { files: HashSet::from([(Source::Codex, path)]), ..Changes::default() });
    until(&mut rt, "first session after a file event", |rt| rt.state.session("first").is_some());
    rt.update(|s: &mut App, _: &mut Tx| s.set_view(View::Browse));
    until(&mut rt, "first session in Browse", |rt| showing(rt, "My first session"));
    drop(rt);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn the_first_history_folder_is_discovered_and_its_session_links_automatically() {
    let (root, mut rt) = first_history("watched");
    rt.state.watch(Arc::new(|| {}));
    rt.update({
        let cwd = root.to_str().unwrap().to_owned();
        move |s: &mut App, _: &mut Tx| s.launch_for_test(workspace::Tool::Agent(Source::Codex), &cwd, vec!["/bin/sleep".into(), "60".into()])
    });
    rt.frame(0.0);
    write_first_session(&rt, &root);
    until(&mut rt, "first session from the filesystem watcher", |rt| rt.state.current().is_some_and(|live| live.session.as_deref() == Some("first")));
    assert_eq!(rt.state.sessions().len(), 1);
    assert!(showing(&rt, "My first session"), "the terminal's title follows the new session");
    std::fs::write(rt.state.roots.get(Source::Codex).join("session_index.jsonl"), line(json!({"id": "first", "thread_name": "First session renamed"}))).unwrap();
    until(&mut rt, "later writes under the new history root", |rt| rt.state.session("first").is_some_and(|s| s.title == "First session renamed"));
    rt.update(|s: &mut App, _: &mut Tx| s.set_view(View::Browse));
    until(&mut rt, "first session in Browse", |rt| showing(rt, "First session renamed"));
    drop(rt);
    std::fs::remove_dir_all(root).unwrap();
}

fn rows(rt: &Runtime<App>) -> Vec<String> {
    let Load::Ready(t) = &rt.state.reader.transcript else { return Vec::new() };
    t.rows.iter().map(|r| r.id.clone()).collect()
}

fn showing(rt: &Runtime<App>, wanted: &str) -> bool {
    rt.elements().into_iter().filter_map(|e| e.text).any(|t| t.contains(wanted))
}

fn activity(rt: &Runtime<App>, id: &str) -> Option<Activity> {
    rt.state.session(id).and_then(|s| rt.state.activity(s))
}

fn register(root: &Path, pid: u32, session: &str, status: &str) -> PathBuf {
    let folder = root.join("home/.claude/sessions");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join(format!("{pid}.json"));
    std::fs::write(&path, json!({"pid": pid, "sessionId": session, "status": status}).to_string()).unwrap();
    path
}

/// Makes a file look an hour old, so it isn't mistaken for a session being written.
fn age(path: &Path) {
    let file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(3_600)).unwrap();
}

fn sleeper() -> std::process::Child {
    std::process::Command::new("sleep").arg("60").spawn().unwrap()
}

#[test]
fn what_an_agent_writes_appears_without_touching_the_app() {
    let root = fixture("append");
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    let before = rows(&rt).len();
    rt.state.reader.expanded.insert("tool:0".to_owned());
    append(&root.join("claude/-src-demo/aaa.jsonl"), &reply("late", "Written while the window was open."));
    until(&mut rt, "the new reply", |rt| rows(rt).len() == before + 1);
    assert_eq!(rows(&rt).last().unwrap(), "late:0");
    assert!(rt.state.reader.expanded.contains("tool:0"), "what was open stays open");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_result_fills_in_a_tool_call_already_on_screen() {
    let root = fixture("result");
    let path = root.join("claude/-src-demo/bbb.jsonl");
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    click(&mut rt, "bbb");
    until(&mut rt, "second transcript", |rt| rt.state.reader.selected.as_deref() == Some("bbb") && loaded(rt));
    append(&path, &line(json!({"type": "assistant", "uuid": "call", "message": {"content": [
        {"type": "tool_use", "id": "t9", "name": "Bash", "input": {"command": "date"}}
    ]}})));
    until(&mut rt, "the call", |rt| rows(rt).len() == 3);
    rt.state.reader.expanded.insert("call:0".to_owned());
    rt.frame(1.0 / 60.0);
    append(&path, &line(json!({"type": "user", "uuid": "answer", "message": {"content": [
        {"type": "tool_result", "tool_use_id": "t9", "content": "Friday afternoon"}
    ]}})));
    until(&mut rt, "the result", |rt| showing(rt, "Friday afternoon"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_new_session_joins_the_sidebar_and_a_new_title_replaces_the_old() {
    let root = fixture("new-session");
    let project = root.join("claude/-src-demo");
    age(&project.join("aaa.jsonl"));
    age(&project.join("bbb.jsonl"));
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    std::fs::write(project.join("zzz.jsonl"), user("z0", "A brand new session")).unwrap();
    until(&mut rt, "the new session", |rt| rt.state.session("zzz").is_some());
    let place = |id: &str| rt.state.sessions().iter().position(|s| s.id == id);
    assert!(place("zzz") < place("aaa"), "the newest comes first");
    append(&project.join("zzz.jsonl"), &line(json!({"type": "custom-title", "customTitle": "Renamed"})));
    until(&mut rt, "the title", |rt| rt.state.session("zzz").is_some_and(|s| s.title == "Renamed"));
    std::fs::remove_file(project.join("zzz.jsonl")).unwrap();
    until(&mut rt, "it to go", |rt| rt.state.session("zzz").is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_subagents_transcript_is_not_a_session() {
    let root = fixture("subagent");
    let nested = root.join("claude/-src-demo/aaa/subagents");
    std::fs::create_dir_all(&nested).unwrap();
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    std::fs::write(nested.join("agent.jsonl"), user("s0", "A subagent's prompt")).unwrap();
    append(&root.join("claude/-src-demo/bbb.jsonl"), &reply("b2", "Later."));
    until(&mut rt, "the later write", |rt| rt.state.session("bbb").is_some_and(|s| s.updated > 0) && !rt.state.stale.job.is_pending() && rt.state.stale.is_empty());
    assert!(rt.state.session("agent").is_none());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_running_agent_shows_until_its_process_exits() {
    let root = fixture("exit");
    let mut child = sleeper();
    let entry = register(&root, child.id(), "aaa", "busy");
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    assert_eq!(activity(&rt, "aaa"), Some(Activity::Working));
    assert_eq!(activity(&rt, "bbb"), None, "a Claude session nothing is running stays quiet, however recent");
    register(&root, child.id(), "aaa", "idle");
    until(&mut rt, "it to go idle", |rt| activity(rt, "aaa") == Some(Activity::Open));
    child.kill().unwrap();
    // Not reaped and its entry left behind, as after a crash.
    until(&mut rt, "the exit", |rt| activity(rt, "aaa").is_none());
    assert!(entry.exists());
    child.wait().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_entry_left_by_a_dead_process_is_not_running() {
    let root = fixture("stale");
    let mut child = sleeper();
    let pid = child.id();
    child.kill().unwrap();
    child.wait().unwrap();
    register(&root, pid, "aaa", "busy");
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    assert_eq!(activity(&rt, "aaa"), None);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_folded_project_shows_that_something_in_it_is_running() {
    let root = fixture("folded");
    let mut child = sleeper();
    register(&root, child.id(), "aaa", "idle");
    age(&root.join("pi/--src-demo--/ppp.jsonl"));
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    assert!(rt.find("project-demo/live").is_none(), "its rows say so while it's open");
    click(&mut rt, "project-demo");
    assert!(rt.find("project-demo/live").is_some());
    child.kill().unwrap();
    until(&mut rt, "the exit", |rt| rt.find("project-demo/live").is_none());
    child.wait().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn with_no_registry_a_recent_write_counts_as_working_until_it_goes_quiet() {
    let root = fixture("fallback");
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    let session = rt.state.session("aaa").unwrap();
    assert_eq!(rt.state.activity_at(session, session.updated + QUIET - 1), Some(Activity::Working));
    assert_eq!(rt.state.activity_at(session, session.updated + QUIET), None);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_codex_or_pi_turn_is_believed_in_a_terminal_here_and_ends_the_guess_anywhere() {
    let now = 10_000;
    let turn = |id: &str, source, updated, turn| Session { turn, ..fake(id, source, "alpha", updated) };
    let mut rt = run_in(AGENT, "/", &["/bin/sleep", "30"]);
    rt.update(move |s: &mut App, _: &mut Tx| {
        s.running = Some(Running::default());
        s.index = Load::Ready(Arc::new(vec![
            turn("going", Source::Codex, now - 600, Some(true)),
            turn("here", Source::Pi, now - 600, Some(true)),
            turn("ended", Source::Codex, now, Some(false)),
            turn("unknown", Source::Pi, now, None),
        ]));
        s.ws.live[0].session = Some("here".to_owned());
    });
    let at = |id: &str| rt.state.activity_at(rt.state.session(id).unwrap(), now);
    assert_eq!(at("here"), Some(Activity::Working), "its process is alive, however long since a write");
    assert_eq!(at("going"), None, "elsewhere a quiet file may be a process that's gone");
    assert_eq!(at("ended"), None, "a finished turn isn't working, however recent");
    assert_eq!(at("unknown"), Some(Activity::Working), "with nothing to go on, a recent write still counts");
    rt.update(|s: &mut App, _: &mut Tx| s.ws.live[0].failed = Some(Some(1)));
    let at = |id: &str| rt.state.activity_at(rt.state.session(id).unwrap(), now);
    assert_eq!(at("here"), None, "a terminal whose agent died mid-turn says nothing of it");
}

/// A terminal running `script`, with the window in the background so nobody is looking.
fn unwatched(tool: workspace::Tool, script: &str) -> Runtime<App> {
    let mut rt = run_in(tool, "/", &["/bin/sh", "-c", script]);
    rt.update(|s: &mut App, _: &mut Tx| s.window_focus(false));
    rt
}

/// Whether the terminal's sidebar row shows the kit's needs-attention dot.
fn marked(rt: &Runtime<App>) -> bool {
    let warning = rune_kit::Kit::neutral(true).c.warning.icon;
    let elements = rt.elements();
    let Some(row) = elements.iter().find(|e| e.key.as_deref() == Some("live-1")) else { return false };
    elements.iter().any(|e| e.path.starts_with(&row.path) && e.fill == warning)
}

fn needs_you(rt: &Runtime<App>) -> bool {
    rt.state.ws.live.first().is_some_and(|l| l.needs_you)
}

#[test]
fn a_terminal_that_speaks_up_while_youre_elsewhere_needs_you_until_you_look() {
    let mut rt = unwatched(workspace::Tool::Shell, r"sleep 0.5; printf '\033]9;Build finished\a'; sleep 30");
    until(&mut rt, "the mark", needs_you);
    assert_eq!(rt.state.badged(), 1);
    assert!(marked(&rt), "the row says so");
    rt.update(|s: &mut App, _: &mut Tx| s.window_focus(true));
    rt.frame(1.0 / 60.0);
    assert!(!needs_you(&rt), "looking at it clears it");
    assert_eq!(rt.state.badged(), 0);
    assert!(!marked(&rt));
}

#[test]
fn a_bell_needs_you_too_but_not_while_youre_looking() {
    let mut rt = unwatched(workspace::Tool::Shell, r"sleep 0.5; printf 'ding\a'; sleep 30");
    until(&mut rt, "the mark", needs_you);
    let mut rt = run_in(workspace::Tool::Shell, "/", &["/bin/sh", "-c", r"sleep 0.5; printf 'ding\a'; sleep 30"]);
    until(&mut rt, "the bell", |rt| printed(rt).contains("ding"));
    assert!(!needs_you(&rt));
}

#[test]
fn an_agent_saying_its_idle_isnt_news_once_youve_seen_it() {
    let mut rt = unwatched(AGENT, r"sleep 0.5; printf 'waiting\033]9;Claude is waiting for your input\a'; sleep 30");
    until(&mut rt, "the reminder", |rt| printed(rt).contains("waiting"));
    assert!(!needs_you(&rt));
}

fn posted() -> Vec<(u64, String, bool)> {
    attention::POSTED.with_borrow(Clone::clone)
}

/// Runs frames for a while, for news that should not come.
fn wait(rt: &mut Runtime<App>, seconds: f32) {
    let started = Instant::now();
    while started.elapsed().as_secs_f32() < seconds {
        rt.frame(1.0 / 60.0);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn codex_saying_more_about_a_finished_turn_replaces_the_plain_notice_quietly() {
    let mut rt = unwatched(workspace::Tool::Agent(Source::Codex), r"sleep 0.5; printf '\033]9;Done\a'; sleep 0.5; printf '\033]9;Renamed the module and fixed its tests\a'; sleep 30");
    until(&mut rt, "the fuller words", |_| posted().iter().any(|(_, body, _)| body.starts_with("Renamed")));
    let sounds: Vec<bool> = posted().iter().map(|(_, _, sound)| *sound).collect();
    assert_eq!(sounds, [true, false], "the same news sounds once");
}

#[test]
fn a_bell_then_words_about_the_same_unseen_terminal_sound_once() {
    let mut rt = unwatched(workspace::Tool::Shell, r"sleep 0.5; printf '\a'; sleep 0.3; printf '\033]9;Build done\a'; sleep 30");
    until(&mut rt, "the words", |_| posted().iter().any(|(_, body, _)| body == "Build done"));
    let sounds: Vec<bool> = posted().iter().map(|(_, _, sound)| *sound).collect();
    assert_eq!(sounds, [true, false]);
}

#[test]
fn an_event_turned_off_doesnt_mark_badge_or_notify() {
    let mut rt = unwatched(workspace::Tool::Shell, r"sleep 0.5; printf 'rang\a'; sleep 30");
    rt.update(|s: &mut App, _: &mut Tx| {
        let mut changed = s.settings.clone();
        changed.notifications.entry("shell".into()).or_default().events.insert("bell".into(), "off".into());
        s.persist_settings(changed);
    });
    until(&mut rt, "the bell", |rt| printed(rt).contains("rang"));
    wait(&mut rt, 0.3);
    assert!(!needs_you(&rt));
    assert!(posted().is_empty(), "{:?}", posted());
}

#[test]
fn a_failure_notice_goes_once_you_look_at_it() {
    let mut rt = unwatched(AGENT, "sleep 0.5; exit 3");
    until(&mut rt, "the terminal to fail", stopped);
    assert!(needs_you(&rt), "a failure needs you");
    assert!(posted().iter().any(|(_, body, sound)| body == "Session failed (exit 3)" && *sound), "{:?}", posted());
    let id = rt.state.ws.live[0].id;
    rt.update(|s: &mut App, _: &mut Tx| s.window_focus(true));
    attention::REMOVED.with_borrow(|removed| assert!(removed.contains(&id), "looking takes it back"));
    assert!(!needs_you(&rt));
}

#[test]
fn claude_ringing_while_idle_isnt_news_once_youve_seen_it() {
    let mut rt = unwatched(AGENT, r"sleep 0.5; printf 'rang\a'; sleep 30");
    until(&mut rt, "the bell", |rt| printed(rt).contains("rang"));
    wait(&mut rt, 0.3);
    assert!(!needs_you(&rt));
    assert!(posted().is_empty(), "{:?}", posted());
}

#[test]
fn claude_asking_for_approval_is_news_even_once_youve_seen_it() {
    let mut rt = unwatched(AGENT, r"sleep 0.5; printf '\033]777;notify;Hyper:approval_requested;Approval requested\a'; sleep 30");
    until(&mut rt, "the request", needs_you);
}

#[test]
fn being_told_while_looking_doesnt_unsee_what_you_looked_at() {
    let root = fixture("always-looking");
    let mut rt = run_in(AGENT, "/", &["/bin/sh", "-c", r"sleep 0.5; printf 'done\033]777;notify;Hyper:turn_completed;Turn completed\a'; sleep 1.5; printf '\033]777;notify;Hyper:idle;Waiting for you\a'; sleep 30"]);
    rt.update(move |s: &mut App, _: &mut Tx| {
        s.settings_path = root.join("settings.json");
        let mut changed = s.settings.clone();
        changed.notifications.entry("claude".into()).or_default().when = notifications::When::Always;
        s.persist_settings(changed);
        s.window_focus(true);
    });
    until(&mut rt, "the turn", |rt| printed(rt).contains("done"));
    wait(&mut rt, 0.2);
    assert!(posted().iter().any(|(_, body, _)| body == "Turn completed"), "Always tells you while you look");
    rt.update(|s: &mut App, _: &mut Tx| s.window_focus(false));
    wait(&mut rt, 2.0);
    assert!(!needs_you(&rt), "the idle reminder is about something you've seen");
    let id = rt.state.ws.live[0].id;
    rt.update(|s: &mut App, _: &mut Tx| s.window_focus(true));
    attention::REMOVED.with_borrow(|removed| assert!(removed.contains(&id), "what was posted while you looked goes when you look again"));
}

#[test]
fn command_w_closes_the_session_on_screen_and_elsewhere_closes_the_window() {
    let mut rt = run_in(AGENT, "/", &["/bin/sleep", "30"]);
    rt.on_key = Some(on_key);
    let cmd_w = || KeyEvent { key: Key::Command('w'), mods: Mods { cmd: true, ..Default::default() } };
    rt.update(|s: &mut App, _: &mut Tx| s.set_view(View::Browse));
    rt.key(cmd_w());
    rt.frame(0.0);
    assert_eq!(rt.state.ws.live.len(), 1, "Browse has no session to close");
    assert!(rt.state.ws.confirming_quit, "so it closes the window, which asks first with an agent running");
    rt.update(|s: &mut App, _: &mut Tx| { s.ws.confirming_quit = false; s.set_view(View::Workspace); });
    let id = rt.state.ws.live[0].id;
    rt.update(move |s: &mut App, _: &mut Tx| s.show(workspace::Main::Terminal(id)));
    rt.key(cmd_w());
    rt.frame(0.0);
    assert!(rt.state.ws.live.is_empty(), "the session on screen closes");
}

#[test]
fn pi_saying_a_turn_ended_needs_you_even_if_it_was_never_seen_working() {
    let mut rt = unwatched(workspace::Tool::Agent(Source::Pi), r"sleep 0.5; printf ']777;notify;Pi;Done'; sleep 30");
    until(&mut rt, "the mark", needs_you);
}

#[test]
fn a_pi_terminal_follows_pi_to_each_session_it_says_its_in() {
    let mut rt = unwatched(workspace::Tool::Agent(Source::Pi), r"sleep 0.3; printf '\033]777;notify;Hyper:session;aaa\a'; sleep 0.5; printf '\033]777;notify;Hyper:session;bbb\a'; sleep 30");
    until(&mut rt, "the first session", |rt| rt.state.ws.live[0].session.as_deref() == Some("aaa"));
    until(&mut rt, "the session after /new", |rt| rt.state.ws.live[0].session.as_deref() == Some("bbb"));
    assert!(!needs_you(&rt), "saying its session isn't news");
    assert!(posted().is_empty(), "{:?}", posted());
}

#[test]
fn a_background_pi_terminal_redraws_when_it_starts_working() {
    let mut rt = run_in(workspace::Tool::Agent(Source::Pi), "/", &["/bin/sh", "-c",
        r"stty -echo; printf ready; read line; printf '\033]777;notify;Hyper:working;\a'; sleep 30"]);
    until(&mut rt, "the program waiting for input", |rt| printed(rt).contains("ready"));
    rt.update(|s, _| s.launch_for_test(workspace::Tool::Shell, "/", vec!["/bin/sleep".into(), "30".into()]));
    wait(&mut rt, 0.2);
    let spinning = |rt: &Runtime<App>| {
        let elements = rt.elements();
        let row = elements.iter().find(|e| e.key.as_deref() == Some("live-1")).unwrap();
        elements.iter().any(|e| e.path.starts_with(&row.path) && e.role == "progressbar")
    };
    assert!(!spinning(&rt));
    assert!(!needs_you(&rt));
    // Send through the PTY without an update that would itself commit the UI.
    rt.state.ws.live[0].term.text("go\r");
    until(&mut rt, "Pi reporting work from the background", |rt| rt.state.ws.live[0].working());
    assert!(spinning(&rt), "the background row must show work without an unrelated commit");
    assert!(!needs_you(&rt));
    assert!(posted().is_empty());
}

#[test]
fn pi_saying_it_waits_on_a_dialog_beats_its_file_and_is_news_once() {
    let say = |what: &str, body: &str| format!(r"printf '\033]777;notify;Hyper:{what};{body}\a'");
    let script = [
        "sleep 0.3".to_owned(),
        say("working", ""),
        "sleep 0.7".to_owned(),
        say("approval_requested", "Allow bash?"),
        "sleep 0.7".to_owned(),
        say("working", ""),
        "sleep 0.7".to_owned(),
        say("turn_completed", "Turn completed"),
        "sleep 30".to_owned(),
    ]
    .join("; ");
    let mut rt = unwatched(workspace::Tool::Agent(Source::Pi), &script);
    until(&mut rt, "it to say it's working", |rt| rt.state.ws.live[0].working());
    assert!(!needs_you(&rt));
    until(&mut rt, "the dialog", needs_you);
    assert!(!rt.state.ws.live[0].working(), "waiting on you isn't working");
    until(&mut rt, "the answer", |rt| rt.state.ws.live[0].working());
    assert!(!needs_you(&rt), "it's moved on, so what it asked has been dealt with");
    until(&mut rt, "the turn's end", needs_you);
    assert!(!rt.state.ws.live[0].working());
    let bodies: Vec<String> = posted().into_iter().map(|(_, body, _)| body).collect();
    assert_eq!(bodies, ["Allow bash?", "Turn completed"], "each told once, in its own words");
}

#[test]
fn an_agent_that_never_says_its_working_still_goes_by_its_file() {
    let mut rt = unwatched(AGENT, r"sleep 0.3; printf '\033]777;notify;Hyper:approval_requested;Approval requested\a'; sleep 30");
    until(&mut rt, "the request", needs_you);
    assert_eq!(rt.state.ws.live[0].said(), None, "asking isn't a claim to say when it works again");
}

#[test]
fn a_shell_cant_say_which_session_its_in() {
    let mut rt = unwatched(workspace::Tool::Shell, r"sleep 0.3; printf '\033]777;notify;Hyper:session;aaa\a'; sleep 30");
    until(&mut rt, "the notice", needs_you);
    assert_eq!(rt.state.ws.live[0].session, None);
}

#[test]
fn an_agent_whose_turn_ends_while_youre_elsewhere_needs_you() {
    let root = fixture("turn-ends");
    let mut child = sleeper();
    register(&root, child.id(), "aaa", "busy");
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    rt.update(|s: &mut App, _: &mut Tx| {
        s.view = View::Workspace;
        s.launch_for_test(AGENT, "/", vec!["/bin/sleep".to_owned(), "30".to_owned()]);
        s.ws.live[0].session = Some("aaa".to_owned());
        s.window_focus(false);
    });
    until(&mut rt, "it to work", |rt| rt.state.ws.live[0].working());
    assert!(!needs_you(&rt), "working isn't waiting");
    register(&root, child.id(), "aaa", "idle");
    until(&mut rt, "the mark", needs_you);
    child.kill().unwrap();
    child.wait().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_turn_that_ends_while_youre_looking_needs_nothing() {
    let root = fixture("turn-seen");
    let mut child = sleeper();
    register(&root, child.id(), "aaa", "busy");
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    rt.update(|s: &mut App, _: &mut Tx| {
        s.view = View::Workspace;
        s.launch_for_test(AGENT, "/", vec!["/bin/sleep".to_owned(), "30".to_owned()]);
        s.ws.live[0].session = Some("aaa".to_owned());
    });
    until(&mut rt, "it to work", |rt| rt.state.ws.live[0].working());
    register(&root, child.id(), "aaa", "idle");
    until(&mut rt, "it to stop", |rt| !rt.state.ws.live[0].working());
    assert!(!needs_you(&rt));
    child.kill().unwrap();
    child.wait().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn an_unreadable_registry_is_not_trusted() {
    let root = fixture("bad-registry");
    let folder = root.join("home/.claude/sessions");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("1.json"), "{\"process\": 1}").unwrap();
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    let session = rt.state.session("aaa").unwrap();
    assert_eq!(rt.state.activity_at(session, session.updated), Some(Activity::Working), "falls back to recent writes");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_running_session_opens_at_its_end_and_follows_it() {
    let root = fixture("follow");
    let mut child = sleeper();
    register(&root, child.id(), "aaa", "busy");
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    assert!(showing(&rt, "Reply number 399"), "opens at the end");
    assert!(!showing(&rt, "Fix the login page\n"), "not the start");
    append(&root.join("claude/-src-demo/aaa.jsonl"), &reply("late", "Followed to the end."));
    until(&mut rt, "the new reply", |rt| showing(rt, "Followed to the end."));
    child.kill().unwrap();
    child.wait().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn scrolling_up_stops_following_and_new_rows_leave_the_place_alone() {
    let root = fixture("scrolled-up");
    let mut child = sleeper();
    register(&root, child.id(), "aaa", "busy");
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    let key = "transcript-aaa";
    assert_eq!(rt.scroll_at_bottom(key), Some(true));
    let (x, y) = rt.find(key).unwrap();
    rt.wheel(x, y, 600.0);
    for _ in 0..30 {
        rt.frame(1.0 / 60.0);
    }
    assert_eq!(rt.scroll_at_bottom(key), Some(false));
    let on_screen = |id: &String| rt.presented(id).is_some_and(|(rect, _)| rect[1] > 100.0 && rect[1] + rect[3] < 700.0);
    let row = rows(&rt).into_iter().find(on_screen).expect("a row in view");
    let place = rt.presented(&row).unwrap().0;
    append(&root.join("claude/-src-demo/aaa.jsonl"), &reply("late", "Below the fold."));
    until(&mut rt, "the new reply", |rt| rows(rt).last().is_some_and(|r| r == "late:0"));
    assert_eq!(rt.scroll_at_bottom(key), Some(false));
    assert_eq!(rt.presented(&row).unwrap().0, place);
    child.kill().unwrap();
    child.wait().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_finished_session_opens_at_its_start_and_stays_there() {
    let root = fixture("no-follow");
    std::fs::create_dir_all(root.join("home/.claude/sessions")).unwrap();
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    append(&root.join("claude/-src-demo/aaa.jsonl"), &reply("late", "Off the bottom."));
    until(&mut rt, "the new reply", |rt| rows(rt).last().is_some_and(|r| r == "late:0"));
    assert!(showing(&rt, "Reply number 0"));
    assert!(!showing(&rt, "Off the bottom."));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_codex_session_survives_being_compressed() {
    let root = fixture("compress");
    let rollout = root.join("codex/sessions/2026/01/02/rollout-ccc.jsonl");
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    click(&mut rt, "source-filter/segment2");
    click(&mut rt, "ccc");
    until(&mut rt, "codex session", |rt| rt.state.reader.selected.as_deref() == Some("ccc") && loaded(rt));
    let answer = line(json!({"type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "Codex answers."}]}}));
    append(&rollout, &answer);
    until(&mut rt, "the answer", |rt| rows(rt).len() == 2);

    let compressed = rollout.with_extension("jsonl.zst");
    std::fs::write(&compressed, zstd::encode_all(std::fs::read(&rollout).unwrap().as_slice(), 3).unwrap()).unwrap();
    std::fs::remove_file(&rollout).unwrap();
    until(&mut rt, "the move", |rt| rt.state.session("ccc").is_some_and(|s| s.path == compressed));
    until(&mut rt, "it to settle", |rt| !rt.state.reader.tail_job.is_pending() && !rt.state.stale.job.is_pending());
    assert_eq!(rt.state.sessions().iter().filter(|s| s.id == "ccc").count(), 1);
    assert_eq!(rows(&rt).len(), 2);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn rewinding_a_pi_session_drops_the_abandoned_branch() {
    let root = fixture("pi-branch");
    let path = root.join("pi/--src-demo--/ppp.jsonl");
    let message = |id: &str, parent: &str, role: &str, text: &str| line(json!({"type": "message", "id": id, "parentId": parent, "message": {"role": role, "content": [{"type": "text", "text": text}]}}));
    append(&path, &message("a1", "m", "assistant", "First try"));
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    click(&mut rt, "source-filter/segment3");
    click(&mut rt, "ppp");
    until(&mut rt, "pi session", |rt| rt.state.reader.selected.as_deref() == Some("ppp") && rows(rt).len() == 2);
    append(&path, &message("a2", "m", "assistant", "Second try"));
    until(&mut rt, "the branch", |rt| rows(rt).last().is_some_and(|r| r == "a2:0"));
    assert_eq!(rows(&rt), ["m:0", "a2:0"]);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_batch_that_lands_just_as_another_starts_is_not_lost() {
    let root = fixture("lost-batch");
    let project = root.join("claude/-src-demo");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    std::fs::write(project.join("one.jsonl"), user("o0", "First new session")).unwrap();
    std::fs::write(project.join("two.jsonl"), user("t0", "Second new session")).unwrap();
    rt.state.stale.add([(Source::Claude, project.join("one.jsonl"))], false);
    rt.state.refresh();
    while rt.state.stale.job.is_pending() {
        std::thread::sleep(Duration::from_millis(1));
    }
    // Its answer is waiting to be taken when the next change comes in.
    rt.state.stale.add([(Source::Claude, project.join("two.jsonl"))], false);
    rt.state.refresh();
    until(&mut rt, "both", |rt| rt.state.session("two").is_some() && !rt.state.stale.job.is_pending());
    assert!(rt.state.session("one").is_some());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn deleting_the_open_session_moves_on_to_another() {
    let root = fixture("deleted");
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    click(&mut rt, "bbb");
    until(&mut rt, "bbb", |rt| rt.state.reader.selected.as_deref() == Some("bbb") && loaded(rt));
    std::fs::remove_file(root.join("claude/-src-demo/bbb.jsonl")).unwrap();
    until(&mut rt, "another session", |rt| rt.state.session("bbb").is_none() && rt.state.reader.selected.is_some() && loaded(rt));
    assert_ne!(rt.state.reader.selected.as_deref(), Some("bbb"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_session_that_failed_to_load_is_tried_again_by_a_reload() {
    use std::os::unix::fs::PermissionsExt;
    let root = fixture("failed");
    let path = root.join("claude/-src-demo/bbb.jsonl");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    click(&mut rt, "bbb");
    until(&mut rt, "the failure", |rt| matches!(rt.state.reader.transcript, Load::Failed(_)));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    rt.state.request_index();
    until(&mut rt, "the retry", |rt| rt.state.reader.selected.as_deref() == Some("bbb") && loaded(rt));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_registry_made_after_launch_is_noticed() {
    let root = fixture("late-registry");
    let mut child = sleeper();
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    assert!(!rt.state.running.as_ref().unwrap().trusted);
    register(&root, child.id(), "aaa", "idle");
    until(&mut rt, "the registry", |rt| activity(rt, "aaa") == Some(Activity::Open));
    child.kill().unwrap();
    child.wait().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_session_archived_to_a_new_folder_stays_listed() {
    let root = fixture("late-archive");
    let rollout = root.join("codex/sessions/2026/01/02/rollout-ccc.jsonl");
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    let archive = root.join("codex/archived_sessions");
    std::fs::create_dir_all(&archive).unwrap();
    std::fs::rename(&rollout, archive.join("rollout-ccc.jsonl")).unwrap();
    until(&mut rt, "the move", |rt| rt.state.session("ccc").is_some_and(|s| s.path.starts_with(&archive)));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_pid_that_went_to_another_process_does_not_revive_a_session() {
    let root = fixture("reused-pid");
    let mut child = sleeper();
    let entry = register(&root, child.id(), "aaa", "busy");
    let body = json!({"pid": child.id(), "sessionId": "aaa", "status": "busy", "procStart": "Thu Jan  1 00:00:00 2026"});
    std::fs::write(&entry, body.to_string()).unwrap();
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    assert_eq!(activity(&rt, "aaa"), None);
    child.kill().unwrap();
    child.wait().unwrap();
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_rewritten_file_shows_its_new_text_under_the_same_row_id() {
    let root = fixture("rewritten");
    let rollout = root.join("codex/sessions/2026/01/02/rollout-ccc.jsonl");
    let mut rt = watching(root.clone());
    until(&mut rt, "first transcript", loaded);
    click(&mut rt, "ccc");
    until(&mut rt, "codex", |rt| rt.state.reader.selected.as_deref() == Some("ccc") && loaded(rt) && showing(rt, "A codex prompt"));
    let meta = line(json!({"type": "session_meta", "payload": {"id": "ccc", "cwd": "/src/other", "source": "cli"}}));
    let prompt = line(json!({"type": "response_item", "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "Short"}]}}));
    std::fs::write(&rollout, meta + &prompt).unwrap();
    until(&mut rt, "the new text", |rt| rt.state.reader.row(0).is_some_and(|r| r.text == "Short"));
    // The sidebar's title changes too, so it's the old text going that shows the row changed.
    assert!(!showing(&rt, "A codex prompt"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn reloading_keeps_the_open_session_as_it_was() {
    let root = fixture("reload");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    rt.state.reader.expanded.insert("tool:0".to_owned());
    rt.state.request_index();
    while rt.state.index_job.is_pending() {
        std::thread::sleep(Duration::from_millis(1));
    }
    rt.frame(1.0 / 60.0);
    assert!(loaded(&rt), "no spinner in its place");
    assert!(rt.state.reader.expanded.contains("tool:0"));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_collapsed_project_stays_collapsed_across_filters_and_reloads() {
    let root = fixture("stay-closed");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    click(&mut rt, "project-demo");
    assert!(!rt.state.open_projects.contains("demo"));
    click(&mut rt, "source-filter/segment3");
    click(&mut rt, "source-filter/segment0");
    assert!(!rt.state.open_projects.contains("demo"), "a filter round trip keeps it closed");
    rt.state.request_index();
    until(&mut rt, "reload", |rt| !rt.state.index_job.is_pending() && loaded(rt));
    assert!(!rt.state.open_projects.contains("demo"), "and so does a reload");
    std::fs::remove_dir_all(root).unwrap();
}

/// A session with priced usage, a skill per name in `skills`, and an MCP server with two tools.
fn usage_fixture(name: &str, skills: usize) -> PathBuf {
    let root = fixture(name);
    std::fs::write(root.join("models-store.json"), json!({"anthropic": {"models": [
        {"id": "claude-x", "cost": {"input": 4, "output": 20, "cacheRead": 0.2, "cacheWrite": 5}}
    ]}}).to_string()).unwrap();
    let used = |uuid: &str, block: Value| line(json!({"type": "assistant", "uuid": uuid, "message": {"id": uuid, "model": "claude-x", "content": [block],
        "usage": {"input_tokens": 1_000_000, "output_tokens": 0, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}}}));
    let mut session = user("u0", "Spend some tokens");
    session += &used("m1", json!({"type": "tool_use", "id": "t1", "name": "mcp__web__open", "input": {}}));
    session += &used("m2", json!({"type": "tool_use", "id": "t2", "name": "mcp__web__click", "input": {}}));
    for i in 0..skills {
        session += &used(&format!("s{i}"), json!({"type": "tool_use", "id": format!("k{i}"), "name": "Skill", "input": {"skill": format!("skill-{i}")}}));
    }
    std::fs::write(root.join("claude/-src-demo/usd.jsonl"), session).unwrap();
    root
}

fn open_usage(rt: &mut Runtime<App>) {
    until(rt, "the index", |rt| rt.find("usd").is_some());
    click(rt, "usd");
    click(rt, "inspector-toggle");
    until(rt, "its report", |rt| rt.state.reader.selected.as_deref() == Some("usd") && rt.state.reader.report.is_some());
    for _ in 0..40 {
        rt.frame(1.0 / 60.0);
    }
}

#[test]
fn details_show_what_a_session_used_and_opens_a_server_to_its_tools() {
    let root = usage_fixture("usage-panel", 1);
    let mut rt = runtime(root.clone());
    open_usage(&mut rt);
    until(&mut rt, "prices", |rt| showing(rt, "$12.00"));
    assert!(showing(&rt, "3.0M") && showing(&rt, "Cache read") && showing(&rt, "skill-0") && showing(&rt, "web"));
    // The transcript's rows name the tools too.
    let tool = |rt: &Runtime<App>| rt.elements().into_iter().filter_map(|e| e.text).filter(|t| t == "click").count();
    let closed = tool(&rt);
    click(&mut rt, "mcp:web/row-toggle");
    assert_eq!(tool(&rt), closed + 1, "tools wait for their server to open");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_long_list_in_details_scrolls() {
    let root = usage_fixture("usage-scroll", 60);
    let mut rt = runtime(root.clone());
    open_usage(&mut rt);
    assert_eq!(rt.scroll_at_bottom("details"), Some(false));
    let (x, y) = rt.find("details").unwrap();
    rt.wheel(x, y, -8_000.0);
    for _ in 0..40 {
        rt.frame(1.0 / 60.0);
    }
    assert_eq!(rt.scroll_at_bottom("details"), Some(true));
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_subagent_writing_updates_the_open_sessions_cost() {
    let root = usage_fixture("usage-live", 0);
    let mut rt = runtime(root.clone());
    open_usage(&mut rt);
    until(&mut rt, "prices", |rt| showing(rt, "$8.00"));
    let folder = root.join("claude/-src-demo/usd/subagents");
    std::fs::create_dir_all(&folder).unwrap();
    let reply = line(json!({"type": "assistant", "uuid": "a1", "message": {"id": "a1", "model": "claude-x", "content": [],
        "usage": {"input_tokens": 500_000, "output_tokens": 0, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}}}));
    std::fs::write(folder.join("agent-a.jsonl"), reply).unwrap();
    rt.state.absorb(Changes { agents: HashSet::from(["usd".to_owned()]), ..Changes::default() });
    until(&mut rt, "the subagent's share", |rt| showing(rt, "$10.00") && showing(rt, "Subagents"));
    std::fs::remove_dir_all(root).unwrap();
}

/// Where the person is, as Back and Forward see it.
fn place(rt: &Runtime<App>) -> Option<history::Place> {
    rt.state.place()
}

fn settle(rt: &mut Runtime<App>) {
    for _ in 0..30 { rt.frame(1.0 / 60.0); }
}

#[test]
fn back_and_forward_retrace_sessions_and_settings_by_button_key_mouse_and_swipe() {
    use history::Place;
    let root = fixture("history");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    let first = place(&rt).expect("a session is open");
    assert_ne!(first, Place::Session("bbb".into()));
    assert!(!rt.state.can_go(history::Way::Back), "nowhere to go back to yet");
    click(&mut rt, "bbb");
    settle(&mut rt);
    click(&mut rt, "settings-row");
    settle(&mut rt);
    assert_eq!(place(&rt), Some(Place::Settings));

    // ⌘['s menu item from Settings, then the button.
    rt.update(|s, tx| s.dispatch(commands::Action::GoBack, tx));
    settle(&mut rt);
    assert_eq!((rt.state.view, place(&rt)), (View::Browse, Some(Place::Session("bbb".into()))));
    click(&mut rt, "history-back");
    settle(&mut rt);
    assert_eq!(place(&rt), Some(first.clone()));
    assert!(!rt.state.can_go(history::Way::Back), "back at the start");

    // The mouse's forward button, anywhere in the window.
    let (x, y) = rt.find("main-pane").unwrap();
    rt.button_down(x, y, rune::PointerButton::Forward);
    rt.button_up(x, y, rune::PointerButton::Forward);
    settle(&mut rt);
    assert_eq!(place(&rt), Some(Place::Session("bbb".into())));

    // A sideways swipe over the transcript goes forward; one that starts up and down scrolls.
    let swipe = |rt: &mut Runtime<App>, dx: f32, dy: f32| {
        let (x, y) = rt.find("main-pane").unwrap();
        let ev = |dx, dy, phase| rune::WheelEvent::new(dx, dy).at(x, y).precise(true).phase(phase);
        rt.wheel_event(ev(dx, dy, rune::GesturePhase::Began));
        rt.frame(1.0 / 60.0);
        for _ in 0..10 {
            rt.wheel_event(ev(dx, dy, rune::GesturePhase::Moved));
            rt.frame(1.0 / 60.0);
        }
        rt.wheel_event(ev(0.0, 0.0, rune::GesturePhase::Ended));
        settle(rt);
    };
    swipe(&mut rt, -2.0, -30.0);
    assert_eq!(place(&rt), Some(Place::Session("bbb".into())), "scrolling isn't a swipe");
    swipe(&mut rt, -3.0, 0.0);
    assert_eq!(place(&rt), Some(Place::Session("bbb".into())), "a swipe too short to go");
    swipe(&mut rt, -30.0, 0.0);
    assert_eq!(place(&rt), Some(Place::Settings), "fingers moving left go forward");
    swipe(&mut rt, 30.0, 2.0);
    assert_eq!(place(&rt), Some(Place::Session("bbb".into())), "and right go back");

    // Holding Back lists where it goes, nearest first; picking one goes straight there.
    let (x, y) = rt.find("history-back").unwrap();
    rt.pointer_down(x, y);
    rt.frame(1.0 / 60.0);
    std::thread::sleep(Duration::from_millis(450));
    rt.frame(1.0 / 60.0);
    let menu = rt.popup_menu().expect("held, Back lists where it goes").clone();
    assert!(menu.find("0").is_some() && menu.find("1").is_none(), "{menu:?}");
    rt.resolve_popup(Some("0")).unwrap();
    rt.pointer_up(x, y);
    settle(&mut rt);
    assert_eq!(place(&rt), Some(first.clone()), "letting go after the list doesn't go back again");

    // Going somewhere new drops what was ahead.
    click(&mut rt, "settings-row");
    settle(&mut rt);
    assert!(!rt.state.can_go(history::Way::Forward));
    drop(rt);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn back_passes_over_a_closed_terminal() {
    use history::Place;
    let app = workspace_app(vec![]);
    app.index_job.cancel();
    let mut rt = Runtime::new(app, view, (1240.0, 800.0), Env::new(true, 2.0));
    rt.frame(0.0);
    for _ in 0..3 {
        rt.update(|s, _| s.launch_for_test(workspace::Tool::Shell, "/", vec!["/bin/cat".into()]));
        rt.frame(1.0 / 60.0);
    }
    let ids: Vec<u64> = rt.state.ws.live.iter().map(|l| l.id).collect();
    assert_eq!(place(&rt), Some(Place::Terminal(ids[2])));
    rt.update(|s, _| s.close_live(ids[1]));
    rt.frame(1.0 / 60.0);
    click(&mut rt, "history-back");
    assert_eq!(place(&rt), Some(Place::Terminal(ids[0])), "the closed terminal is passed over");
    click(&mut rt, "history-back");
    assert_eq!(place(&rt), Some(Place::Launcher));
}

/// A trackpad gesture over `key`: `steps` moves of (dx, dy), then the fingers lift.
fn trackpad(rt: &mut Runtime<App>, key: &str, dx: f32, dy: f32, steps: usize, lift: bool) {
    let (x, y) = rt.find(key).unwrap();
    let ev = |dx, dy, phase| rune::WheelEvent::new(dx, dy).at(x, y).precise(true).phase(phase);
    rt.wheel_event(ev(dx, dy, rune::GesturePhase::Began));
    rt.frame(1.0 / 60.0);
    for _ in 0..steps {
        rt.wheel_event(ev(dx, dy, rune::GesturePhase::Moved));
        rt.frame(1.0 / 60.0);
    }
    if lift {
        rt.wheel_event(ev(0.0, 0.0, rune::GesturePhase::Ended));
        settle(rt);
    }
}

#[test]
fn a_mouse_wheel_still_scrolls_after_a_swipe() {
    let root = fixture("history-wheel");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    assert_eq!(place(&rt), Some(history::Place::Session("aaa".into())));
    click(&mut rt, "bbb");
    settle(&mut rt);
    click(&mut rt, "history-back");
    until(&mut rt, "aaa again", |rt| loaded(rt) && rt.state.reader.selected.as_deref() == Some("aaa"));
    // Too short to go forward.
    trackpad(&mut rt, "main-pane", -3.0, 0.0, 10, true);
    assert_eq!(place(&rt), Some(history::Place::Session("aaa".into())));
    let on_screen = |rt: &Runtime<App>, id: &String| rt.presented(id).is_some_and(|(rect, _)| rect[1] > 100.0 && rect[1] + rect[3] < 700.0);
    let row = rows(&rt).into_iter().find(|id| on_screen(&rt, id)).expect("a row in view");
    let before = rt.presented(&row).unwrap().0[1];
    let (x, y) = rt.find("transcript-aaa").unwrap();
    rt.wheel_event(rune::WheelEvent::new(0.0, -200.0).at(x, y));
    settle(&mut rt);
    let after = rt.presented(&row).map(|(rect, _)| rect[1]);
    assert!(after.is_none_or(|y| y < before - 50.0), "the wheel scrolled down: {before} -> {after:?}");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_swipe_whose_view_goes_away_leaves_no_arrow() {
    let root = fixture("history-unmount");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    click(&mut rt, "bbb");
    settle(&mut rt);
    click(&mut rt, "history-back");
    until(&mut rt, "aaa again", |rt| loaded(rt) && rt.state.reader.selected.as_deref() == Some("aaa"));
    trackpad(&mut rt, "transcript-aaa", -10.0, 0.0, 3, false);
    assert!(rt.find("swipe-arrow").is_some(), "the swipe is going");
    // The transcript under the fingers is replaced before they lift.
    rt.update(|s, _| s.select("ccc"));
    until(&mut rt, "ccc", |rt| rt.find("transcript-ccc").is_some());
    let (x, y) = rt.find("main-pane").unwrap();
    rt.wheel_event(rune::WheelEvent::new(0.0, 0.0).at(x, y).precise(true).phase(rune::GesturePhase::Ended));
    settle(&mut rt);
    assert!(rt.find("swipe-arrow").is_none(), "the arrow went with the fingers");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_place_picked_from_the_list_is_where_back_goes_even_if_another_closed_meanwhile() {
    use history::Place;
    let app = workspace_app(vec![]);
    app.index_job.cancel();
    let mut rt = Runtime::new(app, view, (1240.0, 800.0), Env::new(true, 2.0));
    rt.frame(0.0);
    for _ in 0..3 {
        rt.update(|s, _| s.launch_for_test(workspace::Tool::Shell, "/", vec!["/bin/cat".into()]));
        rt.frame(1.0 / 60.0);
    }
    let ids: Vec<u64> = rt.state.ws.live.iter().map(|l| l.id).collect();
    let (x, y) = rt.find("history-back").unwrap();
    rt.secondary_down(x, y);
    rt.secondary_up(x, y);
    rt.frame(1.0 / 60.0);
    let menu = rt.popup_menu().expect("the list").clone();
    // Terminal 1, terminal 0, the launcher: terminal 1 closes before the second is picked.
    assert!(menu.find("2").is_some(), "{menu:?}");
    rt.update(|s, _| s.close_live(ids[1]));
    rt.frame(1.0 / 60.0);
    rt.resolve_popup(Some("1")).unwrap();
    rt.frame(1.0 / 60.0);
    assert_eq!(place(&rt), Some(Place::Terminal(ids[0])));
}

#[test]
fn arrowing_through_the_list_is_one_step_back() {
    let root = fixture("history-arrows");
    let mut rt = runtime(root.clone());
    rt.on_key = Some(on_key);
    until(&mut rt, "first transcript", loaded);
    click(&mut rt, "settings-row");
    settle(&mut rt);
    rt.update(|s, tx| s.dispatch(commands::Action::GoBack, tx));
    settle(&mut rt);
    let start = rt.state.reader.selected.clone().unwrap();
    click(&mut rt, &start);
    let mut seen = vec![start.clone()];
    for _ in 0..3 {
        key(&mut rt, Key::Down, Mods::default());
        settle(&mut rt);
        seen.push(rt.state.reader.selected.clone().unwrap());
    }
    eprintln!("selected in turn: {seen:?}");
    assert_ne!(seen[0], seen[3], "the arrows moved the selection");
    let back = |rt: &Runtime<App>| rt.state.history_places(history::Way::Back);
    assert_eq!(back(&rt), [history::Place::Session(start)], "the rows passed on the way aren't steps");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_swipe_going_when_the_window_loses_focus_leaves_no_arrow() {
    let root = fixture("history-blur");
    let mut rt = runtime(root.clone());
    until(&mut rt, "first transcript", loaded);
    click(&mut rt, "bbb");
    settle(&mut rt);
    click(&mut rt, "history-back");
    until(&mut rt, "aaa again", |rt| loaded(rt) && rt.state.reader.selected.as_deref() == Some("aaa"));
    trackpad(&mut rt, "transcript-aaa", -10.0, 0.0, 3, false);
    assert!(rt.find("swipe-arrow").is_some(), "the swipe is going");
    rt.update(|s: &mut App, _: &mut Tx| s.window_focus(false));
    settle(&mut rt);
    assert!(rt.find("swipe-arrow").is_none(), "the arrow went with the window's focus");
    std::fs::remove_dir_all(root).unwrap();
}
