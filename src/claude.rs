//! Claude Code: `~/.claude/projects/<project>/<session>.jsonl`.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::agent::{Agent, Found, Titles};
use crate::config::{ConfigItem, documents, item, json_file, masked_json, mcp_servers, none, read_json, rules_file, skills, sorted_dir, without_repeats};
use crate::logos::{self, Layer};
use crate::notifications::Event;
use crate::usage::{self, Request, Tokens, Usage};
use crate::data::{Kind, Session, Source, TITLE_CHARS, Transcript, between, contains, describe_tool, one_line, parse_time, project_name, result_text, str_field};

pub struct Claude;

impl Agent for Claude {
    fn label(&self) -> &'static str {
        "Claude"
    }

    fn id(&self) -> &'static str {
        "claude"
    }

    fn logo(&self) -> &'static [Layer] {
        &logos::CLAUDE_LAYERS
    }

    fn sessions(&self, home: &Path) -> PathBuf {
        home.join(".claude/projects")
    }

    /// Subagent transcripts sit a level deeper than sessions.
    fn classify(&self, rest: &Path) -> Option<Found> {
        if rest.extension().is_none_or(|x| x != "jsonl") {
            return None;
        }
        match rest.iter().collect::<Vec<_>>().as_slice() {
            [_, _] => Some(Found::Session),
            [_, session, folder, _, ..] if *folder == "subagents" => Some(Found::Subagent(session.to_string_lossy().into_owned())),
            _ => None,
        }
    }

    fn keeps_open(&self) -> bool {
        false
    }

    fn files(&self, root: &Path) -> Vec<PathBuf> {
        files(root)
    }

    fn read_session(&self, path: &Path, _: &Titles) -> Option<Session> {
        read_session(path)
    }

    fn load(&self, path: &Path) -> Result<Transcript, String> {
        load(path)
    }

    fn wanted(&self, line: &[u8]) -> bool {
        wanted(line)
    }

    fn add_record(&self, out: &mut Transcript, record: &Value, _: usize) {
        add_record(out, record);
    }

    fn searchable(&self, record: &Value, _: usize) -> Option<(String, Vec<String>)> {
        searchable(record)
    }

    fn providers(&self) -> &'static [&'static str] {
        &["anthropic"]
    }

    fn subagent_files(&self, session: &Session, _: &HashSet<PathBuf>) -> Option<(Vec<PathBuf>, Vec<PathBuf>)> {
        Some((agent_files(&session.path), Vec::new()))
    }

    fn add_usage(&self, usage: &mut Usage, record: &Value) {
        add_usage(usage, record);
    }

    /// It's the only one that can be told its session's id.
    fn start(&self, id: &str) -> (Vec<String>, Option<String>) {
        (["claude", "--session-id", id].map(str::to_owned).to_vec(), Some(id.to_owned()))
    }

    fn resume(&self, id: &str) -> Vec<String> {
        ["claude", "--resume", id].map(str::to_owned).to_vec()
    }

    fn hyper_args(&self, prompt: &str) -> Vec<String> {
        vec!["--settings".into(), hooks(), "--append-system-prompt".into(), prompt.to_owned()]
    }

    fn reports(&self, event: Event) -> bool {
        matches!(event, Event::TurnCompleted | Event::ApprovalRequested | Event::InputRequested | Event::Idle)
    }

    /// It reminds that it's idle, and says its turn is over, after it's been seen.
    fn repeats_itself(&self) -> bool {
        true
    }

    fn registered(&self) -> bool {
        true
    }

    fn config(&self, home: &Path) -> Vec<ConfigItem> {
        if home.join(".claude").is_dir() { config(home) } else { Vec::new() }
    }
}

