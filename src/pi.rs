//! Pi: `~/.pi/agent/sessions/--<folder>--/<time>_<id>.jsonl`. Entries form a tree through
//! `parentId`, so a session rewound with `/tree` has abandoned branches in its file.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;

use crate::agent::{Agent, Found, Titles};
use crate::config::{ConfigItem, file_name, item, json_file, masked_json, mcp_servers, none, read_json, rules_file, skills, sorted_dir, without_repeats};
use crate::logos::{self, Layer};
use crate::notifications::Event;
use crate::usage::{Costs, Request, Tokens, Usage};
use crate::data::{Kind, Session, Source, TITLE_CHARS, Transcript, clip, contains, describe_tool, each_line, fence, one_line, parse_time, project_name, result_text, str_field};

pub struct Pi;

impl Agent for Pi {
    fn label(&self) -> &'static str {
        "Pi"
    }

    fn id(&self) -> &'static str {
        "pi"
    }

    fn logo(&self) -> &'static [Layer] {
        &logos::PI_LAYERS
    }

    fn sessions(&self, home: &Path) -> PathBuf {
        home.join(".pi/agent/sessions")
    }

    fn watched(&self, root: &Path) -> PathBuf {
        root.parent().unwrap_or(root).to_owned()
    }

    fn classify(&self, rest: &Path) -> Option<Found> {
        (rest.components().count() == 2 && rest.extension().is_some_and(|x| x == "jsonl")).then_some(Found::Session)
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

    /// A new entry can move the session to another branch.
    fn appends(&self) -> bool {
        false
    }

    /// A turn goes on while its last message isn't a reply that stopped.
    fn turn(&self, line: &[u8]) -> Option<bool> {
        if !contains(line, b"\"message\"") {
            return None;
        }
        let entry: Value = serde_json::from_slice(line).ok()?;
        let message = &entry["message"];
        match message["role"].as_str()? {
            "user" | "toolResult" => Some(true),
            "assistant" => Some(message["stopReason"] == "toolUse"),
            // A shell command of the user's isn't the agent's.
            _ => None,
        }
    }

    fn searchable(&self, record: &Value, _: usize) -> Option<(String, Vec<String>)> {
        searchable(record)
    }

    fn start(&self, _: &str) -> (Vec<String>, Option<String>) {
        (vec!["pi".to_owned()], None)
    }

    fn resume(&self, id: &str) -> Vec<String> {
        ["pi", "--session", id].map(str::to_owned).to_vec()
    }

    fn hyper_args(&self, prompt: &str) -> Vec<String> {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
        let extension = extension(&home.join("Library/Application Support/Hyper")).map(|e| ["-e".to_owned(), e]);
        extension.into_iter().flatten().chain(["--append-system-prompt".to_owned(), prompt.to_owned()]).collect()
    }

    /// Its extension's dialogs, such as a permission gate's, wait for the user.
    fn reports(&self, event: Event) -> bool {
        matches!(event, Event::TurnCompleted | Event::ApprovalRequested | Event::InputRequested)
    }

    fn config(&self, home: &Path) -> Vec<ConfigItem> {
        if home.join(".pi/agent").is_dir() { config(home) } else { Vec::new() }
    }
}

/// A Pi extension that tells Hyper, in OSC 777 notifications titled the way Claude's hooks
/// title theirs, which session it's in whenever that changes (startup, `/new`, `/resume`,
/// `/fork`), when it starts and stops working, and when an extension's dialog waits for the
/// user. Its session file can't say that last one: it ends on the call being asked about.
const EXTENSION: &str = r#"export default function (pi: any) {
  const say = (what: string, body = "") => process.stdout.write(`\x1b]777;notify;Hyper:${what};${String(body).replace(/[\x00-\x1f\x7f]/g, " ")}\x07`);
  pi.on("session_start", (_event: any, ctx: any) => {
    if (ctx.sessionManager.getSessionFile()) say("session", ctx.sessionManager.getSessionId());
  });
  pi.on("agent_start", () => say("working"));
  pi.on("agent_settled", () => say("turn_completed", "Turn completed"));
  pi.on("ui_prompt_start", (event: any) => {
    const approval = event.kind === "confirm";
    say(approval ? "approval_requested" : "input_requested", event.title || (approval ? "Approval requested" : "Input requested"));
  });
  pi.on("ui_prompt_end", (_event: any, ctx: any) => {
    if (!ctx.isIdle()) say("working");
  });
}
"#;

