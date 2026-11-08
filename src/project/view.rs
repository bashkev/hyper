//! The Files and Changes panes: the file tree or changed files, the preview with its find bar,
//! and the tabs above them.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use rune::{Cursor, El, GrowthSpec, Icon, Key, Phase, ScrollSpec, Tx, col, el, rich, row, text};
use rune_kit::diff::{DiffMode, DiffView};
use rune_kit::syntax::SyntaxTheme;
use rune_kit::{Button, ButtonVariant, Kit, Spinner, TextInput, TreeItem, TreeStyle, glyphs};

use super::data::Area;
use super::{Mode, Tab};
use crate::ui::{EXTERNAL, keys, pane_button};
use crate::{App, Load};

pub static CHANGES: Icon = Icon {
    name: "project-changes",
    view: 16.0,
    body: r#"<path d="M3 1.5H9L13 5.5V14.5H3Z M9 1.5V5.5H13 M5 8H10 M7.5 5.5V10.5 M5 12H10" stroke="currentColor" stroke-width="1.1" stroke-linejoin="round" fill="none"/>"#,
};
static EXPAND: Icon = Icon {
    name: "project-expand",
    view: 16.0,
    body: r#"<path d="M9 2H14V7 M14 2L9 7 M7 14H2V9 M2 14L7 9" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};
static RESTORE: Icon = Icon {
    name: "project-restore",
    view: 16.0,
    body: r#"<path d="M14 2L9 7 M9 2V7H14 M2 14L7 9 M2 9H7V14" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" fill="none"/>"#,
};
static COLLAPSE: Icon = Icon {
    name: "project-collapse-all",
    view: 16.0,
    body: r#"<rect x="2.5" y="2.5" width="11" height="11" rx="2" stroke="currentColor" stroke-width="1.1" fill="none"/><path d="M5.5 8H10.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/>"#,
};
static SPLIT: Icon = Icon {
    name: "project-diff-split",
    view: 16.0,
    body: r#"<rect x="2" y="2.5" width="12" height="11" rx="2" stroke="currentColor" stroke-width="1.1" fill="none"/><path d="M8 2.5V13.5" stroke="currentColor" stroke-width="1.1"/>"#,
};
static UNIFIED: Icon = Icon {
    name: "project-diff-unified",
    view: 16.0,
    body: r#"<rect x="2" y="2.5" width="12" height="11" rx="2" stroke="currentColor" stroke-width="1.1" fill="none"/><path d="M4.5 6.5H11.5 M4.5 9.5H11.5" stroke="currentColor" stroke-width="1.1" stroke-linecap="round"/>"#,
};
static MORE: Icon = Icon {
    name: "project-more",
    view: 16.0,
    body: r#"<circle cx="3" cy="8" r="1" fill="currentColor"/><circle cx="8" cy="8" r="1" fill="currentColor"/><circle cx="13" cy="8" r="1" fill="currentColor"/>"#,
};
fn control(glyph: &'static Icon, label: &str, key: &str, k: &Kit, f: impl Fn(&mut App, &mut Tx) + 'static) -> El {
    pane_button(glyph, label, "", key, true, k, f)
}

fn outline(s: &App, k: &Kit) -> El {
    let p = &s.project;
    let count = p.tree().visible_len();
    let kit = k.clone();
    let style = TreeStyle { row_height: k.ty.ui.line_height.max(20.0) + 8.0, ..TreeStyle::from_kit(k) };
    let rows = col().key("rows").w_full().growth(GrowthSpec::default()).virtual_rows(
        count,
        style.row_height,
        |s: &App, i| Cow::Borrowed(s.project.tree().visible_id(i).unwrap_or_default()),
        move |s: &App, i| {
            let Some((node, r)) = s.project.tree().row(i) else { return el() };
            let mut item = TreeItem::new(&node.label);
            if let Some(count) = node.count {
                // A Changes group heading, not a folder.
                item = item.label_color(kit.c.text_muted).badge(count.to_string());
            } else {
                item = item.icon(if node.dir {
                    if r.expanded == Some(true) { &glyphs::FOLDER_OPEN } else { &glyphs::FOLDER }
                } else {
                    rune_kit::file_icon(&node.path.to_string_lossy())
                });
                if node.ignored || node.placeholder {
                    item = item.label_color(kit.c.text_faint);
                }
            }
            if !s.project.query.text.is_empty() && node.count.is_none() && !node.placeholder {
                item = item.highlight(s.project.query.text.trim_start_matches('?'));
            }
            if let Some(change) = &node.change {
                item = item.badge(change.status.to_string());
            }
            let id = r.id.to_owned();
            let pick = id.clone();
            let path = node.path.clone();
            let openable = r.expanded.is_some();
            let placeholder = node.placeholder && !node.dir;
            item.build(&kit, &r, &style)
                .key(id)
                .focusable(false)
                .preserve_focus_on_press(true)
                .on_press_tx(move |s: &mut App, tx| {
                    if placeholder {
                        return;
                    }
                    s.project.tree_mut().select(&pick);
                    if openable {
                        s.project.tree_mut().toggle(&pick);
                    }
                    s.project_pick(&pick, tx);
                    tx.blur();
                })
                .on_secondary_press(move |s: &mut App, at, tx| s.project_menu(path.clone(), None, at.into(), tx))
        },
    );
    let rows = rows.virtual_rows_version(p.tree_rev);
    col()
        .key(if p.mode == Mode::Changes { "project-changes-tree" } else { "project-files" })
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .focusable(true)
        .role("tree")
        .label(if p.mode == Mode::Changes { "Changed files" } else { "Project files" })
        .on_key(|s: &mut App, ev, tx| s.project_tree_key(ev, tx))
        .child(col().key("project-tree-scroll").w_full().grow(1.0).min_h(0.0).px(6.0).pb(6.0).scroll(ScrollSpec { stick_bottom: false }).child(rows))
}

fn code(s: &App, k: &Kit) -> El {
    let Some(code) = &s.project.code else { return el() };
    let wrap = s.project.wrap;
    let kit = k.clone();
    let id = s.project.tab().map(Tab::id).unwrap_or_default();
    let line_count = code.lines.len();
    let number_width = number_width(line_count, k);
    let rows = col().key(format!("code-{id}")).w_full().selection_group(true).growth(GrowthSpec::default()).virtual_rows(
        line_count,
        k.ty.mono.line_height,
        |_: &App, i| Cow::Owned(format!("code-line-{i}")),
        move |s: &App, i| {
            let Some(line) = s.project.code.as_ref().and_then(|c| c.lines.get(i)) else { return el() };
            let theme = SyntaxTheme::from_kit(&kit);
            let marks = if s.project.finding {
                s.project.match_lines.get(&i).map_or_else(Vec::new, |range| {
                    range
                        .clone()
                        .map(|m| {
                            let current = m == s.project.matched;
                            let r = s.project.matches[m].1.clone();
                            rune::TextHighlight::new(r, kit.alpha(kit.c.text_accent, if current { 0.6 } else { 0.22 }))
                        })
                        .collect()
                })
            } else {
                Vec::new()
            };
            let text = rich(&line.text, theme.spans(&line.highlights, 0))
                .font(kit.ty.mono)
                .color(kit.c.text_base)
                .selectable(true)
                .text_highlights(marks)
                .key(format!("code-text-{i}"));
            row()
                .w_full()
                .min_h(kit.ty.mono.line_height)
                .child(text_number(i + 1, number_width, &kit))
                .child(if wrap { text.grow(1.0).min_w(0.0).wrap_anywhere() } else { text })
                .pr(12.0)
        },
    );
    let rows = rows.virtual_rows_version(s.project.code_rev);
    let rows = if wrap { rows } else {
        rows.min_w((code.columns as f32 * rune::text::advance("M", k.ty.mono) + number_width + 12.0).max(100.0))
    };
    let scroll = col().key("project-code-scroll").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false });
    let scroll = if wrap { scroll } else { scroll.scroll_xy() };
    scroll.child(rows)
}

