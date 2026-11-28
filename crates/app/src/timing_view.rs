//! 所要時間の見せ方（一覧の列・詳細）と、基準のメニュー、統計のダイアログ。

use std::collections::HashMap;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::estimate::elapsed_label;
use malakt_core::timing::{self, Basis, EndBasis, Period, Span, StartBasis, Stats};
use malakt_core::{view, Id, Scope, Task, TaskFilter};

use crate::feel;
use crate::press::button;
use crate::render::{app_icon, chip, col, px, row, Framed};
use crate::state::{MainView, Modal};

/// 統計のダイアログに出す数字。開いたとき・期間を変えたとき・データが変わったときに数え直す。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StatsView {
    /// `None` は全体。
    pub scope: Option<Id>,
    pub total: Stats,
    /// 担当者ごと（名前と数字）。担当なしは最後。
    pub people: Vec<(String, Stats)>,
    /// ワークスペースごと（全体なら最上位のワークスペース、ワークスペースならサブワークスペース）。
    pub groups: Vec<(String, Stats)>,
}

/// 基準を選ぶメニューの、選んだときの動き。`None` は「標準に合わせる」。
type OnBasis = std::rc::Rc<dyn Fn(&mut MainView, Option<Basis>, &mut Context<MainView>)>;

/// 区間を「11/27 10:00 → 11/28 13:00」の形にする。
fn span_text(span: &Span) -> String {
    let at = |ms: i64| {
        view::format_datetime(ms)
            .get(5..)
            .unwrap_or_default()
            .to_owned()
    };
    if span.running {
        format!("{} →", at(span.start))
    } else {
        format!("{} → {}", at(span.start), at(span.end))
    }
}

/// 予想工数との差（「+1時間20分」「−40分」）。予想工数がなければ `None`。
fn diff_text(task: &Task, span: &Span) -> Option<(String, bool)> {
    let est = task.estimate?.total_ms();
    let diff = span.ms() - est;
    let over = diff > 0;
    let text = elapsed_label(diff.abs());
    Some((format!("{}{text}", if over { "+" } else { "−" }), over))
}

/// 数えた区間の補足（ホバーで出す）。
fn span_tip(span: &Span, basis: Basis) -> String {
    let mut tip = span_text(span);
    tip.push('\n');
    if span.from != basis.start {
        // 選んだ始まりの時刻がなく、ほかの時刻から数えた
        tip.push_str(&format!("{} → {}", span.from.label(), basis.end.label()));
    } else {
        tip.push_str(&basis.label());
    }
    tip
}

