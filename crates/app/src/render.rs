//! 画面の描画。状態は `state` にあり、ここは見た目だけを組み立てる。

use std::ops::Range;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::bulk::{BulkOp, ListMode};
use malakt_core::view::{self, AssigneeFilter, DueState, Priority as Prio, Sort, Tab};
use malakt_core::{Comment, DueRule, Role as Raci, Roles, Status, Task as Todo, WORKSPACE_COLORS};

use crate::ai::ShellPlace;
use crate::ai_view::{docked_width, TASKS_MIN};
use crate::state::{Column, CommentMode, Detail, Editing, MainView, Modal, Nav};
use crate::{Dismiss, LockNow, NewWindow, ToggleAi};
use malakt_core::Id;

/// 参照する寸法（CSS の px）を、画面全体の拡大率に合わせる。
const fn px(value: f32) -> Pixels {
    gpui_kit::px(value * crate::UI_SCALE)
}

/// アイコンを選ぶ画面に一度に出す数。それより多いときは検索で絞る。
const ICONS_SHOWN: usize = 240;
/// 見出しの枠（40px）の中のアイコンの大きさ。
const TILE_ICON_SIZE: Pixels = px(24.);
/// ホームのカードの枠（56px）の中のアイコンの大きさ。
const CARD_ICON_SIZE: Pixels = px(32.);
/// アイコンを選ぶ画面のアイコンの大きさ。
const PICKER_ICON_SIZE: Pixels = px(22.);
const HEADER_HEIGHT: Pixels = px(84.);
const PANEL_WIDTH: Pixels = px(544.);
const ROW_HEIGHT: Pixels = px(60.);
const CARD_HEIGHT: Pixels = px(96.);

/// 枠つきのボタン。乗せると淡い金、押している間ははっきり濃くなる。
trait Framed {
    fn framed(self, cx: &App) -> Self;
}

impl Framed for Button {
    fn framed(self, cx: &App) -> Self {
        let theme = cx.theme();
        self.custom(
            ButtonCustomVariant::new(cx)
                .color(theme.background)
                .foreground(theme.foreground)
                .hover(theme.accent)
                .active(theme.primary.opacity(0.4)),
        )
        .border_1()
        .border_color(theme.border)
    }
}

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
        Status::NeedsAction => theme.foreground,
        Status::InProcess => theme.info,
        Status::Completed => theme.success,
        Status::Cancelled => theme.muted_foreground,
    }
}

fn status_icon(status: Status) -> &'static str {
    match status {
        Status::NeedsAction => "circle",
        Status::InProcess => "circle-dot",
        Status::Completed => "circle-check",
        Status::Cancelled => "circle-x",
    }
}

/// 状態の丸いラベル。
fn status_badge(status: Status, cx: &App) -> Div {
    let theme = cx.theme();
    row()
        .gap_1()
        .flex_none()
        .px_2()
        .py_0p5()
        .rounded_full()
        .border_1()
        .border_color(theme.border)
        .bg(theme.background)
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .child(
            app_icon(status_icon(status))
                .xsmall()
                .text_color(status_color(status, cx)),
        )
        .child(status.label())
}

/// 優先度。棒の本数と色で見分ける。
fn priority_label(priority: Prio, cx: &App) -> Div {
    let theme = cx.theme();
    let (icon, color) = match priority {
        Prio::High => ("signal-high", rgb(0xb04e1c).into()),
        Prio::Mid => ("signal-medium", rgb(0x80660d).into()),
        Prio::Low => ("signal-low", theme.muted_foreground),
        Prio::None => {
            return row()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("—");
        }
    };
    row()
        .gap_1p5()
        .text_sm()
        .when(priority == Prio::Low, |d| d.text_color(color))
        .child(app_icon(icon).small().text_color(color))
        .child(priority.label())
}

/// 名前の頭文字の丸。
fn avatar(name: &str, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .size_6()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(theme.secondary)
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .child(name.chars().next().map(String::from).unwrap_or_default())
}

