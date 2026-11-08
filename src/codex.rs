//! Codex: `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`, compressed to `.jsonl.zst` once a
//! session goes quiet. Names are in `session_index.jsonl`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;

use crate::agent::{Agent, Found, Titles};
use crate::config::{ConfigItem, SUBTITLE_CHARS, UNREADABLE, file_name, item, json_file, masked_toml, mcp_servers, none, read_toml, rules_file, scrub, skills, sorted_dir, without_repeats};
use crate::logos::{self, Layer};
use crate::notifications::Event;
use crate::usage::{Request, Tokens, Usage};
use crate::data::{Kind, Session, Source, TITLE_CHARS, Transcript, between, contains, describe_tool, each_line, fence, one_line, parse_time, project_name, result_text, str_field};

pub struct Codex;

impl Agent for Codex {
    fn label(&self) -> &'static str {
        "Codex"
    }

    fn id(&self) -> &'static str {
        "codex"
    }

    fn logo(&self) -> &'static [Layer] {
        &logos::CODEX_LAYERS
    }

    fn sessions(&self, home: &Path) -> PathBuf {
        home.join(".codex")
    }

    fn classify(&self, rest: &Path) -> Option<Found> {
        if rest == Path::new("session_index.jsonl") {
            return Some(Found::Titles);
        }
        let name = rest.file_name()?.to_string_lossy();
        let session = (rest.starts_with("sessions") || rest.starts_with("archived_sessions")) && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zst"));
        session.then_some(Found::Session)
    }

    fn files(&self, root: &Path) -> Vec<PathBuf> {
        files(root)
    }

    fn titles(&self, root: &Path) -> Titles {
        names(root)
    }

    fn read_session(&self, path: &Path, titles: &Titles) -> Option<Session> {
        read_session(path, titles)
    }

    /// It compresses a quiet session, leaving both files for a moment.
    fn moved(&self, path: &Path) -> Option<PathBuf> {
        compressed(path)
    }

    fn load(&self, path: &Path) -> Result<Transcript, String> {
        load(path)
    }

    fn wanted(&self, line: &[u8]) -> bool {
        wanted(line)
    }

    fn add_record(&self, out: &mut Transcript, record: &Value, line: usize) {
        add_record(out, record, line);
    }

    /// It marks a turn's start and end.
    fn turn(&self, line: &[u8]) -> Option<bool> {
        if !contains(line, b"\"event_msg\"") {
            return None;
        }
        let record: Value = serde_json::from_slice(line).ok()?;
        match record["payload"]["type"].as_str()? {
            "task_started" => Some(true),
            "task_complete" | "turn_aborted" => Some(false),
            _ => None,
        }
    }

    fn searchable(&self, record: &Value, line: usize) -> Option<(String, Vec<String>)> {
        searchable(record, line)
    }

    /// `openai-codex` comes first: its entries are the ones Codex's models run under.
    fn providers(&self) -> &'static [&'static str] {
        &["openai-codex", "openai"]
    }

    fn subagent_files(&self, session: &Session, others: &HashSet<PathBuf>) -> Option<(Vec<PathBuf>, Vec<PathBuf>)> {
        Some(agent_files(session, others))
    }

    fn add_usage(&self, usage: &mut Usage, record: &Value) {
        add_usage(usage, record);
    }

    fn start(&self, _: &str) -> (Vec<String>, Option<String>) {
        (vec!["codex".to_owned()], None)
    }

    fn resume(&self, id: &str) -> Vec<String> {
        ["codex", "resume", id].map(str::to_owned).to_vec()
    }

    /// It only says a turn has ended, or that it wants approval, when asked to. It has no
    /// system prompt flag; developer instructions add to its own.
    fn hyper_args(&self, prompt: &str) -> Vec<String> {
        let developer = format!("developer_instructions={}", toml::Value::String(prompt.to_owned()));
        ["-c", "tui.notifications=true", "-c", "tui.notification_method=\"osc9\"", "-c", "tui.notification_condition=\"always\"", "-c"].map(str::to_owned).into_iter().chain([developer]).collect()
    }

    fn reports(&self, event: Event) -> bool {
        matches!(event, Event::TurnCompleted | Event::ApprovalRequested | Event::InputRequested)
    }

    /// Its OSC 9 notifications are display text only: its own prompt prefixes say what they
    /// are, and the rest are previews of its reply.
    fn notice(&self, message: &str) -> Event {
        if message.starts_with("Approval requested:") || message.starts_with("Approval requested by ") || message.starts_with("Codex wants to edit ") {
            return Event::ApprovalRequested;
        }
        if message.starts_with("Plan mode prompt:") || message.starts_with("Question:") {
            return Event::InputRequested;
        }
        Event::TurnCompleted
    }

    fn config(&self, home: &Path) -> Vec<ConfigItem> {
        if home.join(".codex").is_dir() { config(home) } else { Vec::new() }
    }
}

