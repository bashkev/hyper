use super::*;
use std::sync::atomic::AtomicU64;
use serde_json::json;

fn session(name: &str, source: Source, body: &str) -> Session {
    let path = std::env::temp_dir().join(format!("sessions-tail-{name}-{}.jsonl", std::process::id()));
    std::fs::write(&path, body).unwrap();
    Session { id: name.to_owned(), source, path, project: String::new(), cwd: String::new(), title: String::new(), branch: String::new(), updated: 0, size: 0, turn: None }
}

fn append(session: &Session, text: &str) {
    let mut file = std::fs::OpenOptions::new().append(true).open(&session.path).unwrap();
    std::io::Write::write_all(&mut file, text.as_bytes()).unwrap();
}

fn catch_up(t: &mut Transcript, session: &Session) -> bool {
    let update = read_new(session, t.read, &t.agents).unwrap();
    t.apply(session.source, update)
}

fn claude_reply(uuid: &str) -> String {
    json!({"type": "assistant", "uuid": uuid, "message": {"content": [{"type": "text", "text": uuid}]}}).to_string()
}

fn ids(t: &Transcript) -> Vec<&str> {
    t.rows.iter().map(|r| r.id.as_str()).collect()
}

#[test]
fn a_line_caught_half_written_is_read_once_it_is_whole() {
    let line = claude_reply("b");
    let (start, end) = line.split_at(20);
    let s = session("half", Source::Claude, &(claude_reply("a") + "\n"));
    let mut t = load_transcript(&s).unwrap();
    assert_eq!(ids(&t), ["a:0"]);
    append(&s, start);
    assert!(!catch_up(&mut t, &s));
    append(&s, &format!("{end}\n"));
    assert!(catch_up(&mut t, &s));
    assert_eq!(ids(&t), ["a:0", "b:0"]);
    assert_eq!(t.replies, 2);
    assert!(!catch_up(&mut t, &s), "and only once");
    std::fs::remove_file(&s.path).unwrap();
}

#[test]
fn a_last_line_read_before_its_newline_is_not_read_twice() {
    let s = session("no-newline", Source::Claude, &claude_reply("a"));
    let mut t = load_transcript(&s).unwrap();
    assert_eq!(ids(&t), ["a:0"]);
    append(&s, &format!("\n{}\n", claude_reply("b")));
    catch_up(&mut t, &s);
    assert_eq!(ids(&t), ["a:0", "b:0"]);
    std::fs::remove_file(&s.path).unwrap();
}

