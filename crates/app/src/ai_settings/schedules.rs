//! 定時タスクの一覧。未所属（どのワークスペースにも入れない）と、ワークスペースごと。
//!
//! 「＋」で「新規定時タスク」を足し、その場でタイトルを書く。行を押すと開き、タイトル・いつ・担当・
//! 期限・AI への指示をその場で変えられる。タイトルは Enter かほかの場所を押すと、AI への指示は
//! Enter かほかの場所を押すと保存し（Shift+Enter で改行）、Esc で書きかけを捨てて閉じる。

use crate::press::button;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::repeat::{self, Rule};
use malakt_core::schedule::{due_days_label, Schedule, SchedulePatch};
use malakt_core::{Person, Store};

use super::rules::workspace_tree;
use super::{col, px, row, AiSettings, CONTROL_WIDTH};
use crate::feel::{self, Lit};
use crate::render::app_icon;
use crate::repeat_view::{repeat_menu, Pick};

/// 期限のメニューに出す、作った日から何日後か。
const DUE_DAYS: [i64; 6] = [0, 1, 2, 3, 7, 14];
/// 期限の時刻のメニューに出す時。
const DUE_HOURS: [u32; 7] = [9, 10, 12, 13, 15, 17, 18];
/// 開いた行の、項目名の幅。
const LABEL_WIDTH: Pixels = px(88.);

