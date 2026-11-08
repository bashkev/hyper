# Hyper

A Mac app for running Claude Code, Codex and Pi in real terminals, and browsing their sessions and global setup, built with [Rune](https://github.com/anomalyco/rune).

## Install

Building needs macOS, [Rust](https://rustup.rs), the Xcode Command Line Tools and [Zig](https://ziglang.org) 0.17 for the terminal engine.

```bash
git clone https://github.com/bashkev/hyper && cd hyper
scripts/bundle.sh --open   # builds target/Hyper.app, which you can keep in Applications
cargo run --release        # or run it without a bundle
```

The bundle is signed with your first Apple Development certificate, or `HYPER_SIGN_IDENTITY`, so macOS keeps the permissions you grant it across builds. Without one it's signed ad hoc. Set `HYPER_BUNDLE_ID` to build under your own identifier.

## Starting sessions

- **Launcher** (⇧⌘N): type to find a project or a recent session, Tab or ⇧Tab picks Claude, Codex, Pi or Shell, and Return starts it, or resumes the session. Esc goes back. It opens over the terminal on screen, which keeps running, and fills the window when nothing is running.
- **New session** (⌘N): the current session's tool in the same directory, including its worktree. With no session open, it opens the launcher.
- **Choose Folder…** (⌘O): start in any folder.
- **Sidebar**: the + on a project or worktree starts your default tool there. Right-click it for the other tools, or a new worktree. Everything running is listed here and keeps running behind the one on screen.

### Worktrees

Any tool can start in a fresh git worktree at `<repo>/.claude/worktrees/<name>` on branch `worktree-<name>`, where Claude Code keeps its own.

- ⌘Return in the launcher starts in one with a random name. ⇧⌘Return asks for the name: leave it empty for a random one, or type an existing one to go back to it.
- With "Start in a new worktree" on in Settings, ⌘Return starts without one instead.
- Worktrees are listed under their project in the sidebar. When the last terminal in one closes, it's removed if nothing would be lost. If it has uncommitted files, or commits on no other branch, you're asked first. Worktrees you keep stay in the launcher.

## Working in a session

- Drag to select text. Double-click selects a word, triple-click a line, and holding Option selects a rectangle. ⌘C copies and ⌘V pastes.
- ⌘-click a link to open it: a URL in the text, even one the terminal wrapped, or a link a program attached to its text. Web pages open in the Browser pane, which takes the keyboard, or your default browser if Settings says so.
- A spinner means an agent is working, and a green dot means it's waiting for you. An orange dot means it finished or asked for something while you were elsewhere. Looking at it clears the dot.
- A clean exit closes a terminal. One that fails stays open to read. Quitting asks first if an agent is running, then stops it.

### Files and Changes

Files (⇧⌘E) and Changes (⇧⌘G) open a resizable pane beside the terminal. Both follow the selected session's repository or worktree and update as files change.

- **Files**: a read-only, syntax-colored preview with a file picker. Click a file to open a tab. Filter by path, or start with `?` to search file contents. ⌘F finds in the file, and ⌘W closes its tab when the pane has focus. Breadcrumbs browse sibling files. Right-click to copy a path, reveal it in Finder, or open it in your text editor. The menu toggles word wrap and Git-ignored files. Dotfiles are always shown.
- **Changes**: staged, unstaged and untracked files, with unified or split diffs.

### Browser

Browser (⇧⌘L) opens a web page in the same pane, using Safari's engine. ⌘L goes to the address bar. Type a URL, a port (`3000` opens `http://localhost:3000`), a file path, or anything else to search Google. Links that open a new window open here, and other apps' links (`mailto:`) go to macOS. Sign-ins stay between launches, right-click has Inspect Element, and when a page can't load, Try Again retries it, so a dev server that's still starting is one click away. ⌘F finds in the page: Return and ⇧Return go to the next and previous match, and Escape closes it. When the page has focus, ⌘R reloads it, ⌘W closes the page and the pane, as a browser closes a tab, and ⌘B, ⌘U, ⌘J, ⇧⌘U and ⇧⌘J go to the page, so a web editor can bold and underline. ⇧⌘L hides the page and shows it again as it was. A reload of Hyper keeps the page.

### Notifications

Unless Hyper is in front, a terminal that needs you posts a notification with a sound, which takes you to it, and adds to the count on the Dock icon. Settings changes this for each agent.

## Browse

Browse (⌃⌘2) lists every session, grouped by project and filtered by tool, and each tool's rules, settings, MCP servers, skills, plugins and hooks. Config is read again each time you open it.

- **Search** (⌘F or ⌘K) covers what you typed and each agent's replies, not tool output, thinking or subagent transcripts. In Config, it searches the config.
- **Details** (⌥⌘I) show a session's tokens, cost, the skills and MCP servers it called, and how much of that was its subagents. Claude and Codex costs are tokens at today's API prices from Pi's `models-store.json`, whatever plan paid for them. Pi's costs are what Pi stored at the time. A `+` means a model with no listed price was left out. Codex and Pi skills are worked out from `SKILL.md` reads.
- Sessions update live. A running session opens at its end and follows it until you scroll up. Claude Code's status comes from its running processes, Codex's from its session files, and Pi's from what its extension tells Hyper when it runs here, or its session files otherwise. A Codex or Pi session running outside Hyper shows a spinner for at most 30 seconds after a write.
- `auth.json` and `mcp-auth.json` are never opened. Env vars, headers and anything that looks like a credential are masked, and a settings file that doesn't parse isn't shown.

## Settings

Settings (⌘,) saves every change at once to `~/Library/Application Support/Hyper/settings.json`. Hyper reads that file only at launch and writes over it on the next change, so change settings in the app rather than in the file.

- **General**: open maximized, remember the last project, start in a new worktree, the default tool, and whether links open in Hyper or your default browser.
- **Appearance**: interface font and size for navigation; monospace font and content size for terminals, files, diffs and transcripts. Transcript prose uses the interface font at content size. Line spacing applies to all content.
- **Shortcuts**: click one and press the new keys. Backspace clears it and Escape cancels. A shortcut another command has, one macOS keeps, or the launcher's ⌘Return and ⇧⌘Return is refused, with the reason. Only the ones you change are saved.
- **Notifications**: for each agent, when to notify (app in background, not looking at it, or always), the Dock badge, and Off, Silent or Sound for each event: turn completed, approval requested, input requested, idle reminder, other agent notifications, terminal bell, session exited and session failed.

## Shortcuts

These are the defaults. Change them in Settings.

| Keys | Action |
|---|---|
| ⌘N | New session with the current tool and directory |
| ⇧⌘N | Launcher |
| ⌘O | Choose a folder to start in |
| ⌘W | Close the launcher, or the session on screen |
| ⌘1 to ⌘8, ⌘9 | Running sessions in sidebar order; ⌘9 is the last |
| ⇧⌘] / ⇧⌘[ | Next / previous session |
| ⇧⌘A | Next session that needs you |
| ⌘U / ⌘J | Page up / down in the session |
| ⇧⌘U / ⇧⌘J | Scroll the session to the top / bottom |
| ⌘F (or ⌘K) | Search; Esc clears it |
| ⌃⌘1 / ⌃⌘2 | Workspace / Browse |
| ⌘B (or ⇧⌘B, ⌃⌘S) | Toggle the sidebar |
| ⌥⌘I | Toggle details |
| ⇧⌘E / ⇧⌘G | Toggle Files / Changes |
| ⇧⌘L | Toggle Browser |
| ⌘L | Browser address bar |
| ⇧⌘T | Show or hide tool calls in a transcript |
| ⌘= (or ⌘+), ⌘-, ⌘0 | Shared content size: terminals, files, diffs and transcripts |
| ⌥⌘= / ⌥⌘- | Size of the interface text |
| ⌘R | Reload sessions and config |
| ⌃⌘R | Build and reload Hyper |
| ⌘, | Settings |

## What Hyper adds to agents

Agents started here get a few extra arguments. Your own settings files are left untouched.

- **Claude**: hooks for its turn ending, approval and input requests and idle reminders, passed with `--settings`.
- **Codex**: `-c tui.notifications=true`, so it says when a turn ends or needs approval.
- **Pi**: an extension (`-e`) that tells Hyper which session it's in, when it starts and finishes working, and when another extension's dialog, such as a permission prompt, waits for you.
- **All three**: a note in the system prompt that they're running in Hyper, with the path to this README inside the app. Claude and Pi get it with `--append-system-prompt`, and Codex with `-c developer_instructions`. Claude and Codex keep the prompt a session started with, so sessions begun elsewhere don't get it when resumed here.

Every terminal has `HYPER_PID` set to the app's process id.

## Reloading

`scripts/bundle.sh --reload` builds and has the running Hyper take the new build over in place. ⌃⌘R does the same from inside Hyper, and shows the errors if the build fails. A copy moved out of `target/` has nothing to build, so ⌃⌘R reloads it with the build on disk, as `kill -USR1 $HYPER_PID` does from a terminal inside Hyper. Programs keep running through it, mid-turn agents included, and their screens and scrollback come back. Quitting or relaunching Hyper instead stops every terminal in it.

## Development

Rendering headless on your own data:

```bash
target/release/hyper --scene real --snap out.png --size 1240x800 query=worktree
```

## License

MIT. See `LICENSE`.