/// Reading stops here when no prompt has turned up.
const HEAD_MAX: usize = 8 * 1024 * 1024;

pub fn files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    collect(&root.join("sessions"), &mut files);
    collect(&root.join("archived_sessions"), &mut files);
    files
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if path.is_dir() {
            collect(&path, files);
        } else if name.ends_with(".jsonl") || name.ends_with(".jsonl.zst") {
            files.push(path);
        }
    }
}

/// Thread names by session id; a rename appends, so the last line wins.
pub fn names(root: &Path) -> HashMap<String, String> {
    let mut names = HashMap::new();
    let _ = each_line(&root.join("session_index.jsonl"), |_, line| {
        if let Ok(entry) = serde_json::from_slice::<Value>(line) {
            let name = str_field(&entry, "thread_name").trim();
            if !name.is_empty() {
                names.insert(str_field(&entry, "id").to_owned(), one_line(name, TITLE_CHARS));
            }
        }
        true
    });
    names
}

/// One file's listing, or `None` for a subagent's session (guardian reviews, code reviews,
/// spawned agents) or one with no prompt.
pub fn read_session(path: &Path, names: &HashMap<String, String>) -> Option<Session> {
    let meta = std::fs::metadata(path).ok()?;
    let (mut id, mut cwd, mut prompt) = (String::new(), String::new(), None);
    let mut subagent = false;
    let mut read = 0;
    each_line(path, |_, line| {
        read += line.len();
        if id.is_empty() && contains(line, b"\"session_meta\"") {
            if let Ok(record) = serde_json::from_slice::<Value>(line) {
                let payload = &record["payload"];
                id = str_field(payload, "id").to_owned();
                cwd = str_field(payload, "cwd").to_owned();
                subagent = payload["source"].is_object();
            }
        } else if contains(line, b"\"response_item\"") && contains(line, b"\"user\"") && let Ok(record) = serde_json::from_slice::<Value>(line) {
            prompt = user_text(&record["payload"]);
        }
        !subagent && prompt.is_none() && read < HEAD_MAX
    })
    .ok()?;
    if subagent || id.is_empty() {
        return None;
    }
    let title = names.get(&id).cloned().or_else(|| prompt.as_deref().map(|p| one_line(p, TITLE_CHARS)))?;
    let updated = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
    Some(Session { id, source: Source::Codex, path: path.to_owned(), project: project_name(&cwd), cwd, title, branch: String::new(), updated, size: meta.len(), turn: crate::data::turn(path, Source::Codex, updated) })
}