/// Additive CLI settings, never written to the user's Claude settings. Hooks return only
/// a terminalSequence: no permission decisions, injected context, or transcript content.
fn hooks() -> String {
    let hook = |event: Event, body: &str| {
        let output = json!({ "terminalSequence": format!("\u{1b}]777;notify;Hyper:{};{body}\u{7}", event.id()) }).to_string();
        json!({"type": "command", "command": format!("/usr/bin/printf '%s' '{output}'"), "timeout": 5})
    };
    json!({"hooks": {
        "Stop": [{"hooks": [hook(Event::TurnCompleted, "Turn completed")]}],
        "PermissionRequest": [{"hooks": [hook(Event::ApprovalRequested, "Approval requested")]}],
        "PreToolUse": [{"matcher": "AskUserQuestion", "hooks": [hook(Event::InputRequested, "Input requested")]}],
        "Notification": [
            {"matcher": "idle_prompt", "hooks": [hook(Event::Idle, "Waiting for you")]},
            {"matcher": "elicitation_dialog|elicitation_url_dialog|agent_needs_input", "hooks": [hook(Event::InputRequested, "Input requested")]}
        ]
    }}).to_string()
}

const HEAD: usize = 256 * 1024;
const HEAD_MAX: usize = 8 * 1024 * 1024;
const TAIL: u64 = 512 * 1024;

/// Every top-level session. Subagent transcripts sit in folders under a session.
pub fn files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(projects) = std::fs::read_dir(root) else { return files };
    for project in projects.flatten() {
        let Ok(entries) = std::fs::read_dir(project.path()) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|x| x == "jsonl") && path.is_file() {
                files.push(path);
            }
        }
    }
    files
}

/// One file's listing, or `None` when it holds no prompt of the user's.
pub fn read_session(path: &Path) -> Option<Session> {
    let meta = std::fs::metadata(path).ok()?;
    let mut file = File::open(path).ok()?;
    let mut want = HEAD;
    let found = loop {
        let mut bytes = Vec::with_capacity(want);
        (&mut file).take(want as u64).read_to_end(&mut bytes).ok()?;
        file.seek(SeekFrom::Start(0)).ok()?;
        let whole = (bytes.len() as u64) >= meta.len();
        let found = scan_head(&bytes, whole);
        if found.title.is_some() || whole || want >= HEAD_MAX {
            break found;
        }
        want *= 4;
    };
    let prompt = found.title.or(found.command.filter(|_| found.replied))?;
    let title = tail_title(&mut file, meta.len()).or(found.custom).unwrap_or(prompt);
    let id = path.file_stem()?.to_string_lossy().into_owned();
    let folder = path.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let project = if found.cwd.is_empty() { folder } else { project_name(&found.cwd) };
    let updated = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
    Some(Session { id, source: Source::Claude, path: path.to_owned(), project, cwd: found.cwd, title, branch: found.branch, updated, size: meta.len(), turn: None })
}

#[derive(Default)]
pub(crate) struct Head {
    pub(crate) title: Option<String>,
    /// A bare `/clear` or `/effort`, for a session with nothing better to call it.
    command: Option<String>,
    replied: bool,
    custom: Option<String>,
    cwd: String,
    branch: String,
}

pub(crate) fn scan_head(bytes: &[u8], whole: bool) -> Head {
    let mut head = Head::default();
    let mut lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    if !whole {
        lines.pop(); // cut off mid-record
    }
    for line in lines {
        head.replied |= contains(line, b"\"assistant\"");
        if head.custom.is_none() && contains(line, b"custom-title") {
            head.custom = custom_title(line);
        }
        if head.title.is_some() || !contains(line, b"\"user\"") {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Value>(line) else { continue };
        if !is_main(&record, "user") {
            continue;
        }
        let Some(prompt) = prompt_text(&record) else { continue };
        if head.cwd.is_empty() {
            head.cwd = str_field(&record, "cwd").to_owned();
            head.branch = str_field(&record, "gitBranch").to_owned();
        }
        if prompt.starts_with('/') && !prompt.contains(char::is_whitespace) {
            head.command.get_or_insert(prompt);
        } else {
            head.title = Some(one_line(&prompt, TITLE_CHARS));
        }
    }
    head
}

/// The last `custom-title` near the end of the file: renames append, so it's the current one.
fn tail_title(file: &mut File, len: u64) -> Option<String> {
    let start = len.saturating_sub(TAIL);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes).ok()?;
    bytes.split(|b| *b == b'\n').rev().filter(|l| contains(l, b"custom-title")).find_map(custom_title)
}

