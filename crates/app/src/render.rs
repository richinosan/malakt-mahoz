//! 画面の描画。状態は `state` にあり、ここは見た目だけを組み立てる。

use std::ops::Range;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::view::{self, AssigneeFilter, DueState, Priority as Prio, Sort, Tab};
use malakt_core::{Role as Raci, Roles, Status, Task as Todo};

use crate::state::{Editing, MainView, Modal, Nav};
use crate::{Dismiss, LockNow, NewWindow};

const SIDEBAR_WIDTH: Pixels = px(248.);
const PANEL_WIDTH: Pixels = px(480.);
const ROW_HEIGHT: Pixels = px(40.);
const CARD_HEIGHT: Pixels = px(84.);

fn row() -> Div {
    div().flex().flex_row().items_center()
}

fn col() -> Div {
    div().flex().flex_col()
}

/// 押せる小さなラベル。選ばれているものは色を付ける。
fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded_md()
        .border_1()
        .text_sm()
        .cursor_pointer()
        .flex_none()
        .when(active, |d| {
            d.bg(theme.primary)
                .text_color(theme.primary_foreground)
                .border_color(theme.primary)
        })
        .when(!active, |d| {
            d.border_color(theme.border)
                .hover(|s| s.bg(theme.secondary_hover))
        })
        .child(label.into())
}

fn status_color(status: Status, cx: &App) -> Hsla {
    let theme = cx.theme();
    match status {
        Status::NeedsAction => theme.muted_foreground,
        Status::InProcess => theme.info,
        Status::Completed => theme.success,
        Status::Cancelled => theme.muted_foreground,
    }
}

fn status_badge(status: Status, cx: &App) -> Div {
    let color = status_color(status, cx);
    row()
        .gap_1()
        .flex_none()
        .text_sm()
        .child(div().size_2().rounded_full().bg(color))
        .child(status.label())
}

fn section_title(text: &'static str) -> Div {
    div().text_sm().font_weight(FontWeight::BOLD).child(text)
}

impl MainView {
    fn due_label(&self, task: &Todo, cx: &App) -> Div {
        let theme = cx.theme();
        let Some(due) = &task.due else {
            return div().text_color(theme.muted_foreground).child("—");
        };
        let text = view::format_date(due, &self.today);
        if !task.status.is_open() {
            return div().text_color(theme.muted_foreground).child(text);
        }
        match view::due_state(due, &self.today) {
            DueState::Overdue => div()
                .text_color(theme.danger)
                .font_weight(FontWeight::BOLD)
                .child(format!("{text}（期限切れ）")),
            DueState::Today => div()
                .text_color(theme.primary)
                .font_weight(FontWeight::BOLD)
                .child(format!("{text}（今日）")),
            DueState::Soon => div()
                .text_color(theme.primary)
                .font_weight(FontWeight::BOLD)
                .child(text),
            DueState::Later => div().child(text),
        }
    }

    // ---- 左のメニュー ----