/// Writes the extension into `dir`, where `pi -e` can load it, and returns its path.
fn extension(dir: &Path) -> Option<String> {
    let path = dir.join("hyper-pi.ts");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(EXTENSION) {
        std::fs::create_dir_all(dir).ok()?;
        // Written aside and moved in, so a Pi starting meanwhile never loads half of it.
        let partial = path.with_extension(format!("{}.tmp", std::process::id()));
        std::fs::write(&partial, EXTENSION).ok()?;
        std::fs::rename(&partial, &path).ok()?;
    }
    path.to_str().map(str::to_owned)
}

pub fn files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let Ok(folders) = std::fs::read_dir(root) else { return files };
    for folder in folders.flatten() {
        let Ok(entries) = std::fs::read_dir(folder.path()) else { continue };
        files.extend(entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "jsonl")));
    }
    files
}

pub fn read_session(path: &Path) -> Option<Session> {
    let meta = std::fs::metadata(path).ok()?;
    let (mut id, mut cwd, mut prompt, mut name) = (String::new(), String::new(), None, None);
    each_line(path, |number, line| {
        if number == 1 {
            if let Ok(header) = serde_json::from_slice::<Value>(line) {
                id = str_field(&header, "id").to_owned();
                cwd = str_field(&header, "cwd").to_owned();
            }
        } else if contains(line, b"\"session_info\"") {
            if let Ok(info) = serde_json::from_slice::<Value>(line) && info["type"] == "session_info" {
                name = Some(one_line(str_field(&info, "name"), TITLE_CHARS)).filter(|n| !n.is_empty());
            }
        } else if prompt.is_none() && contains(line, b"\"role\":\"user\"") && let Ok(entry) = serde_json::from_slice::<Value>(line) {
            prompt = user_text(&entry["message"]);
        }
        true
    })
    .ok()?;
    if id.is_empty() {
        return None;
    }
    let title = name.or_else(|| prompt.as_deref().map(|p| one_line(p, TITLE_CHARS)))?;
    let updated = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
    Some(Session { id, source: Source::Pi, path: path.to_owned(), project: project_name(&cwd), cwd, title, branch: String::new(), updated, size: meta.len(), turn: crate::data::turn(path, Source::Pi, updated) })
}

fn user_text(message: &Value) -> Option<String> {
    if message["role"] != "user" {
        return None;
    }
    let text = match &message["content"] {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n\n"),
        _ => return None,
    };
    clean_prompt(&text)
}

/// A `/skill:name` invocation expands to the skill's whole text ahead of what was typed.
pub fn clean_prompt(raw: &str) -> Option<String> {
    let text = raw.trim();
    if let Some(name) = text.strip_prefix("<skill name=\"").and_then(|t| t.split('"').next()) {
        let typed = text.rsplit_once("</skill>").map_or("", |(_, rest)| rest).trim();
        return Some(if typed.is_empty() { format!("/skill:{name}") } else { typed.to_owned() });
    }
    (!text.is_empty()).then(|| text.to_owned())
}

pub fn load(path: &Path) -> Result<Transcript, String> {
    let mut out = Transcript::default();
    let mut parents: HashMap<String, String> = HashMap::new();
    let mut last = String::new();
    each_line(path, |_, line| {
        let Ok(entry) = serde_json::from_slice::<Value>(line) else { return true };
        if entry["type"] == "session" {
            out.started = parse_time(str_field(&entry, "timestamp"));
            return true;
        }
        let id = str_field(&entry, "id");
        if !id.is_empty() {
            parents.insert(id.to_owned(), str_field(&entry, "parentId").to_owned());
            last = id.to_owned();
        }
        add_entry(&mut out, &entry);
        true
    })?;
    // The session as it stands: the path from its last entry back to the root.
    let mut live = HashSet::new();
    let mut at = last.as_str();
    while !at.is_empty() && live.insert(at.to_owned()) {
        at = parents.get(at).map_or("", String::as_str);
    }
    out.rows.retain(|r| r.id.split(':').next().is_some_and(|entry| live.contains(entry)));
    out.recount();
    Ok(out)
}

