# Hyper

## Rust

- Keep `cargo clippy --all-targets` clean, without `#[allow]` or `#[expect]`. `Cargo.toml` turns on `collapsible_if`, `uninlined_format_args` and `redundant_closure_for_method_calls`. `main` has no warnings, so don't assume a warning or test failure was already there.
- Avoid bool or ambiguous `Option` parameters that make callsites like `foo(false)` or `bar(None)`. Prefer enums, named methods or newtypes.
- Prefer exhaustive matches without wildcard arms over `matches!`, so a new enum variant has to be handled explicitly.
- Handle fallibility with `if let`, `let else` or `?`. Avoid `panic!`, `unreachable!` and `.unwrap()` outside tests and `Mutex::lock()`.
- Prefer let chains (`if let … && …`) over nested `if let`.
- Keep `unsafe` small and inside the native modules. Give every `unsafe` block a `// SAFETY:` comment in the usual style: one short line saying why the call is sound, e.g. `// SAFETY: a zeroed kevent is valid, and it's only written to.`
- Prefer top-level imports over local imports or fully qualified paths.
- Don't abbreviate names (`version`, not `ver`). The view conventions `s: &App`, `k: &Kit` and `tx` are fine.
- Give new traits doc comments that explain their role and what implementations must do.
- Refer to types as [`TypeName`] in doc comments.
- Prefer private modules.
- Don't create small helpers that are referenced only once.
- Keep modules under 500 LoC, excluding tests. Past roughly 800 LoC, put new functionality in a new module. When extracting code, move its tests and type docs with it.
- Change the lockfile only with `cargo update -p <crate> --precise <version>`, never a full `cargo update`. `rune` and `libghostty-rs` are pinned by `rev` in `Cargo.toml`.

## Comments and prose

- Write comments, commit messages and PR descriptions for readers who haven't seen our conversation. Don't mention prompts, iterations or what was tried.
- Explain current behavior and why. Avoid past-facing wording like "preserve the existing behavior"; name the actual compatibility constraint instead.

## Tests

- Check whether an existing test already covers the behavior before adding one.
- Run the specific tests you need rather than the whole suite.
- Compare whole objects rather than fields one by one.
- Don't test statically defined values or add negative tests for removed logic.

## Hyper

- You may be running inside `target/Hyper.app`. Relaunching it kills every terminal in it, including yours. Test with `scripts/bundle.sh --dev --open` (Hyper Dev.app) instead. Relaunch the real app only when asked.
- To load new code without stopping terminals, use `scripts/bundle.sh --dev --reload`. Drop `--dev` to reload the real app, but only when asked. ⌃⌘R builds and reloads too; `kill -USR1 $HYPER_PID` reloads the build on disk. A copy built before reloading existed refuses, and needs one normal relaunch.
- State that has to survive a reload goes in `hand_off_workspace`/`adopt_workspace` (`workspace/handoff.rs`) and `reload.rs`. A `Term` has to answer `Input::Handoff` without signalling its program.
- After changing the `libghostty-rs` rev, a reload may not restore screens: the snapshot format has no compatibility guarantee. Programs are then asked to redraw (SIGWINCH), and shells lose their scrollback.
- `App` (`main.rs`) holds each part's state in its own field (`reader`, `search`, `config`, `stale`, `ws`, `project`). Each module adds that part's behaviour with `impl App` and its view functions next to it; `main.rs` only wires them up.
- What differs between agents goes in its module (`claude.rs`, `codex.rs`, `pi.rs`) behind the `Agent` trait in `agent.rs`. Don't match on `Source` elsewhere.
- Check UI headless first: `target/release/hyper --scene real --snap out.png --size 1240x800 [session=<id>] [query=<text>] [settings]`.

## Rune

- Code can only focus text elements. Keep the one `terminal` element mounted, and give clickable rows `.focusable(false).preserve_focus_on_press(true)`.
- `poll` commits animate unless you use `tx.instant()`.
- Positive scroll delta scrolls up.
- Rune draws `⏺ ⏸ ⏹` as emoji (it ignores U+FE0E) and has no system font fallback, so `text_form` swaps some glyphs.
- Rune has no file watcher, Dock badge, sound, font list or way to cancel ⌘Q. Hyper adds these natively (`live.rs` kqueue, `quit.rs` swizzle).
- `libghostty-rs` is pinned to a rev. Read its source and `example/ghostling_rs`, not docs.rs.

## Testing

- macOS writes a file event only on close, so follow open files (Codex, Pi) with kqueue.
- Launch with `open -n` and match windows by pid. Post scroll events through the HID tap.
- Codex: don't accept the trust or update prompts. Pass `-c 'projects={"<path>"={trust_level="trusted"}}'`.
- Terminal tests start your real `zsh -l -i`.
