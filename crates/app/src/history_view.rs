//! タスクの変更履歴の見た目。作った人・変えた人、変更履歴のダイアログ、コメント欄に挟む出来事。

use std::collections::HashMap;

use crate::press::button;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::diff::{Hunk, Part, PartKind};
use malakt_core::history::{self, Entry, Event, FieldChange, Kind, Names};
use malakt_core::{view, Actor, Status};

use crate::feel;
use crate::render::{app_icon, avatar, col, row};
use crate::state::{Detail, MainView, Modal};

/// 名前を引く。消したタスクの題は、変更履歴を出す前にまとめて引いておく。
struct ViewNames<'a> {
    view: &'a MainView,
    titles: &'a HashMap<String, String>,
}

impl Names for ViewNames<'_> {
    fn person(&self, id: &str) -> String {
        if self.view.people.iter().any(|p| p.id == id) {
            self.view.person_name(id).to_owned()
        } else {
            "削除した担当者".to_owned()
        }
    }

    fn workspace(&self, id: &str) -> String {
        if self.view.workspaces.iter().any(|w| w.id == id) {
            self.view.workspace_name(Some(id)).to_string()
        } else {
            "削除したワークスペース".to_owned()
        }
    }

    fn task(&self, id: &str) -> String {
        self.titles
            .get(id)
            .cloned()
            .unwrap_or_else(|| "削除したタスク".to_owned())
    }

    fn self_id(&self) -> Option<&str> {
        self.view.self_id.as_deref()
    }
}

/// 小さな丸い印（「AI」「完了チケット」など）。
pub(crate) fn badge(text: String, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .flex_none()
        .px_2()
        .py_0p5()
        .rounded_full()
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(theme.foreground)
        .child(text)
}

impl MainView {
    /// 履歴に出てくる親タスクの題を引く。
    fn titles(&self, detail: &Detail, cx: &App) -> HashMap<String, String> {
        let store = &self.db.read(cx).store;
        let mut titles = HashMap::new();
        for c in detail.changes.iter().filter(|c| c.field == "parent") {
            for id in [&c.old_value, &c.new_value] {
                if !id.is_empty() && !titles.contains_key(id) {
                    if let Ok(task) = store.task(id) {
                        titles.insert(id.clone(), task.title);
                    }
                }
            }
        }
        titles
    }

    /// 変えた人の名前。人なら担当者の名前、AI などならその名前。分からなければ `None`。
    pub fn actor_name(&self, actor: &Actor) -> Option<String> {
        if let Some(name) = actor.machine_name() {
            return Some(name);
        }
        let person = actor.person(self.self_id.as_deref())?;
        Some(if self.people.iter().any(|p| p.id == person) {
            self.person_name(person).to_owned()
        } else {
            "削除した担当者".to_owned()
        })
    }

    /// 出来事の文の主語（「かなで さんが」「AI（Claude）が」）。分からなければ空。
    fn actor_lead(&self, actor: &Actor) -> String {
        match self.actor_name(actor) {
            Some(name) if actor.machine_name().is_some() => format!("{name}が"),
            Some(name) => format!("{name} さんが"),
            None => String::new(),
        }
    }