/// What the user typed in a `message` payload, without the harness's context blocks.
fn user_text(payload: &Value) -> Option<String> {
    if payload["type"] != "message" || payload["role"] != "user" {
        return None;
    }
    let parts: Vec<String> = payload["content"]
        .as_array()?
        .iter()
        .filter(|c| c["type"] == "input_text")
        .filter_map(|c| c["text"].as_str().and_then(clean_prompt))
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

pub fn clean_prompt(raw: &str) -> Option<String> {
    const REQUEST: &str = "## My request for Codex:";
    let mut text = raw.rsplit_once(REQUEST).map_or(raw, |(_, request)| request).trim();
    while text.starts_with("<in-app-browser-context") {
        text = text.split_once("</in-app-browser-context>")?.1.trim();
        text = text.strip_prefix("## My request:").unwrap_or(text).trim();
    }
    const HARNESS: [&str; 15] = [
        "<environment_context",
        "# AGENTS.md instructions",
        "<recommended_plugins",
        "<turn_aborted>",
        "<send_user_message",
        "<skill>",
        "<user_action>",
        "<user_instructions>",
        "<user_shell_command>",
        "<image",
        "</image>",
        "<subagent_notification>",
        "Codex could not read",
        "# Files mentioned by",
        "<INSTRUCTIONS>",
    ];
    if text.is_empty() || HARNESS.iter().any(|p| text.starts_with(p)) {
        return None;
    }
    Some(text.to_owned())
}

fn assistant_text(payload: &Value) -> Option<String> {
    if payload["type"] != "message" || payload["role"] != "assistant" {
        return None;
    }
    let parts: Vec<&str> = payload["content"].as_array()?.iter().filter_map(|c| c["text"].as_str()).collect();
    let text = parts.join("\n\n");
    (!text.trim().is_empty()).then(|| text.trim().to_owned())
}

pub fn wanted(line: &[u8]) -> bool {
    [&b"\"response_item\""[..], b"\"turn_context\"", b"\"compacted\"", b"\"session_meta\"", b"\"token_usage_record\"", b"\"token_count\"", b"\"McpToolCall\""]
        .iter()
        .any(|w| contains(line, w))
}

pub fn load(path: &Path) -> Result<Transcript, String> {
    crate::data::load_records(path, &Codex)
}

/// The compressed copy of a `.jsonl`, once there is one.
pub fn compressed(path: &Path) -> Option<PathBuf> {
    let mut name = path.as_os_str().to_owned();
    name.push(".zst");
    let path = PathBuf::from(name);
    path.exists().then_some(path)
}

pub fn add_record(out: &mut Transcript, record: &Value, number: usize) {
    add_usage(&mut out.usage, record);
    let payload = &record["payload"];
    let id = format!("L{number}:0");
    match str_field(record, "type") {
        "session_meta" => out.started = out.started.or(parse_time(str_field(payload, "timestamp"))),
        "turn_context" => {
            let model = str_field(payload, "model");
            if !model.is_empty() {
                out.model = model.to_owned();
            }
        }
        "compacted" => {
            let text = str_field(payload, "message").trim();
            if !text.is_empty() {
                out.push(id, Kind::Compacted, text.to_owned());
            }
        }
        "response_item" => add_item(out, payload, id),
        _ => {}
    }
}

/// A record's request or calls. Newer files have a record per request. Older ones have only a
/// running total, repeated when nothing's been used, with the last request beside it.
pub fn add_usage(usage: &mut Usage, record: &Value) {
    let payload = &record["payload"];
    match str_field(record, "type") {
        "turn_context" => {
            let model = str_field(payload, "model");
            if !model.is_empty() {
                usage.name_model(model);
            }
        }
        "token_usage_record" => {
            usage.recorded = true;
            let request = request(&usage.model, &payload["usage"]);
            usage.request("", request);
        }
        "event_msg" => match str_field(payload, "type") {
            "token_count" if !usage.recorded => {
                let info = &payload["info"];
                let running = info["total_token_usage"]["total_tokens"].as_u64().unwrap_or(0);
                if running != usage.running && info["last_token_usage"].is_object() {
                    usage.running = running;
                    let request = request(&usage.model, &info["last_token_usage"]);
                    usage.request("", request);
                }
            }
            "item_completed" if payload["item"]["type"] == "McpToolCall" => {
                let item = &payload["item"];
                usage.mcp(str_field(item, "id"), str_field(item, "server"), str_field(item, "tool"));
            }
            _ => {}
        },
        "response_item" => match str_field(payload, "type") {
            "function_call" | "custom_tool_call" => {
                usage.mcp_tool(str_field(payload, "call_id"), str_field(payload, "name"));
                usage.skills_read(payload["arguments"].as_str().or(payload["input"].as_str()).unwrap_or(""));
            }
            "message" if payload["role"] == "user" => {
                for part in payload["content"].as_array().into_iter().flatten() {
                    if let Some(name) = part["text"].as_str().filter(|t| t.starts_with("<skill>")).and_then(|t| between(t, "<name>", "</name>")) {
                        usage.skill(name.trim());
                    }
                }
            }
            _ => {}
        },
        _ => {}
    }
}

/// Input counts what was cached, and output what was reasoned.
fn request(model: &str, tokens: &Value) -> Request {
    let count = |key: &str| tokens[key].as_u64().unwrap_or(0);
    let cache_read = count("cached_input_tokens");
    let cache_write = count("cache_write_input_tokens");
    let input = count("input_tokens").saturating_sub(cache_read + cache_write);
    Request { model: model.to_owned(), tokens: Tokens { input, output: count("output_tokens"), cache_read, cache_write }, ..Request::default() }
}

/// A session's subagents, which are files that name it as their parent, and the files newly
/// found not to be. A subagent starts within its session's life, so only those days' folders
/// are looked in, and archived sessions. `others` have been ruled out already.
pub fn agent_files(session: &Session, others: &HashSet<PathBuf>) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let root = session.path.ancestors().find(|p| p.file_name().is_some_and(|n| n == "sessions" || n == "archived_sessions")).and_then(Path::parent);
    let Some(root) = root else { return Default::default() };
    let first = started_on(&session.path).and_then(|day| chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok());
    let last = chrono::DateTime::from_timestamp(session.updated + 86_400, 0).map(|t| t.date_naive());
    let (Some(first), Some(last)) = (first, last) else { return Default::default() };
    let mut candidates = Vec::new();
    collect(&root.join("archived_sessions"), &mut candidates);
    for day in first.iter_days().take_while(|day| *day <= last) {
        collect(&root.join("sessions").join(day.format("%Y/%m/%d").to_string()), &mut candidates);
    }
    let (mut found, mut strangers) = (Vec::new(), Vec::new());
    for path in candidates {
        let in_life = started_on(&path).is_some_and(|day| day >= first.to_string().as_str() && day <= last.to_string().as_str());
        if path == session.path || !in_life || others.contains(&path) {
            continue;
        }
        match belongs_to(&path, &session.id) {
            Some(true) => found.push(path),
            Some(false) => strangers.push(path),
            None => {}
        }
    }
    (found, strangers)
}

