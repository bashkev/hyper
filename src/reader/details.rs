//! The open session's details in the inspector: where it ran, its size, and what it used.

use rune::{Align, El, ScrollSpec, col, el, row, text};
use rune_kit::{Button, ButtonVariant, Kit};

use super::disclosure;
use crate::format::{money, size, spent, timestamp, token_count};
use crate::ui::field;
use crate::usage::Report;
use crate::{App, Load};

/// A label with figures on its right.
fn figures(label: &str, tokens: String, cost: String, k: &Kit) -> El {
    let figure = |value: String, width: f32| text(value).font(k.ty.small).color(k.c.text_muted).w(width).align(Align::End);
    let line = row()
        .w_full()
        .gap(6.0)
        .center_y()
        .child(el().grow(1.0).min_w(0.0).child(text(label.to_owned()).font(k.ty.small).color(k.c.text_base).truncate().selectable(true)));
    let line = if tokens.is_empty() { line } else { line.child(figure(tokens, 48.0)) };
    line.child(figure(cost, 62.0))
}

fn group(title: &str, k: &Kit) -> El {
    col().w_full().gap(4.0).child(text(title.to_owned()).font(k.ty.small).color(k.c.text_faint))
}

fn usage_details(s: &App, report: &Report, mut body: El, k: &Kit) -> El {
    let total = &report.total;
    if total.tokens.total() > 0 {
        let (t, c) = (total.tokens, total.cost);
        let split = group("Usage", k)
            .child(figures("Total", token_count(t.total()), spent(total), k))
            .child(figures("Input", token_count(t.input), money(c.input), k))
            .child(figures("Output", token_count(t.output), money(c.output), k))
            .child(figures("Cache read", token_count(t.cache_read), money(c.cache_read), k))
            .child(figures("Cache write", token_count(t.cache_write), money(c.cache_write), k));
        body = body.child(split);
    }
    if report.agents.tokens.total() > 0 {
        body = body.child(group("Subagents", k).child(figures("Share of total", token_count(report.agents.tokens.total()), spent(&report.agents), k)));
    }
    if report.models.len() > 1 {
        let models = report.models.iter().fold(group("Models", k), |list, (model, spend)| list.child(figures(model, token_count(spend.tokens.total()), spent(spend), k)));
        body = body.child(models);
    }
    if !report.skills.is_empty() {
        let skills = report.skills.iter().fold(group("Skills", k), |list, (name, calls)| list.child(figures(name, String::new(), calls.to_string(), k)));
        body = body.child(skills);
    }
    if !report.mcp.is_empty() {
        let servers = report.mcp.iter().fold(group("MCPs", k), |list, server| {
            let id = format!("mcp:{}", server.name);
            let open = s.reader.expanded.contains(&id);
            let mut item = col().key(id.clone()).w_full().child(disclosure(&id, &server.name, &server.calls.to_string(), false, open, k));
            if open {
                let tools = server.tools.iter().fold(col().w_full().gap(4.0).pl(20.0).py(2.0), |tools, (tool, calls)| tools.child(figures(tool, String::new(), calls.to_string(), k)));
                item = item.child(tools);
            }
            list.child(item)
        });
        body = body.child(servers);
    }
    body
}

pub fn details(s: &App, k: &Kit) -> El {
    let field = |label: &str, value: String| field(label, value, k);
    let mut body = col().w_full().padding(16.0).gap(14.0);
    let viewport = crate::history::swipeable(col().key("details").w_full().grow(1.0).min_h(0.0).scroll(ScrollSpec { stick_bottom: false }));
    let Some(session) = s.selected_session() else { return viewport };
    body = body.child(field("Source", session.source.label().to_owned()));
    if let Some(activity) = s.activity(session) {
        body = body.child(field("Status", activity.label().to_owned()));
    }
    body = body.child(field("Project", session.cwd.clone()));
    if !session.branch.is_empty() && session.branch != "HEAD" {
        body = body.child(field("Branch", session.branch.clone()));
    }
    if let Load::Ready(t) = &s.reader.transcript {
        if !t.model.is_empty() {
            body = body.child(field("Model", t.model.clone()));
        }
        if let Some(started) = t.started {
            body = body.child(field("Started", timestamp(started)));
        }
        body = body.child(field("Updated", timestamp(session.updated)));
        body = body.child(field("Prompts", t.prompts.to_string()));
        body = body.child(field("Replies", t.replies.to_string()));
        body = body.child(field("Tool calls", t.tools.to_string()));
    }
    body = body.child(field("Size", size(session.size)));
    if let Some(report) = &s.reader.report {
        body = usage_details(s, report, body, k);
    }
    let command = session.resume_command();
    viewport.child(body.child(
        Button::new("Copy resume command")
            .variant(ButtonVariant::Neutral)
            .on_press_tx(move |_: &mut App, tx| tx.copy(command.clone()))
            .build(k)
            .key("copy-resume"),
    ))
}