fn add_entry(out: &mut Transcript, entry: &Value) {
    let entry_id = str_field(entry, "id");
    let id = |i: usize| format!("{entry_id}:{i}");
    if entry["type"] == "compaction" {
        out.push(id(0), Kind::Compacted, str_field(entry, "summary").to_owned());
        return;
    }
    if entry["type"] != "message" {
        return;
    }
    let message = &entry["message"];
    // Every branch counts, even one the session has since left: its tokens were spent.
    add_usage(&mut out.usage, message);
    match str_field(message, "role") {
        "user" => {
            if let Some(text) = user_text(message) {
                out.push(id(0), Kind::User, text);
            }
        }
        "assistant" => {
            let model = str_field(message, "model");
            if !model.is_empty() {
                out.model = model.to_owned();
            }
            let Some(blocks) = message["content"].as_array() else { return };
            for (i, block) in blocks.iter().enumerate() {
                match str_field(block, "type") {
                    "text" => {
                        let text = str_field(block, "text").trim();
                        if !text.is_empty() {
                            out.push(id(i), Kind::Assistant, text.to_owned());
                        }
                    }
                    "thinking" => {
                        let text = str_field(block, "thinking").trim();
                        if !text.is_empty() {
                            out.push(id(i), Kind::Thinking, text.to_owned());
                        }
                    }
                    "toolCall" => out.call(str_field(block, "id"), id(i), describe_tool(str_field(block, "name"), &block["arguments"])),
                    _ => {}
                }
            }
        }
        "toolResult" => out.result(str_field(message, "toolCallId"), result_text(&message["content"]), message["isError"] == true),
        // A command the user ran with `!`: it reads like the agent's own bash call.
        "bashExecution" => {
            let command = str_field(message, "command");
            let mut tool = describe_tool("bash", &serde_json::json!({ "command": command }));
            tool.input = fence("sh", &clip(command));
            tool.result = Some(clip(str_field(message, "output")));
            tool.error = message["exitCode"].as_i64().is_some_and(|code| code != 0);
            out.push(id(0), Kind::Tool(tool), String::new());
        }
        _ => {}
    }
}

fn add_usage(usage: &mut Usage, message: &Value) {
    match str_field(message, "role") {
        "user" => {
            let blocks = message["content"].as_array().map(Vec::as_slice).unwrap_or_default();
            let texts = blocks.iter().filter_map(|b| b["text"].as_str()).chain(message["content"].as_str());
            for name in texts.filter_map(|t| t.trim_start().strip_prefix("<skill name=\"")?.split('"').next()) {
                usage.skill(name);
            }
        }
        "assistant" => {
            let tokens = &message["usage"];
            if tokens.is_object() {
                usage.request("", request(str_field(message, "model"), tokens));
            }
            for block in message["content"].as_array().into_iter().flatten().filter(|b| b["type"] == "toolCall") {
                usage.mcp_tool(str_field(block, "id"), str_field(block, "name"));
                usage.skills_read(&block["arguments"].to_string());
            }
        }
        _ => {}
    }
}

fn request(model: &str, tokens: &Value) -> Request {
    let count = |key: &str| tokens[key].as_u64().unwrap_or(0);
    let cost = &tokens["cost"];
    let dollars = |key: &str| cost[key].as_f64().unwrap_or(0.0);
    Request {
        model: model.to_owned(),
        tokens: Tokens { input: count("input"), output: count("output"), cache_read: count("cacheRead"), cache_write: count("cacheWrite") },
        cache_write_1h: count("cacheWrite1h"),
        fast: false,
        stored: cost.is_object().then(|| Costs { input: dollars("input"), output: dollars("output"), cache_read: dollars("cacheRead"), cache_write: dollars("cacheWrite") }),
    }
}

