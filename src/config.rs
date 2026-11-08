//! Each tool's global setup: rules, settings, MCP servers, skills, plugins and hooks.
//!
//! Credential stores (`auth.json`, `mcp-auth.json`) are never opened, `~/.claude.json` is only
//! read for its MCP servers, and values that could be secrets are masked before anything is
//! shown. A structured file that doesn't parse isn't shown at all, since it couldn't be masked.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::commands::Action;
use crate::data::{Source, fence, one_line, str_field};
use crate::notifications::Event;
use crate::settings::Settings;
use crate::workspace::Tool;

pub const MASK: &str = "••••";
pub(crate) const SUBTITLE_CHARS: usize = 110;

/// Whose setup an item is: Hyper's own, or one tool's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Owner {
    Hyper,
    Tool(Source),
}

impl Owner {
    pub fn label(self) -> &'static str {
        match self {
            Owner::Hyper => "Hyper",
            Owner::Tool(source) => source.label(),
        }
    }
}

impl From<Source> for Owner {
    fn from(source: Source) -> Self {
        Owner::Tool(source)
    }
}

impl PartialEq<Source> for Owner {
    fn eq(&self, source: &Source) -> bool {
        *self == Owner::Tool(*source)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ConfigItem {
    pub id: String,
    pub source: Owner,
    pub group: &'static str,
    pub title: String,
    pub subtitle: String,
    pub path: Option<PathBuf>,
    pub markdown: String,
    /// Stands for an empty group, so "no MCP servers" reads as an answer.
    pub placeholder: bool,
}

pub(crate) fn item(source: impl Into<Owner>, group: &'static str, title: impl Into<String>, subtitle: impl Into<String>, path: Option<&Path>, markdown: String) -> ConfigItem {
    let (source, title) = (source.into(), title.into());
    ConfigItem {
        id: format!("{}/{group}/{title}", source.label()),
        source,
        group,
        subtitle: one_line(&subtitle.into(), SUBTITLE_CHARS),
        path: path.map(Path::to_owned),
        title,
        markdown,
        placeholder: false,
    }
}

pub(crate) fn none(source: Source, group: &'static str, path: &Path) -> ConfigItem {
    let mut empty = item(source, group, "None", "", Some(path), format!("No {} set up in `{}`.", group.to_lowercase(), path.display()));
    empty.placeholder = true;
    empty
}

fn secret_name(name: &str) -> bool {
    const WORDS: [&str; 14] = ["token", "secret", "password", "passwd", "api_key", "apikey", "api-key", "access_key", "accesskey", "authorization", "credential", "cookie", "private_key", "bearer"];
    let name = name.to_lowercase();
    WORDS.iter().any(|w| name.contains(w)) || matches!(name.as_str(), "pass" | "pwd")
}

/// `key` and `auth` usually hold a shortcut or a scheme (`"key": "cmd+k"`, `"auth": "bearer"`),
/// so only a long unbroken value under one of those names counts as a secret.
fn secret_under_weak_name(name: &str, value: &Value) -> bool {
    let weak = matches!(name.to_lowercase().as_str(), "key" | "auth");
    weak && value.as_str().is_some_and(|v| v.len() >= 12 && !v.contains(['+', ' ']))
}

/// Tables whose every value is a secret until shown otherwise.
fn opaque_table(name: &str) -> bool {
    matches!(name.to_lowercase().as_str(), "env" | "headers" | "http_headers" | "env_http_headers")
}

fn secret_shaped(word: &str) -> bool {
    const PREFIXES: [&str; 10] = ["sk-", "ghp_", "gho_", "ghs_", "github_pat_", "xoxb-", "xoxp-", "glpat-", "npm_", "AKIA"];
    PREFIXES.iter().any(|p| word.starts_with(p)) || (word.starts_with("eyJ") && word.contains('.'))
}

/// `https://user:pass@host/x?token=y` without its password and token.
fn scrub_url(word: &str) -> String {
    let Some((scheme, rest)) = word.split_once("://") else { return word.to_owned() };
    let (authority, tail) = rest.find(['/', '?']).map_or((rest, ""), |at| rest.split_at(at));
    let host = authority.rsplit_once('@').map_or(authority.to_owned(), |(_, host)| format!("{MASK}@{host}"));
    let tail = match tail.split_once('?') {
        None => tail.to_owned(),
        Some((path, query)) => {
            let scrubbed: Vec<String> = query
                .split('&')
                .map(|pair| match pair.split_once('=') {
                    Some((name, _)) if secret_name(name) => format!("{name}={MASK}"),
                    _ => pair.to_owned(),
                })
                .collect();
            format!("{path}?{}", scrubbed.join("&"))
        }
    };
    format!("{scheme}://{host}{tail}")
}

/// One word of a command or value. `after_flag` says the word before it was `--token`.
fn scrub_word(word: &str, after_flag: bool) -> String {
    if after_flag || secret_shaped(word) {
        return MASK.to_owned();
    }
    if word.contains("://") {
        return scrub_url(word);
    }
    match word.split_once('=') {
        Some((name, value)) if secret_name(name) && !value.is_empty() => format!("{name}={MASK}"),
        _ => word.to_owned(),
    }
}

fn is_secret_flag(word: &str) -> bool {
    word.starts_with('-') && !word.contains('=') && secret_name(word)
}

/// Masks anything secret-looking inside a string, keeping its words otherwise.
pub fn scrub(text: &str) -> String {
    let mut after_flag = false;
    let lines: Vec<String> = text
        .lines()
        .map(|line| {
            let words: Vec<String> = line
                .split(' ')
                .map(|word| {
                    if word.is_empty() {
                        return String::new();
                    }
                    let bare = word.trim_matches(['"', '\'']);
                    if bare == "Bearer" || (after_flag && matches!(bare, "Token" | "Basic")) {
                        after_flag = true;
                        return word.to_owned();
                    }
                    let out = scrub_word(word, after_flag);
                    // `--token abc`, or a header: `X-API-Key: abc`.
                    after_flag = is_secret_flag(bare) || bare.strip_suffix(':').is_some_and(secret_name);
                    out
                })
                .collect();
            words.join(" ")
        })
        .collect();
    lines.join("\n")
}

/// Masks a settings tree in place. `hide` covers everything under a secret's name.
pub fn mask(value: &mut Value, hide: bool) {
    match value {
        Value::String(_) | Value::Number(_) if hide => *value = Value::String(MASK.to_owned()),
        Value::String(s) => *s = scrub(s),
        Value::Array(items) => {
            let mut after_flag = false;
            for item in items {
                if let Value::String(s) = item {
                    let flag = is_secret_flag(s);
                    if after_flag {
                        *s = MASK.to_owned();
                    }
                    after_flag = flag;
                }
                mask(item, hide);
            }
        }
        Value::Object(map) => {
            for (name, value) in map.iter_mut() {
                // `max_output_tokens = 8000` is a limit, not a token.
                let secret = (secret_name(name) && !value.is_number()) || secret_under_weak_name(name, value);
                mask(value, hide || opaque_table(name) || secret);
            }
        }
        _ => {}
    }
}

pub(crate) fn masked_json(value: &Value) -> String {
    let mut value = value.clone();
    mask(&mut value, false);
    fence("json", &serde_json::to_string_pretty(&value).unwrap_or_default())
}

pub(crate) fn masked_toml(value: &Value) -> String {
    let mut value = value.clone();
    mask(&mut value, false);
    match toml::Value::try_from(&value).ok().and_then(|v| toml::to_string_pretty(&v).ok()) {
        Some(text) => fence("toml", &text),
        None => masked_json(&value),
    }
}

pub(crate) const UNREADABLE: &str = "This file couldn't be parsed, so it isn't shown: its secrets couldn't be masked.";

pub(crate) fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

pub(crate) fn read_toml(path: &Path) -> Option<Value> {
    toml::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// A settings file as an item, or nothing if it isn't there.
pub(crate) fn json_file(source: impl Into<Owner>, group: &'static str, path: &Path) -> Option<ConfigItem> {
    path.is_file().then(|| {
        let markdown = read_json(path).map_or_else(|| UNREADABLE.to_owned(), |v| masked_json(&v));
        item(source, group, file_name(path), "", Some(path), markdown)
    })
}

pub(crate) fn file_name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
}

/// A rules file, noting where a symlink points: several tools often share one.
pub(crate) fn rules_file(source: Source, path: &Path) -> Option<ConfigItem> {
    let markdown = std::fs::read_to_string(path).ok()?;
    let subtitle = std::fs::read_link(path).map(|target| format!("→ {}", target.display())).unwrap_or_default();
    Some(item(source, "Rules", file_name(path), subtitle, Some(path), markdown))
}

struct Frontmatter {
    name: Option<String>,
    description: String,
    body: String,
}

/// The `name` and `description` of a `---` block, and the text after it.
fn frontmatter(text: &str) -> Frontmatter {
    let mut out = Frontmatter { name: None, description: String::new(), body: text.to_owned() };
    let Some(rest) = text.strip_prefix("---\n") else { return out };
    let Some((head, body)) = rest.split_once("\n---") else { return out };
    // What follows the closing fence's own line.
    out.body = body.split_once('\n').map_or("", |(_, text)| text).trim_start_matches('\n').to_owned();
    let unquote = |v: &str| v.trim().trim_matches(['"', '\'']).to_owned();
    let mut lines = head.lines().peekable();
    while let Some(line) = lines.next() {
        if let Some(value) = line.strip_prefix("name:") {
            out.name = Some(unquote(value));
        } else if let Some(value) = line.strip_prefix("description:") {
            out.description = unquote(value);
            if matches!(value.trim(), ">" | "|" | ">-" | "|-") {
                let mut folded = Vec::new();
                while let Some(next) = lines.next_if(|l| l.starts_with(' ')) {
                    folded.push(next.trim());
                }
                out.description = folded.join(" ");
            }
        }
    }
    out
}

pub(crate) fn sorted_dir(dir: &Path) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    paths.retain(|p| !file_name(p).starts_with('.'));
    paths.sort();
    paths
}

fn document(source: Source, group: &'static str, path: &Path, fallback: String, note: &str) -> Option<ConfigItem> {
    let text = std::fs::read_to_string(path).ok()?;
    let parts = frontmatter(&text);
    let subtitle = if note.is_empty() { parts.description.clone() } else { format!("{note} · {}", parts.description) };
    let markdown = if parts.description.is_empty() { parts.body } else { format!("> {}\n\n{}", parts.description, parts.body) };
    Some(item(source, group, parts.name.filter(|n| !n.is_empty()).unwrap_or(fallback), subtitle, Some(path), markdown))
}

/// Every `<dir>/<skill>/SKILL.md`.
pub(crate) fn skills(source: Source, dir: &Path, note: &str) -> Vec<ConfigItem> {
    sorted_dir(dir).iter().filter_map(|skill| document(source, "Skills", &skill.join("SKILL.md"), file_name(skill), note)).collect()
}

/// Every `.md` file in `dir`.
pub(crate) fn documents(source: Source, group: &'static str, dir: &Path) -> Vec<ConfigItem> {
    sorted_dir(dir)
        .iter()
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .filter_map(|p| document(source, group, p, p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(), ""))
        .collect()
}

/// A skill can reach the list through two folders, as a symlink: keep the first.
pub(crate) fn without_repeats(items: Vec<ConfigItem>) -> Vec<ConfigItem> {
    let mut seen = HashSet::new();
    items.into_iter().filter(|i| seen.insert(i.id.clone())).collect()
}

fn mcp_server(source: Source, name: &str, server: &Value, path: &Path, toml: bool) -> ConfigItem {
    let mut shown = server.clone();
    mask(&mut shown, false);
    let target = [str_field(&shown, "url"), str_field(&shown, "command")].into_iter().find(|s| !s.is_empty()).unwrap_or("").to_owned();
    let subtitle = if server["enabled"] == false { format!("Disabled · {target}") } else { target };
    let markdown = if toml { masked_toml(server) } else { masked_json(server) };
    item(source, "MCP servers", name, subtitle, Some(path), markdown)
}

pub(crate) fn mcp_servers(source: Source, servers: &Value, path: &Path, toml: bool) -> Vec<ConfigItem> {
    let Some(map) = servers.as_object() else { return Vec::new() };
    map.iter().map(|(name, server)| mcp_server(source, name, server, path, toml)).collect()
}

/// The sections of Hyper's Settings, as its items are titled.
pub const SETTINGS_TABS: [&str; 4] = ["General", "Appearance", "Shortcuts", "Notifications"];

fn table(head: [&str; 2], rows: impl IntoIterator<Item = (String, String)>) -> String {
    let cell = |s: &str| s.replace('|', "\\|");
    let mut out = format!("| {} | {} |\n|---|---|\n", head[0], head[1]);
    for (name, value) in rows {
        out.push_str(&format!("| {} | {} |\n", cell(&name), cell(&value)));
    }
    out
}

fn on(value: bool) -> String {
    if value { "On" } else { "Off" }.to_owned()
}

/// Hyper's settings as they are in effect, defaults included, then the file they're saved in.
fn hyper(settings: &Settings, path: &Path) -> Vec<ConfigItem> {
    let source = Owner::Hyper;
    let general = table(["Setting", "Value"], [
        ("Open maximized".to_owned(), on(settings.open_maximized)),
        ("Remember last project".to_owned(), on(settings.remember_project)),
        ("Start in a new worktree".to_owned(), on(settings.default_worktree)),
        ("Default tool".to_owned(), Tool::from_id(&settings.default_tool).map_or(settings.default_tool.clone(), |t| t.label().to_owned())),
    ]);

    let a = &settings.appearance;
    let pt = |v: f32| format!("{v:.0} pt");
    let appearance = table(["Setting", "Value"], [
        ("Interface font".to_owned(), crate::fonts::name(&a.interface_font).to_owned()),
        ("Interface size".to_owned(), pt(a.interface_size)),
        ("Monospace font".to_owned(), crate::fonts::name(&a.monospace_font).to_owned()),
        ("Content size".to_owned(), pt(a.content_size)),
        ("Line spacing".to_owned(), format!("{:.2}", a.line_spacing)),
    ]);

    let shortcuts = Action::menus().into_iter().map(|menu| {
        let rows = Action::ALL.iter().filter(|a| a.menu() == menu).map(|a| {
            let keys = a.hint(settings);
            let keys = if keys.is_empty() { "None".to_owned() } else { keys };
            let changed = settings.shortcuts.get(a.id()).is_some_and(|saved| !crate::commands::is_default(a.id(), saved));
            (a.title().to_owned(), if changed { format!("{keys} (changed)") } else { keys })
        });
        format!("### {menu}\n\n{}", table(["Command", "Shortcut"], rows))
    });
    let shortcuts = shortcuts.collect::<Vec<_>>().join("\n");

    let notifications = Tool::ALL.iter().map(|&tool| {
        let policy = settings.notifications_for(tool);
        let mut rows = vec![("Notify when".to_owned(), policy.when.label().to_owned()), ("Dock badge".to_owned(), on(policy.dock_badge))];
        rows.extend(Event::ALL.into_iter().filter(|e| e.supported(tool)).map(|e| (e.label().to_owned(), policy.delivery(e).label().to_owned())));
        format!("### {}\n\n{}", tool.label(), table(["Setting", "Value"], rows))
    });
    let notifications = notifications.collect::<Vec<_>>().join("\n");

    let mut items: Vec<ConfigItem> = [general, appearance, shortcuts, notifications]
        .into_iter()
        .zip(SETTINGS_TABS)
        .map(|(markdown, title)| item(source, "Settings", title, "", Some(path), markdown))
        .collect();
    items.extend(json_file(source, "Settings", path));
    items
}

/// Hyper's settings and the file they're saved in, then everything for every tool that has a
/// folder under `home`.
pub fn load(home: &Path, settings: &Settings, settings_path: &Path) -> Vec<ConfigItem> {
    let mut items = hyper(settings, settings_path);
    for source in Source::ALL {
        items.extend(source.agent().config(home));
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SECRET: &str = "s3cr3t-value-123456";

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sessions-config-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(path: PathBuf, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn titles<'a>(items: &'a [ConfigItem], source: Source, group: &str) -> Vec<&'a str> {
        items.iter().filter(|i| i.source == source && i.group == group).map(|i| i.title.as_str()).collect()
    }

    #[test]
    fn secrets_are_masked_wherever_they_sit_and_harmless_values_stay() {
        let mut v = json!({
            "model": "gpt-x",
            "max_output_tokens": 8000,
            "api_key": SECRET,
            "env": {"ANYTHING": SECRET},
            "headers": {"X-Custom": SECRET},
            "args": ["--port", "80", "--token", SECRET, format!("--api-key={SECRET}")],
            "url": format!("https://me:{SECRET}@example.com/mcp?token={SECRET}&mode=fast"),
            "command": format!("MY_TOKEN={SECRET} run --password {SECRET} --verbose"),
            "note": "ghp_abcdefghijklmnop",
            "auth": format!("Bearer {SECRET}"),
            "nested": [{"client_secret": SECRET}],
        });
        mask(&mut v, false);
        let text = v.to_string();
        assert!(!text.contains(SECRET), "{text}");
        assert!(!text.contains("ghp_"), "{text}");
        assert_eq!(v["model"], "gpt-x");
        assert_eq!(v["args"][1], "80");
        assert_eq!(v["max_output_tokens"], 8000);
        assert!(v["url"].as_str().unwrap().ends_with("example.com/mcp?token=••••&mode=fast"), "{}", v["url"]);
        assert!(v["command"].as_str().unwrap().ends_with("--verbose"), "{}", v["command"]);
    }

    #[test]
    fn plain_names_and_header_style_values_are_masked() {
        let mut v = json!({
            "key": SECRET,
            "auth": SECRET,
            "pass": SECRET,
            "args": ["-H", format!("X-API-Key: {SECRET}")],
            "command": format!("curl -H 'Authorization: Token {SECRET}' -H 'x-token: {SECRET}' https://a.dev"),
            "bindings": [{"key": "cmd+shift+k"}],
            "server": {"auth": "bearer"},
        });
        mask(&mut v, false);
        let text = v.to_string();
        assert!(!text.contains(SECRET), "{text}");
        assert_eq!(v["bindings"][0]["key"], "cmd+shift+k");
        assert_eq!(v["server"]["auth"], "bearer");
    }

    #[test]
    fn frontmatter_leaves_a_list_at_the_start_of_the_body_alone() {
        let parts = frontmatter("---\nname: a\n---\n- first step\n- second");
        assert_eq!(parts.body, "- first step\n- second");
    }

    #[test]
    fn credential_files_are_never_shown_and_nothing_secret_leaks_through_config() {
        let home = temp("leak");
        write(home.join(".claude/CLAUDE.md"), "Be kind.");
        write(home.join(".claude/settings.json"), &json!({"env": {"KEY": SECRET}, "hooks": {"Stop": [{"command": format!("x --token {SECRET}")}]}}).to_string());
        write(home.join(".claude.json"), &json!({"oauthAccount": {"email": SECRET}, "mcpServers": {"db": {"command": "db", "env": {"PW": SECRET}}}}).to_string());
        write(home.join(".codex/auth.json"), &json!({"tokens": SECRET}).to_string());
        write(home.join(".codex/config.toml"), &format!("model = \"x\"\n[mcp_servers.api]\nurl = \"https://a.dev/mcp\"\nbearer_token = \"{SECRET}\"\n[mcp_servers.api.http_headers]\nX-Key = \"{SECRET}\"\n"));
        write(home.join(".pi/agent/auth.json"), &json!({"key": SECRET}).to_string());
        write(home.join(".pi/agent/mcp-auth.json"), &json!({"key": SECRET}).to_string());
        write(home.join(".pi/agent/settings.json"), &json!({"apiKey": SECRET, "packages": ["npm:a"]}).to_string());
        write(home.join(".pi/agent/mcp.json"), &json!({"mcpServers": {"s": {"url": format!("https://x.dev?api_key={SECRET}")}}}).to_string());
        let items = load(&home, &Settings::default(), &Settings::path(&home));
        std::fs::remove_dir_all(&home).unwrap();
        let dump = format!("{items:?}");
        assert!(!dump.contains(SECRET), "{dump}");
        assert!(!items.iter().any(|i| i.path.as_ref().is_some_and(|p| file_name(p).contains("auth"))));
        assert_eq!(titles(&items, Source::Claude, "MCP servers"), ["db"]);
        assert_eq!(titles(&items, Source::Codex, "MCP servers"), ["api"]);
        assert_eq!(titles(&items, Source::Pi, "MCP servers"), ["s"]);
        assert_eq!(titles(&items, Source::Pi, "Packages"), ["a"]);
    }

    #[test]
    fn a_settings_file_that_will_not_parse_is_not_shown_raw() {
        let home = temp("broken");
        write(home.join(".pi/agent/settings.json"), &format!("{{ \"apiKey\": \"{SECRET}\", oops"));
        write(home.join(".codex/config.toml"), &format!("token = \"{SECRET}\"\n[[[broken"));
        let items = load(&home, &Settings::default(), &Settings::path(&home));
        std::fs::remove_dir_all(&home).unwrap();
        assert!(!format!("{items:?}").contains(SECRET));
        assert!(items.iter().any(|i| i.markdown == UNREADABLE));
    }

    #[test]
    fn a_shortcut_is_changed_only_when_it_differs_from_the_default() {
        let mut settings = Settings::default();
        settings.shortcuts.insert("new_session".into(), "super+n".into());
        settings.shortcuts.insert("toggle_sidebar".into(), "alt+super+b".into());
        let items = hyper(&settings, Path::new("/nowhere/settings.json"));
        let shortcuts = &items.iter().find(|i| i.title == "Shortcuts").unwrap().markdown;
        assert!(shortcuts.contains("| New Session | ⌘N |"), "saved as the default: {shortcuts}");
        assert!(shortcuts.contains("| Toggle Sidebar | ⌥⌘B (changed) |"), "{shortcuts}");
    }

    #[test]
    fn skills_read_their_frontmatter_shared_ones_reach_both_tools_and_disabled_ones_say_so() {
        let home = temp("skills");
        write(home.join(".agents/skills/shared/SKILL.md"), "---\nname: shared-skill\ndescription: >\n  Folded over\n  two lines.\n---\n\n# Body");
        write(home.join(".agents/skills/.trash/SKILL.md"), "---\nname: gone\n---\n");
        write(home.join(".codex/config.toml"), "[[skills.config]]\nname = \"shared-skill\"\nenabled = false\n");
        write(home.join(".pi/agent/settings.json"), "{}");
        let items = load(&home, &Settings::default(), &Settings::path(&home));
        std::fs::remove_dir_all(&home).unwrap();
        assert_eq!(titles(&items, Source::Codex, "Skills"), ["shared-skill"]);
        assert_eq!(titles(&items, Source::Pi, "Skills"), ["shared-skill"]);
        let find = |source| items.iter().find(|i| i.source == source && i.group == "Skills").unwrap();
        assert_eq!(find(Source::Pi).subtitle, "Shared in ~/.agents · Folded over two lines.");
        assert!(find(Source::Codex).subtitle.starts_with("Disabled · "));
        assert!(find(Source::Pi).markdown.ends_with("# Body"));
    }

    #[test]
    fn no_mcp_servers_is_shown_as_an_answer_and_a_shared_rules_file_says_where_it_points() {
        let home = temp("empty");
        write(home.join(".claude/CLAUDE.md"), "Rules");
        write(home.join(".pi/agent/mcp.json"), "{\"mcpServers\":{}}");
        std::os::unix::fs::symlink(home.join(".claude/CLAUDE.md"), home.join(".pi/agent/AGENTS.md")).unwrap();
        let items = load(&home, &Settings::default(), &Settings::path(&home));
        std::fs::remove_dir_all(&home).unwrap();
        let empty = items.iter().find(|i| i.source == Source::Pi && i.group == "MCP servers").unwrap();
        assert!(empty.placeholder);
        let rules = items.iter().find(|i| i.source == Source::Pi && i.group == "Rules").unwrap();
        assert!(rules.subtitle.contains("CLAUDE.md"), "{}", rules.subtitle);
        assert_eq!(rules.markdown, "Rules");
    }

    #[test]
    fn a_claude_plugin_lists_its_skills_and_an_off_plugin_adds_none() {
        let home = temp("plugins");
        let on = home.join("cache/on");
        let off = home.join("cache/off");
        write(on.join("skills/draw/SKILL.md"), "---\nname: draw\ndescription: Draws.\n---\nBody");
        write(off.join("skills/hidden/SKILL.md"), "---\nname: hidden\n---\nBody");
        write(home.join(".claude/settings.json"), &json!({"enabledPlugins": {"on@m": true, "off@m": false}}).to_string());
        let installed = json!({"plugins": {"on@m": [{"installPath": on, "version": "1.2"}], "off@m": [{"installPath": off}]}});
        write(home.join(".claude/plugins/installed_plugins.json"), &installed.to_string());
        let items = load(&home, &Settings::default(), &Settings::path(&home));
        std::fs::remove_dir_all(&home).unwrap();
        assert_eq!(titles(&items, Source::Claude, "Skills"), ["draw"]);
        let plugin = items.iter().find(|i| i.title == "on@m").unwrap();
        assert!(plugin.markdown.contains("Version 1.2") && plugin.markdown.contains("- draw"), "{}", plugin.markdown);
    }
}