fn custom_title(line: &[u8]) -> Option<String> {
    let record: Value = serde_json::from_slice(line).ok()?;
    if record["type"] != "custom-title" {
        return None;
    }
    let title = record["customTitle"].as_str()?.trim();
    (!title.is_empty()).then(|| one_line(title, TITLE_CHARS))
}

/// A record of this type in the main conversation, one the user would have seen.
fn is_main(record: &Value, kind: &str) -> bool {
    record["type"] == kind
        && record["isSidechain"] != true
        && record["isMeta"] != true
        && record["isVisibleInTranscriptOnly"] != true
}

/// What the user typed in a `user` record, without the harness's wrappers.
fn prompt_text(record: &Value) -> Option<String> {
    if record["isCompactSummary"] == true {
        return None;
    }
    match &record["message"]["content"] {
        Value::String(s) => clean_prompt(s),
        Value::Array(blocks) => {
            let parts: Vec<String> = blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str().and_then(clean_prompt))
                .collect();
            (!parts.is_empty()).then(|| parts.join("\n\n"))
        }
        _ => None,
    }
}

/// Removes `<system-reminder>` blocks, turns a slash command back into what was typed, and
/// drops text the harness wrote.
pub fn clean_prompt(raw: &str) -> Option<String> {
    let mut text = raw.to_owned();
    while let Some(start) = text.find("<system-reminder>") {
        let end = text[start..].find("</system-reminder>").map_or(text.len(), |e| start + e + "</system-reminder>".len());
        text.replace_range(start..end, "");
    }
    if let Some(name) = between(&text, "<command-name>", "</command-name>") {
        let args = between(&text, "<command-args>", "</command-args>").unwrap_or("").trim();
        let name = name.trim();
        return Some(if args.is_empty() { name.to_owned() } else { format!("{name} {args}") });
    }
    let text = text.trim();
    const HARNESS: [&str; 8] = [
        "<local-command-",
        "<command-message>",
        "<task-notification>",
        "<bash-",
        "<ci-monitor-event>",
        "<side-sessions-event>",
        "[Request interrupted",
        "Caveat: The messages below",
    ];
    if text.is_empty() || HARNESS.iter().any(|p| text.starts_with(p)) {
        return None;
    }
    Some(text.to_owned())
}


/// Most of a transcript's bytes are request logs and file history.
pub fn wanted(line: &[u8]) -> bool {
    contains(line, b"\"uuid\"")
}

pub fn load(path: &Path) -> Result<Transcript, String> {
    crate::data::load_records(path, &Claude)
}

pub fn add_record(out: &mut Transcript, record: &Value) {
    let kind = str_field(record, "type");
    if (kind != "user" && kind != "assistant") || record["isSidechain"] == true {
        return;
    }
    let uuid = str_field(record, "uuid");
    if !out.seen.insert(uuid.to_owned()) {
        return;
    }
    if out.started.is_none() {
        out.started = parse_time(str_field(record, "timestamp"));
    }
    add_usage(&mut out.usage, record);
    if kind == "user" {
        add_results(out, record);
        if !is_main(record, "user") {
            return;
        }
        if record["isCompactSummary"] == true {
            out.push(format!("{uuid}:0"), Kind::Compacted, message_text(record));
        } else if let Some(text) = prompt_text(record) {
            out.push(format!("{uuid}:0"), Kind::User, text);
        }
        return;
    }
    let message = &record["message"];
    let model = str_field(message, "model");
    if !model.is_empty() && !model.starts_with('<') {
        out.model = model.to_owned();
    }
    let Some(blocks) = message["content"].as_array() else { return };
    for (i, block) in blocks.iter().enumerate() {
        let id = format!("{uuid}:{i}");
        match str_field(block, "type") {
            "text" => {
                let text = str_field(block, "text").trim();
                if !text.is_empty() {
                    out.push(id, Kind::Assistant, text.to_owned());
                }
            }
            "thinking" => {
                let text = str_field(block, "thinking").trim();
                if !text.is_empty() {
                    out.push(id, Kind::Thinking, text.to_owned());
                }
            }
            "tool_use" => {
                out.call(str_field(block, "id"), id, describe_tool(str_field(block, "name"), &block["input"]));
            }
            _ => {}
        }
    }
}