    fn nav_item(
        &self,
        id: impl Into<ElementId>,
        label: impl Into<SharedString>,
        nav: Nav,
        count: Option<u32>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let active = self.nav == nav;
        row()
            .id(id)
            .flex_1()
            .min_w_0()
            .h_8()
            .px_2()
            .gap_2()
            .rounded_md()
            .cursor_pointer()
            .text_sm()
            .when(active, |d| {
                d.bg(theme.sidebar_accent)
                    .text_color(theme.sidebar_accent_foreground)
                    .font_weight(FontWeight::BOLD)
            })
            .when(!active, |d| d.hover(|s| s.bg(theme.sidebar_accent)))
            .child(div().flex_1().truncate().child(label.into()))
            .when_some(count.filter(|&c| c > 0), |d, c| {
                d.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(c.to_string()),
                )
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.go(nav.clone(), cx)))
            .test_support()
            .into_any_element()
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut tree = col().gap_0p5();
        for (ix, node) in self.tree.iter().enumerate() {
            let ws = &self.workspaces[node.index];
            let id = ws.id.clone();
            let collapsed = self.collapsed.contains(&ws.id);
            let caret = if !node.has_children {
                div().w_4().flex_none().into_any_element()
            } else {
                let id = id.clone();
                div()
                    .id(("caret", ix))
                    .w_4()
                    .flex_none()
                    .cursor_pointer()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(if collapsed { "▸" } else { "▾" })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.toggle_collapsed(&id, cx);
                    }))
                    .into_any_element()
            };
            tree = tree.child(row().pl(px(12.) * node.depth as f32).child(caret).child(
                self.nav_item(
                    ("ws", ix),
                    ws.name.clone(),
                    Nav::Workspace(id),
                    self.open_counts.get(&ws.id).copied(),
                    cx,
                ),
            ));
        }

        let action = |id: &'static str, label: &'static str| {
            Button::new(id).label(label).ghost().small().w_full()
        };
        col()
            .w(SIDEBAR_WIDTH)
            .flex_none()
            .h_full()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .border_r_1()
            .border_color(theme.sidebar_border)
            .p_2()
            .gap_1()
            .child(
                div()
                    .px_2()
                    .py_1()
                    .font_weight(FontWeight::BOLD)
                    .child("malakt-mahoz"),
            )
            .child(row().child(self.nav_item("nav-home", "ホーム", Nav::Home, None, cx)))
            .child(row().child(self.nav_item("nav-all", "すべてのタスク", Nav::All, None, cx)))
            .child(row().child(self.nav_item("nav-unfiled", "未所属", Nav::Unfiled, None, cx)))
            .child(
                row()
                    .mt_2()
                    .px_2()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child("ワークスペース"),
                    )
                    .child(
                        Button::new("add-workspace")
                            .label("＋")
                            .ghost()
                            .xsmall()
                            .tooltip("ワークスペースを追加")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.ask_workspace_name(None, None, window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .id("workspace-tree")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(tree)
                    .when(self.workspaces.is_empty(), |d| {
                        d.child(
                            div()
                                .px_2()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("まだありません。なくても使えます。"),
                        )
                    }),
            )
            .child(div().h_px().bg(theme.sidebar_border))
            .child(
                action("open-people", "担当者")
                    .on_click(cx.listener(|this, _, window, cx| this.open_people(window, cx))),
            )
            .child(
                action("export", "エクスポート")
                    .on_click(cx.listener(|this, _, window, cx| this.export(window, cx))),
            )
            .child(
                action("import", "インポート")
                    .on_click(cx.listener(|this, _, window, cx| this.import(window, cx))),
            )
            .child(
                action("lock-settings", "ロックの設定").on_click(
                    cx.listener(|this, _, window, cx| this.open_lock_settings(window, cx)),
                ),
            )
            .when(self.lock_enabled(cx), |d| {
                d.child(
                    action("lock-now", "今すぐロック")
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(LockNow), cx)),
                )
            })
            .child(
                action("new-window", "新しいウィンドウ")
                    .on_click(|_, window, cx| window.dispatch_action(Box::new(NewWindow), cx)),
            )
    }

    // ---- ホーム ----

    fn quick_add(&self, cx: &mut Context<Self>) -> Div {
        row()
            .gap_2()
            .child(
                Input::new(&self.inputs.quick_add)
                    .id("quick-add")
                    .aria_label("タスクを追加")
                    .flex_1(),
            )
            .child(
                Button::new("quick-add-submit")
                    .label("追加")
                    .primary()
                    .on_click(cx.listener(|this, _, window, cx| this.submit_quick_add(window, cx))),
            )
    }

    fn render_home(&self, roomy: bool, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let due = self.due.clone();
        let count = due.len();
        let roots: Vec<_> = self
            .workspaces
            .iter()
            .filter(|w| w.parent_id.is_none())
            .cloned()
            .collect();
        col()
            .flex_1()
            .min_h_0()
            .p_4()
            .gap_3()
            .child(self.quick_add(cx))
            .child(
                row()
                    .gap_3()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::BOLD)
                            .child("期限が近いタスク"),
                    )
                    .child(div().flex_1())
                    .child(
                        Checkbox::new("due-mine")
                            .label("自分の担当だけ")
                            .checked(self.due_mine)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.due_mine = *checked;
                                this.reload(cx);
                            })),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(format!(
                        "未完了のタスクのうち、期限が {} 日以内のものと、期限が過ぎたものです。",
                        view::HOME_DUE_DAYS
                    )),
            )
            .child(if count == 0 {
                div()
                    .py_4()
                    .text_color(theme.muted_foreground)
                    .child(if self.due_mine {
                        "自分が担当の、期限が近いタスクはありません。"
                    } else {
                        "期限が近いタスクはありません。"
                    })
            } else {
                col()
                    .flex_1()
                    .min_h(px(120.))
                    .border_1()
                    .border_color(theme.border)
                    .rounded_md()
                    .child(
                        uniform_list(
                            "due-list",
                            count,
                            cx.processor(move |this, range: Range<usize>, _, cx| {
                                range
                                    .map(|ix| this.render_due_row(ix, &due[ix], roomy, cx))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .size_full(),
                    )
            })
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::BOLD)
                    .child("ワークスペース"),
            )
            .when(roots.is_empty(), |d| {
                d.child(div().text_sm().text_color(theme.muted_foreground).child(
                    "ワークスペースがなくても、そのままタスクを追加できます。\
                         あとから整理したくなったら作りましょう。",
                ))
            })
            .child(
                row()
                    .flex_wrap()
                    .gap_2()
                    .children(roots.into_iter().enumerate().map(|(ix, ws)| {
                        let open = self.open_counts.get(&ws.id).copied().unwrap_or(0);
                        let id = ws.id.clone();
                        col()
                            .id(("home-ws", ix))
                            .w(px(200.))
                            .p_3()
                            .gap_1()
                            .border_1()
                            .border_color(theme.border)
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.secondary_hover))
                            .child(
                                div()
                                    .font_weight(FontWeight::BOLD)
                                    .truncate()
                                    .child(ws.name),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("未完了 {open} 件")),
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.go(Nav::Workspace(id.clone()), cx)
                            }))
                    }))
                    .child(
                        Button::new("home-add-workspace")
                            .label("ワークスペースを追加")
                            .outline()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.ask_workspace_name(None, None, window, cx)
                            })),
                    ),
            )
    }

    fn render_due_row(
        &self,
        ix: usize,
        task: &Todo,
        roomy: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let id = task.id.clone();
        row()
            .id(("due", ix))
            .w_full()
            .h(ROW_HEIGHT)
            .px_3()
            .gap_3()
            .border_b_1()
            .border_color(theme.border)
            .cursor_pointer()
            .hover(|s| s.bg(theme.list_hover))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(task.title.clone()),
            )
            // 狭いウィンドウでは、タイトルが読めるように場所と担当を省く
            .when(roomy, |d| {
                d.child(
                    div()
                        .w(px(140.))
                        .truncate()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(self.workspace_name(task.workspace_id.as_deref())),
                )
            })
            .child(self.due_label(task, cx).w(px(130.)).text_sm())
            .child(status_badge(task.status, cx).w(px(76.)))
            .when(roomy, |d| {
                d.child(
                    div()
                        .w(px(120.))
                        .truncate()
                        .text_sm()
                        .child(self.assignee_label(&task.assignee_ids)),
                )
            })
            .on_click(
                cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_panel(&id, window, cx)
                }),
            )
            .into_any_element()
    }

    // ---- 一覧 ----

    fn list_title(&self) -> SharedString {
        match &self.nav {
            Nav::Home => "ホーム".into(),
            Nav::All => "すべてのタスク".into(),
            Nav::Unfiled => "未所属のタスク".into(),
            Nav::Workspace(id) => {
                // 最上位からの道筋を「親 › 子」で出す
                let mut names = Vec::new();
                let mut current = self.workspaces.iter().find(|w| &w.id == id);
                while let Some(w) = current {
                    names.push(w.name.as_str());
                    current = w
                        .parent_id
                        .as_ref()
                        .and_then(|p| self.workspaces.iter().find(|x| &x.id == p));
                    if names.len() > 64 {
                        break;
                    }
                }
                names.reverse();
                names.join(" › ").into()
            }
        }
    }

    fn render_filters(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let group = |label: &'static str, chips: Vec<Stateful<Div>>| {
            row()
                .gap_1()
                .flex_wrap()
                .child(
                    div()
                        .w(px(64.))
                        .flex_none()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .children(chips)
        };
        macro_rules! pick {
            ($id:expr, $label:expr, $active:expr, |$this:ident| $body:expr) => {
                chip($id, $label, $active, cx).on_click(cx.listener(
                    move |$this, _: &ClickEvent, _, cx| {
                        $body;
                        $this.refilter();
                        cx.notify();
                    },
                ))
            };
        }
        let q = &self.query;
        let mut status = vec![pick!(
            "f-status-all",
            "すべて",
            q.status.is_none(),
            |this| this.query.status = None
        )];
        for (i, s) in Status::ALL.into_iter().enumerate() {
            status.push(pick!(
                ("f-status", i),
                s.label(),
                q.status == Some(s),
                |this| this.query.status = Some(s)
            ));
        }
        let mut assignee = vec![
            pick!(
                "f-as-all",
                "すべて",
                q.assignee == AssigneeFilter::All,
                |this| this.query.assignee = AssigneeFilter::All
            ),
            pick!(
                "f-as-me",
                "自分",
                q.assignee == AssigneeFilter::Me,
                |this| this.query.assignee = AssigneeFilter::Me
            ),
            pick!(
                "f-as-none",
                "未割り当て",
                q.assignee == AssigneeFilter::Unassigned,
                |this| this.query.assignee = AssigneeFilter::Unassigned
            ),
        ];
        for (i, p) in self.people.iter().enumerate() {
            let id = p.id.clone();
            assignee.push(pick!(
                ("f-as", i),
                p.name.clone(),
                q.assignee == AssigneeFilter::Person(p.id.clone()),
                |this| this.query.assignee = AssigneeFilter::Person(id.clone())
            ));
        }
        let mut my_role = vec![pick!(
            "f-role-all",
            "すべて",
            q.my_role.is_none(),
            |this| this.query.my_role = None
        )];
        for (i, role) in Raci::ALL.into_iter().enumerate() {
            my_role.push(pick!(
                ("f-role", i),
                format!("{}（{}）", role.code(), role.label()),
                q.my_role == Some(role),
                |this| this.query.my_role = Some(role)
            ));
        }
        let mut priority = vec![pick!(
            "f-pr-all",
            "すべて",
            q.priority.is_none(),
            |this| this.query.priority = None
        )];
        for (i, p) in [Prio::High, Prio::Mid, Prio::Low, Prio::None]
            .into_iter()
            .enumerate()
        {
            priority.push(pick!(
                ("f-pr", i),
                p.label(),
                q.priority == Some(p),
                |this| this.query.priority = Some(p)
            ));
        }
        let mut category = vec![pick!(
            "f-cat-all",
            "すべて",
            q.category.is_none(),
            |this| this.query.category = None
        )];
        for (i, c) in self.categories.iter().enumerate() {
            let value = c.clone();
            category.push(pick!(
                ("f-cat", i),
                c.clone(),
                q.category.as_ref() == Some(c),
                |this| this.query.category = Some(value.clone())
            ));
        }
        let sort = Sort::ALL
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                pick!(("f-sort", i), s.label(), q.sort == s, |this| this
                    .query
                    .sort = s)
            })
            .collect();
        col()
            .gap_2()
            .p_3()
            .border_1()
            .border_color(theme.border)
            .rounded_md()
            .child(
                row()
                    .gap_1()
                    .child(
                        div()
                            .w(px(64.))
                            .flex_none()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child("検索"),
                    )
                    .child(
                        Input::new(&self.inputs.search)
                            .id("task-search")
                            .aria_label("検索")
                            .cleanable(true)
                            .small()
                            .w(px(280.)),
                    ),
            )
            .child(group("状態", status))
            .child(group("担当", assignee))
            .child(group("自分の役", my_role))
            .child(group("優先度", priority))
            .when(!self.categories.is_empty(), |d| {
                d.child(group("カテゴリ", category))
            })
            .child(group("並び順", sort))
    }

    fn render_bulk_bar(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let count = self.selected.len();
        let mut bar = row()
            .gap_2()
            .flex_wrap()
            .p_2()
            .rounded_md()
            .bg(theme.secondary)
            .child(div().text_sm().child(format!("{count} 件を選択中")))
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("状態"),
            );
        for (i, s) in Status::ALL.into_iter().enumerate() {
            bar = bar.child(
                chip(("bulk-status", i), s.label(), false, cx).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| this.bulk_edit(cx, |d| d.status = s),
                )),
            );
        }
        bar = bar.child(
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("優先度"),
        );
        for (i, p) in Prio::ALL.into_iter().enumerate() {
            bar = bar.child(
                chip(("bulk-prio", i), p.label(), false, cx).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.bulk_edit(cx, |d| d.priority = p.value())
                    },
                )),
            );
        }
        bar.child(div().flex_1())
            .child(
                Button::new("bulk-move")
                    .label("移動")
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.modal = Modal::MoveTasks(this.selected_ids());
                        cx.notify();
                    })),
            )
            .child(
                Button::new("bulk-delete")
                    .label("削除")
                    .danger()
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.modal = Modal::DeleteTasks(this.selected_ids());
                        cx.notify();
                    })),
            )
            .child(
                Button::new("bulk-clear")
                    .label("選択を解除")
                    .ghost()
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.selected.clear();
                        cx.notify();
                    })),
            )
    }

    fn render_list(&self, table: bool, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let active = self.query.active_count();
        let visible = self.visible.len();
        let tab =
            |id: &'static str, label: String, tab: Tab, this: &Self, cx: &mut Context<Self>| {
                chip(
                    id,
                    label,
                    this.query.tab == tab && this.query.status.is_none(),
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.query.tab = tab;
                    this.query.status = None;
                    this.refilter();
                    cx.notify();
                }))
                .test_support()
            };
        let workspace_id = self.current_workspace().map(str::to_owned);
        let has_children = workspace_id.as_ref().is_some_and(|id| {
            self.workspaces
                .iter()
                .any(|w| w.parent_id.as_ref() == Some(id))
        });

        let mut header = row().gap_2().flex_wrap().child(
            div()
                .flex_1()
                .min_w(px(120.))
                .text_lg()
                .font_weight(FontWeight::BOLD)
                .truncate()
                .child(self.list_title()),
        );
        if let Some(id) = workspace_id.clone() {
            let (a, b, c) = (id.clone(), id.clone(), id);
            header = header
                .child(
                    Button::new("ws-add-child")
                        .label("子を追加")
                        .small()
                        .outline()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.ask_workspace_name(Some(a.clone()), None, window, cx)
                        })),
                )
                .child(
                    Button::new("ws-rename")
                        .label("名前を変更")
                        .small()
                        .outline()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.ask_workspace_name(None, Some(b.clone()), window, cx)
                        })),
                )
                .child(
                    Button::new("ws-delete")
                        .label("削除")
                        .small()
                        .outline()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.modal = Modal::DeleteWorkspace(c.clone());
                            cx.notify();
                        })),
                );
        }

        let body = if self.tasks.is_empty() {
            col()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_1()
                .child(
                    div()
                        .font_weight(FontWeight::BOLD)
                        .child("まだタスクがありません"),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("最初のタスクを追加しましょう。"),
                )
        } else if visible == 0 {
            col()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(theme.muted_foreground)
                .child(if active > 0 {
                    "条件に合うタスクはありません。"
                } else if self.query.tab == Tab::Open {
                    "未完了のタスクはありません。"
                } else {
                    "完了したタスクはありません。"
                })
                .when(active > 0, |d| {
                    d.child(
                        Button::new("clear-filters")
                            .label("絞り込みを解除")
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.query.clear_filters();
                                this.inputs
                                    .search
                                    .update(cx, |s, cx| s.set_value("", window, cx));
                                this.refilter();
                                cx.notify();
                            })),
                    )
                })
        } else {
            col()
                .flex_1()
                .min_h_0()
                .border_1()
                .border_color(theme.border)
                .rounded_md()
                .when(table, |d| d.child(self.render_table_head(cx)))
                .child(
                    uniform_list(
                        "task-list",
                        visible,
                        cx.processor(move |this, range: Range<usize>, _, cx| {
                            range
                                .map(|ix| this.render_task_row(ix, table, cx))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .flex_1(),
                )
        };

        col()
            .flex_1()
            .min_h_0()
            .p_4()
            .gap_3()
            .child(header)
            .child(
                row()
                    .gap_2()
                    .flex_wrap()
                    .child(tab(
                        "tab-open",
                        format!("未完了 {}", self.counts.0),
                        Tab::Open,
                        self,
                        cx,
                    ))
                    .child(tab(
                        "tab-closed",
                        format!("完了 {}", self.counts.1),
                        Tab::Closed,
                        self,
                        cx,
                    ))
                    .child(
                        chip(
                            "toggle-filters",
                            if active > 0 {
                                format!("絞り込み・並べ替え（{active}）")
                            } else {
                                "絞り込み・並べ替え".to_owned()
                            },
                            self.filters_open,
                            cx,
                        )
                        .on_click(cx.listener(
                            |this, _: &ClickEvent, _, cx| {
                                this.filters_open = !this.filters_open;
                                cx.notify();
                            },
                        )),
                    )
                    .when(has_children, |d| {
                        d.child(
                            Checkbox::new("include-descendants")
                                .label("子のワークスペースも含める")
                                .checked(self.include_descendants)
                                .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                    this.include_descendants = *checked;
                                    this.reload(cx);
                                })),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(format!("{visible} 件のタスクを表示しています。")),
                    ),
            )
            .when(self.filters_open, |d| d.child(self.render_filters(cx)))
            .when(!self.selected.is_empty(), |d| {
                d.child(self.render_bulk_bar(cx))
            })
            .child(body)
            .child(self.quick_add(cx))
    }

    fn render_table_head(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme();
        let all = !self.visible.is_empty() && self.selected.len() == self.visible.len();
        row()
            .h_8()
            .px_3()
            .gap_3()
            .flex_none()
            .bg(theme.table_head)
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .text_color(theme.table_head_foreground)
            .child(
                div().w_5().flex_none().child(
                    Checkbox::new("select-all")
                        .accessibility_label("表示中のタスクをすべて選択")
                        .checked(all)
                        .indeterminate(!all && !self.selected.is_empty())
                        .on_click(cx.listener(|this, _: &bool, _, cx| this.toggle_select_all(cx))),
                ),
            )
            .child(div().flex_1().child("タイトル"))
            .child(div().w(px(84.)).child("状態"))
            .child(div().w(px(56.)).child("優先度"))
            .child(div().w(px(130.)).child("担当"))
            .child(div().w(px(130.)).child("期限"))
            .child(div().w(px(80.)).child("更新"))
    }

    fn render_task_row(&self, ix: usize, table: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let task = &self.tasks[self.visible[ix]];
        let id = task.id.clone();
        let active = self.panel.as_deref() == Some(id.as_str());
        let closed = !task.status.is_open();
        let now = malakt_core::now_ms();

        let select = {
            let id = id.clone();
            div().w_5().flex_none().child(
                Checkbox::new(("select", ix))
                    .accessibility_label(format!("「{}」を選択", task.title))
                    .checked(self.selected.contains(&task.id))
                    .on_click(
                        cx.listener(move |this, _: &bool, _, cx| this.toggle_selected(&id, cx)),
                    ),
            )
        };
        let title = {
            let id = id.clone();
            div()
                .id(("title", ix))
                .flex_1()
                .min_w_0()
                .truncate()
                .cursor_pointer()
                .hover(|s| s.text_color(theme.link))
                .when(closed, |d| {
                    d.text_color(theme.muted_foreground).line_through()
                })
                .when(task.parent_id.is_some(), |d| d.pl_4())
                .child(task.title.clone())
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.open_panel(&id, window, cx)
                }))
                .test_support()
        };
        let status = {
            let id = id.clone();
            status_badge(task.status, cx)
                .id(("status", ix))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.ask_status(&id, window, cx)
                }))
        };
        let priority = {
            let id = id.clone();
            let current = Prio::of(task.priority);
            // 押すたびに なし → 高 → 中 → 低 と回す
            let next = match current {
                Prio::None => Prio::High,
                Prio::High => Prio::Mid,
                Prio::Mid => Prio::Low,
                Prio::Low => Prio::None,
            };
            div()
                .id(("prio", ix))
                .cursor_pointer()
                .text_sm()
                .when(current == Prio::None, |d| {
                    d.text_color(theme.muted_foreground)
                })
                .when(current == Prio::High, |d| {
                    d.text_color(theme.danger).font_weight(FontWeight::BOLD)
                })
                .child(if current == Prio::None {
                    "—"
                } else {
                    current.label()
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.edit_task_by_id(&id, cx, |d| d.priority = next.value())
                }))
        };
        let assignees = div().truncate().text_sm().child({
            // 担当者が複数いるときは、何人が自分の分を終えたかを先に出す
            let (done, total) = task.done_count();
            let names = self.assignee_label(&task.assignee_ids);
            if total > 1 {
                SharedString::from(format!("{done}/{total} {names}"))
            } else {
                names
            }
        });
        let due = self.due_label(task, cx).text_sm();
        let updated = div()
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(view::format_relative(task.updated_at, now));

        let base = div()
            .id(("row", ix))
            .w_full()
            .border_b_1()
            .border_color(theme.border)
            .when(active, |d| d.bg(theme.list_active))
            .when(!active, |d| d.hover(|s| s.bg(theme.list_hover)));
        if table {
            base.h(ROW_HEIGHT)
                .flex()
                .flex_row()
                .items_center()
                .px_3()
                .gap_3()
                .child(select)
                .child(title)
                .child(status.w(px(84.)))
                .child(priority.w(px(56.)))
                .child(assignees.w(px(130.)))
                .child(due.w(px(130.)))
                .child(updated.w(px(80.)))
                .into_any_element()
        } else {
            // 狭い画面では 1 件を 2 段のカードにする
            base.h(CARD_HEIGHT)
                .flex()
                .flex_col()
                .justify_center()
                .px_3()
                .gap_1()
                .child(row().gap_3().child(select).child(title))
                .child(
                    row()
                        .gap_3()
                        .pl_8()
                        .child(status)
                        .child(priority)
                        .child(due)
                        .child(assignees.flex_1().min_w_0()),
                )
                .into_any_element()
        }
    }

    // ---- 詳細パネル ----

    fn property(&self, label: &'static str, cx: &App) -> Div {
        row().gap_2().items_start().child(
            div()
                .w(px(72.))
                .flex_none()
                .pt_0p5()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(label),
        )
    }

    fn render_panel(&self, sheet: bool, cx: &mut Context<Self>) -> Option<Div> {
        let detail = self.detail.as_ref()?;
        let theme = cx.theme().clone();
        let task = &detail.task;
        let now = malakt_core::now_ms();
        let open = task.status.is_open();

        let title = if self.editing == Some(Editing::Title) {
            col()
                .gap_2()
                .child(
                    Input::new(&self.inputs.title)
                        .id("title-input")
                        .aria_label("タイトル"),
                )
                .child(
                    row()
                        .gap_2()
                        .child(
                            Button::new("title-save")
                                .label("保存")
                                .primary()
                                .small()
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.save_title(window, cx)),
                                ),
                        )
                        .child(
                            Button::new("title-cancel")
                                .label("キャンセル")
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.editing = None;
                                    cx.notify();
                                })),
                        ),
                )
        } else {
            div().child(
                div()
                    .id("detail-title")
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .cursor_pointer()
                    .child(task.title.clone())
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.start_editing(Editing::Title, window, cx)
                    }))
                    .test_support(),
            )
        };

        let status_actions = {
            let id = task.id.clone();
            let mut actions = row()
                .gap_2()
                .flex_wrap()
                .child(status_badge(task.status, cx));
            let set = |label: &'static str, key: &'static str, status: Status, primary: bool| {
                let id = id.clone();
                let button = Button::new(key).label(label).small();
                let button = if primary {
                    button.primary()
                } else {
                    button.outline()
                };
                button.on_click(cx.listener(move |this, _, window, cx| {
                    this.ask_status(&id, window, cx);
                    this.modal = Modal::Status(id.clone(), status);
                }))
            };
            if open {
                actions = actions
                    .child(set("完了にする", "to-completed", Status::Completed, true))
                    .child(set("中止にする", "to-cancelled", Status::Cancelled, false));
            } else {
                actions = actions.child(set("再開する", "to-reopen", Status::NeedsAction, true));
            }
            actions
        };

        let pinned: Vec<_> = detail.comments.iter().filter(|c| c.pinned).collect();
        let description = if self.editing == Some(Editing::Description) {
            col()
                .gap_2()
                .child(Textarea::new(&self.inputs.description).h(px(160.)))
                .child(
                    row()
                        .gap_2()
                        .child(
                            Button::new("desc-save")
                                .label("保存")
                                .primary()
                                .small()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.save_description(window, cx)
                                })),
                        )
                        .child(
                            Button::new("desc-cancel")
                                .label("キャンセル")
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.editing = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("Ctrl/⌘+Enter で保存、Esc でやめます"),
                        ),
                )
        } else {
            div().child(
                div()
                    .id("detail-description")
                    .cursor_pointer()
                    .text_sm()
                    .when(task.description.is_empty(), |d| {
                        d.text_color(theme.muted_foreground).child("説明を追加…")
                    })
                    .when(!task.description.is_empty(), |d| {
                        d.child(task.description.clone())
                    })
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.start_editing(Editing::Description, window, cx)
                    })),
            )
        };

        let done = detail
            .subtasks
            .iter()
            .filter(|t| !t.status.is_open())
            .count();
        let mut subtasks = col().gap_1();
        for (ix, sub) in detail.subtasks.iter().enumerate() {
            let id = sub.id.clone();
            subtasks = subtasks.child(
                row()
                    .id(("subtask", ix))
                    .gap_2()
                    .cursor_pointer()
                    .text_sm()
                    .child(status_badge(sub.status, cx))
                    .child(div().flex_1().truncate().child(sub.title.clone()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_panel(&id, window, cx)
                    })),
            );
        }

        // プロパティ
        let mut status_chips = self.property("状態", cx);
        let mut chips = row().gap_1().flex_wrap();
        for (i, s) in Status::ALL.into_iter().enumerate() {
            let id = task.id.clone();
            chips = chips.child(
                chip(("p-status", i), s.label(), task.status == s, cx).on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.ask_status(&id, window, cx);
                        this.modal = Modal::Status(id.clone(), s);
                    },
                )),
            );
        }
        status_chips = status_chips.child(chips);

        let mut prio_chips = row().gap_1().flex_wrap();
        for (i, p) in Prio::ALL.into_iter().enumerate() {
            prio_chips = prio_chips.child(
                chip(("p-prio", i), p.label(), Prio::of(task.priority) == p, cx).on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.edit_task(cx, |d| d.priority = p.value())
                    }),
                ),
            );
        }

        let mut people = row().gap_1().flex_wrap();
        for (i, p) in self.people.iter().enumerate() {
            let id = p.id.clone();
            people = people.child(
                chip(
                    ("p-as", i),
                    p.name.clone(),
                    task.assignee_ids.contains(&p.id),
                    cx,
                )
                .on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_assignee(&id, cx)),
                )
                .test_support(),
            );
        }
        people = people.child(
            Button::new("p-add-person")
                .label(if self.people.is_empty() {
                    "担当者を登録"
                } else {
                    "＋"
                })
                .ghost()
                .xsmall()
                .on_click(cx.listener(|this, _, window, cx| this.open_people(window, cx))),
        );

        // 担当者ごとの行: 自分の分の完了と、役（R・A・C・I）
        let mut people = col().gap_1().child(people);
        let (finished, total) = task.done_count();
        if total > 1 {
            people = people.child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("{finished}/{total} 完了")),
            );
        }
        for (i, person_id) in task.assignee_ids.iter().enumerate() {
            let roles = task.roles_of(person_id).unwrap_or_default();
            let mut line = row().gap_1().child(
                Checkbox::new(("p-done", i))
                    .checked(task.is_done_by(person_id))
                    .on_click(cx.listener({
                        let id = person_id.clone();
                        move |this, _, _, cx| this.toggle_assignee_done(&id, cx)
                    })),
            );
            line = line.child(
                div()
                    .w(px(132.))
                    .flex_none()
                    .truncate()
                    .text_sm()
                    .child(self.person_name(person_id).to_owned()),
            );
            for role in Raci::ALL {
                let id = person_id.clone();
                line = line.child(
                    chip(
                        ("p-role", i * 4 + role as usize),
                        role.code(),
                        roles.contains(role),
                        cx,
                    )
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(role.label()).build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toggle_role(&id, role, cx)
                    }))
                    .test_support(),
                );
            }
            people = people.child(line);
        }

        let due_buttons = [
            ("今日", Some(0)),
            ("明日", Some(1)),
            ("1 週間後", Some(7)),
            ("クリア", None),
        ];
        let mut due = row().gap_1().flex_wrap().child(
            Input::new(&self.inputs.due)
                .id("due-input")
                .aria_label("期限")
                .small()
                .w(px(128.)),
        );
        for (i, (label, days)) in due_buttons.into_iter().enumerate() {
            due = due.child(chip(("p-due", i), label, false, cx).on_click(cx.listener(
                move |this, _: &ClickEvent, window, cx| this.set_due_in(days, window, cx),
            )));
        }

        let mut percent = row().gap_1().flex_wrap();
        for (i, value) in [0u8, 25, 50, 75, 100].into_iter().enumerate() {
            percent = percent.child(
                chip(
                    ("p-percent", i),
                    format!("{value}%"),
                    task.percent == value,
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.edit_task(cx, |d| d.percent = value)
                })),
            );
        }

        let mut categories = row().gap_1().flex_wrap();
        for (i, c) in task.categories.iter().enumerate() {
            let value = c.clone();
            categories =
                categories.child(chip(("p-cat", i), format!("{c} ✕"), false, cx).on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.remove_category(&value, cx)
                    }),
                ));
        }
        categories = categories.child(
            Input::new(&self.inputs.category)
                .id("category-input")
                .aria_label("カテゴリ")
                .small()
                .w(px(168.)),
        );

        let parent = match &detail.parent {
            Some(parent) => {
                let id = parent.id.clone();
                row()
                    .gap_2()
                    .child(div().text_sm().truncate().child(parent.title.clone()))
                    .child(Button::new("parent-open").label("開く").xsmall().on_click(
                        cx.listener(move |this, _, window, cx| this.open_panel(&id, window, cx)),
                    ))
                    .child(
                        Button::new("parent-clear")
                            .label("外す")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.edit_task(cx, |d| d.parent_id = None)
                            })),
                    )
            }
            None => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("なし"),
        };

        let workspace = {
            let id = task.id.clone();
            row()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .child(self.workspace_name(task.workspace_id.as_deref())),
                )
                .child(
                    Button::new("move-task")
                        .label("移動")
                        .xsmall()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.modal = Modal::MoveTasks(vec![id.clone()]);
                            cx.notify();
                        })),
                )
        };

        let mut comments = col().gap_2();
        for (ix, c) in detail.comments.iter().enumerate() {
            let (pin_id, delete_id, pinned) = (c.id.clone(), c.id.clone(), c.pinned);
            let author = c
                .author_id
                .as_deref()
                .map_or("", |a| self.person_name(a))
                .to_owned();
            comments = comments.child(
                col()
                    .p_2()
                    .gap_1()
                    .border_1()
                    .border_color(theme.border)
                    .rounded_md()
                    .child(
                        row()
                            .gap_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(author)
                            .child(view::format_relative(c.created_at, now))
                            .child(div().flex_1())
                            .child(
                                Button::new(("pin", ix))
                                    .label(if pinned {
                                        "ピン留めを外す"
                                    } else {
                                        "ピン留め"
                                    })
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.write(cx, |s| s.set_comment_pinned(&pin_id, !pinned));
                                    })),
                            )
                            .child(
                                Button::new(("del-comment", ix))
                                    .label("削除")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.write(cx, |s| s.delete_comment(&delete_id));
                                    })),
                            ),
                    )
                    .child(div().text_sm().child(c.body.clone())),
            );
        }

        let delete_id = task.id.clone();
        let content =
            col()
                .p_4()
                .gap_4()
                .child(
                    row()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(format!(
                                    "{}のタスク",
                                    self.workspace_name(task.workspace_id.as_deref())
                                )),
                        )
                        .child(
                            Button::new("history")
                                .label("変更履歴")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.modal = Modal::History;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("close-panel")
                                .label("閉じる")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| this.close_panel(cx))),
                        ),
                )
                .child(title)
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "作成 {}　更新 {}",
                            view::format_datetime(task.created_at),
                            view::format_relative(task.updated_at, now)
                        )),
                )
                .child(status_actions)
                .when(!pinned.is_empty(), |d| {
                    d.child(
                        col()
                            .gap_1()
                            .p_2()
                            .rounded_md()
                            .bg(theme.secondary)
                            .child(section_title("ピン留め"))
                            .children(pinned.iter().map(|c| div().text_sm().child(c.body.clone()))),
                    )
                })
                .child(
                    col()
                        .gap_1()
                        .child(section_title("説明"))
                        .child(description),
                )
                .child(
                    col()
                        .gap_2()
                        .child(row().gap_2().child(section_title("サブタスク")).when(
                            !detail.subtasks.is_empty(),
                            |d| {
                                d.child(div().text_xs().text_color(theme.muted_foreground).child(
                                    format!("（{done}/{} 件が完了）", detail.subtasks.len()),
                                ))
                            },
                        ))
                        .child(subtasks)
                        .child(
                            Input::new(&self.inputs.subtask)
                                .id("subtask-input")
                                .aria_label("サブタスクを追加")
                                .small(),
                        ),
                )
                .child(
                    col()
                        .gap_2()
                        .child(status_chips)
                        .child(self.property("優先度", cx).child(prio_chips))
                        .child(self.property("担当", cx).child(people))
                        .child(
                            self.property("開始日", cx).child(
                                row()
                                    .gap_1()
                                    .child(
                                        Input::new(&self.inputs.start)
                                            .id("start-input")
                                            .aria_label("開始日")
                                            .small()
                                            .w(px(128.)),
                                    )
                                    .child(chip("p-start-clear", "クリア", false, cx).on_click(
                                        cx.listener(|this, _: &ClickEvent, window, cx| {
                                            this.clear_start(window, cx)
                                        }),
                                    )),
                            ),
                        )
                        .child(self.property("期限", cx).child(due))
                        .child(self.property("進捗", cx).child(percent))
                        .child(self.property("カテゴリ", cx).child(categories))
                        .child(self.property("親タスク", cx).child(parent))
                        .child(self.property("場所", cx).child(workspace))
                        .when_some(task.completed_at, |d, at| {
                            d.child(
                                self.property("完了日時", cx)
                                    .child(div().text_sm().child(view::format_datetime(at))),
                            )
                        }),
                )
                .child(
                    col()
                        .gap_2()
                        .child(section_title("コメント"))
                        .child(comments)
                        .child(Textarea::new(&self.inputs.comment).h(px(88.)))
                        .child(
                            row()
                                .gap_2()
                                .child(
                                    Button::new("comment-submit")
                                        .label("コメントする")
                                        .primary()
                                        .small()
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.submit_comment(window, cx)
                                        })),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("Ctrl / ⌘ + Enter で送信"),
                                ),
                        ),
                )
                .child(
                    col()
                        .gap_2()
                        .child(section_title("削除"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("タスクを削除すると、元に戻せません。"),
                        )
                        .child(
                            row().child(
                                Button::new("delete-task")
                                    .label("このタスクを削除")
                                    .danger()
                                    .small()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.modal = Modal::DeleteTasks(vec![delete_id.clone()]);
                                        cx.notify();
                                    })),
                            ),
                        ),
                );

        Some(
            div()
                .absolute()
                .top_0()
                .right_0()
                .bottom_0()
                .when(sheet, |d| d.left_0())
                .when(!sheet, |d| {
                    d.w(PANEL_WIDTH)
                        .border_l_1()
                        .border_color(theme.border)
                        .shadow_lg()
                })
                .bg(theme.background)
                .occlude()
                .child(
                    div()
                        .id("detail-panel")
                        .size_full()
                        .overflow_y_scroll()
                        .child(content),
                ),
        )
    }

    // ---- ダイアログ ----

    fn dialog(&self, title: impl Into<SharedString>, body: Div, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(theme.overlay)
            .occlude()
            .child(
                col()
                    .w(px(460.))
                    .max_w(relative(0.94))
                    .max_h(relative(0.9))
                    .p_4()
                    .gap_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.background)
                    .shadow_lg()
                    .child(
                        row()
                            .child(
                                div()
                                    .flex_1()
                                    .text_lg()
                                    .font_weight(FontWeight::BOLD)
                                    .child(title.into()),
                            )
                            .child(
                                Button::new("dialog-close")
                                    .label("閉じる")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.modal = Modal::None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(body),
            )
    }

    fn workspace_options(&self, cx: &mut Context<Self>) -> Div {
        let mut list = col().gap_1().child(
            chip("move-unfiled", "未所属（どこにも入れない）", false, cx)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.move_tasks_to(None, cx))),
        );
        let all = crate::state::build_tree(&self.workspaces, &Default::default());
        for (ix, node) in all.iter().enumerate() {
            let ws = &self.workspaces[node.index];
            let id = ws.id.clone();
            list = list.child(div().pl(px(14.) * node.depth as f32).child(
                chip(("move-to", ix), ws.name.clone(), false, cx).on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| this.move_tasks_to(Some(id.clone()), cx),
                )),
            ));
        }
        div().child(
            div()
                .id("move-options")
                .max_h(px(320.))
                .overflow_y_scroll()
                .child(list),
        )
    }

    fn render_modal(&self, cx: &mut Context<Self>) -> Option<Div> {
        let theme = cx.theme().clone();
        let muted = |text: String| {
            div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let cancel = |cx: &mut Context<Self>| {
            Button::new("modal-cancel")
                .label("キャンセル")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.modal = Modal::None;
                    cx.notify();
                }))
        };
        Some(match &self.modal {
            Modal::None => return None,
            Modal::WorkspaceName { rename, parent } => {
                let title = if rename.is_some() {
                    "ワークスペースの名前を変更".to_owned()
                } else if let Some(parent) = parent {
                    format!(
                        "「{}」の中にワークスペースを追加",
                        self.workspace_name(Some(parent))
                    )
                } else {
                    "ワークスペースを追加".to_owned()
                };
                let body = col()
                    .gap_3()
                    .child(
                        Input::new(&self.inputs.name)
                            .id("name-input")
                            .aria_label("名前"),
                    )
                    .child(
                        row().gap_2().justify_end().child(cancel(cx)).child(
                            Button::new("name-submit")
                                .label(if rename.is_some() { "保存" } else { "追加" })
                                .primary()
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.submit_name(window, cx)),
                                ),
                        ),
                    );
                self.dialog(title, body, cx)
            }
            Modal::DeleteWorkspace(id) => {
                let name = self.workspace_name(Some(id));
                let body = col()
                    .gap_3()
                    .child(muted(format!(
                        "「{name}」と、その中のワークスペース・タスクをすべて削除します。元に戻せません。"
                    )))
                    .child(
                        row().gap_2().justify_end().child(cancel(cx)).child(
                            Button::new("confirm-delete-workspace")
                                .label("削除する")
                                .danger()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.confirm_delete_workspace(cx)
                                })),
                        ),
                    );
                self.dialog("ワークスペースを削除", body, cx)
            }
            Modal::DeleteTasks(ids) => {
                let body = col()
                    .gap_3()
                    .child(muted(format!(
                        "{} 件のタスクを削除します。元に戻せません。",
                        ids.len()
                    )))
                    .child(
                        row().gap_2().justify_end().child(cancel(cx)).child(
                            Button::new("confirm-delete-tasks")
                                .label("削除する")
                                .danger()
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.confirm_delete_tasks(cx)),
                                ),
                        ),
                    );
                self.dialog("タスクを削除", body, cx)
            }
            Modal::MoveTasks(ids) => {
                let body = col()
                    .gap_3()
                    .child(muted(format!(
                        "{} 件のタスクの移動先を選んでください。",
                        ids.len()
                    )))
                    .child(self.workspace_options(cx));
                self.dialog("タスクを移動", body, cx)
            }
            Modal::People => {
                let mut list = col().gap_1();
                for (ix, p) in self.people.iter().enumerate() {
                    let id = p.id.clone();
                    list = list.child(
                        row()
                            .gap_2()
                            .child(div().flex_1().truncate().child(p.name.clone()))
                            .when(p.is_self, |d| {
                                d.child(div().text_xs().text_color(theme.primary).child("自分"))
                            })
                            .child(
                                Button::new(("del-person", ix))
                                    .label("削除")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.write(cx, |s| s.delete_person(&id));
                                    })),
                            ),
                    );
                }
                let body = col()
                    .gap_3()
                    .child(muted(
                        "タスクを割り当てる人を登録します。アカウントは要りません。最初に登録した人が「自分」になります。"
                            .to_owned(),
                    ))
                    .child(list)
                    .child(
                        row()
                            .gap_2()
                            .child(
                                Input::new(&self.inputs.name)
                                    .id("person-input")
                                    .aria_label("名前")
                                    .flex_1(),
                            )
                            .child(Button::new("person-submit").label("追加").primary().on_click(
                                cx.listener(|this, _, window, cx| this.submit_name(window, cx)),
                            )),
                    );
                self.dialog("担当者", body, cx)
            }
            Modal::History => {
                let changes = self
                    .detail
                    .as_ref()
                    .map(|d| d.changes.as_slice())
                    .unwrap_or(&[]);
                let mut list = col().gap_2();
                for c in changes.iter().rev() {
                    let show = |value: &str| -> String {
                        match c.field.as_str() {
                            "status" => Status::parse(value)
                                .map_or_else(|| value.to_owned(), |s| s.label().to_owned()),
                            "priority" => Prio::of(value.parse().unwrap_or(0)).label().to_owned(),
                            "assignees" => value
                                .split(',')
                                .filter(|v| !v.is_empty())
                                .map(|v| self.person_name(v))
                                .collect::<Vec<_>>()
                                .join("、"),
                            // 「人=R,A」の形
                            "assignee_roles" => match value.split_once('=') {
                                Some((person, roles)) => format!(
                                    "{}（{}）",
                                    self.person_name(person),
                                    Roles::parse(roles).labels()
                                ),
                                None => value.to_owned(),
                            },
                            "assignee_done" if !value.is_empty() => {
                                format!("{} が完了", self.person_name(value))
                            }
                            "assignee_done" => "未完了".to_owned(),
                            "workspace" => self
                                .workspace_name((!value.is_empty()).then_some(value))
                                .to_string(),
                            "parent" if !value.is_empty() => "（タスク）".to_owned(),
                            _ => value.to_owned(),
                        }
                    };
                    let (old, new) = (show(&c.old_value), show(&c.new_value));
                    let or_none = |s: String| if s.is_empty() { "なし".to_owned() } else { s };
                    list = list.child(
                        col()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(view::format_datetime(c.created_at)),
                            )
                            .child(div().text_sm().child(format!(
                                "{}: {} → {}",
                                view::field_label(&c.field),
                                or_none(old),
                                or_none(new)
                            ))),
                    );
                }
                let body = div().child(
                    div()
                        .id("history-list")
                        .max_h(px(360.))
                        .overflow_y_scroll()
                        .child(if changes.is_empty() {
                            muted("変更はまだありません。".to_owned())
                        } else {
                            list
                        }),
                );
                self.dialog("変更履歴", body, cx)
            }
            Modal::Status(id, next) => {
                let (id, next) = (id.clone(), *next);
                let report = next == Status::Completed;
                let mut options = row().gap_1().flex_wrap();
                for (i, s) in Status::ALL.into_iter().enumerate() {
                    let id = id.clone();
                    options =
                        options.child(chip(("st-opt", i), s.label(), s == next, cx).on_click(
                            cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.modal = Modal::Status(id.clone(), s);
                                cx.notify();
                            }),
                        ));
                }
                let body = col()
                    .gap_3()
                    .child(section_title("新しい状態"))
                    .child(options)
                    .child(section_title(if report {
                        "完了報告（任意）"
                    } else {
                        "コメント（任意）"
                    }))
                    .child(Textarea::new(&self.inputs.status_comment).h(px(96.)))
                    .child(muted(
                        "本文を書かなければ、状態だけを変えます（コメントは残りません）。"
                            .to_owned(),
                    ))
                    .child(
                        row()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Ctrl / ⌘ + Enter で送信"),
                            )
                            .child(cancel(cx))
                            .child(
                                Button::new("status-submit")
                                    .label(format!("{}にする", next.label()))
                                    .primary()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.submit_status(window, cx)
                                    })),
                            ),
                    );
                self.dialog("状態を変更", body, cx)
            }
            Modal::Lock => {
                let enabled = self.lock_enabled(cx);
                let body = col()
                    .gap_3()
                    .child(muted(
                        "起動したときと「今すぐロック」のあと、パスフレーズを入れるまで中身を表示しません。"
                            .to_owned(),
                    ))
                    .when(enabled, |d| {
                        d.child(section_title("今のパスフレーズ")).child(
                            Input::new(&self.inputs.passphrase)
                                .id("lock-current")
                                .aria_label("今のパスフレーズ"),
                        )
                    })
                    .child(section_title("新しいパスフレーズ"))
                    .child(
                        Input::new(&self.inputs.new_passphrase)
                            .id("lock-new")
                            .aria_label("新しいパスフレーズ"),
                    )
                    .child(
                        row()
                            .gap_2()
                            .justify_end()
                            .when(enabled, |d| {
                                d.child(
                                    Button::new("lock-disable")
                                        .label("ロックを無効にする")
                                        .danger()
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.disable_lock(window, cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new("lock-save")
                                    .label(if enabled { "変更する" } else { "ロックを有効にする" })
                                    .primary()
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.save_lock(window, cx)
                                    })),
                            ),
                    );
                self.dialog("ロックの設定", body, cx)
            }
        })
    }

    fn render_lock_screen(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        col()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .bg(theme.background)
            .text_color(theme.foreground)
            .child(
                div()
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .child("ロックされています"),
            )
            .child(
                Input::new(&self.inputs.passphrase)
                    .id("unlock-input")
                    .aria_label("パスフレーズ")
                    .w(px(280.)),
            )
            .child(
                Button::new("unlock")
                    .label("解除")
                    .primary()
                    .on_click(cx.listener(|this, _, window, cx| this.submit_unlock(window, cx))),
            )
            .when_some(self.error.clone(), |d, error| {
                d.child(div().text_sm().text_color(theme.danger).child(error))
            })
    }
}

