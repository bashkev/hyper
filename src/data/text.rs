//! Small text transformations shared by the agent formats.

use serde_json::Value;

pub fn contains(hay: &[u8], needle: &[u8]) -> bool {
    memchr::memmem::find(hay, needle).is_some()
}

pub fn str_field<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}

pub fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)? + start;
    Some(&text[start..end])
}

pub fn parse_time(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text).ok().map(|t| t.timestamp())
}

/// The project a working folder belongs to, so a worktree lands with its repository:
/// `~/src/app/.claude/worktrees/x` and `~/.codex/worktrees/1a2b/app` are both `app`.
pub fn project_name(cwd: &str) -> String {
    let base = |path: &str| path.trim_end_matches('/').rsplit('/').next().unwrap_or(path).to_owned();
    if cwd.starts_with("/tmp") || cwd.starts_with("/private/") {
        return "tmp".to_owned();
    }
    // Codex Desktop gives each chat outside a project its own dated folder.
    if cwd.contains("/chatgpt-desktop-projectless-chats/") || cwd.contains("/Documents/Codex/") {
        return "No project".to_owned();
    }
    if let Some((repo, _)) = cwd.split_once("/.claude/worktrees/") {
        return base(repo);
    }
    base(cwd)
}

/// `text` on one line, whitespace collapsed, at most `max` characters.
pub fn one_line(text: &str, max: usize) -> String {
    let mut out = String::new();
    for (i, word) in text.split_whitespace().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(word);
        if out.len() > max * 4 {
            break;
        }
    }
    if out.chars().count() > max {
        out = out.chars().take(max.saturating_sub(1)).collect::<String>().trim_end().to_owned();
        out.push('…');
    }
    out
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_titles_cut_on_character_boundaries() {
        let title = one_line(&"é".repeat(200), 10);
        assert_eq!(title.chars().count(), 10);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn worktrees_group_with_their_repository() {
        assert_eq!(project_name("/Users/me/src/app"), "app");
        assert_eq!(project_name("/Users/me/src/app/.claude/worktrees/lovely-name"), "app");
        assert_eq!(project_name("/Users/me/.codex/worktrees/1a2b/app"), "app");
        assert_eq!(project_name("/private/tmp/claude-501/x/scratchpad"), "tmp");
        assert_eq!(project_name("/Users/me/Documents/Codex/2026-09-04/how"), "No project");
        assert_eq!(project_name("/Users/me/src/chatgpt-desktop-projectless-chats/2026-09-06/i"), "No project");
    }

}
