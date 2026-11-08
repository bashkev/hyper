//! What agents started here are told about Hyper, added to their system prompt the way Pi
//! points at its own docs: where they are, and to read them only when asked.

use std::path::{Path, PathBuf};

/// The note for an agent started by this copy of Hyper.
pub fn text() -> String {
    note(docs().as_deref(), &crate::access::missing())
}

fn note(docs: Option<&Path>, missing: &[&str]) -> String {
    let mut text = "You are running in a terminal inside Hyper, a Mac app for running Claude Code, Codex and Pi.".to_owned();
    if let Some(docs) = docs {
        text.push_str(&format!("\nHyper documentation (read only when the user asks about Hyper, its settings, shortcuts or features): {}", docs.display()));
    }
    // Agents testing a UI otherwise retry osascript and screencapture, failing each time.
    if docs.is_some() && !missing.is_empty() {
        let cant: Vec<_> = missing.iter().map(|m| match *m {
            "Accessibility" => "control apps (osascript, System Events, keystrokes)",
            _ => "capture the screen (screencapture)",
        }).collect();
        let (missing, cant) = (missing.join(" and "), cant.join(" or "));
        text.push_str(&format!("\nHyper lacks macOS {missing} permission, so commands here can't {cant}. Don't retry them: test another way, or ask the user to allow Hyper under {missing} in System Settings › Privacy & Security, then start a new session."));
    }
    text
}

/// The README `scripts/bundle.sh` puts in the app's Resources. A bare build has none.
fn docs() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let readme = exe.parent()?.parent()?.join("Resources/README.md");
    readme.is_file().then_some(readme)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_hear_which_permissions_hyper_lacks() {
        let docs = Path::new("/Applications/Hyper.app/Contents/Resources/README.md");
        let note = note(Some(docs), &["Accessibility", "Screen Recording"]);
        assert!(note.contains("lacks macOS Accessibility and Screen Recording permission"), "{note}");
        assert!(note.contains("can't control apps") && note.contains("or capture the screen"), "{note}");
        let screen = super::note(Some(docs), &["Screen Recording"]);
        assert!(screen.contains("can't capture the screen") && !screen.contains("control apps"), "{screen}");
        assert!(!super::note(Some(docs), &[]).contains("permission"), "with both granted there's nothing to say");
        assert!(!super::note(None, &["Accessibility"]).contains("permission"), "a bare build has its terminal's permissions");
    }
}