    /// 変えた人の表示。アイコンと名前、どこから変えたかの印。分からなければ `None`。
    pub(crate) fn actor_label(&self, actor: &Actor, cx: &App) -> Option<Div> {
        let theme = cx.theme().clone();
        let name = self.actor_name(actor)?;
        let icon = if actor.machine_name().is_some() {
            div()
                .size_6()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(theme.muted)
                .text_color(theme.muted_foreground)
                .child(app_icon(actor.machine_icon()).xsmall())
        } else {
            avatar(&name, cx)
        };
        Some(
            row()
                .gap_1p5()
                .min_w_0()
                .child(icon)
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.foreground)
                        .child(name),
                )
                .children(actor.badge().map(|b| badge(b, cx))),
        )
    }

    /// 詳細の上の「〇〇 が作成 日時」「〇〇 が更新 3 分前」。
    pub(crate) fn render_authorship(&self, task: &malakt_core::Task, now: i64, cx: &App) -> Div {
        let part = |actor: &Actor, verb: &'static str, when: String| {
            let label = self.actor_label(actor, cx);
            let known = label.is_some();
            row()
                .gap_1p5()
                .min_w_0()
                .flex_wrap()
                .children(label)
                .child(format!("{}{verb} {when}", if known { "が" } else { "" }))
        };
        row()
            .gap_x_3()
            .gap_y_1()
            .flex_wrap()
            .min_w_0()
            .child(part(
                &task.created_by,
                "作成",
                view::format_datetime(task.created_at),
            ))
            .child(part(
                &task.updated_by,
                "更新",
                view::format_relative(task.updated_at, now),
            ))
    }

    /// コメント欄に挟む出来事。古い順。
    pub fn timeline_events(&self, detail: &Detail, cx: &App) -> Vec<Event> {
        let titles = self.titles(detail, cx);
        let names = ViewNames {
            view: self,
            titles: &titles,
        };
        let status_comments: Vec<(i64, Status)> = detail
            .comments
            .iter()
            .filter_map(|c| c.status.map(|s| (c.created_at, s)))
            .collect();
        history::entries(&detail.changes)
            .iter()
            .flat_map(|entry| history::events(entry, &names, &status_comments))
            .collect()
    }

    /// コメント欄の、変更の出来事の 1 行。
    pub(crate) fn render_event(
        &self,
        ix: usize,
        event: &Event,
        now: i64,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme().clone();
        let lead = self.actor_lead(&event.actor);
        row()
            .gap_2()
            .items_start()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .size_6()
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.muted)
                    .child(app_icon(event.icon).xsmall()),
            )
            .child(
                row()
                    .flex_1()
                    .min_w_0()
                    .min_h_6()
                    .gap_x_2()
                    .gap_y_0p5()
                    .flex_wrap()
                    .child(div().min_w_0().child(
                        StyledText::new(format!("{lead}{}", event.text)).with_highlights(
                            (!lead.is_empty()).then(|| {
                                (
                                    0..lead.len(),
                                    HighlightStyle {
                                        font_weight: Some(FontWeight::BOLD),
                                        color: Some(theme.foreground),
                                        ..Default::default()
                                    },
                                )
                            }),
                        ),
                    ))
                    .children(event.actor.badge().map(|b| badge(b, cx)))
                    .child(
                        div()
                            .text_xs()
                            .whitespace_nowrap()
                            .child(view::format_relative(event.at, now)),
                    )
                    .when(event.opens_history, |d| {
                        d.child(feel::gloss(
                            button(("event-history", ix))
                                .icon(app_icon("history"))
                                .label("変更履歴")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.modal = Modal::History;
                                    cx.notify();
                                })),
                        ))
                    }),
            )
    }

    /// 変更履歴のダイアログの中身。新しい変更が上。
    pub(crate) fn render_history(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let Some(detail) = self.detail.as_ref() else {
            return div();
        };
        let titles = self.titles(detail, cx);
        let names = ViewNames {
            view: self,
            titles: &titles,
        };
        let entries = history::entries(&detail.changes);
        let mut list = col().gap_3();
        for (n, entry) in entries.iter().rev().enumerate() {
            list = list.child(self.render_entry(n, entry, &names, cx));
        }
        let body = if entries.is_empty() {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("変更履歴はまだありません。")
        } else {
            list.when(history::misses_older(&entries), |d| {
                d.child(
                    div()
                        .px_3()
                        .py_2()
                        .rounded(feel::md())
                        .border_1()
                        .border_color(theme.border)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("これより前の変更は記録されていません"),
                )
            })
        };
        div().child(
            div()
                .id("history-list")
                .max_h(crate::render::px(520.))
                .overflow_y_scrollbar()
                .child(body),
        )
    }

    fn render_entry(&self, n: usize, entry: &Entry, names: &ViewNames<'_>, cx: &App) -> Div {
        let theme = cx.theme().clone();
        let changes = history::field_changes(entry, names);
        let mut fields = col().gap_2();
        for (f, change) in changes.iter().enumerate() {
            fields = fields.child(field_card(&format!("history-{n}-{f}"), change, cx));
        }
        col()
            .gap_1p5()
            .p_3()
            .min_w_0()
            .rounded(feel::lg())
            .border_1()
            .border_color(theme.border)
            .bg(theme.secondary.opacity(0.4))
            .child(
                row()
                    .gap_3()
                    .justify_between()
                    .text_sm()
                    .child(div().font_weight(FontWeight::BOLD).child(if entry.created {
                        "作成"
                    } else {
                        "更新"
                    }))
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme.muted_foreground)
                            .child(view::format_datetime(entry.at)),
                    ),
            )
            .children(
                self.actor_label(&entry.actor, cx)
                    .map(|label| label.text_xs().text_color(theme.muted_foreground)),
            )
            .child(if changes.is_empty() {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("差分はありません。")
            } else {
                fields
            })
    }
}