/// 欄の上に付ける小さな見出し。
fn field_label(text: &'static str, cx: &App) -> Div {
    div()
        .text_xs()
        .font_weight(FontWeight::BOLD)
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

/// ワークスペースのアイコンの色。`None` は文字と同じ色。
fn workspace_color(color: Option<&str>) -> Option<Hsla> {
    let hex = match color? {
        "red" => 0xe5484d,
        "orange" => 0xf76b15,
        "yellow" => 0xd99a00,
        "green" => 0x30a46c,
        "teal" => 0x12a594,
        "blue" => 0x3e63dd,
        "purple" => 0x8e4ec6,
        "pink" => 0xd6409f,
        _ => return None,
    };
    Some(rgb(hex).into())
}

fn workspace_icon(icon: Option<&str>, color: Option<&str>) -> Icon {
    let icon = Icon::default().path(crate::state::workspace_icon_path(icon));
    match workspace_color(color) {
        Some(color) => icon.text_color(color),
        None => icon,
    }
}

/// 同梱の線画アイコン。
pub(crate) fn app_icon(name: &str) -> Icon {
    Icon::default().path(crate::state::icon_path(name))
}

/// 出てくるときの動き。すっと出て、ゆっくり止まる。
fn appear(ms: u64) -> Animation {
    Animation::new(std::time::Duration::from_millis(ms)).with_easing(ease_out_quint())
}

fn section_title(text: &'static str) -> Div {
    div().font_weight(FontWeight::BOLD).child(text)
}

/// 詳細の中の 1 まとまり。白い地に枠。
fn card(cx: &App) -> Div {
    let theme = cx.theme();
    col()
        .p_4()
        .gap_2()
        .rounded_lg()
        .border_1()
        .border_color(theme.border)
        .bg(theme.popover)
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
        match view::due_state(due, &view::now()) {
            DueState::Overdue => div()
                .text_color(theme.danger)
                .child(format!("{text}（期限切れ）")),
            DueState::Today => div()
                .text_color(theme.primary)
                .child(format!("{text}（今日）")),
            DueState::Soon => div().text_color(theme.primary).child(text),
            DueState::Later => div().child(text),
        }
    }

    // ---- 左のメニュー ----

    /// 左のメニューの 1 行。選んでいる行は色を付ける。
    fn nav_link(
        &self,
        id: impl Into<ElementId>,
        icon: Icon,
        label: impl Into<SharedString>,
        active: bool,
        cx: &App,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        row()
            .id(id)
            .flex_1()
            .min_w_0()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .when(active, |d| {
                d.bg(theme.accent).text_color(theme.accent_foreground)
            })
            .when(!active, |d| d.hover(|s| s.bg(theme.accent.opacity(0.6))))
            .active(|s| s.bg(theme.accent).opacity(0.8))
            .child(icon.small().flex_none())
            .child(div().flex_1().truncate().child(label.into()))
    }

    fn nav_item(
        &self,
        id: impl Into<ElementId>,
        icon: Icon,
        label: impl Into<SharedString>,
        nav: Nav,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.nav_link(id, icon, label, self.nav == nav, cx)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| this.go(nav.clone(), cx)))
            .test_support()
            .into_any_element()
    }

    /// 左のメニューを開く・閉じる。狭いウィンドウでは中身と入れ替えで出す。
    fn toggle_sidebar(&mut self, wide: bool, cx: &mut Context<Self>) {
        if wide {
            self.sidebar_hidden = !self.sidebar_hidden;
        } else {
            self.sidebar_open = !self.sidebar_open;
        }
        cx.notify();
    }

    fn render_sidebar(&self, wide: bool, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut tree = col()
            .ml_5()
            .pl_2()
            .border_l_1()
            .border_color(theme.sidebar_border)
            .gap_0p5();
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
                    .text_color(theme.muted_foreground)
                    .child(
                        app_icon(if collapsed {
                            "chevron-right"
                        } else {
                            "chevron-down"
                        })
                        .xsmall(),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.toggle_collapsed(&id, cx);
                    }))
                    .into_any_element()
            };
            let group = SharedString::from(format!("ws-row-{ix}"));
            let active = self.nav == Nav::Workspace(id.clone());
            let view = cx.entity().downgrade();
            let menu_id = id.clone();
            let menu = move |menu: PopupMenu| {
                let item = |label: &'static str,
                            run: fn(&mut MainView, Id, &mut Window, &mut Context<MainView>)| {
                    let view = view.clone();
                    let id = menu_id.clone();
                    PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        let _ = view.update(cx, |this, cx| run(this, id.clone(), window, cx));
                    })
                };
                menu.item(item("アイコンを変更", |this, id, window, cx| {
                    // 開いたらすぐ打って探せるよう、検索欄を空にしてフォーカスする
                    this.inputs.icon_search.update(cx, |state, cx| {
                        state.set_value("", window, cx);
                        state.focus(window, cx);
                    });
                    this.modal = Modal::WorkspaceIcon(id);
                    cx.notify();
                }))
                .item(item("名前を変更", |this, id, window, cx| {
                    this.ask_workspace_name(None, Some(id), window, cx)
                }))
                .item(item(
                    "サブワークスペースを追加",
                    |this, id, window, cx| this.ask_workspace_name(Some(id), None, window, cx),
                ))
                .item(item("設定", |this, id, window, cx| {
                    this.open_workspace_settings(&id, window, cx)
                }))
                .separator()
                .item(item("削除", |this, id, _, cx| {
                    this.modal = Modal::DeleteWorkspace(id);
                    cx.notify();
                }))
            };
            // 「…」は、行に乗せたときと、開いているワークスペースでだけ見せる
            let more = Button::new(("ws-more", ix))
                .icon(app_icon("ellipsis"))
                .ghost()
                .xsmall()
                .when(!active, |b| {
                    b.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.))
                })
                .dropdown_menu({
                    let menu = menu.clone();
                    move |m, _, _| menu(m)
                });
            tree = tree.child(
                row()
                    .id(("ws-row", ix))
                    .group(group)
                    .pl(px(12.) * node.depth as f32)
                    .child(caret)
                    .child(self.nav_item(
                        ("ws", ix),
                        workspace_icon(ws.icon.as_deref(), ws.color.as_deref()),
                        ws.name.clone(),
                        Nav::Workspace(id),
                        cx,
                    ))
                    .child(more)
                    // 右クリックでも同じメニューを出す
                    .context_menu(move |m, _, _| menu(m)),
            );
        }

        let unread = self.unread_notices;
        let view = cx.entity().downgrade();
        let inner = col()
            .id("sidebar")
            .size_full()
            .bg(theme.sidebar)
            .text_color(theme.sidebar_foreground)
            .border_r_1()
            .border_color(theme.sidebar_border)
            // macOS は左上に信号機ボタンが乗るので、その分を空ける
            .when(cfg!(target_os = "macos"), |d| d.pt_6())
            .child(
                row()
                    .h(HEADER_HEIGHT)
                    .flex_none()
                    .px_4()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.sidebar_border)
                    .child(
                        col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::BOLD)
                                    .truncate()
                                    .child(crate::APP_NAME.to_uppercase()),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child("タスク管理"),
                            ),
                    )
                    .child(
                        Button::new("close-sidebar")
                            .icon(app_icon("x"))
                            .framed(cx)
                            .tooltip("メニューを閉じる")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.toggle_sidebar(wide, cx)),
                            ),
                    ),
            )
            .child(
                col()
                    .id("sidebar-nav")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_3()
                    .gap_1()
                    .child(row().child(self.nav_item(
                        "nav-home",
                        app_icon("house"),
                        "ホーム",
                        Nav::Home,
                        cx,
                    )))
                    .child(
                        row().child(
                            self.nav_link(
                                "nav-notices",
                                app_icon("bell"),
                                "通知",
                                self.modal == Modal::Notices,
                                cx,
                            )
                            .when(unread > 0, |d| {
                                d.child(
                                    div()
                                        .px_1p5()
                                        .rounded_full()
                                        .bg(theme.danger)
                                        .text_color(theme.danger_foreground)
                                        .text_xs()
                                        .child(unread.to_string()),
                                )
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.modal = Modal::Notices;
                                cx.notify();
                            }))
                            .test_support(),
                        ),
                    )
                    .child(row().child(self.nav_item(
                        "nav-all",
                        app_icon("list-todo"),
                        "すべてのタスク",
                        Nav::All,
                        cx,
                    )))
                    .child(row().child(self.nav_item(
                        "nav-unfiled",
                        app_icon("inbox"),
                        "未所属",
                        Nav::Unfiled,
                        cx,
                    )))
                    .child(
                        row()
                            .mt_3()
                            .px_3()
                            .gap_2()
                            .text_color(theme.muted_foreground)
                            .child(app_icon("layers").xsmall().flex_none())
                            .child(div().flex_1().text_xs().child("ワークスペース"))
                            .child(
                                Button::new("add-workspace")
                                    .icon(app_icon("plus"))
                                    .ghost()
                                    .xsmall()
                                    .tooltip("ワークスペースを追加")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.ask_workspace_name(None, None, window, cx)
                                    })),
                            ),
                    )
                    .when(!self.workspaces.is_empty(), |d| d.child(tree)),
            )
            // 何もないところの右クリックで、ワークスペースを足せる
            .context_menu(move |menu, _, _| {
                let view = view.clone();
                menu.item(PopupMenuItem::new("ワークスペースを追加").on_click(
                    move |_, window, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.ask_workspace_name(None, None, window, cx)
                        });
                    },
                ))
            });
        let outer = div()
            .relative()
            .w(px(self.sidebar_width))
            .flex_none()
            .h_full()
            .child(inner);
        if !wide {
            return outer;
        }
        // 右の境界をドラッグして幅を変える。ダブルクリックで最初の幅に戻す
        let resizing = self.sidebar_resizing;
        outer.child(
            div()
                .id("sidebar-resize")
                .absolute()
                .top_0()
                .bottom_0()
                .right(px(-3.))
                .w(px(6.))
                .cursor_col_resize()
                .when(resizing, |d| d.bg(theme.ring))
                .hover(|s| s.bg(theme.ring))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        if event.click_count >= 2 {
                            this.reset_sidebar_width(cx);
                        } else {
                            this.sidebar_resizing = true;
                            cx.notify();
                        }
                    }),
                )
                .test_support(),
        )
    }

    // ---- 上のバー ----

    /// 今の画面の名前と、その下に出す一言、名前の左に出すアイコン。
    fn header_title(&self) -> (SharedString, Option<&'static str>, Option<Icon>) {
        match &self.nav {
            Nav::Home => ("ホーム".into(), None, None),
            Nav::All => ("すべてのタスク".into(), Some("タスク一覧"), None),
            Nav::Unfiled => ("未所属のタスク".into(), Some("タスク一覧"), None),
            Nav::Workspace(id) => {
                let ws = self.workspaces.iter().find(|w| &w.id == id);
                (
                    self.list_title(),
                    Some("タスク一覧"),
                    ws.map(|w| {
                        workspace_icon(w.icon.as_deref(), w.color.as_deref()).size(TILE_ICON_SIZE)
                    }),
                )
            }
        }
    }

    fn theme_toggle(&self, cx: &mut Context<Self>) -> Div {
        use crate::theme::Choice;
        let theme = cx.theme();
        let current = crate::theme::choice(cx);
        // 隣の丸いボタンと同じ高さにそろえる
        let mut toggle = row()
            .flex_none()
            .h_8()
            .px_1()
            .gap_0p5()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted);
        for (ix, (choice, icon)) in [
            (Choice::Light, "sun"),
            (Choice::Dark, "moon"),
            (Choice::System, "monitor"),
        ]
        .into_iter()
        .enumerate()
        {
            let active = choice == current;
            toggle = toggle.child(
                div()
                    .rounded_md()
                    .when(active, |d| d.bg(theme.background).shadow_sm())
                    .child(
                        Button::new(("theme", ix))
                            .icon(app_icon(icon))
                            .ghost()
                            .small()
                            .tooltip(choice.label())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Err(error) = crate::theme::set(choice, window, cx) {
                                    this.error = Some(
                                        format!("配色の選択を保存できませんでした。{error}").into(),
                                    );
                                }
                                cx.notify();
                            })),
                    ),
            );
        }
        toggle
    }

    /// 最小化・最大化・閉じるのボタン。macOS は OS が描くので出さない。
    /// 上のバーのほかのボタンと同じ高さで並べ、乗せたときの色はボタンの中だけに付ける。
    fn window_controls(&self, window: &Window, cx: &mut Context<Self>) -> Option<Div> {
        if cfg!(target_os = "macos") {
            return None;
        }
        // OS がタイトルバーを描いているときは、そちらのボタンを使う
        #[cfg(target_os = "linux")]
        if !matches!(window.window_decorations(), Decorations::Client { .. }) {
            return None;
        }
        let theme = cx.theme().clone();
        let supported = window.window_controls();
        let max = if window.is_maximized() {
            IconName::WindowRestore
        } else {
            IconName::WindowMaximize
        };
        let buttons = [
            (
                "window-minimize",
                IconName::WindowMinimize,
                WindowControlArea::Min,
                supported.minimize,
            ),
            (
                "window-maximize",
                max,
                WindowControlArea::Max,
                supported.maximize,
            ),
            (
                "window-close",
                IconName::WindowClose,
                WindowControlArea::Close,
                true,
            ),
        ];
        let mut controls = row().flex_none().ml_1();
        for (id, icon, area, shown) in buttons {
            if !shown {
                continue;
            }
            let close = area == WindowControlArea::Close;
            // 閉じるは Windows の標準と同じ赤
            let (hover_bg, hover_fg, active_bg) = if close {
                (
                    gpui_kit::rgb(0xc42b1c).into(),
                    gpui_kit::white(),
                    gpui_kit::rgb(0x94141a).into(),
                )
            } else {
                (
                    theme.secondary_hover,
                    theme.foreground,
                    theme.secondary_active,
                )
            };
            controls = controls.child(
                div()
                    .id(id)
                    .h_8()
                    .w(rems(2.75))
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .text_color(theme.foreground)
                    .hover(move |s| s.bg(hover_bg).text_color(hover_fg))
                    .active(move |s| s.bg(active_bg).text_color(hover_fg))
                    // Windows では、OS がこの場所をウィンドウのボタンとして扱う
                    .when(cfg!(target_os = "windows"), |d| d.window_control_area(area))
                    .when(!cfg!(target_os = "windows"), |d| {
                        d.on_mouse_down(MouseButton::Left, |_, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                        })
                        .on_click(move |_, window, cx| {
                            cx.stop_propagation();
                            match area {
                                WindowControlArea::Min => window.minimize_window(),
                                WindowControlArea::Max => window.zoom_window(),
                                _ => window.remove_window(),
                            }
                        })
                    })
                    .child(Icon::new(icon).small())
                    .test_support(),
            );
        }
        Some(controls)
    }

    fn render_header(
        &self,
        unlocked: bool,
        wide: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = cx.theme().clone();
        // バーの空いているところを掴むと、ウィンドウを動かせる。
        // Windows は OS に任せ、ほかは押したまま動かしたときに自分で動かし始める
        let bar = row()
            .flex_none()
            .h(HEADER_HEIGHT)
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.background)
            .when(cfg!(target_os = "windows"), |d| {
                d.window_control_area(WindowControlArea::Drag)
            })
            .when(!cfg!(target_os = "windows"), |d| {
                d.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, window, cx| {
                        // ボタンが受けた押下では動かさない
                        if window.default_prevented() {
                            return;
                        }
                        if event.click_count == 2 {
                            if cfg!(target_os = "macos") {
                                window.titlebar_double_click();
                            } else {
                                window.zoom_window();
                            }
                            return;
                        }
                        this.header_pressed = true;
                        cx.notify();
                    }),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, _| this.header_pressed = false),
                )
                .on_mouse_move(cx.listener(
                    |this, event: &MouseMoveEvent, window, _| {
                        if !this.header_pressed {
                            return;
                        }
                        this.header_pressed = false;
                        if event.pressed_button == Some(MouseButton::Left) {
                            window.start_window_move();
                        }
                    },
                ))
            });
        if !unlocked {
            return bar
                .child(
                    div()
                        .flex_1()
                        .px_4()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(crate::APP_NAME),
                )
                // 下のバーの判定に取られないよう、ボタンのある場所はマウスを受け止める
                .child(
                    row()
                        .occlude()
                        .pr_3()
                        .children(self.window_controls(window, cx)),
                );
        }
        let sidebar_shown = if wide {
            !self.sidebar_hidden
        } else {
            self.sidebar_open
        };
        let (title, subtitle, icon) = self.header_title();
        let unread = self.unread_notices;
        let lock_enabled = self.lock_enabled(cx);
        let view = cx.entity().downgrade();
        bar.child(
            row()
                .flex_1()
                .min_w_0()
                .gap_3()
                .pl_4()
                // macOS でメニューを閉じているときは、信号機ボタンの分を空ける
                .when(cfg!(target_os = "macos") && !sidebar_shown, |d| {
                    d.pl(px(84.))
                })
                .child(
                    // ボタンのある場所はマウスを受け止める（右のボタンと同じ理由）
                    div().occlude().child(
                        Button::new("toggle-sidebar")
                            .icon(app_icon("menu"))
                            .framed(cx)
                            .tooltip("メニュー")
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.toggle_sidebar(wide, cx)),
                            ),
                    ),
                )
                .when_some(icon, |d, icon| {
                    d.child(
                        div()
                            .size_10()
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .bg(theme.muted)
                            .child(icon),
                    )
                })
                .child(
                    col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_xl()
                                .font_weight(FontWeight::BOLD)
                                .truncate()
                                .child(title),
                        )
                        .when_some(subtitle, |d, subtitle| {
                            d.child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(subtitle),
                            )
                        }),
                ),
        )
        .child(
            row()
                .flex_none()
                .gap_2()
                .pr_3()
                // 下のバーの判定に取られないよう、ボタンのある場所はマウスを受け止める。
                // これがないと、Windows ではボタンを離したときのクリックがウィンドウの移動に取られる
                .occlude()
                .child(self.theme_toggle(cx))
                .child(
                    Button::new("ai-toggle")
                        .icon(app_icon("sparkles"))
                        // AI が動いている間は、回る印に変わる
                        .loading(self.ai_session.read(cx).busy)
                        .outline()
                        .rounded(px(999.))
                        .tooltip("AI チャット")
                        .on_click(cx.listener(|this, _, window, cx| this.toggle_ai(window, cx))),
                )
                .child(
                    div()
                        .relative()
                        .child(
                            Button::new("open-notices")
                                .icon(app_icon("bell"))
                                .framed(cx)
                                .rounded(px(999.))
                                .tooltip("通知")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.modal = Modal::Notices;
                                    cx.notify();
                                })),
                        )
                        .when(unread > 0, |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .top(px(-4.))
                                    .right(px(-4.))
                                    .px_1()
                                    .rounded_full()
                                    .bg(theme.danger)
                                    .text_color(theme.danger_foreground)
                                    .text_xs()
                                    .child(unread.to_string()),
                            )
                        }),
                )
                .child(
                    Button::new("open-menu")
                        .icon(app_icon("settings"))
                        .framed(cx)
                        .rounded(px(999.))
                        .tooltip("設定")
                        .dropdown_menu(move |menu, _, _| {
                            let item = |label: &'static str,
                                        run: fn(
                                &mut MainView,
                                &mut Window,
                                &mut Context<MainView>,
                            )| {
                                let view = view.clone();
                                PopupMenuItem::new(label).on_click(move |_, window, cx| {
                                    let _ = view.update(cx, |this, cx| run(this, window, cx));
                                })
                            };
                            let menu = menu
                                .item(item("担当者", |this, window, cx| {
                                    this.open_people(window, cx)
                                }))
                                .separator()
                                .item(item("エクスポート", |this, window, cx| {
                                    this.export(window, cx)
                                }))
                                .item(item("インポート", |this, window, cx| {
                                    this.import(window, cx)
                                }))
                                .separator()
                                .item(if lock_enabled {
                                    item("今すぐロック", |_, window, cx| {
                                        window.dispatch_action(Box::new(LockNow), cx)
                                    })
                                } else {
                                    item("ロックを有効にする", |this, window, cx| {
                                        this.enable_lock(window, cx)
                                    })
                                });
                            let menu = if lock_enabled {
                                menu.item(item(
                                    "ロックを無効にする",
                                    |this, window, cx| this.disable_lock(window, cx),
                                ))
                            } else {
                                menu
                            };
                            menu.separator()
                                .item(item("新しいウィンドウ", |_, window, cx| {
                                    window.dispatch_action(Box::new(NewWindow), cx)
                                }))
                        }),
                )
                .children(self.window_controls(window, cx)),
        )
    }

    // ---- ホーム ----

    fn render_home(&self, roomy: bool, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme().clone();
        let due = self.due.clone();
        let count = due.len();
        let roots: Vec<_> = self
            .workspaces
            .iter()
            .filter(|w| w.parent_id.is_none())
            .cloned()
            .collect();
        let heading =
            |text: &'static str| div().text_lg().font_weight(FontWeight::BOLD).child(text);
        col()
            .id("home")
            .size_full()
            .overflow_y_scroll()
            .p_6()
            .gap_3()
            .child(
                row()
                    .gap_3()
                    .child(heading("期限が近いタスク"))
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
            .child(if count == 0 {
                div()
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_dashed()
                    .border_color(theme.border)
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(if self.due_mine {
                        "自分が担当の、期限が近いタスクはありません。"
                    } else {
                        "期限が近いタスクはありません。"
                    })
            } else {
                col()
                    .h(ROW_HEIGHT * count.min(8) as f32 + px(2.))
                    .flex_none()
                    .border_1()
                    .border_color(theme.border)
                    .rounded_lg()
                    .bg(theme.popover)
                    .overflow_hidden()
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
            .child(div().mt_6().child(heading("ワークスペース")))
            .child(row().flex_wrap().items_start().gap_3().children(
                roots.into_iter().enumerate().map(|(ix, ws)| {
                    let id = ws.id.clone();
                    row()
                        .id(("home-ws", ix))
                        .w(px(248.))
                        .p_2()
                        .pr_3()
                        .gap_2p5()
                        .items_center()
                        .border_1()
                        .border_color(theme.border)
                        .rounded_lg()
                        .bg(theme.popover)
                        .cursor_pointer()
                        .hover(|s| s.border_color(theme.primary))
                        .active(|s| s.bg(theme.accent))
                        .child(
                            div()
                                .size(px(44.))
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_md()
                                .bg(theme.muted)
                                .child(
                                    workspace_icon(ws.icon.as_deref(), ws.color.as_deref())
                                        .size(CARD_ICON_SIZE),
                                ),
                        )
                        .child(
                            col()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .font_weight(FontWeight::BOLD)
                                        .truncate()
                                        .child(ws.name.clone()),
                                )
                                .when_some(ws.date.clone(), |d, date| {
                                    d.child(
                                        div()
                                            .text_sm()
                                            .text_color(theme.muted_foreground)
                                            .child(view::format_date(&date, &self.today)),
                                    )
                                }),
                        )
                        .child(
                            app_icon("chevron-right")
                                .small()
                                .text_color(theme.muted_foreground),
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.go(Nav::Workspace(id.clone()), cx)
                        }))
                }),
            ))
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

    /// 選択肢から 1 つを選ぶ欄。押すとメニューが開く。
    fn select<T: Clone + 'static>(
        &self,
        id: &'static str,
        label: &'static str,
        options: Vec<(SharedString, bool, T)>,
        apply: fn(&mut MainView, T),
        cx: &mut Context<Self>,
    ) -> Div {
        let current = options
            .iter()
            .find(|o| o.1)
            .map(|o| o.0.clone())
            .unwrap_or_default();
        let view = cx.entity().downgrade();
        col()
            .flex_1()
            .min_w(px(110.))
            .gap_1()
            .child(field_label(label, cx))
            .child(
                Button::new(id)
                    .framed(cx)
                    .w_full()
                    .child(
                        row()
                            .w_full()
                            .gap_1()
                            .child(div().flex_1().min_w_0().truncate().child(current))
                            .child(app_icon("chevron-down").xsmall().flex_none()),
                    )
                    .dropdown_menu(move |mut menu, _, _| {
                        for (label, checked, value) in options.iter().cloned() {
                            let view = view.clone();
                            menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                                move |_, _, cx| {
                                    // ウィンドウを閉じたあとなら、反映する先がない
                                    let _ = view.update(cx, |this, cx| {
                                        apply(this, value.clone());
                                        this.refilter();
                                        cx.notify();
                                    });
                                },
                            ));
                        }
                        menu
                    }),
            )
    }

    fn render_filters(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let q = &self.query;
        let mut status = vec![("すべて".into(), q.status.is_none(), None)];
        for s in Status::ALL {
            status.push((s.label().into(), q.status == Some(s), Some(s)));
        }
        let mut assignee = vec![
            (
                SharedString::from("すべて"),
                q.assignee == AssigneeFilter::All,
                AssigneeFilter::All,
            ),
            (
                "自分".into(),
                q.assignee == AssigneeFilter::Me,
                AssigneeFilter::Me,
            ),
            (
                "未割り当て".into(),
                q.assignee == AssigneeFilter::Unassigned,
                AssigneeFilter::Unassigned,
            ),
        ];
        for p in &self.people {
            let value = AssigneeFilter::Person(p.id.clone());
            assignee.push((p.name.clone().into(), q.assignee == value, value));
        }
        let mut my_role = vec![(SharedString::from("すべて"), q.my_role.is_none(), None)];
        for role in Raci::ALL {
            my_role.push((
                format!("{}（{}）", role.code(), role.label()).into(),
                q.my_role == Some(role),
                Some(role),
            ));
        }
        let mut priority = vec![(SharedString::from("すべて"), q.priority.is_none(), None)];
        for p in [Prio::High, Prio::Mid, Prio::Low, Prio::None] {
            priority.push((p.label().into(), q.priority == Some(p), Some(p)));
        }
        let mut category = vec![(SharedString::from("すべて"), q.category.is_none(), None)];
        for c in &self.categories {
            category.push((
                c.clone().into(),
                q.category.as_ref() == Some(c),
                Some(c.clone()),
            ));
        }
        let sort = Sort::ALL
            .into_iter()
            .map(|s| (SharedString::from(s.label()), q.sort == s, s))
            .collect();
        row()
            .flex_none()
            .flex_wrap()
            .items_end()
            .gap_3()
            .p_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                col()
                    .flex_1()
                    .min_w(px(180.))
                    .gap_1()
                    .child(field_label("検索", cx))
                    .child(
                        Input::new(&self.inputs.search)
                            .id("task-search")
                            .aria_label("検索")
                            .prefix(app_icon("search").small())
                            .cleanable(true),
                    ),
            )
            .child(self.select(
                "f-status",
                "状態",
                status,
                |this, v| this.query.status = v,
                cx,
            ))
            .child(self.select(
                "f-assignee",
                "担当",
                assignee,
                |this, v| this.query.assignee = v,
                cx,
            ))
            .child(self.select(
                "f-role",
                "自分の役",
                my_role,
                |this, v| this.query.my_role = v,
                cx,
            ))
            .child(self.select(
                "f-priority",
                "優先度",
                priority,
                |this, v| this.query.priority = v,
                cx,
            ))
            .child(self.select(
                "f-category",
                "カテゴリ",
                category,
                |this, v| this.query.category = v,
                cx,
            ))
            .child(self.select("f-sort", "並び順", sort, |this, v| this.query.sort = v, cx))
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
                    move |this, _: &ClickEvent, _, cx| {
                        this.bulk_apply(&BulkOp::Status(s), cx);
                    },
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
                        this.bulk_apply(&BulkOp::Priority(p.value()), cx);
                    },
                )),
            );
        }
        for (button, label, modal) in [
            ("bulk-due", "期限", Modal::BulkDue),
            ("bulk-assignees", "担当", Modal::BulkAssignees),
            ("bulk-categories", "カテゴリ", Modal::BulkCategories),
        ] {
            bar = bar.child(Button::new(button).label(label).small().on_click(
                cx.listener(move |this, _, window, cx| this.open_bulk(modal.clone(), window, cx)),
            ));
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
        let tab = |id: &'static str,
                   icon: &'static str,
                   label: &'static str,
                   count: usize,
                   tab: Tab,
                   this: &Self,
                   cx: &mut Context<Self>| {
            let on = this.query.tab == tab;
            row()
                .id(id)
                .gap_1p5()
                .px_3()
                .py_1p5()
                .rounded_md()
                .text_sm()
                .cursor_pointer()
                .when(on, |d| d.bg(theme.background).font_weight(FontWeight::BOLD))
                .when(!on, |d| {
                    d.text_color(theme.muted_foreground)
                        .hover(|s| s.text_color(theme.foreground))
                })
                .active(|s| s.bg(theme.background).opacity(0.7))
                .child(app_icon(icon).small())
                .child(label)
                .child(
                    div()
                        .font_weight(FontWeight::NORMAL)
                        .text_color(theme.muted_foreground)
                        .child(count.to_string()),
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

        let mut header = row().flex_none().gap_2().flex_wrap();
        let date = workspace_id.as_ref().and_then(|id| {
            self.workspaces
                .iter()
                .find(|w| &w.id == id)
                .and_then(|w| w.date.clone())
        });
        header = header.child(
            row()
                .flex_1()
                .min_w(px(120.))
                .gap_2()
                .text_sm()
                .when_some(date, |d, date| {
                    d.child(div().text_color(theme.muted_foreground).child("基準日"))
                        .child(view::format_date(&date, &self.today))
                })
                .when(has_children, |d| {
                    d.child(
                        Checkbox::new("include-descendants")
                            .label("サブワークスペースも含める")
                            .checked(self.include_descendants)
                            .on_click(cx.listener(|this, checked: &bool, _, cx| {
                                this.include_descendants = *checked;
                                this.reload(cx);
                            })),
                    )
                }),
        );
        if let Some(id) = workspace_id.clone() {
            let (c, d) = (id.clone(), id.clone());
            header = header
                .child(
                    Button::new("ws-settings")
                        .icon(app_icon("sliders-horizontal"))
                        .tooltip("タスク追加時の既定値")
                        .framed(cx)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_workspace_settings(&d, window, cx)
                        })),
                )
                .child(
                    Button::new("ws-export")
                        .icon(app_icon("download"))
                        .tooltip("エクスポート")
                        .framed(cx)
                        .on_click(cx.listener(|this, _, window, cx| this.export(window, cx))),
                )
                .child(
                    Button::new("ws-import")
                        .icon(app_icon("upload"))
                        .tooltip("インポート")
                        .framed(cx)
                        .on_click(cx.listener(|this, _, window, cx| this.import(window, cx))),
                )
                .child(
                    Button::new("ws-delete")
                        .icon(app_icon("trash"))
                        .tooltip("削除")
                        .framed(cx)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.modal = Modal::DeleteWorkspace(c.clone());
                            cx.notify();
                        })),
                );
        }
        header = header.child(
            Button::new("new-task-open")
                .icon(app_icon("plus"))
                .label("新しいタスク")
                .primary()
                .on_click(cx.listener(|this, _, window, cx| this.open_new_task(window, cx))),
        );

        let rows = if visible == 0 {
            col()
                .flex_1()
                .items_center()
                .justify_center()
                .gap_2()
                .py_8()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(if self.tasks.is_empty() {
                    "まだタスクがありません"
                } else if active > 0 {
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
            // 件数が少ないときは、表を中身の高さで終える
            col().h(ROW_HEIGHT * visible as f32).min_h_0().child(
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

        let card = col()
            .min_h_0()
            .border_1()
            .border_color(theme.border)
            .rounded_lg()
            .bg(theme.popover)
            .overflow_hidden()
            .child(
                row()
                    .flex_none()
                    .gap_1()
                    .p_2()
                    .bg(theme.muted.opacity(0.5))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(tab(
                        "tab-open",
                        "circle-dot",
                        "未完了",
                        self.counts.0,
                        Tab::Open,
                        self,
                        cx,
                    ))
                    .child(tab(
                        "tab-closed",
                        "circle-check",
                        "完了",
                        self.counts.1,
                        Tab::Closed,
                        self,
                        cx,
                    )),
            )
            .child(self.render_filters(cx))
            .when(!self.selected.is_empty(), |d| {
                d.child(div().p_2().child(self.render_bulk_bar(cx)))
            })
            .when(table, |d| d.child(self.render_table_head(cx)))
            .child(rows)
            .child(self.render_quick_add_row(table, cx));

        div()
            .size_full()
            .relative()
            .child(col().size_full().p_6().gap_3().child(header).child(card))
            // 右下の「＋」。どこまでスクロールしていても、新しいタスクを始められる
            .child(
                div().absolute().right_6().bottom_24().child(
                    Button::new("fab-new-task")
                        .icon(app_icon("plus"))
                        .primary()
                        .large()
                        .rounded(px(999.))
                        .tooltip("新しいタスク")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.open_new_task(window, cx)),
                        ),
                ),
            )
    }

    fn render_table_head(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let all = !self.visible.is_empty() && self.selected.len() == self.visible.len();
        row()
            .h_9()
            .px_3()
            .gap_3()
            .flex_none()
            .bg(theme.muted.opacity(0.4))
            .border_b_1()
            .border_color(theme.border)
            .text_xs()
            .font_weight(FontWeight::BOLD)
            .text_color(theme.muted_foreground)
            .child(
                div().w_6().flex_none().child(
                    Checkbox::new("select-all")
                        .accessibility_label("表示中のタスクをすべて選択")
                        .checked(all)
                        .indeterminate(!all && !self.selected.is_empty())
                        .on_click(cx.listener(|this, _: &bool, _, cx| this.toggle_select_all(cx))),
                ),
            )
            .child(div().flex_1().min_w_0().flex().child(self.column_menu(
                "col-title",
                "タイトル",
                None,
                cx,
            )))
            .children(
                self.shown_columns()
                    .cloned()
                    .collect::<Vec<_>>()
                    .into_iter()
                    .map(|c| {
                        let column = c.column;
                        let over = self.column_drag.as_ref().is_some_and(|d| {
                            d.moved && d.over == Some(column) && d.column != column
                        });
                        let dragged = self
                            .column_drag
                            .as_ref()
                            .is_some_and(|d| d.moved && d.column == column);
                        let resizing = self.column_resize.is_some_and(|(r, _, _)| r == column);
                        div()
                            .id(("col-head", column as usize))
                            .relative()
                            .w(px(c.width))
                            .h_full()
                            .flex_none()
                            .flex()
                            .items_center()
                            .when(over, |d| d.bg(theme.accent))
                            .when(dragged, |d| d.opacity(0.5))
                            // 見出しを押したまま横へ動かすと、列を並べ替える
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                    this.column_drag = Some(crate::state::ColumnDrag {
                                        column,
                                        start_x: f32::from(event.position.x),
                                        moved: false,
                                        over: None,
                                    });
                                    cx.notify();
                                }),
                            )
                            .on_mouse_move(cx.listener(move |this, _: &MouseMoveEvent, _, cx| {
                                if let Some(drag) = &mut this.column_drag {
                                    if drag.moved && drag.over != Some(column) {
                                        drag.over = Some(column);
                                        cx.notify();
                                    }
                                }
                            }))
                            .child(self.column_menu(
                                ("col-menu", column as usize),
                                column.label(),
                                Some(column),
                                cx,
                            ))
                            .child(
                                // 左の境界をドラッグして幅を変える。ダブルクリックで最初の幅に戻す
                                div()
                                    .id(("col-resize", column as usize))
                                    .absolute()
                                    .top_0()
                                    .bottom_0()
                                    .left(px(-9.))
                                    .w(px(6.))
                                    .cursor_col_resize()
                                    .when(resizing, |d| d.bg(theme.ring))
                                    .hover(|s| s.bg(theme.ring))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                            cx.stop_propagation();
                                            if event.click_count >= 2 {
                                                this.reset_column_width(column, cx);
                                            } else {
                                                this.start_column_resize(
                                                    column,
                                                    f32::from(event.position.x),
                                                    cx,
                                                );
                                            }
                                        }),
                                    )
                                    .test_support(),
                            )
                            .test_support()
                    }),
            )
    }

    /// 表の見出し。押すと、並べ替えと、列を出す・隠すメニューが開く。`column` が `None` はタイトル。
    fn column_menu(
        &self,
        id: impl Into<ElementId>,
        label: &'static str,
        column: Option<Column>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme();
        let current = self.column_sorted(column);
        let open = self
            .column_menu
            .as_ref()
            .filter(|open| open.column == column);
        div()
            .relative()
            .child(
                row()
                    .id(id)
                    .ml(px(-6.))
                    .px(px(6.))
                    .py_0p5()
                    .gap_1()
                    .rounded_md()
                    .cursor_pointer()
                    .when(open.is_some(), |d| d.bg(theme.accent))
                    .hover(|s| s.bg(theme.accent))
                    .active(|s| s.bg(theme.primary.opacity(0.4)))
                    .child(label)
                    .when_some(current, |d, descending| {
                        d.child(
                            app_icon(if descending { "arrow-down" } else { "arrow-up" }).xsmall(),
                        )
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        // 見出しを動かして離したときは、開かない
                        if this.column_drag.as_ref().is_some_and(|d| d.moved) {
                            return;
                        }
                        this.toggle_column_menu(column, window, cx);
                    }))
                    .test_support(),
            )
            .when_some(open, |d, open| {
                d.child(
                    div().absolute().top_full().left(px(-6.)).child(
                        deferred(
                            anchored()
                                .snap_to_window_with_margin(px(8.))
                                .child(div().occlude().pt_1().child(open.menu.clone())),
                        )
                        .with_priority(1),
                    ),
                )
            })
    }

    /// 表の 1 行に、今出している列の順でセルを並べる。
    fn column_cells(&self, mut cells: [Option<AnyElement>; 5]) -> Vec<Div> {
        self.shown_columns()
            .map(|c| {
                div()
                    .w(px(c.width))
                    .flex_none()
                    .min_w_0()
                    .overflow_hidden()
                    .children(cells[c.column as usize].take())
            })
            .collect()
    }

    /// 一覧での担当の表示。全員を頭文字の丸と名前で並べ、幅が足りなければ人ごとに折り返す。
    fn assignee_cell(&self, ids: &[Id], cx: &App) -> Div {
        let theme = cx.theme();
        if ids.is_empty() {
            return row()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("未割り当て");
        }
        let mut cell = row()
            .min_w_0()
            .max_h(px(52.))
            .overflow_hidden()
            .flex_wrap()
            .gap_x_2p5()
            .gap_y_1()
            .text_sm();
        for id in ids {
            let name = self.person_name(id);
            cell = cell.child(
                row()
                    .min_w_0()
                    .gap_1p5()
                    .child(avatar(name, cx))
                    .child(div().truncate().child(name.to_owned())),
            );
        }
        cell
    }

    /// 値を押して選ぶメニュー。表の中と追加行で使う。
    fn value_menu<T: Clone + 'static>(
        &self,
        id: impl Into<ElementId>,
        shown: Div,
        options: Vec<(SharedString, bool, T)>,
        apply: impl Fn(&mut MainView, T, &mut Context<MainView>) + Clone + 'static,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let view = cx.entity().downgrade();
        Button::new(id)
            .ghost()
            .small()
            .justify_start()
            .child(shown)
            .dropdown_menu(move |mut menu, _, _| {
                for (label, checked, value) in options.iter().cloned() {
                    let view = view.clone();
                    let apply = apply.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                        move |_, _, cx| {
                            // ウィンドウを閉じたあとなら、反映する先がない
                            let _ = view.update(cx, |this, cx| apply(this, value.clone(), cx));
                        },
                    ));
                }
                menu
            })
    }

    fn priority_options(current: Prio) -> Vec<(SharedString, bool, u8)> {
        [Prio::High, Prio::Mid, Prio::Low, Prio::None]
            .into_iter()
            .map(|p| (p.label().into(), p == current, p.value()))
            .collect()
    }

    fn assignee_options(&self, ids: &[Id]) -> Vec<(SharedString, bool, Id)> {
        self.people
            .iter()
            .map(|p| (p.name.clone().into(), ids.contains(&p.id), p.id.clone()))
            .collect()
    }

    /// 期限の選択肢。今日からの日数で決める。
    fn due_options(&self, current: Option<&str>) -> Vec<(SharedString, bool, Option<String>)> {
        let mut options: Vec<(SharedString, bool, Option<String>)> =
            [("今日", 0), ("明日", 1), ("1 週間後", 7), ("1 か月後", 30)]
                .into_iter()
                .map(|(label, days)| {
                    let date = view::add_days(&self.today, days);
                    (label.into(), current == Some(date.as_str()), Some(date))
                })
                .collect();
        options.push(("なし".into(), current.is_none(), None));
        options
    }

    fn render_task_row(&self, ix: usize, table: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let task = &self.tasks[self.visible[ix]];
        let id = task.id.clone();
        let active = self.panel.as_deref() == Some(id.as_str());
        let closed = !task.status.is_open();
        let now = malakt_core::now_ms();
        let arranging = self.ai_session.read(cx).arranging.contains(&task.id);

        let select = {
            let id = id.clone();
            div().w_6().flex_none().child(
                Checkbox::new(("select", ix))
                    .accessibility_label(format!("「{}」を選択", task.title))
                    .checked(self.selected.contains(&task.id))
                    .on_click(
                        // Shift を押しながらなら、前に押した行からの範囲をまとめて選ぶ
                        cx.listener(move |this, _: &bool, window, cx| {
                            this.click_select(&id, window.modifiers().shift, cx)
                        }),
                    ),
            )
        };
        let mut chips = row().gap_1().overflow_hidden();
        let mut has_chips = false;
        for category in &task.categories {
            has_chips = true;
            chips = chips.child(
                div()
                    .flex_none()
                    .px_2()
                    .rounded_full()
                    .border_1()
                    .border_color(theme.foreground.opacity(0.2))
                    .bg(theme.secondary)
                    .text_xs()
                    .child(category.clone()),
            );
        }
        if !task.linked_workspace_ids.is_empty() {
            has_chips = true;
            // ほかのワークスペースにも出しているタスクには、か所の数を添える
            chips = chips.child(
                row()
                    .flex_none()
                    .gap_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(app_icon("link").xsmall())
                    .child(format!("同期 {}", task.linked_workspace_ids.len() + 1)),
            );
        }
        let title = {
            let id = id.clone();
            col()
                .flex_1()
                .min_w_0()
                .gap_1()
                .when(task.parent_id.is_some(), |d| d.pl_4())
                .child(
                    div()
                        .id(("title", ix))
                        .flex()
                        .items_center()
                        .gap_2()
                        // AI が整えている間は、タイトルの前で印が回る
                        .when(arranging, |d| {
                            d.child(
                                div()
                                    .id(("ai-working", ix))
                                    .flex_none()
                                    .child(Spinner::new().small())
                                    .test_support(),
                            )
                        })
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::BOLD)
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme.primary))
                        .active(|s| s.opacity(0.6))
                        .when(closed, |d| {
                            d.text_color(theme.muted_foreground).line_through()
                        })
                        .child(task.title.clone())
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.open_panel(&id, window, cx)
                        }))
                        .test_support(),
                )
                .when(has_chips, |d| d.child(chips))
        };
        let status = {
            let id = id.clone();
            div().flex().child(
                status_badge(task.status, cx)
                    .id(("status", ix))
                    .cursor_pointer()
                    .hover(|s| s.border_color(theme.primary))
                    .active(|s| s.bg(theme.accent))
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.ask_status(&id, window, cx)
                    })),
            )
        };
        let current = Prio::of(task.priority);
        let priority = {
            let id = id.clone();
            self.value_menu(
                ("prio", ix),
                priority_label(current, cx),
                Self::priority_options(current),
                move |this, value, cx| this.edit_task_by_id(&id, cx, |d| d.priority = value),
                cx,
            )
        };
        let assignees = {
            let id = id.clone();
            self.value_menu(
                ("assignee", ix),
                self.assignee_cell(&task.assignee_ids, cx),
                self.assignee_options(&task.assignee_ids),
                move |this, person: Id, cx| {
                    this.edit_task_by_id(&id, cx, |d| {
                        if let Some(i) = d.assignee_ids.iter().position(|a| a == &person) {
                            d.assignee_ids.remove(i);
                        } else {
                            d.assignee_ids.push(person);
                        }
                    })
                },
                cx,
            )
        };
        let due = {
            let id = id.clone();
            self.value_menu(
                ("due", ix),
                self.due_label(task, cx).text_sm(),
                self.due_options(task.due.as_deref()),
                move |this, value, cx| this.edit_task_by_id(&id, cx, |d| d.due = value),
                cx,
            )
        };
        let updated = div()
            .text_sm()
            .text_color(theme.muted_foreground)
            .child(view::format_relative(task.updated_at, now));

        let view = cx.entity().downgrade();
        let menu_id = id.clone();
        let base = div()
            .id(("row", ix))
            .w_full()
            .border_b_1()
            .border_color(theme.border)
            .when(active, |d| d.bg(theme.accent.opacity(0.4)))
            .when(!active, |d| d.hover(|s| s.bg(theme.muted.opacity(0.4))));
        let row_el = if table {
            base.h(ROW_HEIGHT)
                .flex()
                .flex_row()
                .items_center()
                .px_3()
                .gap_3()
                .child(select)
                .child(title)
                .children(self.column_cells([
                    Some(status.into_any_element()),
                    Some(priority.into_any_element()),
                    Some(assignees.into_any_element()),
                    Some(due.into_any_element()),
                    Some(updated.into_any_element()),
                ]))
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
                        .gap_1()
                        .pl_8()
                        .child(status)
                        .child(priority)
                        .child(due)
                        .child(assignees),
                )
        };
        // 右クリックで、その行の操作を出す
        row_el
            .context_menu(move |menu, _, _| {
                let item = |label: &'static str,
                            run: fn(&mut MainView, Id, &mut Window, &mut Context<MainView>)| {
                    let view = view.clone();
                    let id = menu_id.clone();
                    PopupMenuItem::new(label).on_click(move |_, window, cx| {
                        // ウィンドウを閉じたあとなら、反映する先がない
                        let _ = view.update(cx, |this, cx| run(this, id.clone(), window, cx));
                    })
                };
                menu.item(item("開く", |this, id, window, cx| {
                    this.open_panel(&id, window, cx)
                }))
                .item(item("状態を変更", |this, id, window, cx| {
                    this.ask_status(&id, window, cx)
                }))
                .separator()
                .item(item("削除", |this, id, _, cx| {
                    this.modal = Modal::DeleteTasks(vec![id]);
                    cx.notify();
                }))
            })
            .into_any_element()
    }

    /// 表の最後の追加行。タイトルと、状態・優先度・担当・期限を決めて足す。
    fn render_quick_add_row(&self, table: bool, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let quick = &self.quick;
        let status = self.value_menu(
            "quick-status",
            status_badge(quick.status, cx),
            Status::ALL
                .into_iter()
                .map(|s| (s.label().into(), s == quick.status, s))
                .collect(),
            |this, value, cx| {
                this.quick.status = value;
                this.quick_touched = true;
                cx.notify();
            },
            cx,
        );
        let current = Prio::of(quick.priority);
        let priority = self.value_menu(
            "quick-priority",
            priority_label(current, cx),
            Self::priority_options(current),
            |this, value, cx| {
                this.quick.priority = value;
                this.quick_touched = true;
                cx.notify();
            },
            cx,
        );
        let assignees = self.value_menu(
            "quick-assignee",
            self.assignee_cell(&quick.assignee_ids, cx),
            self.assignee_options(&quick.assignee_ids),
            |this, person: Id, cx| {
                this.quick_touched = true;
                if let Some(i) = this.quick.assignee_ids.iter().position(|a| a == &person) {
                    this.quick.assignee_ids.remove(i);
                } else {
                    this.quick.assignee_ids.push(person);
                }
                cx.notify();
            },
            cx,
        );
        let due = self.value_menu(
            "quick-due",
            match &quick.due {
                Some(due) => div().text_sm().child(view::format_date(due, &self.today)),
                None => div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("—"),
            },
            self.due_options(quick.due.as_deref()),
            |this, value, cx| {
                this.quick.due = value;
                this.quick_touched = true;
                cx.notify();
            },
            cx,
        );
        let input = Input::new(&self.inputs.quick_add)
            .id("quick-add")
            .aria_label("タスクを追加")
            .prefix(app_icon("plus").small())
            .flex_1();
        let view = cx.entity().downgrade();
        let submit = move || {
            let view = view.clone();
            Button::new("quick-add-submit")
                .label("追加")
                .primary()
                .on_click(move |_, window, cx| {
                    // ウィンドウを閉じたあとなら、反映する先がない
                    let _ = view.update(cx, |this, cx| this.submit_quick_add(window, cx));
                })
        };
        // 追加のボタンは「更新」の列に置く。隠しているときは右の端に出す
        let updated_shown = self.shown_columns().any(|c| c.column == Column::Updated);
        let base = div()
            .flex_none()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.muted.opacity(0.3))
            .px_3()
            .py_2();
        if table {
            base.flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(div().w_6().flex_none())
                .child(input)
                .children(self.column_cells([
                    Some(status.into_any_element()),
                    Some(priority.into_any_element()),
                    Some(assignees.into_any_element()),
                    Some(due.into_any_element()),
                    updated_shown.then(|| submit().into_any_element()),
                ]))
                .when(!updated_shown, |d| d.child(submit()))
        } else {
            base.flex()
                .flex_col()
                .gap_2()
                .child(row().gap_2().child(input).child(submit()))
                .child(
                    row()
                        .gap_1()
                        .flex_wrap()
                        .child(status)
                        .child(priority)
                        .child(assignees)
                        .child(due),
                )
        }
    }

    // ---- 詳細パネル ----

    /// コメント 1 件。書いた人・時刻・操作と、Markdown の本文。
    fn render_attachments(&self, detail: &Detail, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let mut list = col().gap_1();
        for (ix, a) in detail.attachments.iter().enumerate() {
            let (open_id, save_id, delete_id) = (a.id.clone(), a.id.clone(), a.id.clone());
            list = list.child(
                row()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(a.name.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(view::format_bytes(a.size)),
                    )
                    .child(
                        Button::new(("att-open", ix))
                            .label("開く")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_attachment(&open_id, cx);
                            })),
                    )
                    .child(
                        Button::new(("att-save", ix))
                            .label("保存")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.save_attachment(&save_id, window, cx);
                            })),
                    )
                    .child(
                        Button::new(("att-del", ix))
                            .label("削除")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.write(cx, |s| s.delete_attachment(&delete_id));
                            })),
                    ),
            );
        }
        col()
            .gap_2()
            .child(
                row()
                    .gap_2()
                    .items_center()
                    .child(section_title("添付"))
                    .when(!detail.attachments.is_empty(), |d| {
                        d.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{} 件", detail.attachments.len())),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("att-add")
                            .label("ファイルを追加")
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| this.attach(window, cx))),
                    ),
            )
            .child(list)
    }

    fn render_comment(&self, ix: usize, c: &Comment, now: i64, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let (pin_id, delete_id, edit_id, pinned) =
            (c.id.clone(), c.id.clone(), c.id.clone(), c.pinned);
        let author = c
            .author_id
            .as_deref()
            .map_or("", |a| self.person_name(a))
            .to_owned();
        // スレッドの中では枠を重ねない
        let boxed = !c.review && c.reply_to.is_none();
        let links = malakt_core::tasklink::find(&c.body);
        col()
            .gap_1()
            .when(boxed, |d| {
                d.p_2().border_1().border_color(theme.border).rounded_md()
            })
            .child(
                row()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(author)
                    .child(view::format_relative(c.created_at, now))
                    .when(c.is_edited(), |d| d.child("（編集済み）"))
                    .child(div().flex_1())
                    .child(
                        Button::new(("edit-comment", ix))
                            .label("編集")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_edit_comment(&edit_id, window, cx)
                            })),
                    )
                    // 完了報告は最初から上に固定されているので、留めない
                    .when(!c.is_completion_report(), |d| {
                        d.child(
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
                    })
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
            .child({
                let body = if links.is_empty() {
                    c.body.clone()
                } else {
                    malakt_core::tasklink::strip(&c.body)
                };
                div().text_sm().when(!body.is_empty(), |d| {
                    d.child(TextView::markdown(("comment-body", ix), body))
                })
            })
            .children(
                links
                    .into_iter()
                    .enumerate()
                    .map(|(n, id)| self.task_card(("task-card", ix * 16 + n), id, cx)),
            )
    }

    /// リンクが指すタスクのカード。状態とタイトルを出し、押すとそのタスクを開く。
    fn task_card(&self, id: impl Into<ElementId>, task_id: &str, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let task = self
            .detail
            .as_ref()
            .and_then(|d| d.linked.iter().find(|t| t.id == task_id));
        let card = row()
            .id(id)
            .gap_2()
            .px_2()
            .py_1p5()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.popover)
            .text_sm();
        let card = match task {
            Some(task) => {
                let open = task.id.clone();
                card.cursor_pointer()
                    .hover(|s| s.border_color(theme.primary))
                    .active(|s| s.bg(theme.accent))
                    .child(status_badge(task.status, cx).flex_none())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::BOLD)
                            .child(task.title.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(self.workspace_name(task.workspace_id.as_deref())),
                    )
                    .child(
                        app_icon("arrow-right")
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.open_panel(&open, window, cx)
                    }))
            }
            None => card
                .text_color(theme.muted_foreground)
                .child(app_icon("link").small())
                .child("リンク先のタスクが見つかりません"),
        };
        div().child(card.test_support())
    }

    /// 詳細の「担当」。1 人ずつ、完了のチェック・頭文字の丸・名前・役（R・A など）を並べる。
    /// 押すと、担当者と役を選ぶところが下に開く。
    fn assignee_property(&self, task: &Todo, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut list = col().gap_1p5();
        if task.assignee_ids.is_empty() {
            list = list.child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("未割り当て"),
            );
        }
        for (i, person_id) in task.assignee_ids.iter().enumerate() {
            let roles = task.roles_of(person_id).unwrap_or_default();
            let done = task.is_done_by(person_id);
            let name = self.person_name(person_id);
            let check = div()
                .id(("p-done", i))
                .size_5()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .cursor_pointer()
                .when(done, |d| {
                    d.bg(theme.success)
                        .border_color(theme.success)
                        .text_color(theme.success_foreground)
                        .child(app_icon("check").size_3p5())
                })
                .when(!done, |d| {
                    d.bg(theme.background)
                        .border_color(theme.muted_foreground.opacity(0.6))
                        .hover(|s| s.border_color(theme.success))
                })
                .active(|s| s.opacity(0.7))
                .tooltip({
                    let text = if done {
                        format!("{name} さんの分を未完了に戻す")
                    } else {
                        format!("{name} さんの分を完了にする")
                    };
                    move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
                    }
                })
                .on_click(cx.listener({
                    let id = person_id.clone();
                    move |this, _: &ClickEvent, _, cx| {
                        // 行を押したことにしない（担当を選ぶところを開かない）
                        cx.stop_propagation();
                        this.toggle_assignee_done(&id, cx)
                    }
                }))
                .test_support();
            let badge = div()
                .id(("p-roles", i))
                .flex_none()
                .px_1p5()
                .py_0p5()
                .rounded_sm()
                .border_1()
                .border_color(theme.border)
                .bg(theme.muted)
                .text_xs()
                .font_weight(FontWeight::BOLD)
                .text_color(theme.muted_foreground)
                .hover(|s| s.bg(theme.accent))
                .tooltip({
                    let text = roles.labels();
                    move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(text.clone()).build(window, cx)
                    }
                })
                .child(roles.iter().map(Raci::code).collect::<Vec<_>>().join("・"));
            list = list.child(
                row()
                    .min_w_0()
                    .gap_2()
                    .child(check)
                    .child(avatar(name, cx))
                    .child(div().min_w_0().text_sm().child(name.to_owned()))
                    .child(badge),
            );
        }
        let value = div()
            .id("p-assignees")
            .mx(px(-8.))
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .hover(|s| s.bg(theme.accent.opacity(0.4)))
            .when(self.assignee_picker, |d| d.bg(theme.accent.opacity(0.4)))
            .child(list)
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.assignee_picker = !this.assignee_picker;
                cx.notify();
            }))
            .test_support();
        let mut people = col().flex_1().min_w_0().gap_1().child(value);
        if self.assignee_picker {
            people = people.child(self.assignee_picker(task, cx));
        }
        people
    }

    /// 担当者を選ぶところ。選んだ人の下に、役（R・A・C・I）を選ぶチップが出る。
    fn assignee_picker(&self, task: &Todo, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut list = col()
            .rounded_md()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background);
        for (i, p) in self.people.iter().enumerate() {
            let row_ix = task.assignee_ids.iter().position(|a| a == &p.id);
            let id = p.id.clone();
            let mut item = col()
                .when(i > 0, |d| d.border_t_1().border_color(theme.border))
                .child(
                    row()
                        .id(("p-as", i))
                        .gap_2()
                        .px_3()
                        .py_2()
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.accent.opacity(0.4)))
                        .active(|s| s.bg(theme.accent))
                        .child(
                            div()
                                .size_5()
                                .flex_none()
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_sm()
                                .border_1()
                                .when(row_ix.is_some(), |d| {
                                    d.bg(theme.primary)
                                        .border_color(theme.primary)
                                        .text_color(theme.primary_foreground)
                                        .child(app_icon("check").size_3p5())
                                })
                                .when(row_ix.is_none(), |d| d.border_color(theme.input)),
                        )
                        .child(avatar(&p.name, cx))
                        .child(div().min_w_0().truncate().text_sm().child(p.name.clone()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.toggle_assignee(&id, cx)
                        }))
                        .test_support(),
                );
            if let Some(row_ix) = row_ix {
                let roles = task.roles_of(&p.id).unwrap_or_default();
                let mut chips = row().flex_wrap().gap_1p5().pt_0p5().pr_3().pb_2().pl_10();
                for role in Raci::ALL {
                    let id = p.id.clone();
                    let on = roles.contains(role);
                    chips = chips.child(
                        row()
                            .id(("p-role", row_ix * 4 + role as usize))
                            .gap_1()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .text_xs()
                            .cursor_pointer()
                            .when(on, |d| {
                                d.bg(theme.primary)
                                    .border_color(theme.primary)
                                    .text_color(theme.primary_foreground)
                            })
                            .when(!on, |d| {
                                d.bg(theme.background)
                                    .border_color(theme.border)
                                    .hover(|s| s.bg(theme.accent))
                            })
                            .active(|s| s.opacity(0.7))
                            .child(div().font_weight(FontWeight::BOLD).child(role.code()))
                            .child(role.label())
                            .tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(role.hint())
                                    .build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.toggle_role(&id, role, cx)
                            }))
                            .test_support(),
                    );
                }
                item = item.child(chips);
            }
            list = list.child(item);
        }
        col().gap_1().child(list).child(
            row().child(
                Button::new("p-add-person")
                    .icon(app_icon("user-plus"))
                    .label("担当者を登録")
                    .ghost()
                    .small()
                    .on_click(cx.listener(|this, _, window, cx| this.open_people(window, cx))),
            ),
        )
    }

    fn property(&self, label: &'static str, cx: &App) -> Div {
        row().gap_2().items_start().child(
            div()
                .w(px(96.))
                .flex_none()
                .pt_1()
                .text_sm()
                .font_weight(FontWeight::BOLD)
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
            // その場で書き直す。文字の大きさも太さも位置も、読むときと同じ。
            // Enter かほかの場所を押すと保存、Esc で取り消し
            div()
                .mx(px(-8.))
                .text_xl()
                .font_weight(FontWeight::BOLD)
                .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                    if this.editing == Some(Editing::Title) {
                        this.save_title(window, cx);
                        cx.notify();
                    }
                }))
                .child(
                    Input::new(&self.inputs.title)
                        .id("title-input")
                        .aria_label("タイトル")
                        .text_xl()
                        .bg(theme.background)
                        .border_color(theme.ring)
                        .px_2(),
                )
        } else {
            div().child(
                div()
                    .id("detail-title")
                    .mx(px(-8.))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(gpui_kit::transparent_black())
                    .text_xl()
                    .font_weight(FontWeight::BOLD)
                    .when(!open, |d| d.text_color(theme.muted_foreground))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme.muted))
                    .child(task.title.clone())
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.start_editing(Editing::Title, window, cx)
                    }))
                    .test_support(),
            )
        };

        // AI が整えている間の印と、整えられなかった理由
        let ai_state = {
            let session = self.ai_session.read(cx);
            if session.arranging.contains(&task.id) {
                Some(
                    row()
                        .id("ai-working")
                        .gap_2()
                        .items_center()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(Spinner::new().small())
                        .child("AI が整えています")
                        .test_support()
                        .into_any_element(),
                )
            } else {
                session.arrange_notes.get(&task.id).map(|note| {
                    row()
                        .id("ai-note")
                        .gap_2()
                        .items_start()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(app_icon("sparkles").small().flex_none().mt_1())
                        .child(div().min_w_0().child(note.clone()))
                        .test_support()
                        .into_any_element()
                })
            }
        };

        let status_actions = {
            let id = task.id.clone();
            let mut actions = row().gap_2().flex_wrap();
            let set = |label: &'static str, key: &'static str, status: Status, primary: bool| {
                let id = id.clone();
                let button = Button::new(key)
                    .icon(app_icon(status_icon(status)))
                    .label(label);
                let button = if primary {
                    button.primary()
                } else {
                    button.framed(cx)
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
                                .child("Ctrl / ⌘ + Enter で保存、Esc でキャンセル"),
                        ),
                )
        } else if task.description.is_empty() {
            div().child(
                div()
                    .id("detail-description")
                    .cursor_pointer()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("説明を追加…")
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.start_editing(Editing::Description, window, cx)
                    })),
            )
        } else {
            // Markdown で見せる。文字を選べるので、押して編集にはしない
            col()
                .gap_1()
                .child(
                    div()
                        .id("detail-description")
                        .text_sm()
                        .child(TextView::new(&self.description_view)),
                )
                .child(
                    row()
                        .gap_1()
                        .child(
                            Button::new("desc-edit")
                                .label("編集")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.start_editing(Editing::Description, window, cx)
                                })),
                        )
                        .child(
                            Button::new("desc-quote")
                                .label("選択範囲にコメント")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.quote_selection(window, cx)
                                })),
                        ),
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
        let status_chips = {
            let id = task.id.clone();
            self.property("状態", cx).child(
                div().flex().child(
                    status_badge(task.status, cx)
                        .id("p-status")
                        .cursor_pointer()
                        .hover(|s| s.border_color(theme.primary))
                        .active(|s| s.bg(theme.accent))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.ask_status(&id, window, cx)
                        })),
                ),
            )
        };
        let current_priority = Prio::of(task.priority);
        let prio_chips = self.value_menu(
            "p-prio",
            priority_label(current_priority, cx),
            Self::priority_options(current_priority),
            |this, value, cx| this.edit_task(cx, |d| d.priority = value),
            cx,
        );

        let people = self.assignee_property(task, cx);

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
            let link_id = task.id.clone();
            let shared = !task.linked_workspace_ids.is_empty();
            let mut places =
                col().gap_1().child(
                    row()
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .text_sm()
                                .child(self.workspace_name(task.workspace_id.as_deref())),
                        )
                        .child(Button::new("move-task").label("移動").xsmall().on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.modal = Modal::MoveTasks(vec![id.clone()]);
                                cx.notify();
                            }),
                        ))
                        .when_some(task.workspace_id.clone().filter(|_| shared), |d, home| {
                            let task_id = task.id.clone();
                            d.child(
                                Button::new("unlink-home")
                                    .label("外す")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.unlink_task_from(&task_id, &home, cx)
                                    })),
                            )
                        }),
                );
            for (ix, linked) in task.linked_workspace_ids.iter().enumerate() {
                let (task_id, linked_id) = (task.id.clone(), linked.clone());
                places = places.child(
                    row()
                        .gap_2()
                        .items_center()
                        .child(div().text_sm().child(self.workspace_name(Some(linked))))
                        .child(
                            Button::new(("unlink", ix))
                                .label("外す")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.unlink_task_from(&task_id, &linked_id, cx)
                                })),
                        ),
                );
            }
            // どこにも入っていないタスクは「移動」で入れる
            places.when(task.workspace_id.is_some(), |d| {
                d.child(
                    row().child(
                        Button::new("link-add")
                            .label("ほかのワークスペースと同期")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.modal = Modal::LinkTask(link_id.clone());
                                cx.notify();
                            })),
                    ),
                )
            })
        };

        // 返信はルートの下にまとめる。完了報告は上に固定して出すので、ここには出さない
        let unresolved = detail
            .comments
            .iter()
            .filter(|c| c.is_thread_root() && c.resolved_at.is_none())
            .count();
        let mut comments = col().gap_2();
        for (ix, c) in detail.comments.iter().enumerate() {
            if c.reply_to.is_some() || c.is_completion_report() {
                continue;
            }
            if !c.is_thread_root() {
                comments = comments.child(self.render_comment(ix, c, now, cx));
                continue;
            }
            let resolved = c.resolved_at.is_some();
            let open = !resolved || self.expanded_threads.contains(&c.id);
            let (toggle_id, resolve_id, reply_id) = (c.id.clone(), c.id.clone(), c.id.clone());
            let mut thread =
                col()
                    .gap_1()
                    .p_2()
                    .border_1()
                    .border_color(theme.border)
                    .rounded_md()
                    .child(
                        row()
                            .gap_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(if resolved {
                                "レビュー（解決済み）"
                            } else {
                                "レビュー"
                            })
                            .when(!open, |d| {
                                d.child(
                                    div().flex_1().min_w_0().truncate().child(
                                        c.body.lines().next().unwrap_or_default().to_owned(),
                                    ),
                                )
                            })
                            .when(open, |d| d.child(div().flex_1()))
                            .when(resolved, |d| {
                                d.child(
                                    Button::new(("thread-toggle", ix))
                                        .label(if open { "折りたたむ" } else { "開く" })
                                        .ghost()
                                        .xsmall()
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.toggle_thread(&toggle_id, cx)
                                        })),
                                )
                            })
                            .child(
                                Button::new(("thread-resolve", ix))
                                    .label(if resolved {
                                        "再開する"
                                    } else {
                                        "解決済みにする"
                                    })
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.set_thread_resolved(&resolve_id, !resolved, cx)
                                    })),
                            ),
                    );
            if open {
                if let Some(anchor) = &c.anchor {
                    // 引用。説明が書き換わって見つからなくなったら、そう伝える
                    let found = task.description.contains(anchor.exact.as_str());
                    thread = thread.child(
                        col()
                            .pl_2()
                            .border_l_2()
                            .border_color(theme.border)
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(anchor.exact.clone())
                            .when(!found, |d| {
                                d.child(
                                    div()
                                        .text_xs()
                                        .child("（説明のこの部分は書き換えられました）"),
                                )
                            }),
                    );
                }
                thread = thread.child(self.render_comment(ix, c, now, cx));
                let mut replies = col().gap_1().pl_4();
                for (reply_ix, reply) in detail.comments.iter().enumerate() {
                    if reply.reply_to.as_deref() == Some(c.id.as_str()) {
                        replies = replies.child(self.render_comment(reply_ix, reply, now, cx));
                    }
                }
                thread = thread.child(replies).child(
                    row().child(
                        Button::new(("thread-reply", ix))
                            .label("返信")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_reply(&reply_id, window, cx)
                            })),
                    ),
                );
            }
            comments = comments.child(thread);
        }
        let report_items: Vec<Div> = detail
            .comments
            .iter()
            .enumerate()
            .filter(|(_, c)| c.is_completion_report())
            .map(|(ix, c)| self.render_comment(ix, c, now, cx))
            .collect();

        // コメント欄の上に、いま何を書いているかを出す
        let composing: Option<SharedString> = match (&self.comment_mode, &self.pending_anchor) {
            (CommentMode::Edit(_), _) => Some("コメントを書き直しています".into()),
            (CommentMode::Reply(_), _) => Some("スレッドに返信します".into()),
            (CommentMode::New, Some(anchor)) => {
                Some(format!("「{}」にレビューコメントを付けます", anchor.exact).into())
            }
            (CommentMode::New, None) => None,
        };
        let is_new = self.comment_mode == CommentMode::New;
        let mut mention_row = row().gap_1().flex_wrap();
        for (i, person) in self.mention_candidates(cx).into_iter().enumerate() {
            mention_row = mention_row.child(
                chip(("mention", i), format!("@{}", person.name), false, cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.complete_mention(&person, window, cx)
                    }))
                    .test_support(),
            );
        }

        let delete_id = task.id.clone();
        let bar =
            row()
                .flex_none()
                .h_12()
                .px_4()
                .gap_2()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .font_weight(FontWeight::BOLD)
                        .child("タスクの詳細"),
                )
                .child({
                    let link_task = task.id.clone();
                    Button::new("copy-link")
                        .icon(app_icon("link"))
                        .tooltip("リンクをコピー")
                        .framed(cx)
                        .small()
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.copy_task_link(&link_task, cx)),
                        )
                })
                .child({
                    let ticket_task = task.id.clone();
                    Button::new("ticket-open")
                        .icon(app_icon("bookmark"))
                        .tooltip("完了チケット")
                        .framed(cx)
                        .small()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_ticket_dialog(&ticket_task, cx)
                        }))
                })
                .child(
                    Button::new("close-panel")
                        .icon(app_icon("x"))
                        .tooltip("閉じる")
                        .framed(cx)
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.close_panel(cx))),
                );
        let content =
            col()
                .p_4()
                .gap_4()
                .child(title)
                .children(ai_state)
                .child(
                    row()
                        .gap_3()
                        .flex_wrap()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(status_badge(task.status, cx))
                        .child(
                            Button::new("history")
                                .icon(app_icon("rotate-ccw-clock"))
                                .label("変更履歴")
                                .framed(cx)
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.modal = Modal::History;
                                    cx.notify();
                                })),
                        )
                        .child(format!(
                            "{}　作成 {}　更新 {}",
                            self.workspace_name(task.workspace_id.as_deref()),
                            view::format_datetime(task.created_at),
                            view::format_relative(task.updated_at, now)
                        )),
                )
                .child(
                    card(cx)
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
                        .child(self.property("ワークスペース", cx).child(workspace))
                        .when_some(task.completed_at, |d, at| {
                            d.child(
                                self.property("完了日時", cx)
                                    .child(div().text_sm().child(view::format_datetime(at))),
                            )
                        }),
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
                            .children(pinned.iter().enumerate().map(|(i, c)| {
                                div()
                                    .text_sm()
                                    .child(TextView::markdown(("pinned-body", i), c.body.clone()))
                            })),
                    )
                })
                .when(!report_items.is_empty(), |d| {
                    d.child(
                        col()
                            .gap_1()
                            .p_2()
                            .rounded_md()
                            .border_1()
                            .border_color(theme.border)
                            .child(section_title("完了報告"))
                            .children(report_items),
                    )
                })
                .child(card(cx).child(section_title("説明")).child(description))
                .child(
                    card(cx)
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
                .child(card(cx).child(self.render_attachments(detail, cx)))
                .child(
                    card(cx)
                        .child(row().gap_2().child(section_title("コメント")).when(
                            unresolved > 0,
                            |d| {
                                d.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("（未解決のレビュー {unresolved} 件）")),
                                )
                            },
                        ))
                        .child(comments)
                        .when_some(composing, |d, text| {
                            d.child(
                                row()
                                    .gap_2()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(div().flex_1().min_w_0().truncate().child(text))
                                    .child(
                                        Button::new("comment-cancel")
                                            .label("キャンセル")
                                            .ghost()
                                            .xsmall()
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.cancel_comment_mode(window, cx)
                                            })),
                                    ),
                            )
                        })
                        .child(Textarea::new(&self.inputs.comment).h(px(88.)))
                        .child(mention_row)
                        .child(
                            row()
                                .gap_2()
                                .child(
                                    Button::new("comment-submit")
                                        .label(match self.comment_mode {
                                            CommentMode::Edit(_) => "保存",
                                            CommentMode::Reply(_) => "返信する",
                                            CommentMode::New => "コメントする",
                                        })
                                        .primary()
                                        .small()
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.submit_comment(window, cx)
                                        })),
                                )
                                .when(is_new && self.pending_anchor.is_none(), |d| {
                                    d.child(
                                        Button::new("comment-review")
                                            .label("レビューとしてコメント")
                                            .small()
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.submit_comment_as(true, window, cx)
                                            })),
                                    )
                                })
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child("Ctrl / ⌘ + Enter で送信"),
                                ),
                        ),
                )
                .child(
                    card(cx).child(section_title("削除")).child(
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
                .top(HEADER_HEIGHT)
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
                .flex()
                .flex_col()
                .child(bar)
                .child(
                    div()
                        .id("detail-panel")
                        .flex_1()
                        .min_h_0()
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
                            .child(div().flex_1().text_lg().child(title.into()))
                            .child(
                                Button::new("dialog-close")
                                    .label("閉じる")
                                    .ghost()
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.modal = Modal::None;
                                        this.issued_ticket = None;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .child(body),
            )
    }

    fn workspace_options(&self, cx: &mut Context<Self>) -> Div {
        let mut list = col().gap_1().child(
            chip("move-unfiled", "未所属", false, cx)
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
                    this.issued_ticket = None;
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
                        "「{}」にサブワークスペースを追加",
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
            Modal::WorkspaceIcon(id) => {
                let theme = cx.theme().clone();
                let current = self.workspaces.iter().find(|w| &w.id == id);
                let icon = current.and_then(|w| w.icon.clone());
                let color = current.and_then(|w| w.color.clone());
                let chosen = icon
                    .clone()
                    .unwrap_or_else(|| crate::state::DEFAULT_WORKSPACE_ICON.to_owned());
                let mut colors = row().gap_1().flex_wrap();
                let all = std::iter::once(None).chain(WORKSPACE_COLORS.iter().copied().map(Some));
                for (ix, key) in all.enumerate() {
                    let (ws, icon) = (id.clone(), icon.clone());
                    let selected = color.as_deref() == key;
                    colors = colors.child(
                        div()
                            .id(("ws-color", ix))
                            .size_8()
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .cursor_pointer()
                            .border_1()
                            .border_color(if selected {
                                theme.foreground
                            } else {
                                theme.transparent
                            })
                            .hover(|s| s.bg(theme.secondary_hover))
                            .child(
                                div()
                                    .size_4()
                                    .rounded_full()
                                    .bg(workspace_color(key).unwrap_or(theme.foreground)),
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_workspace_icon(&ws, icon.as_deref(), key, cx)
                            }))
                            .test_support(),
                    );
                }
                let query = self.inputs.icon_search.read(cx).value().to_string();
                let found =
                    crate::icons::search(&query, &crate::state::WORKSPACE_ICONS, ICONS_SHOWN);
                let none_found = found.is_empty();
                let mut icons = row().gap_1().flex_wrap();
                for (ix, name) in found.into_iter().enumerate() {
                    let (ws, color) = (id.clone(), color.clone());
                    let selected = chosen == name;
                    icons = icons.child(
                        div()
                            .id(("ws-icon", ix))
                            .size(px(40.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_md()
                            .cursor_pointer()
                            .border_1()
                            .border_color(if selected {
                                theme.foreground
                            } else {
                                theme.transparent
                            })
                            .hover(|s| s.bg(theme.secondary_hover))
                            .child(
                                workspace_icon(Some(name), color.as_deref()).size(PICKER_ICON_SIZE),
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_workspace_icon(&ws, Some(name), color.as_deref(), cx)
                            }))
                            .test_support(),
                    );
                }
                let body = col()
                    .gap_3()
                    .child(colors)
                    .child(
                        Input::new(&self.inputs.icon_search)
                            .id("icon-search")
                            .aria_label("アイコンを検索")
                            .prefix(app_icon("search").small())
                            .cleanable(true),
                    )
                    .child(
                        div()
                            .id("icon-list")
                            .h(px(320.))
                            .overflow_y_scroll()
                            .child(icons)
                            .when(none_found, |d| {
                                d.child(muted("見つかりません。".to_owned()))
                            }),
                    )
                    .when_some(self.error.clone(), |d, error| {
                        d.child(div().text_sm().text_color(theme.danger).child(error))
                    });
                let title = format!("「{}」のアイコン", self.workspace_name(Some(id)));
                self.dialog(title, body, cx)
            }
            Modal::DeleteWorkspace(id) => {
                let name = self.workspace_name(Some(id));
                let body = col()
                    .gap_3()
                    .child(muted(format!(
                        "「{name}」と、そのサブワークスペースとタスクをすべて削除します。元に戻せません。"
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
                // ほかのワークスペースにも出しているタスクは、ここから外すだけにもできる
                let here = self.current_workspace();
                let shared = here.is_some()
                    && self
                        .tasks
                        .iter()
                        .chain(self.detail.as_ref().map(|d| &d.task))
                        .any(|t| ids.contains(&t.id) && !t.linked_workspace_ids.is_empty());
                let body = col()
                    .gap_3()
                    .child(muted(format!(
                        "{} 件のタスクを削除します。元に戻せません。",
                        ids.len()
                    )))
                    .when(shared, |d| {
                        d.child(muted(
                            "ほかのワークスペースと同期しているタスクがあります。削除すると、すべてのワークスペースから消えます。"
                                .to_owned(),
                        ))
                    })
                    .child(
                        row().gap_2().justify_end().child(cancel(cx)).when(shared, |d| {
                            d.child(
                                Button::new("unlink-here")
                                    .label("このワークスペースから外す")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.unlink_selected_here(cx)),
                                    ),
                            )
                        }).child(
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
            Modal::MoveTasks(_) => {
                let body = col().gap_3().child(self.workspace_options(cx));
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
                let body = col().gap_3().child(list).child(
                    row()
                        .gap_2()
                        .child(
                            Input::new(&self.inputs.name)
                                .id("person-input")
                                .aria_label("名前")
                                .flex_1(),
                        )
                        .child(
                            Button::new("person-submit")
                                .label("追加")
                                .primary()
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.submit_name(window, cx)),
                                ),
                        ),
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
                            "link" if !value.is_empty() => {
                                self.workspace_name(Some(value)).to_string()
                            }
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
            Modal::Ticket(id) => {
                let theme = cx.theme().clone();
                let task = self
                    .detail
                    .as_ref()
                    .map(|d| &d.task)
                    .filter(|t| &t.id == id);
                // 自分（タスク全体を完了にする）と、ほかの担当者（その人の分だけ）
                let mut who = row().gap_1().flex_wrap();
                let mut options: Vec<(Option<malakt_core::Id>, String)> = Vec::new();
                match self.people.iter().find(|p| p.is_self) {
                    Some(me) => options.push((Some(me.id.clone()), format!("{}（自分）", me.name))),
                    None => options.push((None, "自分".to_owned())),
                }
                for p in &self.people {
                    if !p.is_self && task.is_some_and(|t| t.roles_of(&p.id).is_some()) {
                        options.push((Some(p.id.clone()), p.name.clone()));
                    }
                }
                for (ix, (person, name)) in options.into_iter().enumerate() {
                    let active = self.ticket_person == person;
                    who = who.child(
                        chip(("ticket-who", ix), name, active, cx)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.ticket_person = person.clone();
                                this.issued_ticket = None;
                                cx.notify();
                            }))
                            .test_support(),
                    );
                }
                let active = self.active_tickets(id, cx);
                let body = col()
                    .gap_3()
                    .child(
                        col()
                            .gap_1()
                            .child(div().text_sm().child("完了にする担当者"))
                            .child(who),
                    )
                    .when_some(self.issued_ticket.as_ref(), |d, ticket| {
                        d.child(
                            col()
                                .gap_1()
                                .p_2()
                                .rounded_md()
                                .bg(theme.secondary)
                                .child(
                                    div()
                                        .text_sm()
                                        .child("発行しました。チケットは今だけ表示されます。"),
                                )
                                .child(
                                    div()
                                        .id("ticket-secret")
                                        .text_sm()
                                        .font_family(theme.mono_font_family.clone())
                                        .child(ticket.secret.clone()),
                                )
                                .child(muted(format!(
                                    "使い方: malakt-mahoz redeem <チケット> --message \"完了報告\"　（{} まで有効）",
                                    view::format_datetime(ticket.expires_at)
                                )))
                                .child(
                                    row().child(
                                        Button::new("ticket-copy")
                                            .label("コピー")
                                            .small()
                                            .on_click(cx.listener(|this, _, _, cx| this.copy_ticket(cx))),
                                    ),
                                ),
                        )
                    })
                    .when_some(self.error.clone(), |d, error| {
                        d.child(div().text_sm().text_color(theme.danger).child(error))
                    })
                    .child(
                        row()
                            .gap_2()
                            .items_center()
                            .child(muted(format!("未使用のチケット: {active} 枚")))
                            .child(div().flex_1())
                            .when(active > 0, |d| {
                                d.child(
                                    Button::new("ticket-revoke")
                                        .label("すべて取り消す")
                                        .small()
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.revoke_tickets(cx)),
                                        ),
                                )
                            })
                            .child(
                                Button::new("ticket-issue")
                                    .label("発行")
                                    .primary()
                                    .on_click(cx.listener(|this, _, _, cx| this.issue_ticket(cx))),
                            ),
                    );
                self.dialog("完了チケット", body, cx)
            }
            Modal::Notices => {
                let theme = cx.theme().clone();
                let now = malakt_core::now_ms();
                let mut list = col().gap_1();
                for (ix, n) in self.notices.iter().enumerate() {
                    let id = n.id.clone();
                    list = list.child(
                        col()
                            .id(("notice", ix))
                            .gap_0p5()
                            .p_2()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.secondary_hover))
                            .when(n.is_unread(), |d| d.bg(theme.secondary))
                            .child(
                                div()
                                    .text_sm()
                                    .when(n.is_unread(), |d| d)
                                    .child(n.headline()),
                            )
                            .when(!n.excerpt.is_empty(), |d| {
                                d.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .truncate()
                                        .child(n.excerpt.clone()),
                                )
                            })
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(view::format_relative(n.created_at, now)),
                            )
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.open_notice(&id, window, cx)
                            }))
                            .test_support(),
                    );
                }
                let body =
                    col()
                        .gap_3()
                        .min_h_0()
                        .when(self.notices.is_empty(), |d| {
                            d.child(muted("通知はまだありません。".to_owned()))
                        })
                        .child(
                            div()
                                .id("notices-scroll")
                                .flex_1()
                                .min_h_0()
                                .max_h(px(420.))
                                .overflow_y_scroll()
                                .child(list),
                        )
                        .when(self.unread_notices > 0, |d| {
                            d.child(
                                row().justify_end().child(
                                    Button::new("notices-read-all")
                                        .label("すべて既読にする")
                                        .small()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.mark_all_notices_read(cx)
                                        })),
                                ),
                            )
                        });
                self.dialog("通知", body, cx)
            }
            Modal::BulkDue => {
                let body =
                    col()
                        .gap_3()
                        .child(
                            Input::new(&self.inputs.bulk_due)
                                .id("bulk-due-input")
                                .aria_label("期限"),
                        )
                        .when_some(self.error.clone(), |d, error| {
                            d.child(div().text_sm().text_color(cx.theme().danger).child(error))
                        })
                        .child(
                            row()
                                .gap_2()
                                .justify_end()
                                .child(cancel(cx))
                                .child(
                                    Button::new("bulk-due-clear")
                                        .label("期限なしにする")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.submit_bulk_due(true, cx)
                                        })),
                                )
                                .child(
                                    Button::new("bulk-due-submit")
                                        .label("変更する")
                                        .primary()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.submit_bulk_due(false, cx)
                                        })),
                                ),
                        );
                self.dialog("期限をまとめて変更", body, cx)
            }
            Modal::BulkAssignees => {
                let mut modes = row().gap_1();
                for (ix, mode) in ListMode::ALL.into_iter().enumerate() {
                    modes = modes.child(
                        chip(("bulk-mode", ix), mode.label(), self.bulk_mode == mode, cx)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.bulk_mode = mode;
                                cx.notify();
                            }))
                            .test_support(),
                    );
                }
                let mut people = row().gap_1().flex_wrap();
                for (ix, p) in self.people.iter().enumerate() {
                    let pid = p.id.clone();
                    people = people.child(
                        chip(
                            ("bulk-person", ix),
                            p.name.clone(),
                            self.bulk_people.contains(&p.id),
                            cx,
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.toggle_bulk_person(&pid, cx)
                        }))
                        .test_support(),
                    );
                }
                let body = col()
                    .gap_3()
                    .child(modes)
                    .child(people)
                    .when_some(self.error.clone(), |d, error| {
                        d.child(div().text_sm().text_color(cx.theme().danger).child(error))
                    })
                    .child(
                        row().gap_2().justify_end().child(cancel(cx)).child(
                            Button::new("bulk-assignees-submit")
                                .label("変更する")
                                .primary()
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.submit_bulk_assignees(cx)),
                                ),
                        ),
                    );
                self.dialog("担当者をまとめて変更", body, cx)
            }
            Modal::BulkCategories => {
                let mut modes = row().gap_1();
                for (ix, (text, add)) in [("追加", true), ("外す", false)].into_iter().enumerate()
                {
                    modes = modes.child(
                        chip(
                            ("bulk-cat-mode", ix),
                            text,
                            self.bulk_category_add == add,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.bulk_category_add = add;
                            cx.notify();
                        }))
                        .test_support(),
                    );
                }
                let body = col()
                    .gap_3()
                    .child(modes)
                    .child(
                        Input::new(&self.inputs.bulk_categories)
                            .id("bulk-categories-input")
                            .aria_label("カテゴリ"),
                    )
                    .when_some(self.error.clone(), |d, error| {
                        d.child(div().text_sm().text_color(cx.theme().danger).child(error))
                    })
                    .child(
                        row().gap_2().justify_end().child(cancel(cx)).child(
                            Button::new("bulk-categories-submit")
                                .label("変更する")
                                .primary()
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.submit_bulk_categories(cx)),
                                ),
                        ),
                    );
                self.dialog("カテゴリをまとめて変更", body, cx)
            }
            Modal::NewTask => {
                let theme = cx.theme().clone();
                let label = |text: &'static str| div().text_sm().child(text);
                let mut priority = row().gap_1().flex_wrap();
                for (ix, p) in Prio::ALL.into_iter().enumerate() {
                    priority = priority.child(
                        chip(
                            ("nt-prio", ix),
                            p.label(),
                            Prio::of(self.new_task.priority) == p,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.set_new_task_priority(p.value(), cx)
                        }))
                        .test_support(),
                    );
                }
                let mut assignees = row().gap_1().flex_wrap();
                for (ix, p) in self.people.iter().enumerate() {
                    let pid = p.id.clone();
                    assignees = assignees.child(
                        chip(
                            ("nt-as", ix),
                            p.name.clone(),
                            self.new_task.assignee_ids.contains(&p.id),
                            cx,
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.toggle_new_task_assignee(&pid, cx)
                        }))
                        .test_support(),
                    );
                }
                let field = |text: &'static str, input: Input| {
                    col().gap_1().child(label(text)).child(input)
                };
                let form = col()
                    .gap_3()
                    .child(field(
                        "タイトル",
                        Input::new(&self.inputs.nt_title)
                            .id("nt-title")
                            .aria_label("タイトル"),
                    ))
                    .child(
                        col()
                            .gap_1()
                            .child(label("説明"))
                            .child(Textarea::new(&self.inputs.nt_description).h(px(96.))),
                    )
                    .child(col().gap_1().child(label("優先度")).child(priority))
                    .when(!self.people.is_empty(), |d| {
                        d.child(col().gap_1().child(label("担当者")).child(assignees))
                    })
                    .child(field(
                        "開始日",
                        Input::new(&self.inputs.nt_start)
                            .id("nt-start")
                            .aria_label("開始日"),
                    ))
                    .child(field(
                        "期限",
                        Input::new(&self.inputs.nt_due)
                            .id("nt-due")
                            .aria_label("期限"),
                    ))
                    .child(field(
                        "カテゴリ",
                        Input::new(&self.inputs.nt_categories)
                            .id("nt-categories")
                            .aria_label("カテゴリ"),
                    ));
                let place = self.workspace_name(self.current_workspace());
                let body = col()
                    .gap_3()
                    .min_h_0()
                    .child(muted(format!("「{place}」")))
                    .child(
                        div()
                            .id("new-task-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .child(form),
                    )
                    .when_some(self.error.clone(), |d, error| {
                        d.child(div().text_sm().text_color(theme.danger).child(error))
                    })
                    .child(
                        row().gap_2().justify_end().child(cancel(cx)).child(
                            Button::new("new-task-submit")
                                .label("作成")
                                .primary()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.submit_new_task(window, cx)
                                })),
                        ),
                    );
                self.dialog("新しいタスク", body, cx)
            }
            Modal::LinkTask(id) => {
                let task = self
                    .detail
                    .as_ref()
                    .map(|d| &d.task)
                    .filter(|t| &t.id == id);
                // 基準日が同じワークスペースを先に出す（同じ日の催しに出すことが多い）
                let date_of = |ws: Option<&str>| {
                    self.workspaces
                        .iter()
                        .find(|w| Some(w.id.as_str()) == ws)
                        .and_then(|w| w.date.clone())
                };
                let home_date = date_of(task.and_then(|t| t.workspace_id.as_deref()));
                let mut same = col().gap_1();
                let mut others = col().gap_1();
                let (mut any_same, mut any_other) = (false, false);
                for (ix, ws) in self.workspaces.iter().enumerate() {
                    if task.is_none_or(|t| t.is_in(&ws.id)) {
                        continue;
                    }
                    let target = ws.id.clone();
                    let item = chip(("link-to", ix), ws.name.clone(), false, cx)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.link_task_to(&target, cx)
                        }))
                        .test_support();
                    if home_date.is_some() && ws.date == home_date {
                        any_same = true;
                        same = same.child(item);
                    } else {
                        any_other = true;
                        others = others.child(item);
                    }
                }
                let body = col()
                    .gap_3()
                    .when(!any_same && !any_other, |d| {
                        d.child(muted("同期できるワークスペースがありません。".to_owned()))
                    })
                    .child(
                        div()
                            .id("link-options")
                            .max_h(px(320.))
                            .overflow_y_scroll()
                            .child(
                                col()
                                    .gap_2()
                                    .when(any_same, |d| {
                                        d.child(muted("基準日が同じ".to_owned())).child(same)
                                    })
                                    .when(any_same && any_other, |d| {
                                        d.child(muted("その他".to_owned()))
                                    })
                                    .child(others),
                            ),
                    );
                self.dialog("ほかのワークスペースと同期", body, cx)
            }
            Modal::WorkspaceSettings(id) => {
                let theme = cx.theme().clone();
                let draft = &self.defaults_draft;
                let label = |text: &'static str| div().text_sm().child(text);
                let mut assignees = row().gap_1().flex_wrap();
                for (ix, p) in self.people.iter().enumerate() {
                    let pid = p.id.clone();
                    assignees = assignees.child(
                        chip(
                            ("def-as", ix),
                            p.name.clone(),
                            draft.assignee_ids.contains(&p.id),
                            cx,
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.toggle_default_assignee(&pid, cx)
                        }))
                        .test_support(),
                    );
                }
                if self.people.is_empty() {
                    assignees = assignees.child(muted("担当者はまだいません。".to_owned()));
                }
                let due_kinds = [
                    ("def-due-none", "なし", DueRule::None),
                    (
                        "def-due-relative",
                        "基準日からの日数",
                        DueRule::Relative { days: 0 },
                    ),
                    (
                        "def-due-fixed",
                        "日付を指定",
                        DueRule::Fixed {
                            date: String::new(),
                        },
                    ),
                ];
                let mut due = row().gap_1().flex_wrap();
                for (chip_id, text, rule) in due_kinds {
                    let active =
                        std::mem::discriminant(&draft.due) == std::mem::discriminant(&rule);
                    due = due.child(
                        chip(chip_id, text, active, cx)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_default_due(rule.clone(), cx)
                            }))
                            .test_support(),
                    );
                }
                let due_detail = match &draft.due {
                    DueRule::None => col(),
                    DueRule::Relative { .. } => col()
                        .gap_1()
                        .child(muted("基準日からの日数と時刻".to_owned()))
                        .child(
                            Input::new(&self.inputs.def_days)
                                .id("def-days")
                                .aria_label("日数"),
                        )
                        .child(
                            Input::new(&self.inputs.def_time)
                                .id("def-time")
                                .aria_label("時刻"),
                        ),
                    DueRule::Fixed { .. } => col()
                        .gap_1()
                        .child(
                            Input::new(&self.inputs.def_date)
                                .id("def-date")
                                .aria_label("日付"),
                        )
                        .child(
                            Input::new(&self.inputs.def_time)
                                .id("def-time")
                                .aria_label("時刻"),
                        ),
                };
                let mut priority = row().gap_1().flex_wrap();
                for (ix, (text, value)) in [
                    ("指定しない", None),
                    ("高", Some(1u8)),
                    ("中", Some(5)),
                    ("低", Some(9)),
                ]
                .into_iter()
                .enumerate()
                {
                    priority = priority.child(
                        chip(("def-prio", ix), text, draft.priority == value, cx)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.set_default_priority(value, cx)
                            }))
                            .test_support(),
                    );
                }
                let mut links = row().gap_1().flex_wrap();
                let mut any_link = false;
                for (ix, w) in self.workspaces.iter().enumerate() {
                    if &w.id == id {
                        continue;
                    }
                    any_link = true;
                    let wid = w.id.clone();
                    links = links.child(
                        chip(
                            ("def-link", ix),
                            w.name.clone(),
                            draft.link_workspace_ids.contains(&w.id),
                            cx,
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.toggle_default_link(&wid, cx)
                        }))
                        .test_support(),
                    );
                }
                let form = col()
                    .gap_3()
                    .child(
                        col().gap_1().child(label("基準日")).child(
                            Input::new(&self.inputs.ws_date)
                                .id("ws-date")
                                .aria_label("基準日"),
                        ),
                    )
                    .child(div().h(px(1.)).bg(theme.border))
                    .child(col().gap_1().child(label("担当者")).child(assignees))
                    .child(
                        col()
                            .gap_1()
                            .child(label("期限"))
                            .child(due)
                            .child(due_detail),
                    )
                    .child(col().gap_1().child(label("優先度")).child(priority))
                    .child(
                        col().gap_1().child(label("カテゴリ")).child(
                            Input::new(&self.inputs.def_categories)
                                .id("def-categories")
                                .aria_label("カテゴリ"),
                        ),
                    )
                    .child(
                        col()
                            .gap_1()
                            .child(label("説明の雛形"))
                            .child(Textarea::new(&self.inputs.def_description).h(px(96.))),
                    )
                    .when(any_link, |d| {
                        d.child(
                            col()
                                .gap_1()
                                .child(label("同期するワークスペース"))
                                .child(links),
                        )
                    });
                let body = col()
                    .gap_3()
                    .min_h_0()
                    .child(
                        div()
                            .id("ws-settings-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .child(form),
                    )
                    .when_some(self.error.clone(), |d, error| {
                        d.child(div().text_sm().text_color(theme.danger).child(error))
                    })
                    .child(
                        row().gap_2().justify_end().child(cancel(cx)).child(
                            Button::new("ws-settings-save")
                                .label("保存")
                                .primary()
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.save_workspace_settings(cx)),
                                ),
                        ),
                    );
                self.dialog(
                    format!("「{}」の設定", self.workspace_name(Some(id))),
                    body,
                    cx,
                )
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
            .child(div().text_xl().child("ロックされています"))
            .child(
                Button::new("unlock")
                    .icon(app_icon("fingerprint-pattern"))
                    .label("パスキーで解除")
                    .primary()
                    .loading(self.verifying(cx))
                    .on_click(cx.listener(|this, _, window, cx| this.request_unlock(window, cx))),
            )
            .when_some(self.error.clone(), |d, error| {
                d.child(div().text_sm().text_color(theme.danger).child(error))
            })
    }
}