/// A record's request and calls. A reply takes a record per block, each with the same usage.
pub fn add_usage(usage: &mut Usage, record: &Value) {
    let message = &record["message"];
    match str_field(record, "type") {
        "user" => {
            // A skill the user typed, which has no call. One the agent called says which.
            if record["isMeta"] == true && record["sourceToolUseID"].is_null() {
                let text = message["content"][0]["text"].as_str().or(message["content"].as_str()).unwrap_or("");
                let folder = text.strip_prefix("Base directory for this skill:").and_then(|t| t.lines().next());
                if let Some(name) = folder.and_then(usage::folder_name) {
                    usage.skill(name);
                }
            }
        }
        "assistant" => {
            let model = str_field(message, "model");
            let tokens = &message["usage"];
            if tokens.is_object() && !model.is_empty() && !model.starts_with('<') {
                usage.request(str_field(message, "id"), request(model, tokens));
            }
            for block in message["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "tool_use") {
                let name = str_field(block, "name");
                if name == "Skill" {
                    usage.skill(str_field(&block["input"], "skill"));
                } else {
                    usage.mcp_tool(str_field(block, "id"), name);
                }
            }
        }
        _ => {}
    }
}

fn request(model: &str, tokens: &Value) -> Request {
    let count = |key: &str| tokens[key].as_u64().unwrap_or(0);
    Request {
        model: model.to_owned(),
        tokens: Tokens {
            input: count("input_tokens"),
            output: count("output_tokens"),
            cache_read: count("cache_read_input_tokens"),
            cache_write: count("cache_creation_input_tokens"),
        },
        cache_write_1h: tokens["cache_creation"]["ephemeral_1h_input_tokens"].as_u64().unwrap_or(0),
        fast: tokens["speed"] == "fast",
        stored: None,
    }
}

/// A session's subagent transcripts, in `<session>/subagents` and its workflows' folders.
pub fn agent_files(session: &Path) -> Vec<PathBuf> {
    fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
        for path in std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()) {
            if path.is_dir() {
                collect(&path, files);
            } else if path.extension().is_some_and(|x| x == "jsonl") {
                files.push(path);
            }
        }
    }
    let mut files = Vec::new();
    collect(&session.with_extension("").join("subagents"), &mut files);
    files
}

fn message_text(record: &Value) -> String {
    match &record["message"]["content"] {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n\n"),
        _ => String::new(),
    }
}

/// Files each `tool_result` under the call it answers.
fn add_results(out: &mut Transcript, record: &Value) {
    let Some(blocks) = record["message"]["content"].as_array() else { return };
    for block in blocks.iter().filter(|b| b["type"] == "tool_result") {
        out.result(str_field(block, "tool_use_id"), result_text(&block["content"]), block["is_error"] == true);
    }
}


/// A running agent, from the file each interactive Claude Code process keeps in
/// `~/.claude/sessions`.
pub struct Process {
    pub pid: i32,
    pub session: String,
    pub busy: bool,
    /// When the process started, in seconds since the epoch.
    pub started: Option<i64>,
}

/// Every agent in the registry, and whether it can be relied on. It's Claude Code's own
/// undocumented format, so entries that all fail to read mean it has changed. One bad entry
/// among good ones is only that.
pub fn agents(dir: &Path) -> (Vec<Process>, bool) {
    let Ok(entries) = std::fs::read_dir(dir) else { return (Vec::new(), false) };
    let mut agents = Vec::new();
    let mut unreadable = false;
    for path in entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")) {
        // It may have gone since the listing, which is an agent quitting, not a bad entry.
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let agent = serde_json::from_slice::<Value>(&bytes).ok().and_then(|entry| {
            let pid = i32::try_from(entry["pid"].as_i64()?).ok()?;
            let session = entry["sessionId"].as_str()?.to_owned();
            let started = entry["procStart"].as_str().and_then(|t| chrono::NaiveDateTime::parse_from_str(t, "%a %b %e %H:%M:%S %Y").ok());
            Some(Process { pid, session, busy: entry["status"] == "busy", started: started.map(|t| t.and_utc().timestamp()) })
        });
        unreadable |= agent.is_none();
        agents.extend(agent);
    }
    let trusted = !unreadable || !agents.is_empty();
    (agents, trusted)
}

