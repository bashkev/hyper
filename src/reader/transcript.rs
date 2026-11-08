//! The open session's transcript: its rows, and the outline of its prompts beside them.

use rune::{Cursor, El, GrowthSpec, ScrollSpec, TextHighlight, TextStyle, col, el, memo, row, text};
use rune_kit::markdown::{self, Case};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, Markdown, Placement, Spinner, Tooltip, TooltipStyle, glyphs};

use crate::data::{self, Kind, Row};
use crate::ui::{TOOLS, center_message, centered};
use crate::{App, Load};

/// Longer prompts are cut here until they're opened.
const USER_TEXT_CAP: usize = 6_000;

fn matches_in(source: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    let (hay, needle) = (source.to_ascii_lowercase(), query.to_ascii_lowercase());
    hay.match_indices(&needle).map(|(at, m)| at..at + m.len()).collect()
}

fn user_row(r: &Row, query: &str, open: bool, k: &Kit) -> El {
    let long = r.text.len() > USER_TEXT_CAP;
    let mut shown = r.text.as_str();
    if long && !open {
        let mut end = USER_TEXT_CAP;
        while !shown.is_char_boundary(end) {
            end -= 1;
        }
        shown = &shown[..end];
    }
    let marks = matches_in(shown, query).into_iter().map(|m| TextHighlight::new(m, k.c.warning.fg.alpha(0.35)));
    let mut bubble = col()
        .w_full()
        .gap(6.0)
        .padding(12.0)
        .radius(k.radius.md)
        .fill(k.c.layer_02)
        .child(text(shown.to_owned()).font(k.ty.ui).color(k.c.text_base).wrap_anywhere().selectable(true).text_highlights(marks));
    if long {
        let id = r.id.clone();
        bubble = bubble.child(
            Button::new(if open { "Show less" } else { "Show more" })
                .variant(ButtonVariant::Ghost)
                .control_size(ControlSize::Small)
                .on_press(move |s: &mut App| s.reader.toggle(&id))
                .build(k),
        );
    }
    col().w_full().py(10.0).child(bubble)
}

pub fn content_markdown(source: impl Into<String>, k: &Kit) -> Markdown {
    let body = k.ty.body;
    let mut style = markdown::MarkdownStyle::from_kit(k)
        .body_font(body)
        .code_block_font(k.ty.mono)
        .inline_code_font(1.0, 400.0);
    for (level, scale) in [(1, 17.0 / 14.0), (2, 15.0 / 14.0), (3, 1.0)] {
        style = style.heading_font(level, TextStyle { size: body.size * scale, line_height: body.line_height * scale, weight: 600.0, ..body });
    }
    Markdown::new(source).style(style)
}

fn reply_row(r: &Row, query: &str, k: &Kit) -> El {
    let md = content_markdown(r.text.clone(), k).selectable(true).highlights(markdown::find(&r.text, query, Case::Insensitive));
    col().w_full().py(6.0).child(md.build(k))
}

/// A row that opens to show more, such as a tool call.
pub fn disclosure(id: &str, name: &str, summary: &str, error: bool, open: bool, k: &Kit) -> El {
    let toggled = id.to_owned();
    let ink = if error { k.c.danger.fg } else { k.c.text_muted };
    let line = row()
        .key("row-toggle")
        .role("button")
        .label(format!("{name} {summary}"))
        .expanded(open)
        .focusable(true)
        .cursor(Cursor::Pointer)
        .w_full()
        .gap(8.0)
        .px(6.0)
        .py(3.0)
        .center_y()
        .radius(k.radius.sm)
        .on_press(move |s: &mut App| s.reader.toggle(&toggled))
        .child(
            text(if open { "▾" } else { "▸" }).font(k.ty.small).color(k.c.text_faint),
        )
        .child(text(name.to_owned()).font(k.ty.small_medium).color(ink))
        .child(el().grow(1.0).min_w(0.0).child(text(summary.to_owned()).font(k.ty.small).color(k.c.text_faint).truncate()));
    k.interactive(line)
}

fn tool_row(r: &Row, tool: &data::Tool, open: bool, k: &Kit) -> El {
    let mut body = col().w_full().py(1.0).child(disclosure(&r.id, &tool.name, &tool.summary, tool.error, open, k));
    if open {
        let mut source = tool.input.clone();
        if let Some(result) = &tool.result {
            if !source.is_empty() {
                source.push_str("\n\n");
            }
            source.push_str(&data::fence("", result));
        }
        body = body.child(col().w_full().pt(4.0).pb(8.0).child(content_markdown(source, k).selectable(true).scroll_wide(true).build(k)));
    }
    body
}

fn note_row(r: &Row, name: &str, open: bool, k: &Kit) -> El {
    let summary = data::one_line(&r.text, 120);
    let mut body = col().w_full().py(1.0).child(disclosure(&r.id, name, &summary, false, open, k));
    if open {
        body = body.child(
            col().w_full().px(6.0).pt(4.0).pb(8.0).child(content_markdown(r.text.clone(), k).selectable(true).build(k)),
        );
    }
    body
}

fn transcript_row(s: &App, i: usize, k: &Kit) -> El {
    let Some(r) = s.reader.row(i) else { return el() };
    let open = s.reader.expanded.contains(&r.id);
    let query = s.search.searched.as_str();
    // A tool call's row is shown before its result arrives.
    let result = match &r.kind {
        Kind::Tool(tool) => (tool.result.is_some(), tool.error),
        _ => (false, false),
    };
    memo(r.id.clone(), (open, query, result, s.reader.rewrites, k.ty.ui, k.ty.body, k.ty.mono, format!("{:?}", k.c.text_base)), || {
        centered(match &r.kind {
            Kind::User => user_row(r, query, open, k),
            Kind::Assistant => reply_row(r, query, k),
            Kind::Thinking => note_row(r, "Thinking", open, k),
            Kind::Compacted => note_row(r, "Compacted", open, k),
            Kind::Tool(tool) => tool_row(r, tool, open, k),
        })
    })
}