impl MainView {
    /// 一覧の「所要時間」の列。予想工数を超えていれば赤。
    pub(crate) fn elapsed_cell(&self, task: &Task, ix: usize, cx: &App) -> AnyElement {
        let theme = cx.theme();
        let Some(span) = self.task_span(task) else {
            return div().into_any_element();
        };
        let over = timing::over_estimate(task, Some(span));
        let tip = span_tip(&span, self.task_basis(task));
        div()
            .id(("elapsed", ix))
            .flex()
            .items_center()
            .gap_1()
            .text_sm()
            .when(over, |d| d.text_color(theme.danger))
            .when(!over && span.running, |d| {
                d.text_color(theme.muted_foreground)
            })
            .when(span.running, |d| d.child(app_icon("timer").xsmall()))
            .child(div().truncate().child(elapsed_label(span.ms())))
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(window, cx)
            })
            .test_support()
            .into_any_element()
    }

    /// 詳細の「所要時間」。押すと、このタスクの基準を選ぶメニューが開く。
    pub(crate) fn elapsed_property(&self, task: &Task, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let basis = self.task_basis(task);
        let span = self.task_span(task);
        let mut value = row().min_w_0().gap_2().items_center().text_sm();
        match &span {
            Some(span) => {
                let over = timing::over_estimate(task, Some(*span));
                value = value
                    .when(span.running, |d| {
                        d.child(app_icon("timer").small().text_color(theme.muted_foreground))
                    })
                    .child(
                        div()
                            .id("elapsed-value")
                            .when(over, |d| d.text_color(theme.danger))
                            .child(elapsed_label(span.ms()))
                            .test_support(),
                    )
                    .when_some(diff_text(task, span), |d, (text, over)| {
                        d.child(
                            div()
                                .id("elapsed-diff")
                                .text_xs()
                                .px_1p5()
                                .rounded(feel::md())
                                .when(over, |d| {
                                    d.bg(theme.danger.opacity(0.12)).text_color(theme.danger)
                                })
                                .when(!over, |d| {
                                    d.bg(theme.secondary).text_color(theme.muted_foreground)
                                })
                                .child(text)
                                .test_support(),
                        )
                    })
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(span_text(span)),
                    );
            }
            None => {
                value = value.child(div().text_color(theme.muted_foreground).child("—"));
            }
        }
        let id = task.id.clone();
        let own = task.time_basis;
        let standard = self.workspace_or_standard(task.workspace_id.as_deref());
        let view = cx.entity().downgrade();
        let on: OnBasis = std::rc::Rc::new(move |this, basis, cx| {
            this.edit_task_by_id(&id, cx, |d| d.time_basis = basis)
        });
        let muted = theme.muted_foreground;
        div()
            .flex()
            .min_w_0()
            .child(
                button("elapsed-basis")
                    .ghost()
                    .small()
                    .h_auto()
                    .min_h_6()
                    .max_w_full()
                    .py_0p5()
                    .justify_start()
                    .tooltip(format!("所要時間の基準: {}", basis.label()))
                    .child(
                        row()
                            .min_w_0()
                            .gap_2()
                            .child(value)
                            .child(app_icon("chevron-down").small().text_color(muted)),
                    )
                    .pop_menu(move |menu, _, _| {
                        basis_items(menu, &view, own, standard, "標準に合わせる", on.clone())
                    }),
            )
            .into_any_element()
    }

    /// そのワークスペースの基準（なければ全体の標準）。タスクで「標準に合わせる」ときの値。
    pub(crate) fn workspace_or_standard(&self, workspace_id: Option<&str>) -> Basis {
        workspace_id
            .and_then(|id| timing::workspace_basis(&self.workspaces, id))
            .unwrap_or(self.time_standard)
    }

    /// 全体の標準を変える。
    pub(crate) fn set_time_standard(&mut self, basis: Basis, cx: &mut Context<Self>) {
        self.write(cx, |s| s.set_time_basis_standard(basis));
        self.reload(cx);
    }

    /// ワークスペースの基準を変える。`None` は全体の標準に合わせる。
    pub(crate) fn set_workspace_basis(
        &mut self,
        id: &str,
        basis: Option<Basis>,
        cx: &mut Context<Self>,
    ) {
        self.write(cx, |s| s.set_workspace_time_basis(id, basis).map(|_| ()));
        self.reload(cx);
    }

    /// 基準を選ぶボタン（統計とワークスペースの設定）。`workspace` が `None` なら全体の標準を変える。
    pub(crate) fn basis_button(
        &self,
        id: &'static str,
        workspace: Option<&str>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let view = cx.entity().downgrade();
        let (current, standard, reset, on): (Option<Basis>, Basis, &'static str, OnBasis) =
            match workspace {
                Some(ws) => {
                    let own = self
                        .workspaces
                        .iter()
                        .find(|w| w.id == ws)
                        .and_then(|w| w.time_basis);
                    // 親のワークスペースの基準があれば、それが「合わせる」先
                    let parent = self
                        .workspaces
                        .iter()
                        .find(|w| w.id == ws)
                        .and_then(|w| w.parent_id.as_deref())
                        .and_then(|p| timing::workspace_basis(&self.workspaces, p));
                    let ws = ws.to_owned();
                    (
                        own,
                        parent.unwrap_or(self.time_standard),
                        if parent.is_some() {
                            "親のワークスペースに合わせる"
                        } else {
                            "標準に合わせる"
                        },
                        std::rc::Rc::new(
                            move |this: &mut MainView, basis, cx: &mut Context<MainView>| {
                                this.set_workspace_basis(&ws, basis, cx)
                            },
                        ),
                    )
                }
                None => (
                    Some(self.time_standard),
                    self.time_standard,
                    "",
                    std::rc::Rc::new(
                        |this: &mut MainView, basis: Option<Basis>, cx: &mut Context<MainView>| {
                            this.set_time_standard(basis.unwrap_or_default(), cx)
                        },
                    ),
                ),
            };
        let shown = current.unwrap_or(standard);
        button(id)
            .outline()
            .small()
            .tooltip("所要時間の基準")
            .child(
                row()
                    .min_w_0()
                    .gap_2()
                    .child(app_icon("timer").small().text_color(muted))
                    .child(div().min_w_0().truncate().child(shown.label()))
                    .child(app_icon("chevron-down").small().text_color(muted)),
            )
            .pop_menu(move |menu, _, _| {
                basis_items(menu, &view, current, standard, reset, on.clone())
            })
    }

    // ---- 統計 ----

    /// 統計のダイアログを開く。`scope` が `None` なら全体。
    pub(crate) fn open_stats(&mut self, scope: Option<Id>, cx: &mut Context<Self>) {
        self.modal = Modal::Stats(scope);
        self.refresh_stats(cx);
        cx.notify();
    }

    /// 統計の期間を変える。
    pub(crate) fn set_stats_period(&mut self, period: Period, cx: &mut Context<Self>) {
        self.stats_period = period;
        self.refresh_stats(cx);
        cx.notify();
    }

    /// 統計を数え直す。ダイアログを開いていなければ捨てる。
    pub(crate) fn refresh_stats(&mut self, cx: &App) {
        let Modal::Stats(scope) = &self.modal else {
            self.stats_view = None;
            return;
        };
        let scope = scope.clone();
        let store = &self.db.read(cx).store;
        let filter = TaskFilter {
            include_descendants: true,
            ..Default::default()
        };
        let tasks = match &scope {
            Some(id) => store.tasks(id, &filter),
            None => store.tasks(Scope::All, &filter),
        };
        let tasks = match tasks {
            Ok(tasks) => tasks,
            Err(e) => {
                self.error = Some(format!("統計を出すタスクを読めませんでした: {e}").into());
                self.stats_view = None;
                return;
            }
        };
        let now = malakt_core::now_ms();
        let since = self.stats_period.since(now);
        let me = self.self_id.as_deref();
        let items: Vec<timing::Item> = tasks
            .iter()
            .map(|task| timing::Item {
                task,
                basis: self.task_basis(task),
            })
            .collect();
        let total = timing::stats(&items, me, now, since);
        let people = timing::stats_by_person(&items, me, now, since)
            .into_iter()
            .map(|(id, stats)| {
                let name = match id.as_deref() {
                    Some(id) => self.person_name(id).to_string(),
                    None => "担当なし".to_owned(),
                };
                (name, stats)
            })
            .collect();
        // 1 段下のワークスペースごと。タスクは入っている場所から上へたどって、その段のどれかに入れる
        let children: Vec<&malakt_core::Workspace> = self
            .workspaces
            .iter()
            .filter(|w| w.parent_id.as_deref() == scope.as_deref())
            .collect();
        let mut sorted = children.clone();
        sorted.sort_by_key(|w| (w.position, w.id.clone()));
        let group_of = |task: &Task| -> Option<usize> {
            let mut at = task.workspace_id.as_deref();
            for _ in 0..=self.workspaces.len() {
                let w = self.workspaces.iter().find(|w| Some(w.id.as_str()) == at)?;
                if w.parent_id.as_deref() == scope.as_deref() {
                    return sorted.iter().position(|c| c.id == w.id);
                }
                at = w.parent_id.as_deref();
            }
            None
        };
        let mut grouped: HashMap<Option<usize>, Vec<timing::Item>> = HashMap::new();
        for item in &items {
            grouped.entry(group_of(item.task)).or_default().push(*item);
        }
        let mut groups: Vec<(String, Stats)> = Vec::new();
        for (i, w) in sorted.iter().enumerate() {
            if let Some(items) = grouped.get(&Some(i)) {
                groups.push((w.name.clone(), timing::stats(items, me, now, since)));
            }
        }
        if !groups.is_empty() {
            if let Some(items) = grouped.get(&None) {
                let name = if scope.is_some() {
                    "このワークスペース"
                } else {
                    "未所属"
                };
                groups.push((name.to_owned(), timing::stats(items, me, now, since)));
            }
        }
        groups.retain(|(_, s)| s.done > 0 || s.open > 0);
        self.stats_view = Some(StatsView {
            scope,
            total,
            people,
            groups,
        });
    }

    /// 統計のダイアログ。描く関数のスタックを大きくしないよう、ダイアログの描き分けから分けている。
    #[inline(never)]
    pub(crate) fn stats_dialog(&self, scope: Option<&str>, cx: &mut Context<Self>) -> Div {
        let title = match scope {
            Some(id) => format!("「{}」の統計", self.workspace_name(Some(id))),
            None => "すべてのタスクの統計".to_owned(),
        };
        let body = self.render_stats(cx);
        self.dialog_as("dialog", px(720.), title, body, cx)
    }

    /// ワークスペースの設定の「所要時間の基準」の欄。
    #[inline(never)]
    pub(crate) fn workspace_basis_setting(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        col()
            .gap_1()
            .child(div().text_sm().child("所要時間の基準"))
            .child(
                div()
                    .flex()
                    .child(self.basis_button("ws-time-basis", Some(id), cx)),
            )
            .into_any_element()
    }

    /// 統計のダイアログの中身。
    pub(crate) fn render_stats(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let Some(stats) = &self.stats_view else {
            return col();
        };
        let mut periods = row().gap_1().flex_wrap();
        for (ix, period) in Period::ALL.into_iter().enumerate() {
            periods =
                periods.child(
                    chip(
                        ("stats-period", ix),
                        period.label(),
                        self.stats_period == period,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_stats_period(period, cx)
                    }))
                    .test_support(),
                );
        }
        let s = &stats.total;
        let ms = |v: Option<i64>| v.map(elapsed_label).unwrap_or_else(|| "—".into());
        let tile = |id: &'static str, label: &'static str, value: String, note: Option<String>| {
            col()
                .id(id)
                .flex_1()
                .min_w(px(120.))
                .gap_0p5()
                .p_3()
                .rounded(feel::md())
                .border_1()
                .border_color(theme.border)
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(label),
                )
                .child(div().text_xl().child(value))
                .when_some(note, |d, note| {
                    d.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(note),
                    )
                })
                .test_support()
        };
        let within = match s.within_percent() {
            Some(p) => format!("{p}%"),
            None => "—".into(),
        };
        let actual = match s.actual_percent() {
            Some(p) => format!("{p}%"),
            None => "—".into(),
        };
        let tiles = col()
            .gap_2()
            .child(
                row()
                    .gap_2()
                    .flex_wrap()
                    .items_stretch()
                    .child(tile("stats-done", "完了", format!("{} 件", s.done), None))
                    .child(tile("stats-mean", "平均", ms(s.mean_ms), None))
                    .child(tile("stats-median", "中央値", ms(s.median_ms), None))
                    .child(tile(
                        "stats-total",
                        "合計",
                        if s.done > 0 {
                            elapsed_label(s.total_ms)
                        } else {
                            "—".into()
                        },
                        s.longest_ms.map(|l| format!("最長 {}", elapsed_label(l))),
                    )),
            )
            .child(
                row()
                    .gap_2()
                    .flex_wrap()
                    .items_stretch()
                    .child(tile(
                        "stats-within",
                        "予想工数に収まった",
                        within,
                        (s.estimated > 0).then(|| format!("{} / {} 件", s.within, s.estimated)),
                    ))
                    .child(tile(
                        "stats-ratio",
                        "予想工数に対する所要時間",
                        actual,
                        (s.estimated > 0).then(|| {
                            format!(
                                "{} / {}",
                                elapsed_label(s.estimated_actual_ms),
                                elapsed_label(s.estimated_ms)
                            )
                        }),
                    ))
                    .child(tile(
                        "stats-open",
                        "未完了の予想工数",
                        if s.open_estimated > 0 {
                            elapsed_label(s.open_estimate_ms)
                        } else {
                            "—".into()
                        },
                        Some(format!("{} / {} 件", s.open_estimated, s.open)),
                    ))
                    .child(tile(
                        "stats-over",
                        "予想工数を超えて進行中",
                        format!("{} 件", s.open_over),
                        None,
                    )),
            );
        let table = |id: &'static str, head: &'static str, rows: &[(String, Stats)]| {
            let cell = |w: f32| div().w(px(w)).flex_none().text_right();
            let line = |a: Div, b: String, c: String, d: String, e: String| {
                row()
                    .gap_2()
                    .py_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_sm()
                    .child(a.flex_1().min_w_0().truncate())
                    .child(cell(56.).child(b))
                    .child(cell(96.).child(c))
                    .child(cell(96.).child(d))
                    .child(cell(88.).child(e))
            };
            let mut list = col().id(id).child(
                line(
                    div().child(head),
                    "完了".into(),
                    "平均".into(),
                    "中央値".into(),
                    "予想内".into(),
                )
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(theme.muted_foreground),
            );
            for (name, s) in rows {
                list = list.child(line(
                    div().child(name.clone()),
                    s.done.to_string(),
                    ms(s.mean_ms),
                    ms(s.median_ms),
                    match s.within_percent() {
                        Some(p) => format!("{p}%"),
                        None => "—".into(),
                    },
                ));
            }
            list.test_support()
        };
        let scope = stats.scope.clone();
        col()
            .gap_4()
            .child(
                row()
                    .gap_2()
                    .flex_wrap()
                    .items_center()
                    .child(div().flex_1().child(periods))
                    .child(self.basis_button("stats-basis", scope.as_deref(), cx)),
            )
            .child(tiles)
            .when(!stats.people.is_empty(), |d| {
                d.child(table("stats-people", "担当", &stats.people))
            })
            .when(!stats.groups.is_empty(), |d| {
                d.child(table("stats-groups", "ワークスペース", &stats.groups))
            })
    }

    /// 統計を開くボタン（一覧の上）。ワークスペースを見ていればそのワークスペース、ほかは全体。
    pub(crate) fn stats_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let scope = self.current_workspace().map(str::to_owned);
        feel::gloss(
            button("open-stats")
                .icon(app_icon("chart-column"))
                .tooltip("統計")
                .framed(cx)
                .on_click(cx.listener(move |this, _, _, cx| this.open_stats(scope.clone(), cx))),
        )
    }
}