/// The day in a `rollout-2026-01-02T03-04-05-<id>.jsonl`.
fn started_on(path: &Path) -> Option<&str> {
    path.file_name()?.to_str()?.strip_prefix("rollout-")?.get(..10)
}

/// Whether a file is a subagent of the session with this id, or of one of its subagents.
/// `None` until the file has its first record, which a new one may not yet.
fn belongs_to(path: &Path, id: &str) -> Option<bool> {
    let mut belongs = None;
    let _ = each_line(path, |_, line| {
        if let Ok(record) = serde_json::from_slice::<Value>(line) {
            let meta = &record["payload"];
            let spawn = &meta["source"]["subagent"]["thread_spawn"];
            let linked = [&meta["session_id"], &meta["parent_thread_id"], &spawn["parent_thread_id"]].iter().any(|v| *v == id);
            belongs = (record["type"] == "session_meta").then(|| meta["source"].is_object() && meta["id"] != id && linked);
        }
        false
    });
    belongs
}

fn add_item(out: &mut Transcript, item: &Value, id: String) {
    let call_id = str_field(item, "call_id");
    match str_field(item, "type") {
        "message" => {
            if let Some(text) = user_text(item) {
                out.push(id, Kind::User, text);
            } else if let Some(text) = assistant_text(item) {
                out.push(id, Kind::Assistant, text);
            }
        }
        "reasoning" => {
            let parts: Vec<&str> = item["summary"].as_array().map(Vec::as_slice).unwrap_or_default().iter().filter_map(|p| p["text"].as_str()).collect();
            if !parts.is_empty() {
                out.push(id, Kind::Thinking, parts.join("\n\n"));
            }
        }
        "function_call" => {
            let arguments = str_field(item, "arguments");
            let input = serde_json::from_str::<Value>(arguments).unwrap_or_else(|_| Value::String(arguments.to_owned()));
            out.call(call_id, id, describe_tool(str_field(item, "name"), &input));
        }
        "custom_tool_call" => out.call(call_id, id, describe_tool(str_field(item, "name"), &item["input"])),
        "web_search_call" => out.call(call_id, id, describe_tool("web_search", &item["action"])),
        "function_call_output" | "custom_tool_call_output" => out.result(call_id, output_text(&item["output"]), false),
        _ => {}
    }
}

/// An output, unwrapping the `{"output": ..}` JSON older sessions stored as a string.
fn output_text(output: &Value) -> String {
    if let Some(wrapped) = output.as_str().and_then(|s| serde_json::from_str::<Value>(s).ok()).filter(|v| v["output"].is_string()) {
        return result_text(&wrapped["output"]);
    }
    result_text(output)
}

/// A record's id (its line) and the text a reader sees in it.
pub fn searchable(record: &Value, number: usize) -> Option<(String, Vec<String>)> {
    if record["type"] != "response_item" {
        return None;
    }
    let payload = &record["payload"];
    let text = user_text(payload).or_else(|| assistant_text(payload))?;
    Some((format!("L{number}"), vec![text]))
}