/// A record's id and the text a reader sees in it: prompts and replies, not tool output.
pub fn searchable(record: &Value) -> Option<(String, Vec<String>)> {
    let texts = if is_main(record, "user") {
        prompt_text(record).into_iter().collect()
    } else if is_main(record, "assistant") {
        let blocks = record["message"]["content"].as_array()?;
        blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str().map(str::to_owned)).collect()
    } else {
        return None;
    };
    Some((str_field(record, "uuid").to_owned(), texts))
}

fn config(home: &Path) -> Vec<ConfigItem> {
    let source = Source::Claude;
    let root = home.join(".claude");
    let settings_path = root.join("settings.json");
    let settings = read_json(&settings_path).unwrap_or(Value::Null);
    let mut items = Vec::new();

    items.extend(rules_file(source, &root.join("CLAUDE.md")));
    for path in sorted_dir(&root.join("rules")) {
        items.extend(rules_file(source, &path));
    }

    items.extend(json_file(source, "Settings", &settings_path));
    items.extend(json_file(source, "Settings", &root.join("settings.local.json")));
    items.extend(json_file(source, "Settings", &root.join("keybindings.json")));

    // Only these two keys are read: the rest of this file is account state.
    let state_path = home.join(".claude.json");
    let state = read_json(&state_path).unwrap_or(Value::Null);
    let mut servers = mcp_servers(source, &state["mcpServers"], &state_path, false);
    for name in state["claudeAiMcpEverConnected"].as_array().map(Vec::as_slice).unwrap_or_default().iter().filter_map(Value::as_str) {
        let title = name.strip_prefix("claude.ai ").unwrap_or(name);
        let markdown = format!("**{title}** has connected through claude.ai at least once. Its settings are in your claude.ai account, not on this Mac, so whether it's still on can't be read from here.");
        servers.push(item(source, "MCP servers", title, "claude.ai connector", None, markdown));
    }
    if servers.is_empty() {
        servers.push(none(source, "MCP servers", &state_path));
    }
    items.extend(servers);

    let mut found = skills(source, &root.join("skills"), "");
    let installed_path = root.join("plugins/installed_plugins.json");
    let installed = read_json(&installed_path).unwrap_or(Value::Null);
    let mut plugins = Vec::new();
    if let Some(enabled) = settings["enabledPlugins"].as_object() {
        for (name, on) in enabled {
            let install = &installed["plugins"][name][0];
            let folder = PathBuf::from(str_field(install, "installPath"));
            let version = str_field(install, "version");
            let plugin = name.split('@').next().unwrap_or(name);
            let provided = skills(source, &folder.join("skills"), plugin);
            let mut lines = vec![format!("**{}**", if *on == true { "Enabled" } else { "Disabled" })];
            if !version.is_empty() {
                lines.push(format!("Version {version}"));
            }
            lines.extend(provided.iter().map(|s| format!("- {}", s.title)));
            if *on == true {
                found.extend(provided);
            }
            let path = (!folder.as_os_str().is_empty()).then_some(folder.as_path());
            plugins.push(item(source, "Plugins", name.as_str(), if *on == true { "Enabled" } else { "Disabled" }, path, lines.join("\n\n")));
        }
    }
    items.extend(without_repeats(found));
    items.extend(plugins);

    if settings["hooks"].is_object() {
        items.push(item(source, "Hooks", "settings.json", "", Some(&settings_path), masked_json(&settings["hooks"])));
    }
    items.extend(documents(source, "Output styles", &root.join("output-styles")));
    items.extend(documents(source, "Agents", &root.join("agents")));
    items.extend(documents(source, "Commands", &root.join("commands")));
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{self, Roots};
    use crate::usage::{Prices, Report};
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sessions-claude-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn parse(name: &str, records: &[Value]) -> Transcript {
        let dir = temp(name);
        let path = dir.join("t.jsonl");
        std::fs::write(&path, records.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
        let t = load(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        t
    }

    fn user(uuid: &str, content: Value) -> Value {
        json!({"type": "user", "uuid": uuid, "timestamp": "2026-01-02T03:04:05.000Z", "message": {"content": content}})
    }

    fn assistant(uuid: &str, blocks: Value) -> Value {
        json!({"type": "assistant", "uuid": uuid, "message": {"model": "claude-x", "content": blocks}})
    }

    #[test]
    fn the_registry_says_when_a_process_started() {
        let dir = temp("started");
        std::fs::write(dir.join("1.json"), json!({"pid": 1, "sessionId": "a", "procStart": "Sat Nov  7 03:28:02 2026"}).to_string()).unwrap();
        assert_eq!(agents(&dir).0[0].started, Some(1_794_022_082));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_registry_names_running_agents_and_owns_up_to_what_it_cant_read() {
        let dir = temp("registry");
        std::fs::write(dir.join("1.json"), json!({"pid": 1, "sessionId": "a", "status": "busy", "kind": "interactive"}).to_string()).unwrap();
        std::fs::write(dir.join("2.json"), json!({"pid": 2, "sessionId": "b", "status": "idle"}).to_string()).unwrap();
        std::fs::write(dir.join("2.abc.key"), "not json").unwrap();
        let (mut found, readable) = agents(&dir);
        found.sort_by_key(|a| a.pid);
        assert!(readable);
        assert_eq!(found.iter().map(|a| (a.pid, a.session.as_str(), a.busy)).collect::<Vec<_>>(), [(1, "a", true), (2, "b", false)]);
        assert_eq!(found[0].started, None);
        std::fs::write(dir.join("3.json"), "{\"pid\":3,\"sess").unwrap();
        let (found, readable) = agents(&dir);
        assert_eq!((found.len(), readable), (2, true), "one bad entry doesn't spoil the rest");
        std::fs::remove_file(dir.join("1.json")).unwrap();
        std::fs::remove_file(dir.join("2.json")).unwrap();
        assert!(!agents(&dir).1, "but nothing readable at all means the format changed");
        assert!(!agents(&dir.join("missing")).1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_tool_result_lands_on_its_call_not_as_a_message() {
        let t = parse("result", &[
            user("u1", json!("hello")),
            assistant("a1", json!([{"type": "tool_use", "id": "t1", "name": "Bash", "input": {"command": "ls", "description": "List"}}])),
            user("u2", json!([{"type": "tool_result", "tool_use_id": "t1", "content": "boom", "is_error": true}])),
        ]);
        assert_eq!(t.rows.len(), 2);
        let Kind::Tool(tool) = &t.rows[1].kind else { panic!("{:?}", t.rows[1]) };
        assert_eq!(tool.result.as_deref(), Some("boom"));
        assert!(tool.error);
        assert_eq!((t.prompts, t.tools), (1, 1));
    }

    #[test]
    fn sidechains_meta_and_request_logs_stay_out() {
        let mut side = user("u2", json!("subagent prompt"));
        side["isSidechain"] = json!(true);
        let mut meta = user("u3", json!("[Image: original 100x100]"));
        meta["isMeta"] = json!(true);
        let log = json!({"type": "api-request-blob", "message": {"content": "logged"}});
        let t = parse("side", &[user("u1", json!("real")), side, meta, log]);
        assert_eq!(t.rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["real"]);
    }

    #[test]
    fn a_repeated_record_counts_once() {
        let t = parse("repeat", &[user("u1", json!("once")), user("u1", json!("once"))]);
        assert_eq!(t.rows.len(), 1);
    }

    #[test]
    fn reminders_and_harness_text_are_removed_from_prompts() {
        assert_eq!(clean_prompt("<system-reminder>x</system-reminder>\nfix it").as_deref(), Some("fix it"));
        assert_eq!(clean_prompt("<system-reminder>only</system-reminder>"), None);
        assert_eq!(clean_prompt("<local-command-stdout>ok</local-command-stdout>"), None);
        assert_eq!(
            clean_prompt("<command-name>/review</command-name><command-args>12</command-args>").as_deref(),
            Some("/review 12")
        );
    }

    #[test]
    fn a_session_is_named_for_its_first_real_prompt_not_a_slash_command() {
        let lines = [user("u1", json!("/clear")), user("u2", json!("fix the login"))];
        let body = lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
        assert_eq!(scan_head(body.as_bytes(), true).title.as_deref(), Some("fix the login"));
    }

    #[test]
    fn search_finds_replies_but_not_tool_output_or_logs() {
        let dir = temp("search");
        let path = dir.join("s.jsonl");
        let records = [
            assistant("a1", json!([{"type": "tool_use", "id": "t", "name": "Bash", "input": {"command": "needle"}}])),
            json!({"type": "api-request-blob", "message": {"content": "needle"}}),
            assistant("a2", json!([{"type": "text", "text": "Found the NEEDLE here, and a needle there"}])),
        ];
        std::fs::write(&path, records.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
        let session = Session {
            id: "s".into(),
            source: Source::Claude,
            path,
            project: "p".into(),
            cwd: String::new(),
            title: "t".into(),
            branch: String::new(),
            updated: 0,
            size: 1,
            turn: None,
        };
        let hits = data::search(&[session], "needle", &Arc::new(AtomicU64::new(0)), 0);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].count, hits[0].uuid.as_str()), (2, "a2"));
        assert!(hits[0].snippet.contains("NEEDLE"));
    }

    #[test]
    fn the_index_takes_the_latest_rename_and_skips_empty_and_nested_files() {
        let root = temp("index");
        let project = root.join("-a-b");
        std::fs::create_dir_all(project.join("sub")).unwrap();
        let mut first = user("u1", json!("first prompt"));
        first["cwd"] = json!("/src/app");
        first["gitBranch"] = json!("main");
        let lines = [
            first,
            json!({"type": "custom-title", "customTitle": "Old name"}),
            json!({"type": "custom-title", "customTitle": "New name"}),
        ];
        std::fs::write(project.join("one.jsonl"), lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")).unwrap();
        std::fs::write(project.join("empty.jsonl"), "{\"type\":\"summary\"}\n").unwrap();
        std::fs::write(project.join("sub/agent.jsonl"), user("u9", json!("nested")).to_string()).unwrap();
        let index = data::load_index(&Roots::under(&root.join("none")).with(Source::Claude, root.clone())).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].title, "New name");
        assert_eq!((index[0].project.as_str(), index[0].branch.as_str()), ("app", "main"));
    }

    #[test]
    fn a_bare_command_with_no_reply_is_not_a_session_but_one_with_a_reply_is() {
        let root = temp("bare");
        let project = root.join("-a");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("empty.jsonl"), user("u1", json!("/clear")).to_string()).unwrap();
        let reply = assistant("a1", json!([{"type": "text", "text": "Reviewed."}]));
        std::fs::write(project.join("used.jsonl"), format!("{}\n{reply}", user("u2", json!("/review")))).unwrap();
        let index = data::load_index(&Roots::under(&root.join("none")).with(Source::Claude, root.clone())).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(index.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(), ["/review"]);
    }

    fn report(t: &Transcript) -> Report {
        Report::new(Source::Claude, &t.usage, &t.agents, &Prices::default())
    }

    fn used(uuid: &str, message: &str, block: Value) -> Value {
        json!({"type": "assistant", "uuid": uuid, "message": {"id": message, "model": "claude-x", "content": [block], "usage": {
            "input_tokens": 10, "output_tokens": 20, "cache_read_input_tokens": 300, "cache_creation_input_tokens": 40,
            "cache_creation": {"ephemeral_1h_input_tokens": 40, "ephemeral_5m_input_tokens": 0}}}})
    }

    #[test]
    fn a_reply_written_as_several_records_is_one_request() {
        let t = parse("usage", &[
            used("a", "m1", json!({"type": "text", "text": "hi"})),
            used("b", "m1", json!({"type": "tool_use", "id": "t1", "name": "Bash", "input": {}})),
            used("c", "m2", json!({"type": "text", "text": "done"})),
        ]);
        assert_eq!(report(&t).total.tokens, Tokens { input: 20, output: 40, cache_read: 600, cache_write: 80 });
    }

    #[test]
    fn skills_and_mcp_calls_are_told_from_other_tools() {
        let t = parse("calls", &[
            used("a", "m1", json!({"type": "tool_use", "id": "t1", "name": "Skill", "input": {"skill": "rex"}})),
            used("b", "m1", json!({"type": "tool_use", "id": "t2", "name": "mcp__web__open", "input": {}})),
            used("c", "m1", json!({"type": "tool_use", "id": "t3", "name": "mcp__web__open", "input": {}})),
            used("d", "m1", json!({"type": "tool_use", "id": "t4", "name": "Bash", "input": {}})),
        ]);
        let report = report(&t);
        assert_eq!(report.skills, [("rex".to_owned(), 1)]);
        assert_eq!((report.mcp.len(), report.mcp[0].name.as_str(), report.mcp[0].calls), (1, "web", 2));
    }

    #[test]
    fn a_skill_the_user_typed_counts_and_one_the_agent_called_counts_once() {
        let meta = |uuid: &str, source: Value| json!({"type": "user", "uuid": uuid, "isMeta": true, "sourceToolUseID": source,
            "message": {"content": [{"type": "text", "text": "Base directory for this skill: /Users/a/.claude/skills/bro\n\nBody"}]}});
        let t = parse("typed", &[meta("a", Value::Null), meta("b", json!("toolu_1"))]);
        assert_eq!(report(&t).skills, [("bro".to_owned(), 1)]);
    }

    #[test]
    fn a_resumed_sessions_repeated_records_arent_counted_again() {
        let record = used("a", "m1", json!({"type": "tool_use", "id": "t1", "name": "Skill", "input": {"skill": "rex"}}));
        let t = parse("resumed", &[record.clone(), record]);
        assert_eq!(report(&t).skills, [("rex".to_owned(), 1)]);
    }

    #[test]
    fn subagents_count_wherever_they_sit_and_grow_as_they_write() {
        let root = temp("agents");
        let project = root.join("-src-app");
        let nested = project.join("s1/subagents/workflows/wf_1");
        std::fs::create_dir_all(&nested).unwrap();
        let lines = |records: &[Value]| records.iter().map(|r| r.to_string() + "\n").collect::<String>();
        let text = json!({"type": "text", "text": "hi"});
        let prompt = json!({"type": "user", "uuid": "u", "cwd": "/src/app", "message": {"content": "go"}});
        std::fs::write(project.join("s1.jsonl"), lines(&[prompt, used("a", "m1", text.clone())])).unwrap();
        std::fs::write(project.join("s1/subagents/agent-a.jsonl"), lines(&[used("b", "m2", text.clone())])).unwrap();
        std::fs::write(nested.join("agent-b.jsonl"), lines(&[used("c", "m3", text.clone())])).unwrap();
        std::fs::write(project.join("s1/subagents/agent-a.meta.json"), "{}").unwrap();

        let roots = Roots::under(&root.join("none")).with(Source::Claude, root.clone());
        let session = data::load_index(&roots).unwrap().remove(0);
        let mut t = data::load_transcript(&session).unwrap();
        let before = report(&t);
        assert_eq!((before.total.tokens.input, before.agents.tokens.input), (30, 20));

        let mut file = std::fs::OpenOptions::new().append(true).open(project.join("s1/subagents/agent-a.jsonl")).unwrap();
        std::io::Write::write_all(&mut file, lines(&[used("d", "m2", text.clone()), used("e", "m4", text)]).as_bytes()).unwrap();
        let update = data::read_new(&session, t.read, &t.agents).unwrap();
        assert!(t.apply(Source::Claude, update), "a subagent writing is a change to show");
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(report(&t).agents.tokens.input, 30, "the repeated m2 isn't counted, the new m4 is");
    }
}
