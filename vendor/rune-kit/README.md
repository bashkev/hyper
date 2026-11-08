# rune-kit (vendored)

Source: https://github.com/anomalyco/rune/tree/47cb857795c3b1dd5097c15d6161719ae738ddda/crates/rune-kit/src

MIT, copyright Anomaly; see `LICENSE`. The source is preserved at Hyper's pinned Rune revision. The standalone manifest uses git dependencies at that same revision instead of upstream workspace paths.

Local patch, limited to `src/diff/view.rs` and `src/diff/state.rs`:

- Inherit code and line-number typography from `Kit::ty.mono`, and file headers from `Kit::ty.ui_medium`.
- Measure the chosen monospace font's column advance rather than assuming 0.6 × size.
- Derive row, header, gap and note heights from typography; use those metrics for drawing, virtualisation, and navigation.
- Include complete typography and column advance in the memoised row version, so spacing-only changes also redraw.

Hyper's regression coverage is in `src/project/tests.rs`: real Git diffs in unified and split modes, with live size and spacing changes. Run from the repository root:

```sh
cargo test --release -- --test-threads=1
```

Remove this vendored copy once upstream exposes equivalent typography support. No terminal-engine or screen-snapshot changes are involved.