fn config(home: &Path) -> Vec<ConfigItem> {
    let source = Source::Codex;
    let root = home.join(".codex");
    let config_path = root.join("config.toml");
    let config = read_toml(&config_path);
    let table = config.clone().unwrap_or(Value::Null);
    let mut items = Vec::new();

    items.extend(rules_file(source, &root.join("AGENTS.md")));
    for path in sorted_dir(&root.join("rules")) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            items.push(item(source, "Rules", file_name(&path), "Command rules", Some(&path), fence("python", &scrub(&text))));
        }
    }

    if config_path.is_file() {
        let markdown = config.as_ref().map_or_else(|| UNREADABLE.to_owned(), masked_toml);
        items.push(item(source, "Settings", "config.toml", "", Some(&config_path), markdown));
    }
    items.extend(json_file(source, "Settings", &root.join("keybindings.json")));

    let servers = mcp_servers(source, &table["mcp_servers"], &config_path, true);
    if servers.is_empty() {
        items.push(none(source, "MCP servers", &config_path));
    }
    items.extend(servers);

    let mut found = skills(source, &root.join("skills"), "");
    found.extend(skills(source, &home.join(".agents/skills"), "Shared in ~/.agents"));
    let rules = table["skills"]["config"].as_array().map(Vec::as_slice).unwrap_or_default();
    let disabled = |skill: &ConfigItem| {
        rules.iter().filter(|r| r["enabled"] == false).any(|r| {
            r["name"] == skill.title.as_str() || skill.path.as_ref().is_some_and(|p| Path::new(str_field(r, "path")) == p)
        })
    };
    for skill in &mut found {
        if disabled(skill) {
            skill.subtitle = one_line(&format!("Disabled · {}", skill.subtitle), SUBTITLE_CHARS);
        }
    }
    items.extend(without_repeats(found));

    if let Some(plugins) = table["plugins"].as_object() {
        for (name, plugin) in plugins {
            let state = if plugin["enabled"] == false { "Disabled" } else { "Enabled" };
            items.push(item(source, "Plugins", name.as_str(), state, Some(&config_path), masked_toml(plugin)));
        }
    }
    let hooks_path = root.join("hooks.json");
    if let Some(mut hooks) = json_file(source, "Hooks", &hooks_path) {
        hooks.subtitle = String::new();
        items.push(hooks);
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{self, Roots};
    use crate::usage::{Prices, Report};
    use serde_json::json;
    use std::io::Write;
    use std::sync::Arc;
    use std::sync::atomic::AtomicU64;

    fn meta(id: &str, source: Value) -> Value {
        json!({"type": "session_meta", "payload": {"id": id, "cwd": "/src/app", "source": source, "timestamp": "2026-01-02T03:04:05Z"}})
    }

    fn message(role: &str, kind: &str, text: &str) -> Value {
        json!({"type": "response_item", "payload": {"type": "message", "role": role, "content": [{"type": kind, "text": text}]}})
    }

    fn body(records: &[Value]) -> String {
        records.iter().map(Value::to_string).collect::<Vec<_>>().join("\n")
    }

    fn temp(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("sessions-codex-{name}-{}", std::process::id()));
        std::fs::create_dir_all(root.join("sessions/2026/01/02")).unwrap();
        root
    }

    fn roots(root: &Path) -> Roots {
        Roots::under(&root.join("none")).with(Source::Codex, root.to_owned())
    }

    #[test]
    fn a_compressed_session_is_listed_opened_and_searched() {
        let root = temp("zst");
        let lines = [
            meta("z1", json!("cli")),
            message("user", "input_text", "# AGENTS.md instructions for /src/app"),
            message("user", "input_text", "<environment_context>x</environment_context>"),
            message("user", "input_text", "fix the flaky test"),
            message("assistant", "output_text", "The zebrafish case was racing."),
        ];
        let path = root.join("sessions/2026/01/02/rollout-a.jsonl.zst");
        let mut encoder = zstd::Encoder::new(std::fs::File::create(&path).unwrap(), 3).unwrap();
        encoder.write_all(body(&lines).as_bytes()).unwrap();
        encoder.finish().unwrap();

        let index = data::load_index(&roots(&root)).unwrap();
        assert_eq!(index.len(), 1);
        assert_eq!((index[0].title.as_str(), index[0].project.as_str(), index[0].source), ("fix the flaky test", "app", Source::Codex));

        let t = data::load_transcript(&index[0]).unwrap();
        assert_eq!(t.rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["fix the flaky test", "The zebrafish case was racing."]);

        let hits = data::search(&index, "zebrafish", &Arc::new(AtomicU64::new(0)), 0);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(format!("{}:0", hits[0].uuid), t.rows[1].id, "a hit points at its row");
    }

    #[test]
    fn subagent_sessions_are_not_listed_and_thread_names_are_used() {
        let root = temp("sub");
        let day = root.join("sessions/2026/01/02");
        let prompt = message("user", "input_text", "the prompt");
        std::fs::write(day.join("a.jsonl"), body(&[meta("a", json!("vscode")), prompt.clone()])).unwrap();
        std::fs::write(day.join("b.jsonl"), body(&[meta("b", json!({"subagent": "review"})), prompt])).unwrap();
        let index = [json!({"id": "a", "thread_name": "Old"}), json!({"id": "a", "thread_name": "Named thread"})];
        std::fs::write(root.join("session_index.jsonl"), body(&index)).unwrap();
        let sessions = data::load_index(&roots(&root)).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(sessions.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(), ["Named thread"]);
    }

    #[test]
    fn calls_of_every_shape_pair_with_their_outputs() {
        let mut out = Transcript::default();
        let items = [
            json!({"type": "function_call", "call_id": "c1", "name": "exec_command", "arguments": "{\"cmd\":\"ls -la\"}"}),
            json!({"type": "function_call_output", "call_id": "c1", "output": "{\"output\":\"a.txt\",\"metadata\":{}}"}),
            json!({"type": "custom_tool_call", "call_id": "c2", "name": "apply_patch", "input": "*** Add File: b.rs\n+x"}),
            json!({"type": "custom_tool_call_output", "call_id": "c2", "output": [{"type": "input_text", "text": "Done"}, {"type": "input_image"}]}),
            json!({"type": "web_search_call", "action": {"type": "search", "query": "rust zstd"}}),
            json!({"type": "reasoning", "summary": [{"type": "summary_text", "text": "Looking"}]}),
        ];
        for (i, item) in items.iter().enumerate() {
            add_item(&mut out, item, format!("L{i}:0"));
        }
        let t = { out.recount(); out };
        let tool = |i: usize| match &t.rows[i].kind {
            Kind::Tool(tool) => tool.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!((tool(0).summary, tool(0).result), ("ls -la".to_owned(), Some("a.txt".to_owned())));
        assert_eq!((tool(1).summary, tool(1).result), ("b.rs".to_owned(), Some("Done\n[input_image]".to_owned())));
        assert_eq!(tool(2).summary, "rust zstd");
        assert_eq!((&t.rows[3].kind, t.rows[3].text.as_str()), (&Kind::Thinking, "Looking"));
    }

    #[test]
    fn the_request_is_taken_out_of_an_ide_context_block() {
        let text = "# Context from my IDE setup:\n\n## Open tabs:\n- a.rs\n\n## My request for Codex:\nrename the field\n";
        assert_eq!(clean_prompt(text).as_deref(), Some("rename the field"));
        assert_eq!(clean_prompt("<turn_aborted>x</turn_aborted>"), None);
    }

    #[test]
    fn browser_context_keeps_the_request_and_does_not_become_a_prompt() {
        let context = "<in-app-browser-context source=\"ambient-ui-state\">\nAutomatically supplied browser state.\n</in-app-browser-context>";
        let text = format!("\n{context}\n\n## My request:\nalso check the iPhone app");
        let record = message("user", "input_text", &text);
        assert_eq!(user_text(&record["payload"]).as_deref(), Some("also check the iPhone app"));
        assert_eq!(clean_prompt(context), None);
        assert_eq!(clean_prompt("<in-app-browser-context>unfinished"), None);
    }

    #[test]
    fn a_cut_off_archive_gives_the_records_before_the_cut() {
        let root = temp("cut");
        let path = root.join("sessions/2026/01/02/cut.jsonl.zst");
        let lines: Vec<Value> = std::iter::once(meta("c", json!("cli")))
            .chain((0..2000).map(|i| message("assistant", "output_text", &format!("reply {i} {}", "x".repeat(i % 97)))))
            .collect();
        let whole = zstd::encode_all(body(&lines).as_bytes(), 3).unwrap();
        std::fs::write(&path, &whole[..whole.len() / 2]).unwrap();
        let t = load(&path).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert!(t.rows.len() < 2000 && !t.rows.is_empty(), "{} rows", t.rows.len());
    }

    fn report(t: &Transcript) -> Report {
        Report::new(Source::Codex, &t.usage, &t.agents, &Prices::default())
    }

    fn parsed(name: &str, records: &[Value]) -> Transcript {
        let root = temp(name);
        let path = root.join("sessions/2026/01/02/rollout-2026-01-02T03-04-05-a.jsonl");
        std::fs::write(&path, body(records)).unwrap();
        let t = load(&path).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        t
    }

    fn turn(model: &str) -> Value {
        json!({"type": "turn_context", "payload": {"model": model}})
    }

    fn tokens(input: u64, cached: u64, output: u64) -> Value {
        json!({"input_tokens": input, "cached_input_tokens": cached, "output_tokens": output, "total_tokens": input + output})
    }

    fn record(input: u64, cached: u64, output: u64) -> Value {
        json!({"type": "token_usage_record", "payload": {"usage": tokens(input, cached, output)}})
    }

    fn count(running: u64, last: Value) -> Value {
        json!({"type": "event_msg", "payload": {"type": "token_count", "info": {"total_token_usage": {"total_tokens": running}, "last_token_usage": last}}})
    }

    #[test]
    fn cached_input_is_taken_out_of_input() {
        let t = parsed("cached", &[turn("gpt-x"), record(100, 80, 5)]);
        assert_eq!(report(&t).total.tokens, Tokens { input: 20, output: 5, cache_read: 80, cache_write: 0 });
    }

    #[test]
    fn a_requests_running_total_doesnt_count_it_twice() {
        let t = parsed("both", &[turn("gpt-x"), record(100, 0, 5), count(105, tokens(100, 0, 5)), record(50, 0, 5), count(160, tokens(50, 0, 5))]);
        assert_eq!(report(&t).total.tokens.input, 150);
    }

    #[test]
    fn an_older_session_has_only_running_totals_some_of_them_repeats() {
        let t = parsed("older", &[turn("gpt-x"), count(105, tokens(100, 0, 5)), count(105, tokens(100, 0, 5)), count(160, tokens(50, 0, 5))]);
        assert_eq!(report(&t).total.tokens.input, 150);
    }

    #[test]
    fn a_request_goes_to_the_model_of_its_turn() {
        let t = parsed("models", &[record(1, 0, 0), turn("gpt-a"), record(2, 0, 0), turn("gpt-b"), record(4, 0, 0)]);
        let models = report(&t).models;
        assert_eq!(models.iter().map(|(m, s)| (m.as_str(), s.tokens.input)).collect::<Vec<_>>(), [("gpt-b", 4), ("gpt-a", 3)]);
    }

    #[test]
    fn mcp_calls_and_skills_come_from_each_form_codex_writes() {
        let item = json!({"type": "event_msg", "payload": {"type": "item_completed", "item": {"type": "McpToolCall", "id": "x1", "server": "app", "tool": "list"}}});
        let old = json!({"type": "response_item", "payload": {"type": "function_call", "call_id": "c1", "name": "mcp__docs__find", "arguments": "{}"}});
        let read = json!({"type": "response_item", "payload": {"type": "custom_tool_call", "call_id": "c2", "name": "exec", "input": "cat /Users/a/.codex/skills/gh-cli/SKILL.md"}});
        let typed = message("user", "input_text", "<skill>\n<name>deep</name>\n<path>/x/SKILL.md</path>\nbody</skill>");
        let report = report(&parsed("calls", &[item, old, read, typed]));
        assert_eq!(report.mcp.iter().map(|s| (s.name.as_str(), s.calls)).collect::<Vec<_>>(), [("app", 1), ("docs", 1)]);
        assert_eq!(report.skills, [("deep".to_owned(), 1), ("gh-cli".to_owned(), 1)]);
    }

    #[test]
    fn a_subagent_is_found_by_its_parent_or_root_and_a_stranger_isnt() {
        let root = temp("agents");
        let day = root.join("sessions/2026/01/02");
        let child = |id: &str, link: Value| {
            let mut m = meta(id, json!({"subagent": {"thread_spawn": {"parent_thread_id": "other"}}}));
            m["payload"].as_object_mut().unwrap().extend(link.as_object().unwrap().clone());
            m
        };
        let file = |name: &str| day.join(format!("rollout-2026-01-02T03-04-05-{name}.jsonl"));
        std::fs::write(file("p"), body(&[meta("p", json!("cli")), message("user", "input_text", "go"), turn("gpt-x"), record(1, 0, 0)]) + "\n").unwrap();
        std::fs::write(file("a"), body(&[child("a", json!({"parent_thread_id": "p"})), turn("gpt-x"), record(10, 0, 0)]) + "\n").unwrap();
        std::fs::write(file("b"), body(&[child("b", json!({"session_id": "p", "parent_thread_id": "a"})), turn("gpt-x"), record(100, 0, 0)]) + "\n").unwrap();
        std::fs::write(file("c"), body(&[child("c", json!({"session_id": "q"})), turn("gpt-x"), record(1000, 0, 0)]) + "\n").unwrap();
        std::fs::write(file("d"), body(&[meta("d", json!("cli")), message("user", "input_text", "other"), turn("gpt-x"), record(10_000, 0, 0)]) + "\n").unwrap();

        let index = data::load_index(&roots(&root)).unwrap();
        let session = index.iter().find(|s| s.id == "p").unwrap();
        let mut t = data::load_transcript(session).unwrap();
        let report_of = |t: &Transcript| (report(t).total.tokens.input, report(t).agents.tokens.input);
        assert_eq!(report_of(&t), (111, 110));

        let mut a = std::fs::OpenOptions::new().append(true).open(file("a")).unwrap();
        a.write_all((record(20, 0, 0).to_string() + "\n").as_bytes()).unwrap();
        let update = data::read_new(session, t.read, &t.agents).unwrap();
        assert!(t.apply(Source::Codex, update));
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(report_of(&t), (131, 130));
    }

    #[test]
    fn a_subagent_still_being_written_is_looked_at_again() {
        let root = temp("agent-late");
        let day = root.join("sessions/2026/01/02");
        let file = |name: &str| day.join(format!("rollout-2026-01-02T03-04-05-{name}.jsonl"));
        std::fs::write(file("p"), body(&[meta("p", json!("cli")), message("user", "input_text", "go")]) + "\n").unwrap();
        let mut child = meta("a", json!({"subagent": {"thread_spawn": {"parent_thread_id": "p"}}}));
        child["payload"]["parent_thread_id"] = json!("p");
        let whole = body(&[child, turn("gpt-x"), record(10, 0, 0)]) + "\n";
        std::fs::write(file("a"), &whole[..20]).unwrap();

        let index = data::load_index(&roots(&root)).unwrap();
        let session = index.iter().find(|s| s.id == "p").unwrap();
        let mut t = data::load_transcript(session).unwrap();
        std::fs::write(file("a"), &whole).unwrap();
        let update = data::read_new(session, t.read, &t.agents).unwrap();
        t.apply(Source::Codex, update);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(report(&t).agents.tokens.input, 10);
    }

    #[test]
    fn a_subagent_first_found_in_both_forms_is_counted_once() {
        let root = temp("agent-both");
        let day = root.join("sessions/2026/01/02");
        let file = |name: &str| day.join(format!("rollout-2026-01-02T03-04-05-{name}.jsonl"));
        std::fs::write(file("p"), body(&[meta("p", json!("cli")), message("user", "input_text", "go")]) + "\n").unwrap();
        let mut child = meta("a", json!({"subagent": {"thread_spawn": {"parent_thread_id": "p"}}}));
        child["payload"]["parent_thread_id"] = json!("p");
        let whole = body(&[child, turn("gpt-x"), record(10, 0, 0)]) + "\n";
        std::fs::write(file("a"), &whole).unwrap();
        // Codex writes the compressed copy before it removes the original.
        let mut compressed = file("a").into_os_string();
        compressed.push(".zst");
        std::fs::write(compressed, zstd::encode_all(whole.as_bytes(), 3).unwrap()).unwrap();

        let index = data::load_index(&roots(&root)).unwrap();
        let session = index.iter().find(|s| s.id == "p").unwrap();
        let t = data::load_transcript(session).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(report(&t).agents.tokens.input, 10);
    }

    #[test]
    fn a_subagent_compressed_after_reading_isnt_counted_again() {
        let root = temp("agent-zst");
        let day = root.join("sessions/2026/01/02");
        let plain = day.join("rollout-2026-01-02T03-04-05-a.jsonl");
        let mut m = meta("a", json!({"subagent": {"thread_spawn": {"parent_thread_id": "p"}}}));
        m["payload"]["parent_thread_id"] = json!("p");
        let text = body(&[m, turn("gpt-x"), record(10, 0, 0)]) + "\n";
        std::fs::write(&plain, &text).unwrap();
        let parent = day.join("rollout-2026-01-02T03-04-05-p.jsonl");
        std::fs::write(&parent, body(&[meta("p", json!("cli")), message("user", "input_text", "go")]) + "\n").unwrap();

        let index = data::load_index(&roots(&root)).unwrap();
        let session = index.iter().find(|s| s.id == "p").unwrap();
        let mut t = data::load_transcript(session).unwrap();
        std::fs::write(day.join("rollout-2026-01-02T03-04-05-a.jsonl.zst"), zstd::encode_all(text.as_bytes(), 0).unwrap()).unwrap();
        std::fs::remove_file(&plain).unwrap();
        let update = data::read_new(session, t.read, &t.agents).unwrap();
        t.apply(Source::Codex, update);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(report(&t).agents.tokens.input, 10);
    }
}