fn prompt_outline(s: &App, k: &Kit, layer: &mut Vec<El>) -> El {
    let previous = IconButton::new(&glyphs::CHEVRON_UP)
        .variant(IconButtonVariant::GhostMuted)
        .control_size(ControlSize::Small)
        .label("Previous prompt")
        .help("Previous prompt")
        .disabled(s.reader.prompts.is_empty() || s.reader.prompt == 0)
        .on_press_tx(|s: &mut App, tx| s.reader.jump_prompt(s.reader.prompt.saturating_sub(1), tx))
        .build(k)
        .key("previous-prompt");
    let next = IconButton::new(&glyphs::CHEVRON_DOWN)
        .variant(IconButtonVariant::GhostMuted)
        .control_size(ControlSize::Small)
        .label("Next prompt")
        .help("Next prompt")
        .disabled(s.reader.prompt + 1 >= s.reader.prompts.len())
        .on_press_tx(|s: &mut App, tx| s.reader.jump_prompt(s.reader.prompt + 1, tx))
        .build(k)
        .key("next-prompt");
    let style = TooltipStyle::from_kit(k, Default::default()).max_width(360.0).padding(12.0, 10.0).radius(k.radius.md);
    let marks = s.reader.prompts.iter().enumerate().filter_map(|(prompt, &i)| {
        let r = s.reader.row(i)?;
        let id = r.id.clone();
        let hovered = s.reader.prompt_hover.as_deref() == Some(id.as_str());
        let active = s.reader.prompt == prompt;
        let mark = k.interactive(
            row().key(format!("prompt-mark-{prompt}"))
                .role("button")
                .label(format!("Prompt {}: {}", prompt + 1, data::one_line(&r.text, 100)))
                .checked(active)
                .focusable(true)
                .cursor(Cursor::Pointer)
                .size(32.0, 10.0)
                .center_y()
                .pl(5.0)
                .on_press_tx(move |s: &mut App, tx| s.reader.jump_prompt(prompt, tx))
                .on_hover_change(move |s: &mut App, inside, _| {
                    if inside {
                        s.reader.prompt_hover = Some(id.clone());
                    } else if s.reader.prompt_hover.as_deref() == Some(id.as_str()) {
                        s.reader.prompt_hover = None;
                    }
                })
                .child(el()
                    .w(if active { 22.0 } else if hovered { 16.0 } else { 8.0 })
                    .h(2.0)
                    .fill(if active || hovered { k.c.text_base } else { k.c.text_faint })),
        );
        Some(Tooltip::new(format!("prompt-preview-{prompt}"))
            .trigger(mark)
            .label(data::one_line(&r.text, 320))
            .open(hovered)
            .placement(Placement::Right)
            .render(&style, layer))
    });
    col().key("prompt-outline").role("navigation").label("Prompts")
        .w(40.0).h_full().min_h(0.0).pt(8.0).items_center()
        .child(previous)
        .child(next)
        .child(
            col().key("prompt-outline-scroll").w_full().grow(1.0).min_h(0.0)
                .scroll(ScrollSpec { stick_bottom: false })
                .child(col().w_full().items_center().py(8.0).children(marks)),
        )
}

pub fn transcript_pane(s: &App, k: &Kit, layer: &mut Vec<El>) -> El {
    match (&s.reader.transcript, s.reader.selected.as_deref()) {
        (_, None) if s.index == Load::Waiting => center_message(Spinner::new().build(k)),
        (_, None) => center_message(text("No session").font(k.ty.ui).color(k.c.text_muted)),
        (Load::Waiting, _) => center_message(Spinner::new().build(k).key("loading")),
        (Load::Failed(e), _) => center_message(text(e.clone()).font(k.ty.ui).color(k.c.danger.fg).wrap()),
        (Load::Ready(_), Some(id)) => {
            let kit = k.clone();
            let rows = col().key("rows").w_full().growth(GrowthSpec::default()).virtual_rows(
                s.reader.shown.len(),
                64.0,
                |s: &App, i| s.reader.row(i).map_or("".into(), |r| r.id.as_str().into()),
                move |s: &App, i| transcript_row(s, i, &kit),
            ).on_visible_rows(|s: &mut App, visible| s.reader.visible_prompts(visible));
            // A running session opens at its end and follows it until scrolled away from.
            let live = s.session(id).is_some_and(|x| s.activity(x).is_some());
            let viewport = col().key(format!("transcript-{id}")).w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: live }).child(rows);
            let viewport = crate::history::swipeable(viewport);
            row().w_full().h_full().min_h(0.0)
                .child(prompt_outline(s, k, layer))
                .child(viewport.grow(1.0).min_w(0.0))
        }
    }
}

/// Shows or hides tool calls and thinking.
pub fn tools_toggle(s: &App, k: &Kit) -> El {
    let label = if s.reader.show_tools { "Hide tool calls" } else { "Show tool calls" };
    IconButton::new(&TOOLS)
        .variant(IconButtonVariant::GhostMuted)
        .expanded(s.reader.show_tools)
        .label(label)
        .help(label)
        .shortcut(crate::commands::Action::ToggleTools.hint(&s.settings))
        .on_press(|s: &mut App| s.reader.toggle_tools())
        .build(k)
        .key("tools-toggle")
}