fn number_width(lines: usize, k: &Kit) -> f32 {
    let digits = (lines.max(1).ilog10() + 1).max(3);
    (digits as f32 * rune::text::advance("M", k.ty.mono) + 20.0).ceil()
}

fn text_number(n: usize, width: f32, k: &Kit) -> El {
    text(n.to_string())
        .font(k.ty.mono)
        .color(k.c.text_faint)
        .w(width)
        .pr(10.0)
        .align(rune::Align::End)
        .selectable(false)
        .a11y_hidden()
}

fn preview(s: &App, k: &Kit) -> El {
    let p = &s.project;
    let body = match &p.preview {
        Load::Waiting => col().grow(1.0).center().child(Spinner::new().build(k)),
        Load::Failed(e) => message(e, k),
        _ if p.code.is_some() => code(s, k),
        _ if !p.diff.files().is_empty() => {
            DiffView::new(|s: &App| &s.project.diff, |s: &mut App| &mut s.project.diff).key("project-diff").build(s, k)
        }
        _ => message(p.note.as_deref().unwrap_or(if p.mode == Mode::Changes { "Select a changed file" } else { "Select a file" }), k),
    };
    let mut pane = col()
        .key("project-preview")
        .grow(1.0)
        .min_w(0.0)
        .min_h(0.0)
        .focusable(true)
        .no_focus_ring()
        .on_key(|s: &mut App, ev, tx| s.project_key(ev, tx));
    if p.finding {
        let find = TextInput::new(&p.find)
            .placeholder("Find in file")
            .on_edit(|s: &mut App| &mut s.project.find)
            .on_change(|s: &mut App, tx| {
                s.project.find_matches();
                s.project_reveal_match(tx);
            })
            .on_key(|s: &mut App, ev, tx| {
                if ev.key == Key::Enter {
                    s.project_next_match(!ev.mods.shift, tx);
                    true
                } else if ev.key == Key::Escape {
                    s.project.finding = false;
                    tx.blur();
                    true
                } else {
                    false
                }
            })
            .build(k)
            .key("project-find")
            .grow(1.0)
            .min_w(0.0);
        pane = pane.child(
            row()
                .w_full()
                .padding(8.0)
                .gap(4.0)
                .center_y()
                .child(find)
                .children((!p.find.text.is_empty()).then(|| {
                    let said = if p.matches.is_empty() {
                        "No results".to_owned()
                    } else {
                        format!("{} of {}", p.matched + 1, p.matches.len())
                    };
                    text(said).font(k.ty.small.features("'tnum' 1")).color(k.c.text_faint).truncate()
                }))
                .child(pane_button(&glyphs::CHEVRON_UP, "Previous match", keys(p.find.focused, "shift+enter"), "project-find-prev", true, k, |s, tx| s.project_next_match(false, tx)))
                .child(pane_button(&glyphs::CHEVRON_DOWN, "Next match", keys(p.find.focused, "enter"), "project-find-next", true, k, |s, tx| s.project_next_match(true, tx)))
                .child(pane_button(&glyphs::CLOSE, "Close find", keys(p.find.focused, "escape"), "project-find-close", true, k, |s, _| s.project.finding = false)),
        );
    }
    pane.child(body)
}

