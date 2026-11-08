//! Tool-call summaries and safe Markdown formatting, independent of file I/O and UI.

use serde_json::Value;

use super::{one_line, str_field};

const MAX_TOOL_TEXT: usize = 12_000;

#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub name: String,
    pub summary: String,
    /// Markdown for the call's input.
    pub input: String,
    pub result: Option<String>,
    pub error: bool,
}

/// A tool result's content: a string, or parts of text and media.
pub fn result_text(content: &Value) -> String {
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|p| p["text"].as_str().map_or_else(|| format!("[{}]", str_field(p, "type")), str::to_owned))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    clip(&text)
}

pub fn clip(text: &str) -> String {
    if text.len() <= MAX_TOOL_TEXT {
        return text.to_owned();
    }
    let mut end = MAX_TOOL_TEXT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… {} more bytes", &text[..end], text.len() - end)
}

/// A fenced code block whose fence no line of `code` can close.
pub fn fence(lang: &str, code: &str) -> String {
    let mut run = 0;
    let mut longest = 2;
    for c in code.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let ticks = "`".repeat(longest + 1);
    format!("{ticks}{lang}\n{}\n{ticks}", code.trim_end_matches('\n'))
}

fn diff(old: &str, new: &str) -> String {
    let removed = old.lines().map(|l| format!("-{l}"));
    let added = new.lines().map(|l| format!("+{l}"));
    removed.chain(added).collect::<Vec<_>>().join("\n")
}

fn base_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

/// The first file an `apply_patch` touches, and how many more.
fn patch_summary(patch: &str) -> String {
    const HEADERS: [&str; 3] = ["*** Update File: ", "*** Add File: ", "*** Delete File: "];
    let files: Vec<&str> = patch.lines().filter_map(|l| HEADERS.iter().find_map(|h| l.strip_prefix(h))).collect();
    match files.as_slice() {
        [] => "patch".to_owned(),
        [one] => base_name(one),
        [first, rest @ ..] => format!("{} and {} more", base_name(first), rest.len()),
    }
}

/// How a call reads in the transcript, for any of the tools' names. Custom tools pass their
/// input as a plain string.
pub fn describe_tool(name: &str, input: &Value) -> Tool {
    let s = |key: &str| str_field(input, key);
    let path = || if s("file_path").is_empty() { s("path") } else { s("file_path") };
    let (summary, markdown) = match name {
        "Bash" | "bash" => {
            let summary = if s("description").is_empty() { s("command") } else { s("description") };
            (summary.to_owned(), fence("sh", &clip(s("command"))))
        }
        "exec_command" => (s("cmd").to_owned(), fence("sh", &clip(s("cmd")))),
        "Edit" => (base_name(path()), fence("diff", &clip(&diff(s("old_string"), s("new_string"))))),
        "edit" => {
            let edits = input["edits"].as_array().map(Vec::as_slice).unwrap_or_default();
            let body: Vec<String> = edits.iter().map(|e| diff(str_field(e, "oldText"), str_field(e, "newText"))).collect();
            (base_name(path()), fence("diff", &clip(&body.join("\n"))))
        }
        "apply_patch" => {
            let patch = input.as_str().unwrap_or(s("input"));
            (patch_summary(patch), fence("diff", &clip(patch)))
        }
        "Write" | "write" => {
            let lang = path().rsplit('.').next().unwrap_or("");
            (base_name(path()), fence(lang, &clip(s("content"))))
        }
        "Read" | "read" => (base_name(path()), String::new()),
        "Grep" | "Glob" => (s("pattern").to_owned(), String::new()),
        "WebSearch" | "web_search" => (s("query").to_owned(), String::new()),
        "WebFetch" => (s("url").to_owned(), String::new()),
        "Agent" | "Task" => (s("description").to_owned(), fence("", &clip(s("prompt")))),
        "js" if !s("code").is_empty() => (s("title").to_owned(), fence("js", &clip(s("code")))),
        _ if input.is_string() => {
            let text = input.as_str().unwrap_or("");
            (text.to_owned(), fence("js", &clip(text)))
        }
        _ => {
            let json = serde_json::to_string_pretty(input).unwrap_or_default();
            (one_line(&json, 120), fence("json", &clip(&json)))
        }
    };
    Tool { name: short_tool(name), summary: one_line(&summary, 160), input: markdown, result: None, error: false }
}

/// `mcp__computer-use__left_click` reads as `left_click`.
fn short_tool(name: &str) -> String {
    name.strip_prefix("mcp__").and_then(|n| n.rsplit("__").next()).unwrap_or(name).to_owned()
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn an_edit_reads_as_a_diff_for_claude_and_pi() {
        let claude = describe_tool("Edit", &json!({"file_path": "/a/b.rs", "old_string": "x", "new_string": "y"}));
        let pi = describe_tool("edit", &json!({"path": "/a/b.rs", "edits": [{"oldText": "x", "newText": "y"}]}));
        assert_eq!(claude.summary, "b.rs");
        assert_eq!(claude.input, "```diff\n-x\n+y\n```");
        assert_eq!((pi.summary, pi.input), (claude.summary, claude.input));
    }

    #[test]
    fn a_patch_names_its_files() {
        let patch = "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-x\n+y\n*** Add File: src/b.rs\n+z\n*** End Patch";
        assert_eq!(describe_tool("apply_patch", &json!(patch)).summary, "a.rs and 1 more");
        assert_eq!(describe_tool("apply_patch", &json!({"input": "*** Delete File: c.rs"})).summary, "c.rs");
    }

    #[test]
    fn a_fence_outlasts_the_backticks_in_its_code() {
        assert_eq!(fence("md", "a\n````\nb"), "`````md\na\n````\nb\n`````");
    }

    #[test]
    fn clipped_tool_text_says_how_much_is_missing() {
        let text = "é".repeat(MAX_TOOL_TEXT);
        let clipped = clip(&text);
        assert!(clipped.len() < text.len());
        assert!(clipped.ends_with(&format!("{} more bytes", text.len() - MAX_TOOL_TEXT)));
    }

}