/// 1 つの項目の差分。追加・削除・変更の印と項目名、変更前（赤）と変更後（緑）の枠。
/// `key` は枠の文字を選ぶための名前の頭で、履歴の中で重ならない。
fn field_card(key: &str, change: &FieldChange, cx: &App) -> Div {
    let theme = cx.theme().clone();
    let (fg, bg) = match change.kind {
        Kind::Added => (theme.success, theme.success.opacity(0.15)),
        Kind::Removed => (theme.danger, theme.danger.opacity(0.15)),
        Kind::Changed => (theme.primary, theme.primary.opacity(0.15)),
    };
    let head = row()
        .gap_2()
        .child(
            div()
                .px_1p5()
                .rounded_full()
                .bg(bg)
                .text_color(fg)
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .child(change.kind.label()),
        )
        .child(
            div()
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .child(change.label),
        );
    let mut card = col()
        .gap_1p5()
        .p_2()
        .min_w_0()
        .rounded(feel::md())
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
        .child(head);
    let plain = |text: &str, kind: PartKind| vec![Part::new(kind, text)];
    if !change.hunks.is_empty() {
        for (h, hunk) in change.hunks.iter().enumerate() {
            card = card.children(hunk_blocks(&format!("{key}-{h}"), hunk, cx));
        }
    } else {
        match change.kind {
            Kind::Changed => {
                card = card
                    .child(block(key, &[&change.before_parts], false, cx))
                    .child(block(key, &[&change.after_parts], true, cx));
            }
            Kind::Added => {
                card = card.child(block(
                    key,
                    &[&plain(&change.after, PartKind::Equal)],
                    true,
                    cx,
                ));
            }
            Kind::Removed => {
                card = card.child(block(
                    key,
                    &[&plain(&change.before, PartKind::Equal)],
                    false,
                    cx,
                ));
            }
        }
    }
    card
}

/// 行ごとの差分の 1 まとまり。変更前の行と変更後の行を、それぞれ 1 つの枠にする。
fn hunk_blocks(key: &str, hunk: &Hunk, cx: &App) -> Vec<AnyElement> {
    let before: Vec<&Vec<Part>> = hunk
        .lines
        .iter()
        .map(|l| &l.before)
        .filter(|p| !p.is_empty())
        .collect();
    let after: Vec<&Vec<Part>> = hunk
        .lines
        .iter()
        .map(|l| &l.after)
        .filter(|p| !p.is_empty())
        .collect();
    let mut out = Vec::new();
    if !before.is_empty() {
        out.push(block(key, &before, false, cx));
    }
    if !after.is_empty() {
        out.push(block(key, &after, true, cx));
    }
    out
}

/// 変更前（赤）か変更後（緑）の枠。消えた・増えたところを濃くする。
/// 中の文字はドラッグで選んでコピーできる。
fn block(key: &str, lines: &[&Vec<Part>], after: bool, cx: &App) -> AnyElement {
    let theme = cx.theme().clone();
    let tone = if after { theme.success } else { theme.danger };
    let mut text = String::new();
    let mut highlights = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            text.push('\n');
        }
        for part in line.iter() {
            let start = text.len();
            text.push_str(&part.text);
            if part.kind != PartKind::Equal {
                highlights.push((
                    start..text.len(),
                    HighlightStyle {
                        color: Some(tone),
                        background_color: Some(tone.opacity(0.2)),
                        ..Default::default()
                    },
                ));
            }
        }
    }
    let id = SharedString::from(format!("{key}-{}", if after { "after" } else { "before" }));
    div()
        .id(id.clone())
        .px_2p5()
        .py_1p5()
        .min_w_0()
        .rounded(feel::md())
        .border_1()
        .border_color(tone.opacity(0.3))
        .bg(tone.opacity(0.05))
        .text_xs()
        .text_color(theme.foreground)
        .child(crate::render::selectable_text(id, text, cx).highlights(highlights))
        .test_support()
        .into_any_element()
}