/// 基準を選ぶメニューの項目。`current` が `None` なら「合わせる」の側（`standard`）を選んでいる。
/// `reset` が空なら「合わせる」の項目を出さない（全体の標準そのもの）。
fn basis_items(
    mut menu: PopupMenu,
    view: &WeakEntity<MainView>,
    current: Option<Basis>,
    standard: Basis,
    reset: &'static str,
    on: OnBasis,
) -> PopupMenu {
    let pick = |basis: Option<Basis>| {
        let view = view.clone();
        let on = on.clone();
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            // ウィンドウを閉じたあとなら、反映する先がない
            let _ = view.update(cx, |this, cx| on(this, basis, cx));
        }
    };
    if !reset.is_empty() {
        menu = menu
            .item(
                PopupMenuItem::new(format!("{reset}（{}）", standard.label()))
                    .checked(current.is_none())
                    .on_click(pick(None)),
            )
            .separator();
    }
    let shown = current.unwrap_or(standard);
    menu = menu.label("開始");
    for start in StartBasis::ALL {
        let next = Basis::new(start, shown.end);
        menu = menu.item(
            PopupMenuItem::new(start.label())
                .checked(current.is_some() && shown.start == start)
                .on_click(pick(Some(next))),
        );
    }
    menu = menu.separator().label("完了");
    for end in EndBasis::ALL {
        let next = Basis::new(shown.start, end);
        menu = menu.item(
            PopupMenuItem::new(end.label())
                .checked(current.is_some() && shown.end == end)
                .on_click(pick(Some(next))),
        );
    }
    menu
}