impl Render for MainView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.unlocked(cx) {
            return self.render_lock_screen(cx).into_any_element();
        }
        let theme = cx.theme().clone();
        let width = window.viewport_size().width;
        // 狭いウィンドウでは左のメニューをたたみ、詳細は全面のシートにする
        let wide = width >= px(860.);
        let sidebar = wide || self.sidebar_open && self.panel.is_none();
        let main_width = if wide { width - SIDEBAR_WIDTH } else { width };
        let table = main_width >= px(720.);
        let sheet = width < px(1000.);

        let main = if !wide && sidebar {
            // 狭い画面でメニューを開いているあいだは、メニューだけを見せる
            self.render_sidebar(cx).w_full().flex_1().min_h_0()
        } else {
            match self.nav {
                Nav::Home => self.render_home(table, cx),
                _ => self.render_list(table, cx),
            }
        };

        div()
            .id("main-view")
            .key_context("MainView")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.dismiss(cx)))
            .size_full()
            .relative()
            .flex()
            .flex_row()
            .bg(theme.background)
            .text_color(theme.foreground)
            .when(wide, |d| d.child(self.render_sidebar(cx)))
            .child(
                col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .when(!wide, |d| {
                        d.child(
                            row().px_4().pt_2().child(
                                Button::new("toggle-sidebar")
                                    .label(if sidebar { "閉じる" } else { "メニュー" })
                                    .ghost()
                                    .small()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sidebar_open = !this.sidebar_open;
                                        cx.notify();
                                    })),
                            ),
                        )
                    })
                    .child(main),
            )
            .children(self.render_panel(sheet, cx))
            .children(self.render_modal(cx))
            .when_some(
                self.error
                    .clone()
                    .map(|e| (e, true))
                    .or(self.notice.clone().map(|n| (n, false))),
                |d, (text, is_error)| {
                    d.child(
                        div()
                            .absolute()
                            .bottom_4()
                            .left_0()
                            .right_0()
                            .flex()
                            .justify_center()
                            .child(
                                row()
                                    .id("notice")
                                    .gap_3()
                                    .px_3()
                                    .py_2()
                                    .rounded_md()
                                    .shadow_lg()
                                    .text_sm()
                                    .cursor_pointer()
                                    .when(is_error, |d| {
                                        d.bg(theme.danger).text_color(theme.danger_foreground)
                                    })
                                    .when(!is_error, |d| {
                                        d.bg(theme.primary).text_color(theme.primary_foreground)
                                    })
                                    .child(text)
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.error = None;
                                        this.notice = None;
                                        cx.notify();
                                    }))
                                    .test_support(),
                            ),
                    )
                },
            )
            .into_any_element()
    }
}