impl Render for MainView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let window_width = window.viewport_size().width;
        // 右に AI チャットを置いた分、タスクの画面は狭くなる
        let ai = self.ai_docked(cx).then(|| self.ai_pane.clone()).flatten();
        let ai_width = ai.as_ref().and_then(|_| {
            let s = self.ai_session.read(cx);
            let shell = s.shell_place == ShellPlace::Docked;
            docked_width(window_width, shell, (s.chat_width, s.shell_width))
        });
        let width = window_width - ai_width.unwrap_or(px(0.));
        let unlocked = self.unlocked(cx);
        // 狭いウィンドウでは左のメニューをたたみ、詳細は全面のシートにする
        let wide = width >= TASKS_MIN;
        let sidebar = if wide {
            !self.sidebar_hidden
        } else {
            self.sidebar_open && self.panel.is_none()
        };
        if !unlocked {
            return col()
                .size_full()
                .bg(theme.background)
                .text_color(theme.foreground)
                .child(self.render_header(false, wide, window, cx))
                .child(div().flex_1().min_h_0().child(self.render_lock_screen(cx)))
                .into_any_element();
        }
        let main_width = if wide && sidebar {
            width - px(self.sidebar_width)
        } else {
            width
        };
        // 表は、タイトルの列に読める幅が残るときだけ
        let table = main_width >= px(880.);
        let sheet = width < px(1000.);

        let main = if !wide && sidebar {
            // 狭い画面でメニューを開いているあいだは、メニューだけを見せる
            self.render_sidebar(wide, cx)
                .w_full()
                .flex_1()
                .min_h_0()
                .into_any_element()
        } else {
            match self.nav {
                Nav::Home => self.render_home(table, cx).into_any_element(),
                _ => self.render_list(table, cx).into_any_element(),
            }
        };

        // 詳細は右から入り、ダイアログはふわっと出る
        let panel = self.render_panel(sheet, cx).map(|panel| {
            if !self.motion {
                return panel.into_any_element();
            }
            panel
                .with_animation("panel-in", appear(180), |panel, t| {
                    panel.right(px(-48.) * (1. - t)).opacity(t)
                })
                .into_any_element()
        });
        // 幅が足りないときは、AI チャットを上のバーの下で、タスクの画面の右に重ねる
        let (ai_beside, ai_over) = if ai_width.is_some() {
            (ai, None)
        } else {
            (None, ai)
        };
        let body = div()
            .id("main-view")
            .key_context("MainView")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Dismiss, _, cx| this.dismiss(cx)))
            .on_action(cx.listener(|this, _: &ToggleAi, window, cx| this.toggle_ai(window, cx)))
            // 左のメニューの境界をドラッグしている間は、どこにマウスがあっても幅を追う
            .when(self.sidebar_resizing, |d| {
                d.on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.drag_sidebar(f32::from(event.position.x) / crate::UI_SCALE, cx);
                    } else {
                        this.finish_sidebar_drag(cx);
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.finish_sidebar_drag(cx)),
                )
            })
            // 表の列の幅を変えている間と、見出しを動かしている間も、どこにマウスがあっても追う
            .when(self.column_resize.is_some(), |d| {
                d.on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    if event.pressed_button == Some(MouseButton::Left) {
                        this.drag_column(f32::from(event.position.x), cx);
                    } else {
                        this.finish_column_resize(cx);
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.finish_column_resize(cx)),
                )
            })
            .when(self.column_drag.is_some(), |d| {
                d.on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    if event.pressed_button != Some(MouseButton::Left) {
                        this.finish_column_drag(cx);
                    } else if let Some(drag) = &mut this.column_drag {
                        if !drag.moved && (f32::from(event.position.x) - drag.start_x).abs() > 6. {
                            drag.moved = true;
                            cx.notify();
                        }
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.finish_column_drag(cx)),
                )
            })
            .size_full()
            .relative()
            .flex()
            .flex_row()
            .bg(theme.background)
            .text_color(theme.foreground)
            .when(wide && sidebar, |d| d.child(self.render_sidebar(wide, cx)))
            .child(
                col()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .child(self.render_header(true, wide, window, cx))
                    .child(
                        // 上のバーの下に、タスクの画面と AI チャットを横に並べる
                        row()
                            .flex_1()
                            .min_h_0()
                            .relative()
                            .items_stretch()
                            .child(
                                // 画面を切り替えたときは、少し下から浮かび上がる
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .h_full()
                                    .relative()
                                    .child(div().absolute().inset_0().child(main).with_animation(
                                        ElementId::Name(format!("page-{:?}", self.nav).into()),
                                        appear(160),
                                        |page, t| page.top(px(8.) * (1. - t)).opacity(t),
                                    ))
                                    .children(panel),
                            )
                            .children(ai_beside)
                            .children(ai_over),
                    ),
            )
            .children(self.render_modal(cx).map(|modal| {
                if !self.motion {
                    return modal.into_any_element();
                }
                modal
                    .with_animation("modal-in", appear(140), |modal, t| modal.opacity(t))
                    .into_any_element()
            }))
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
            .into_any_element();
        body
    }
}