fn message(value: &str, k: &Kit) -> El {
    col().w_full().grow(1.0).min_h(0.0).center().padding(16.0).child(text(value).font(k.ty.small).color(k.c.text_faint).wrap_anywhere())
}

fn breadcrumbs(s: &App, k: &Kit) -> El {
    let path = s.project.tab().map(|t| t.path.clone()).unwrap_or_default();
    let mut crumb = row().gap(2.0).center_y();
    let root =
        s.project.ctx.as_ref().and_then(|c| c.root.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Files".into());
    let mut parts = vec![(root, PathBuf::new())];
    let mut prefix = PathBuf::new();
    for component in path.components() {
        let dir = prefix.clone();
        prefix.push(component);
        parts.push((component.as_os_str().to_string_lossy().into_owned(), dir));
    }
    for (i, (label, dir)) in parts.into_iter().enumerate() {
        if i > 0 {
            crumb = crumb.child(text("/").font(k.ty.small).color(k.c.text_faint));
        }
        let key = format!("project-crumb-{i}");
        let anchor = key.clone();
        crumb = crumb.child(
            Button::new(label)
                .variant(ButtonVariant::Ghost)
                .on_press_tx(move |s: &mut App, tx| s.project_breadcrumb(dir.clone(), anchor.clone().into(), tx))
                .build(k)
                .key(key),
        );
    }
    row().key("project-path").grow(1.0).min_w(0.0).center_y().scroll_x().child(crumb)
}

pub fn tabs(s: &App, k: &Kit) -> El {
    let p = &s.project;
    // One content row inside the viewport: scroll_x stretches its content to the viewport, which
    // would stretch a lone tab.
    let strip = row().gap(2.0).center_y().children(
        p.tabs.iter().enumerate().filter(|(_, t)| t.mode() == p.mode).map(|(i, t)| {
            let mut name = t.path.file_name().unwrap_or_default().to_string_lossy().into_owned();
            // The staged and unstaged diffs of one file can both be open.
            if t.change.as_ref().is_some_and(|c| c.area == Area::Staged) {
                name.push_str(" (Staged)");
            }
            let path = t.path.clone();
            let active = p.active == Some(i);
            let close = control(&glyphs::CLOSE, "Close file", &format!("project-tab-close-{i}"), k, move |s, _| s.project.close_tab(i));
            row()
                .key(format!("project-tab-{i}"))
                .h(28.0)
                .pl(10.0)
                .pr(2.0)
                .gap(2.0)
                .center_y()
                .radius(6.0)
                .fill(if active { k.c.hover } else { rune::Color::TRANSPARENT })
                .hover(k.c.hover)
                .hover_group()
                .focusable(false)
                .preserve_focus_on_press(true)
                .role("tab")
                .label(&name)
                .help(t.path.to_string_lossy())
                .focus_selected(active)
                .child(
                    text(name)
                        .font(k.ty.small)
                        .color(if active { k.c.text_base } else { k.c.text_muted })
                        .max_w(180.0)
                        .truncate(),
                )
                .child(if active { close } else { close.group_opacity(0.0, 1.0) })
                .on_press_tx(move |s: &mut App, tx| {
                    // Like a tree row: the pane takes the keyboard without focusing a row.
                    s.project.keyboard = true;
                    tx.blur();
                    s.project.select_tab(i);
                })
                .on_secondary_press(move |s: &mut App, at, tx| s.project_menu(path.clone(), Some(i), at.into(), tx))
        }),
    );
    let tabs = row().key("project-tabs").grow(1.0).min_w(0.0).center_y().scroll_x().child(strip);
    row().h(32.0).role("tablist").label("Open files").window_drag().gap(4.0).center_y().child(tabs)
}

/// Fills the workspace with the pane, or puts it back.
pub fn expand_control(s: &App, k: &Kit) -> El {
    let expanded = s.project.expanded;
    control(
        if expanded { &RESTORE } else { &EXPAND },
        if expanded { "Restore pane" } else { "Expand pane" },
        "project-expand",
        k,
        |s, _| s.toggle_project_expanded(),
    )
}

pub fn pane(s: &App, k: &Kit) -> El {
    let p = &s.project;
    let tabpath = p.tab().map(|t| t.path.clone());
    let mut bar = row()
        .w_full()
        .h(36.0)
        .px(10.0)
        .gap(4.0)
        .center_y()
        .child(if tabpath.is_some() { breadcrumbs(s, k) } else { el().grow(1.0) })
        .children(
            tabpath
                .map(|path| control(&EXTERNAL, "Open in Editor", "project-open-editor", k, move |s, _| s.project_external(&path, false))),
        )
        .children((p.tree_shown && p.mode == Mode::Files).then(|| {
            control(&COLLAPSE, "Collapse folders", "project-collapse", k, |s, _| {
                s.project.tree_mut().collapse_all();
                s.project.tree_rev = s.project.tree_rev.wrapping_add(1);
                s.project.remember_folders();
            })
        }))
        .child(control(
            &rune_kit::shell::INSPECTOR,
            if p.tree_shown { "Hide file tree" } else { "Show file tree" },
            "project-tree-toggle",
            k,
            |s, _| s.project.tree_shown = !s.project.tree_shown,
        ));
    if p.mode == Mode::Changes {
        let unified = p.diff.mode() == DiffMode::Unified;
        bar = bar.child(control(
            if unified { &SPLIT } else { &UNIFIED },
            if unified { "Show split diff" } else { "Show unified diff" },
            "project-diff-mode",
            k,
            |s, _| {
                let mode = if s.project.diff.mode() == DiffMode::Unified { DiffMode::Split } else { DiffMode::Unified };
                s.project.diff.set_mode(mode);
            },
        ));
    }
    bar = bar.child(control(&MORE, "File settings", "project-settings", k, App::project_settings)).child(expand_control(s, k));
    let mut content = row().key("project-content").w_full().grow(1.0).min_h(0.0).child(preview(s, k));
    if p.tree_shown {
        let tree_w = p.tree_width.min((s.inspector.width() * 0.45).max(140.0)).max(140.0);
        // Rune wraps a single-line field's placeholder instead of clipping it, so shorten it to
        // fit: tree padding 16, icon inset 8 + 16, gap 6, trailing inset 8.
        let full = if p.mode == Mode::Changes { "Filter changes" } else { "Filter or ?search" };
        let fits = rune::text::measure(full, &[], k.ty.ui_compact, None).0 <= tree_w - 54.0;
        let filter = TextInput::new(&p.query)
            .width(None)
            .placeholder(if fits { full } else { "Filter" })
            .leading_icon(&glyphs::SEARCH)
            .on_edit(|s: &mut App| &mut s.project.query)
            .on_change(|s: &mut App, _| s.project.filter())
            .on_key(|s: &mut App, ev, tx| {
                if ev.key == Key::Down {
                    s.project.keyboard = true;
                    tx.blur();
                    s.project_tree_key(ev, tx);
                    true
                } else {
                    false
                }
            })
            .build(k)
            .key("project-filter");
        let mut tree = col()
            .key("project-picker")
            .w(tree_w)
            .min_w(140.0)
            .h_full()
            .fill(k.c.bg_base)
            .child(col().w_full().padding(8.0).child(filter));
        let error = if p.mode == Mode::Changes {
            if let Load::Failed(e) = &p.status { Some(e.as_str()) } else { None }
        } else {
            p.search_paths
                .as_ref()
                .and_then(|r| r.as_ref().err())
                .map(String::as_str)
                .or_else(|| p.directories.get(Path::new("")).and_then(|r| r.as_ref().err()).map(String::as_str))
        };
        tree = tree.child(if let Some(e) = error {
            message(e, k)
        } else if (p.opening.is_pending() || p.search.is_pending() || p.status_job.is_pending()) && p.tree().is_empty() {
            col().grow(1.0).center().child(Spinner::new().build(k))
        } else if p.tree().is_empty() {
            message(
                if !p.query.text.is_empty() {
                    "No matches"
                } else if p.mode == Mode::Changes {
                    "No changes"
                } else if p.ctx.is_none() {
                    "Select a project"
                } else {
                    "No files"
                },
                k,
            )
        } else {
            outline(s, k)
        });
        let grip = el()
            .key("project-tree-grip")
            .w(5.0)
            .h_full()
            .cursor(Cursor::ResizeColumn)
            .role("separator")
            .label("Resize file tree")
            .range_value(f64::from(p.tree_width), 140.0, f64::from((s.inspector.width() * 0.6).max(140.0)))
            .range_step(10.0)
            .fill(k.c.border_weak)
            .opacity(0.35)
            .focusable(false)
            .on_drag(|s: &mut App, drag, tx| {
                if drag.phase == Phase::Down {
                    s.project.tree_drag = s.project.tree_width;
                }
                s.project.tree_width = (s.project.tree_drag - drag.dx).clamp(140.0, (s.inspector.width() * 0.6).max(140.0));
                tx.instant();
            })
            .on_double_press(|s: &mut App| s.project.tree_width = 210.0)
            .on_step(|s: &mut App, steps, tx| {
                s.project.tree_width = (s.project.tree_width + steps as f32 * 10.0).clamp(140.0, (s.inspector.width() * 0.6).max(140.0));
                tx.instant();
            });
        content = content.child(grip).child(tree);
    }
    col().key("project-pane").w_full().h_full().min_h(0.0).fill(k.c.bg_base).child(bar).child(content).children(
        p.watch_error.as_ref().filter(|e| e.as_str() != "headless").map(|e| {
            Button::new("Live updates unavailable · Reload")
                .variant(ButtonVariant::Ghost)
                .on_press(|s: &mut App| s.project_reload())
                .build(k)
                .help(e)
                .key("project-watch-error")
        }),
    )
}