#[test]
fn the_newline_ending_a_line_already_read_is_not_a_line_of_its_own() {
    let record = |text: &str| json!({"type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}}).to_string();
    let s = session("line-numbers", Source::Codex, &record("one"));
    let mut t = load_transcript(&s).unwrap();
    append(&s, &format!("\n{}\n", record("two")));
    catch_up(&mut t, &s);
    assert_eq!(ids(&t), ["L1:0", "L2:0"], "search scrolls to Codex rows by line");
    std::fs::remove_file(&s.path).unwrap();
}

#[test]
fn reading_on_gives_what_reading_it_all_gives() {
    let record = |text: &str| json!({"type": "response_item", "payload": {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": text}]}}).to_string();
    let s = session("same", Source::Codex, &format!("{}\n", record("one")));
    let mut t = load_transcript(&s).unwrap();
    append(&s, &format!("{{\"type\":\"event_msg\"}}\n{}\n", record("two")));
    catch_up(&mut t, &s);
    let whole = load_transcript(&s).unwrap();
    assert_eq!(ids(&t), ["L1:0", "L3:0"]);
    assert_eq!(t, whole, "row ids included, which search scrolls to by line");
    std::fs::remove_file(&s.path).unwrap();
}

#[test]
fn a_file_cut_shorter_is_read_again() {
    let s = session("cut", Source::Claude, &format!("{}\n{}\n", claude_reply("a"), claude_reply("b")));
    let mut t = load_transcript(&s).unwrap();
    std::fs::write(&s.path, format!("{}\n", claude_reply("c"))).unwrap();
    assert!(catch_up(&mut t, &s));
    assert_eq!(ids(&t), ["c:0"]);
    std::fs::remove_file(&s.path).unwrap();
}

#[test]
fn a_file_that_has_not_changed_is_not_read_again() {
    let s = session("still", Source::Pi, "{\"type\":\"session\",\"id\":\"p\"}\n");
    let t = load_transcript(&s).unwrap();
    assert!(matches!(read_new(&s, t.read, &t.agents), Ok(Update::Records { records, .. }) if records.is_empty()));
    std::fs::remove_file(&s.path).unwrap();
}

fn lines(records: &[Value]) -> Vec<u8> {
    records.iter().map(|r| format!("{r}\n")).collect::<String>().into_bytes()
}

#[test]
fn a_codex_turn_goes_on_until_it_completes_or_is_aborted() {
    let event = |kind: &str| json!({"type": "event_msg", "payload": {"type": kind}});
    let item = json!({"type": "response_item", "payload": {"type": "message", "content": "task_complete"}});
    let started = lines(&[event("task_complete"), event("task_started"), event("token_count"), item.clone()]);
    assert_eq!(turn_in(&started, false, Source::Codex), Some(true), "a reply mentioning an event isn't one");
    assert_eq!(turn_in(&lines(&[event("task_started"), event("task_complete")]), false, Source::Codex), Some(false));
    assert_eq!(turn_in(&lines(&[event("task_started"), event("turn_aborted")]), false, Source::Codex), Some(false));
    assert_eq!(turn_in(&lines(&[item]), false, Source::Codex), None);
}

#[test]
fn a_pi_turn_goes_on_until_a_reply_stops_without_calling_a_tool() {
    let message = |role: &str, stop: &str| json!({"type": "message", "message": {"role": role, "stopReason": stop, "content": []}});
    let pi = |records: &[Value]| turn_in(&lines(records), false, Source::Pi);
    assert_eq!(pi(&[message("assistant", "stop"), message("user", "")]), Some(true));
    assert_eq!(pi(&[message("user", ""), message("assistant", "toolUse"), message("toolResult", "")]), Some(true));
    assert_eq!(pi(&[message("user", ""), message("assistant", "toolUse")]), Some(true));
    assert_eq!(pi(&[message("user", ""), message("assistant", "stop")]), Some(false));
    assert_eq!(pi(&[message("assistant", "aborted"), message("bashExecution", "")]), Some(false), "a shell command of the user's starts no turn");
}

#[test]
fn a_line_cut_by_where_the_tail_starts_is_skipped() {
    let whole = lines(&[json!({"type": "event_msg", "payload": {"type": "task_started"}})]);
    let cut = [&b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}"[20..], b"\n", &whole].concat();
    assert_eq!(turn_in(&cut, true, Source::Codex), Some(true));
    assert_eq!(turn_in(&whole[5..], true, Source::Codex), None);
}

#[test]
fn a_resume_command_quotes_its_folder_and_uses_each_tools_flag() {
    let mut session = Session {
        id: "abc".into(),
        source: Source::Codex,
        path: PathBuf::new(),
        project: String::new(),
        cwd: "/a b/it's".into(),
        title: String::new(),
        branch: String::new(),
        updated: 0,
        size: 0,
        turn: None,
    };
    assert_eq!(session.resume_command(), r"cd '/a b/it'\''s' && codex resume abc");
    session.source = Source::Pi;
    assert!(session.resume_command().ends_with("pi --session abc"));
    session.source = Source::Claude;
    assert!(session.resume_command().ends_with("claude --resume abc"));
}

fn file_session(name: &str, body: &str) -> Session {
    let path = std::env::temp_dir().join(format!("sessions-data-{name}-{}.jsonl", std::process::id()));
    std::fs::write(&path, body).unwrap();
    Session {
        id: "s".into(),
        source: Source::Claude,
        path,
        project: String::new(),
        cwd: String::new(),
        title: String::new(),
        branch: String::new(),
        updated: 0,
        size: 0,
        turn: None,
    }
}

fn reply_line(text: &str) -> String {
    json!({"type": "assistant", "uuid": "a", "message": {"content": [{"type": "text", "text": text}]}}).to_string()
}

#[test]
fn a_superseded_search_stops() {
    let session = file_session("stops", &reply_line("the needle"));
    let sessions = [session];
    let current = search(&sessions, "needle", &Arc::new(AtomicU64::new(4)), 4);
    let stale = search(&sessions, "needle", &Arc::new(AtomicU64::new(5)), 4);
    std::fs::remove_file(&sessions[0].path).unwrap();
    assert_eq!((current.len(), stale.len()), (1, 0));
}

#[test]
fn search_finds_text_that_json_stores_escaped() {
    let session = file_session("escaped", &reply_line(r#"she said "hi" from C:\temp"#));
    let sessions = [session];
    let generation = Arc::new(AtomicU64::new(0));
    let quoted = search(&sessions, "\"hi\"", &generation, 0);
    let slashed = search(&sessions, r"C:\temp", &generation, 0);
    std::fs::remove_file(&sessions[0].path).unwrap();
    assert_eq!((quoted.len(), slashed.len()), (1, 1));
}

#[test]
fn missing_history_and_empty_history_both_load_as_an_empty_index() {
    let none = PathBuf::from("/nonexistent");
    let roots = Roots::under(&none);
    assert_eq!(load_index(&roots).unwrap(), Vec::new());
    let dir = std::env::temp_dir().join(format!("sessions-empty-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    assert_eq!(load_index(&roots.with(Source::Pi, dir.clone())).unwrap(), Vec::new());
    std::fs::remove_dir_all(dir).unwrap();
}