/// An entry's id and the text a reader sees in it.
pub fn searchable(entry: &Value) -> Option<(String, Vec<String>)> {
    if entry["type"] != "message" {
        return None;
    }
    let message = &entry["message"];
    let texts: Vec<String> = match str_field(message, "role") {
        "user" => user_text(message).into_iter().collect(),
        "assistant" => {
            message["content"].as_array()?.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str().map(str::to_owned)).collect()
        }
        _ => return None,
    };
    Some((str_field(entry, "id").to_owned(), texts))
}

fn config(home: &Path) -> Vec<ConfigItem> {
    let source = Source::Pi;
    let root = home.join(".pi/agent");
    let settings_path = root.join("settings.json");
    let settings = read_json(&settings_path).unwrap_or(Value::Null);
    let mut items = Vec::new();

    items.extend(rules_file(source, &root.join("AGENTS.md")));

    items.extend(json_file(source, "Settings", &settings_path));
    items.extend(json_file(source, "Settings", &root.join("keybindings.json")));
    items.extend(json_file(source, "Settings", &root.join("advisor.json")));

    let mcp_path = root.join("mcp.json");
    let servers = mcp_servers(source, &read_json(&mcp_path).unwrap_or(Value::Null)["mcpServers"], &mcp_path, false);
    if servers.is_empty() {
        items.push(none(source, "MCP servers", &mcp_path));
    }
    items.extend(servers);

    let mut found = skills(source, &root.join("skills"), "");
    found.extend(skills(source, &home.join(".agents/skills"), "Shared in ~/.agents"));
    items.extend(without_repeats(found));

    for package in settings["packages"].as_array().map(Vec::as_slice).unwrap_or_default() {
        let name = package.as_str().or(package["source"].as_str()).unwrap_or("package");
        let title = name.strip_prefix("npm:").unwrap_or(name);
        items.push(item(source, "Packages", title, "", Some(&settings_path), masked_json(package)));
    }
    for path in sorted_dir(&root.join("extensions")) {
        items.push(item(source, "Extensions", file_name(&path), "", Some(&path), format!("`{}`", path.display())));
    }
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

    fn entry(id: &str, parent: &str, message: Value) -> Value {
        json!({"type": "message", "id": id, "parentId": parent, "timestamp": "2026-01-02T03:04:05Z", "message": message})
    }

    fn user(id: &str, parent: &str, text: &str) -> Value {
        entry(id, parent, json!({"role": "user", "content": [{"type": "text", "text": text}]}))
    }

    fn reply(id: &str, parent: &str, text: &str) -> Value {
        entry(id, parent, json!({"role": "assistant", "model": "gpt-x", "content": [{"type": "text", "text": text}]}))
    }

    #[test]
    fn extension_is_written_into_a_new_folder_and_kept_current() {
        let dir = std::env::temp_dir().join(format!("hyper-pi-ext-{}", std::process::id())).join("Hyper");
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
        let path = extension(&dir).expect("creates the folder");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION);
        std::fs::write(&path, "stale").unwrap();
        assert_eq!(extension(&dir).as_deref(), Some(path.as_str()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), EXTENSION, "an older build's copy is replaced");
        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    fn write(name: &str, lines: &[Value]) -> (PathBuf, Roots) {
        let root = std::env::temp_dir().join(format!("sessions-pi-{name}-{}", std::process::id()));
        let folder = root.join("--src-app--");
        std::fs::create_dir_all(&folder).unwrap();
        let header = json!({"type": "session", "id": "p1", "cwd": "/src/app", "timestamp": "2026-01-02T03:04:05Z"});
        let body: Vec<String> = std::iter::once(&header).chain(lines).map(Value::to_string).collect();
        std::fs::write(folder.join("p1.jsonl"), body.join("\n")).unwrap();
        let roots = Roots::under(&root.join("none")).with(Source::Pi, root.clone());
        (root, roots)
    }

    #[test]
    fn a_rewound_session_shows_only_the_branch_it_ended_on() {
        let (root, roots) = write("branch", &[
            user("a", "", "first"),
            reply("b", "a", "abandoned reply"),
            reply("c", "a", "kept reply"),
        ]);
        let index = data::load_index(&roots).unwrap();
        let t = data::load_transcript(&index[0]).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(t.rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(), ["first", "kept reply"]);
        assert_eq!((t.replies, t.model.as_str()), (1, "gpt-x"));
    }

    #[test]
    fn a_session_name_beats_the_first_prompt() {
        let (root, roots) = write("name", &[user("a", "", "the prompt"), json!({"type": "session_info", "id": "i", "parentId": "a", "name": "Named"})]);
        let index = data::load_index(&roots).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!((index[0].title.as_str(), index[0].project.as_str(), index[0].source), ("Named", "app", Source::Pi));
    }

    #[test]
    fn tool_results_and_user_shell_commands_read_as_tools() {
        let (root, roots) = write("tools", &[
            user("a", "", "go"),
            entry("b", "a", json!({"role": "assistant", "content": [{"type": "toolCall", "id": "t1", "name": "read", "arguments": {"path": "/x/y.rs"}}]})),
            entry("c", "b", json!({"role": "toolResult", "toolCallId": "t1", "content": [{"type": "text", "text": "nope"}], "isError": true})),
            entry("d", "c", json!({"role": "bashExecution", "command": "git status", "output": "clean", "exitCode": 1})),
        ]);
        let index = data::load_index(&roots).unwrap();
        let t = data::load_transcript(&index[0]).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        let Kind::Tool(read) = &t.rows[1].kind else { panic!() };
        assert_eq!((read.summary.as_str(), read.result.as_deref(), read.error), ("y.rs", Some("nope"), true));
        let Kind::Tool(bash) = &t.rows[2].kind else { panic!() };
        assert_eq!((bash.summary.as_str(), bash.result.as_deref(), bash.error), ("git status", Some("clean"), true));
    }

    #[test]
    fn a_skill_shows_as_what_was_typed() {
        assert_eq!(clean_prompt("<skill name=\"deep\" location=\"/x\">\nbody\n</skill>\n\nlook at this").as_deref(), Some("look at this"));
        assert_eq!(clean_prompt("<skill name=\"deep\" location=\"/x\">\nbody\n</skill>").as_deref(), Some("/skill:deep"));
    }

    #[test]
    fn search_points_at_the_matching_entry() {
        let (root, roots) = write("search", &[user("a", "", "hello"), reply("b", "a", "zebrafish here")]);
        let index = data::load_index(&roots).unwrap();
        let hits = data::search(&index, "zebrafish", &Arc::new(AtomicU64::new(0)), 0);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!((hits.len(), hits[0].uuid.as_str()), (1, "b"));
    }

    #[test]
    fn its_stored_cost_is_used_and_a_left_branch_still_counts() {
        let used = |id: &str, parent: &str, cost: f64| entry(id, parent, json!({"role": "assistant", "model": "gpt-x", "content": [],
            "usage": {"input": 10, "output": 5, "cacheRead": 100, "cacheWrite": 0, "cost": {"input": cost, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": cost}}}));
        let (root, roots) = write("usage", &[user("a", "", "first"), used("b", "a", 1.5), used("c", "a", 0.25)]);
        let index = data::load_index(&roots).unwrap();
        let t = data::load_transcript(&index[0]).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        let report = Report::new(Source::Pi, &t.usage, &t.agents, &Prices::default());
        assert_eq!(report.total.cost.total(), 1.75);
        assert_eq!((report.total.tokens.input, report.total.unpriced), (20, 0));
    }

    #[test]
    fn a_skill_is_the_one_expanded_into_a_prompt_or_read_from_its_file() {
        let read = entry("b", "a", json!({"role": "assistant", "content": [{"type": "toolCall", "id": "t1", "name": "read", "arguments": {"path": "/x/skills/rex/SKILL.md"}}]}));
        let (root, roots) = write("skills", &[user("a", "", "<skill name=\"deep\" location=\"/x\">\nbody\n</skill>\n\nlook"), read]);
        let index = data::load_index(&roots).unwrap();
        let t = data::load_transcript(&index[0]).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        let report = Report::new(Source::Pi, &t.usage, &t.agents, &Prices::default());
        assert_eq!(report.skills, [("deep".to_owned(), 1), ("rex".to_owned(), 1)]);
    }
}