/// 項目名と値の 1 行。
fn field(label: &'static str, value: impl IntoElement, cx: &App) -> Div {
    row()
        .gap_2()
        .items_start()
        .child(
            div()
                .w(LABEL_WIDTH)
                .flex_none()
                .pt_1()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
        .child(div().flex_1().min_w_0().flex().child(value))
}

/// メニューを開く、値を出したボタンの中身。
fn menu_face(icon: &'static str, text: SharedString, cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    row()
        .min_w_0()
        .gap_2()
        .child(app_icon(icon).small().text_color(muted))
        .child(div().min_w_0().truncate().child(text))
        .child(app_icon("chevron-down").small().text_color(muted))
}

/// メニューの 1 行を押したときに、定時タスクを `patch` で変える。
fn on_patch(
    settings: WeakEntity<AiSettings>,
    id: String,
    patch: SchedulePatch,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    move |_, _, cx| {
        let patch = patch.clone();
        // 設定を閉じたあとなら、反映する先がない
        let _ = settings.update(cx, |this, cx| this.patch_schedule(&id, patch, cx));
    }
}

impl AiSettings {
    /// 定時タスクの一覧で見る場所を変える。`None` は未所属。
    pub fn show_schedules(&mut self, scope: Option<String>, cx: &mut Context<Self>) {
        if self.schedules_scope != scope {
            self.schedules_scope = scope;
            self.schedule_open = None;
        }
        self.schedules_error = None;
        cx.notify();
    }

    /// データベースを変える。1 回の操作を、元に戻す 1 つの段にする。
    fn write_schedules<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Store) -> malakt_core::Result<R>,
    ) -> Option<R> {
        let result = crate::db::shared(cx).update(cx, |db, cx| {
            let result = db.store.undo_mark().and_then(|()| f(&mut db.store));
            let marked = db.store.undo_mark();
            cx.notify();
            result.and_then(|value| marked.map(|()| value))
        });
        match result {
            Ok(value) => {
                self.schedules_error = None;
                cx.notify();
                Some(value)
            }
            Err(error) => {
                self.schedules_error = Some(error.to_string().into());
                cx.notify();
                None
            }
        }
    }

    fn schedule(&self, id: &str, cx: &App) -> Option<Schedule> {
        crate::db::shared(cx).read(cx).store.schedule(id).ok()
    }

    /// 「新規定時タスク」を足して開き、タイトルを全部選んで書けるようにする。
    pub fn add_schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_open_schedule(cx);
        let scope = self.schedules_scope.clone();
        let now = malakt_core::view::now();
        let Some(made) =
            self.write_schedules(cx, |s| s.create_schedule(scope.as_deref(), "", None, &now))
        else {
            return;
        };
        self.open_schedule_now(&made, window, cx);
        let input = self.schedule_title.clone();
        // メニューやボタンからフォーカスが戻ったあとに移す
        cx.defer_in(window, move |this, window, cx| {
            if this.schedule_open.is_none() {
                return;
            }
            input.update(cx, |s, cx| {
                s.focus(window, cx);
                s.select_all(window, cx);
            });
        });
    }

    fn open_schedule_now(&mut self, s: &Schedule, window: &mut Window, cx: &mut Context<Self>) {
        self.schedule_open = Some(s.id.clone());
        let (title, ai) = (s.title.clone(), s.ai.clone());
        self.schedule_title
            .update(cx, |i, cx| i.set_value(title, window, cx));
        self.schedule_ai
            .update(cx, |i, cx| i.set_value(ai, window, cx));
        cx.notify();
    }

    /// 行を押した。閉じているなら開き、開いているなら保存して閉じる。
    pub fn toggle_schedule(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let same = self.schedule_open.as_deref() == Some(id);
        self.save_open_schedule(cx);
        if same {
            self.schedule_open = None;
            cx.notify();
            return;
        }
        if let Some(s) = self.schedule(id, cx) {
            self.open_schedule_now(&s, window, cx);
        }
    }

    fn save_open_schedule(&mut self, cx: &mut Context<Self>) {
        self.save_schedule_title(cx);
        self.save_schedule_ai(cx);
    }

    /// 書いたタイトルを保存する。空にしたときと変えていないときは、今のタイトルのまま。
    pub(super) fn save_schedule_title(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self
            .schedule_open
            .clone()
            .and_then(|id| self.schedule(&id, cx))
        else {
            return;
        };
        let title = self.schedule_title.read(cx).value().trim().to_owned();
        if title.is_empty() || title == s.title {
            return;
        }
        self.patch_schedule(
            &s.id,
            SchedulePatch {
                title: Some(title),
                ..SchedulePatch::default()
            },
            cx,
        );
    }

    pub(super) fn save_schedule_ai(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self
            .schedule_open
            .clone()
            .and_then(|id| self.schedule(&id, cx))
        else {
            return;
        };
        let ai = self.schedule_ai.read(cx).value().to_string();
        if ai.trim() == s.ai {
            return;
        }
        self.patch_schedule(
            &s.id,
            SchedulePatch {
                ai: Some(ai),
                ..SchedulePatch::default()
            },
            cx,
        );
    }

    /// Esc。書きかけのタイトルと AI への指示を捨てて、閉じる。
    pub fn cancel_schedule(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.schedule_open.take() else {
            return;
        };
        if let Some(s) = self.schedule(&id, cx) {
            self.schedule_title
                .update(cx, |i, cx| i.set_value(s.title, window, cx));
            self.schedule_ai
                .update(cx, |i, cx| i.set_value(s.ai, window, cx));
        }
        self.schedules_error = None;
        cx.notify();
    }

    fn patch_schedule(&mut self, id: &str, patch: SchedulePatch, cx: &mut Context<Self>) {
        let id = id.to_owned();
        let now = malakt_core::view::now();
        self.write_schedules(cx, |s| s.update_schedule(&id, &patch, &now));
    }

    fn delete_schedule(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.schedule_open.as_deref() == Some(id) {
            self.schedule_open = None;
        }
        let id = id.to_owned();
        self.write_schedules(cx, |s| s.delete_schedule(&id));
    }

    /// 定時タスクの一覧。`picker` のときは、上に場所（未所属かワークスペース）を選ぶボタンを出す。
    pub fn schedules_editor(&self, picker: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let store = &crate::db::shared(cx).read(cx).store;
        let scope = self.schedules_scope.clone();
        let loaded = store.workspaces().and_then(|all| {
            let scope = scope.filter(|id| all.iter().any(|w| &w.id == id));
            let list = store.schedules(scope.as_deref())?;
            // ワークスペースの既定値（担当と期限）。選んでいないときに、何が入るかを出す
            let defaults = store.draft_for(scope.as_deref(), "")?;
            Ok((all, scope, list, store.people()?, defaults))
        });
        let (workspaces, scope, list, people, defaults) = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
                return div()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(format!("定時タスクを読めませんでした: {error}"))
                    .into_any_element();
            }
        };
        let tree = workspace_tree(&workspaces);
        let shown: SharedString = match &scope {
            None => "未所属".into(),
            Some(id) => tree
                .iter()
                .find(|(w, _)| &w.id == id)
                .map(|(w, _)| w.name.clone())
                .unwrap_or_default()
                .into(),
        };
        let settings = cx.entity().downgrade();
        let scope_menu = button("schedules-scope")
            .outline()
            .w(CONTROL_WIDTH)
            .h_8()
            .child(
                row()
                    .min_w_0()
                    .gap_2()
                    .child(div().flex_1().min_w_0().truncate().child(shown))
                    .child(
                        app_icon("chevron-down")
                            .small()
                            .text_color(theme.muted_foreground),
                    ),
            )
            .pop_menu({
                let scope = scope.clone();
                move |mut menu, _, _| {
                    let choices = std::iter::once((None, "未所属".to_owned(), 0)).chain(
                        tree.iter()
                            .map(|(w, depth)| (Some(w.id.clone()), w.name.clone(), *depth)),
                    );
                    for (id, name, depth) in choices {
                        let settings = settings.clone();
                        let checked = id == scope;
                        // サブワークスペースは、深さの分だけ下げて出す
                        let label = format!("{}{name}", "　".repeat(depth));
                        menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                            move |_, _, cx| {
                                // 設定を閉じたあとなら、反映する先がない
                                let _ = settings.update(cx, |this, cx| {
                                    this.save_open_schedule(cx);
                                    this.show_schedules(id.clone(), cx)
                                });
                            },
                        ));
                    }
                    menu
                }
            });
        let add = feel::gloss(
            button("schedule-add")
                .icon(app_icon("plus"))
                .tooltip("定時タスクを追加")
                .ghost()
                .small()
                .on_click(cx.listener(|this, _, window, cx| this.add_schedule(window, cx))),
        );
        let top = row()
            .gap_1()
            .when(picker, |d| d.child(scope_menu))
            .child(div().flex_1())
            .child(add);

        let mut rows = col()
            .id("schedules")
            .rounded(feel::md())
            .border_1()
            .border_color(theme.border)
            .overflow_hidden();
        for (ix, s) in list.iter().enumerate() {
            rows = rows.child(self.schedule_row(ix, s, &people, &defaults, cx));
        }
        col()
            .w_full()
            .gap_2()
            .child(top)
            .when(!list.is_empty(), |d| d.child(rows.test_support()))
            .when_some(self.schedules_error.clone(), |d, error| {
                d.child(
                    div()
                        .id("schedules-error")
                        .text_xs()
                        .text_color(theme.danger)
                        .child(error)
                        .test_support(),
                )
            })
            .into_any_element()
    }

    fn schedule_row(
        &self,
        ix: usize,
        s: &Schedule,
        people: &[Person],
        defaults: &malakt_core::TaskDraft,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let open = self.schedule_open.as_deref() == Some(s.id.as_str());
        let id = s.id.clone();
        let group = SharedString::from(format!("schedule-{ix}"));
        // タイトルのところ。開いているときは、同じ大きさ・位置のまま入力欄になる
        let title = if open {
            div()
                .flex_1()
                .min_w_0()
                .mx(px(-8.))
                .child(
                    Input::new(&self.schedule_title)
                        .id("schedule-title")
                        .aria_label("定時タスクのタイトル")
                        .text_sm()
                        .bg(theme.background)
                        .border_color(theme.ring)
                        .px_2(),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .when(!s.enabled, |d| d.text_color(theme.muted_foreground))
                .child(s.title.clone())
                .into_any_element()
        };
        let head = row()
            .id(("schedule", ix))
            .flex_1()
            .min_w_0()
            .gap_2()
            .py_1()
            .cursor_pointer()
            .child(
                div()
                    .id(("schedule-fold", ix))
                    .flex_none()
                    .child(
                        app_icon(if open {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        })
                        .small()
                        .text_color(theme.muted_foreground),
                    )
                    .test_support(),
            )
            .child(title)
            .when(!open, |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(s.rule.label()),
                )
            })
            .on_click(cx.listener({
                let id = id.clone();
                move |this, _: &ClickEvent, window, cx| {
                    // 開いている間にタイトルの欄を押したときは、書き続ける
                    if this.schedule_open.as_deref() == Some(id.as_str())
                        && this
                            .schedule_title
                            .read(cx)
                            .focus_handle(cx)
                            .is_focused(window)
                    {
                        return;
                    }
                    this.toggle_schedule(&id, window, cx)
                }
            }))
            .test_support();
        let switch = Switch::new(("schedule-on", ix))
            .checked(s.enabled)
            .small()
            .tooltip(if s.enabled { "動かす" } else { "止める" })
            .on_click(cx.listener({
                let id = id.clone();
                move |this, on: &bool, _, cx| {
                    this.patch_schedule(
                        &id,
                        SchedulePatch {
                            enabled: Some(*on),
                            ..SchedulePatch::default()
                        },
                        cx,
                    )
                }
            }));
        let delete = button(("schedule-delete", ix))
            .icon(app_icon("trash"))
            .tooltip("削除")
            .ghost()
            .xsmall()
            // 乗せたときと、開いているときだけ見せる
            .when(!open, |b| {
                b.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.))
            })
            .on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| this.delete_schedule(&id, cx)
            }));
        let line = row()
            .group(group)
            .gap_1()
            .px_3()
            .min_h(px(36.))
            .lit(theme.muted, cx)
            .child(head)
            .child(switch)
            .child(feel::gloss(delete));
        col()
            .when(ix > 0, |d| d.border_t_1().border_color(theme.border))
            .when(open, |d| d.bg(theme.accent.opacity(0.4)))
            .child(line)
            .when(open, |d| {
                d.child(self.schedule_fields(s, people, defaults, cx))
            })
            // Esc は、書きかけを捨てて閉じる。設定のウィンドウは閉じない
            .on_action(
                cx.listener(|this, _: &crate::Dismiss, window, cx| {
                    this.cancel_schedule(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &gpui_kit::base::actions::Cancel, window, cx| {
                    this.cancel_schedule(window, cx)
                }),
            )
            .into_any_element()
    }

    /// 開いた定時タスクの、いつ・担当・期限・AI への指示。
    fn schedule_fields(
        &self,
        s: &Schedule,
        people: &[Person],
        defaults: &malakt_core::TaskDraft,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let settings = cx.entity().downgrade();
        let id = s.id.clone();

        // いつ
        let pick: Pick = std::rc::Rc::new({
            let (settings, id) = (settings.clone(), id.clone());
            move |rule: Option<Rule>, cx: &mut App| {
                let Some(rule) = rule else {
                    return;
                };
                // 設定を閉じたあとなら、反映する先がない
                let _ = settings.update(cx, |this, cx| {
                    this.patch_schedule(
                        &id,
                        SchedulePatch {
                            rule: Some(rule),
                            ..SchedulePatch::default()
                        },
                        cx,
                    )
                });
            }
        });
        let current = s.rule.clone();
        let when = row()
            .gap_2()
            .flex_wrap()
            .child(
                div().flex().child(
                    button("schedule-rule")
                        .accessibility_label(format!("いつ: {}", s.rule.label()))
                        .outline()
                        .small()
                        .child(menu_face("repeat", s.rule.label().into(), cx))
                        .pop_menu(move |menu, window, cx| {
                            repeat_menu(menu, &pick, &current, Some(&current), false, window, cx)
                        }),
                ),
            )
            .when(s.enabled, |d| {
                d.child(
                    div()
                        .id("schedule-next")
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(repeat::next_label(&s.next_at))
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new("次に作る日時")
                                .build(window, cx)
                        }),
                )
            });

        // 担当。選んでいなければ、ワークスペースの既定値
        let name_of = |pid: &str| people.iter().find(|p| p.id == pid).map(|p| p.name.clone());
        let names = |ids: &[String]| {
            ids.iter()
                .filter_map(|pid| name_of(pid))
                .collect::<Vec<_>>()
                .join("、")
        };
        let chosen = names(&s.assignee_ids);
        let shown_people: SharedString = if !chosen.is_empty() {
            chosen.into()
        } else if defaults.assignee_ids.is_empty() {
            "なし".into()
        } else {
            format!("ワークスペースの設定（{}）", names(&defaults.assignee_ids)).into()
        };
        let assignees = button("schedule-assignees")
            .accessibility_label(format!("担当: {shown_people}"))
            .outline()
            .small()
            .child(menu_face("user", shown_people, cx))
            .pop_menu({
                let (settings, id) = (settings.clone(), id.clone());
                let people: Vec<Person> = people.to_vec();
                let ids = s.assignee_ids.clone();
                move |mut menu, _, _| {
                    for person in &people {
                        let on = ids.contains(&person.id);
                        let mut next = ids.clone();
                        if on {
                            next.retain(|p| p != &person.id);
                        } else {
                            next.push(person.id.clone());
                        }
                        menu = menu.item(
                            PopupMenuItem::new(person.name.clone())
                                .checked(on)
                                .on_click(on_patch(
                                    settings.clone(),
                                    id.clone(),
                                    SchedulePatch {
                                        assignee_ids: Some(next),
                                        ..SchedulePatch::default()
                                    },
                                )),
                        );
                    }
                    menu
                }
            });

        // 期限。決めていなければ、ワークスペースの既定値
        let shown_due: SharedString = match s.due_label() {
            Some(label) => label.into(),
            None if defaults.due.is_some() => "ワークスペースの設定".into(),
            None => "なし".into(),
        };
        let none_label = if defaults.due.is_some() {
            "ワークスペースの設定"
        } else {
            "なし"
        };
        let due = button("schedule-due")
            .accessibility_label(format!("期限: {shown_due}"))
            .outline()
            .small()
            .child(menu_face("calendar", shown_due, cx))
            .pop_menu({
                let (settings, id) = (settings.clone(), id.clone());
                let (days, time) = (s.due_days, s.due_time.clone());
                move |mut menu, window, cx| {
                    let set = |patch| on_patch(settings.clone(), id.clone(), patch);
                    menu = menu.item(
                        PopupMenuItem::new(none_label)
                            .checked(days.is_none())
                            .on_click(set(SchedulePatch {
                                due_days: Some(None),
                                ..SchedulePatch::default()
                            })),
                    );
                    menu = menu.separator();
                    for n in DUE_DAYS {
                        menu = menu.item(
                            PopupMenuItem::new(due_days_label(n))
                                .checked(days == Some(n))
                                .on_click(set(SchedulePatch {
                                    due_days: Some(Some(n)),
                                    ..SchedulePatch::default()
                                })),
                        );
                    }
                    if days.is_none() {
                        return menu;
                    }
                    let (settings, id, time) = (settings.clone(), id.clone(), time.clone());
                    menu.separator()
                        .submenu("時刻", window, cx, move |mut menu, _, _| {
                            let set = |patch| on_patch(settings.clone(), id.clone(), patch);
                            menu = menu.item(
                                PopupMenuItem::new("なし")
                                    .checked(time.is_none())
                                    .on_click(set(SchedulePatch {
                                        due_time: Some(None),
                                        ..SchedulePatch::default()
                                    })),
                            );
                            for hour in DUE_HOURS {
                                let value = format!("{hour:02}:00");
                                menu = menu.item(
                                    PopupMenuItem::new(format!("{hour}:00"))
                                        .checked(time.as_deref() == Some(value.as_str()))
                                        .on_click(set(SchedulePatch {
                                            due_time: Some(Some(value)),
                                            ..SchedulePatch::default()
                                        })),
                                );
                            }
                            menu
                        })
                }
            });

        let ai = div()
            .id("schedule-ai")
            .flex_1()
            .min_w_0()
            .child(
                Textarea::new(&self.schedule_ai)
                    .aria_label("AI への指示")
                    .small()
                    .bg(theme.background),
            )
            .test_support();
        col()
            .id("schedule-fields")
            .gap_2()
            .px_3()
            .pb_3()
            .pt_1()
            .child(field("いつ", when, cx))
            .child(field("担当", assignees, cx))
            .child(field("期限", due, cx))
            .child(field("AI への指示", ai, cx))
            .test_support()
    }
}
