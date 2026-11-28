//! ブラウザのツールバーの右のダウンロードのボタンと、その下に出す一覧（Chrome と同じ）。
//! 一覧の項目を押すと、OS のファイルマネージャーでそのファイルを選んで開く。

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::{ActiveTheme, Placement, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::feel::{self, Lit, Sunk};
use crate::press::button;
use crate::render::{app_icon, col, count_bubble, file_icon, remove_x, round_icon, row, Bubble};
use crate::scroll::Menus;
use crate::shadow::{Elevate, Elevation};
use crate::term::{self, Terms};

/// 一覧の幅。
const WIDTH: f32 = 380.;

/// ダウンロードの一覧。ブラウザの画面ごとに 1 つ持ち、ツールバーのボタンの下に出す。
pub(crate) struct DownloadsBubble {
    terms: Entity<Terms>,
    /// 一覧で隠すページ（見ているページ）。
    page: crate::browser::PageOf,
    /// 出している。隠しているページ。
    open: Option<Option<u64>>,
    /// ツールバーのボタンの場所。
    anchor: Rc<Cell<Bounds<Pixels>>>,
    /// 開いた回数。出てくる動きを毎回はじめから描く。
    opens: usize,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl DownloadsBubble {
    pub(crate) fn new(
        page: impl Fn(&App) -> Option<u64> + 'static,
        cx: &mut Context<Self>,
    ) -> Self {
        let downloads = crate::downloads::shared(cx);
        let subscriptions = vec![
            // 一覧を出している間に増えたものは、見たことにする
            cx.observe(&downloads, |this, downloads, cx| {
                if this.open.is_some() && downloads.read(cx).unseen() > 0 {
                    downloads.update(cx, |d, cx| d.opened(cx));
                }
                cx.notify();
            }),
        ];
        Self {
            terms: term::shared(cx),
            page: Box::new(page),
            open: None,
            anchor: Rc::default(),
            opens: 0,
            focus: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// 出す・閉じる（ボタン、Ctrl+J）。
    pub(crate) fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open.is_some() {
            self.hide(cx);
        } else {
            self.show(window, cx);
        }
    }

    pub(crate) fn show(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open.is_some() {
            window.focus(&self.focus, cx);
            return;
        }
        let page = (self.page)(cx);
        // ページは一覧より手前に出るので、出している間は隠して画像を出す
        if let Some(id) = page {
            self.terms.update(cx, |terms, cx| {
                terms.release_page(id, cx);
                terms.set_covered(id, true, cx);
            });
        }
        self.open = Some(page);
        self.opens += 1;
        crate::downloads::shared(cx).update(cx, |d, cx| d.opened(cx));
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// 閉じる（Esc、外を押す）。出していなければ false。
    pub(crate) fn hide(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(page) = self.open.take() else {
            return false;
        };
        if let Some(id) = page {
            self.terms.update(cx, |terms, cx| {
                terms.set_covered(id, false, cx);
                terms.focus_page(id, cx);
            });
        }
        cx.notify();
        true
    }
}

/// ツールバーの右の、ダウンロードのボタン。一覧に何かあるときだけ出す。
/// まだ見ていない数を右上に出し、ダウンロード中は下に進み具合の線を出す。
pub(crate) fn toolbar_button(bubble: &Entity<DownloadsBubble>, cx: &App) -> Option<AnyElement> {
    let downloads = crate::downloads::shared(cx);
    let (empty, unseen, activity) = {
        let d = downloads.read(cx);
        (d.items.is_empty(), d.unseen(), d.activity())
    };
    if empty {
        return None;
    }
    let theme = cx.theme().clone();
    let motion = !cx.reduce_motion();
    let bar = activity.map(|ratio| {
        let track = div()
            .absolute()
            .left(px(6.))
            .right(px(6.))
            .bottom(px(3.))
            .h(px(2.))
            .rounded_full()
            .overflow_hidden()
            .bg(theme.muted);
        match ratio {
            Some(ratio) => track.child(
                div()
                    .h_full()
                    .w(relative(ratio.max(0.04)))
                    .rounded_full()
                    .bg(theme.primary),
            ),
            None => track.child(crate::browser::loading_line(theme.primary)),
        }
    });
    let anchor = bubble.read(cx).anchor.clone();
    let bubble = bubble.clone();
    Some(
        div()
            .flex_none()
            .relative()
            .child(
                canvas(move |bounds, _, _| anchor.set(bounds), |_, _, _, _| {})
                    .absolute()
                    .inset_0(),
            )
            .child(feel::gloss(
                button("open-downloads")
                    .icon(app_icon("download"))
                    .ghost()
                    .small()
                    .tooltip("ダウンロード")
                    // ページは上に重なるので、名前はボタンの上に出す
                    .tooltip_placement(Placement::Top)
                    .on_click(move |_, window, cx| {
                        bubble.update(cx, |bubble, cx| bubble.toggle(window, cx))
                    }),
            ))
            .children(bar)
            .children(count_bubble(
                "open-downloads-count",
                unseen,
                Bubble::Bell,
                Some((px(-4.), px(-4.))),
                cx,
            ))
            // 新しいものが増えるたびに、アイコンを少し弾ませる
            .with_animation(
                ("open-downloads-bump", unseen as usize),
                Animation::new(Duration::from_millis(if motion { 360 } else { 1 }))
                    .with_easing(ease_in_out),
                move |d, t| {
                    if unseen == 0 {
                        return d;
                    }
                    d.top(px(-3. * (t * std::f32::consts::PI).sin()))
                },
            )
            .into_any_element(),
    )
}

/// 一覧の中身（項目、失敗、下のボタン）。
fn list(cx: &mut App) -> Div {
    let theme = cx.theme().clone();
    let downloads = crate::downloads::shared(cx);
    let (items, error) = {
        let d = downloads.read(cx);
        (d.items.clone(), d.error.clone())
    };
    let now = malakt_core::now_ms();
    let mut list = col().gap_0p5();
    for (ix, item) in items.iter().enumerate() {
        let id = item.id;
        let running = item.running();
        let failed = matches!(item.state, crate::downloads::State::Failed { .. });
        let gone = item.missing || failed;
        let group = SharedString::from(format!("download-{ix}"));
        let icon = match item.kind {
            crate::downloads::Kind::Pdf => "file-text",
            crate::downloads::Kind::File => file_icon(&item.name()),
        };
        let menu_downloads = downloads.clone();
        let open = !running && !gone;
        let url = item.url.clone();
        let detail_color = if failed {
            theme.danger
        } else {
            theme.muted_foreground
        };
        list = list.child(
            row()
                .id(("download", ix))
                .test_support()
                .group(group.clone())
                .gap_3()
                .px_2()
                .py_1p5()
                .rounded(feel::md())
                .when(open, |d| {
                    d.cursor_pointer()
                        .lit(theme.secondary_hover, cx)
                        .sunk(theme.accent, cx)
                })
                .child(
                    div()
                        .flex_none()
                        .size_8()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(feel::md())
                        .bg(theme.muted)
                        .text_color(if gone {
                            theme.muted_foreground
                        } else {
                            theme.foreground
                        })
                        .child(app_icon(if gone { "file-x" } else { icon }).size_4()),
                )
                .child(
                    col()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            div()
                                .text_sm()
                                .truncate()
                                .when(gone, |d| {
                                    d.line_through().text_color(theme.muted_foreground)
                                })
                                .child(item.name()),
                        )
                        .when(running, |d| {
                            let track = div()
                                .h(gpui_kit::px(3.))
                                .w_full()
                                .rounded_full()
                                .overflow_hidden()
                                .bg(theme.muted);
                            d.child(match item.ratio() {
                                Some(ratio) => track.child(
                                    div()
                                        .h_full()
                                        .w(relative(ratio.max(0.02)))
                                        .rounded_full()
                                        .bg(theme.primary),
                                ),
                                None => track.child(crate::browser::loading_line(theme.primary)),
                            })
                        })
                        .child(
                            div()
                                .text_xs()
                                .truncate()
                                .text_color(detail_color)
                                .child(item.detail(now)),
                        ),
                )
                // 乗せたときに出すボタン。「フォルダで表示」と「一覧から削除」は同じ形と動き
                .when(open, |d| {
                    let downloads = downloads.clone();
                    d.child(
                        div()
                            .opacity(0.)
                            .group_hover(group.clone(), |s| s.opacity(1.))
                            .child(
                                round_icon(
                                    ("download-reveal", ix),
                                    "folder-open",
                                    "フォルダで表示",
                                    move |_, _, cx| downloads.update(cx, |d, cx| d.reveal(id, cx)),
                                    cx,
                                )
                                .test_support(),
                            ),
                    )
                })
                .when(!running, |d| {
                    let downloads = downloads.clone();
                    d.child(
                        div()
                            .opacity(0.)
                            .group_hover(group.clone(), |s| s.opacity(1.))
                            .child(
                                remove_x(
                                    ("download-remove", ix),
                                    "一覧から削除",
                                    move |_, _, cx| downloads.update(cx, |d, cx| d.remove(id, cx)),
                                    cx,
                                )
                                .test_support(),
                            ),
                    )
                })
                .when(open, |d| {
                    let downloads = downloads.clone();
                    d.on_click(move |_, _, cx| downloads.update(cx, |d, cx| d.reveal(id, cx)))
                })
                .right_menu(move |menu, _, _| {
                    let (reveal, opener, remover) = (
                        menu_downloads.clone(),
                        menu_downloads.clone(),
                        menu_downloads.clone(),
                    );
                    let url = url.clone();
                    menu.item(
                        PopupMenuItem::new("フォルダで表示")
                            .icon(app_icon("folder-open"))
                            .disabled(!open)
                            .on_click(move |_, _, cx| reveal.update(cx, |d, cx| d.reveal(id, cx))),
                    )
                    .item(
                        PopupMenuItem::new("開く")
                            .icon(app_icon("external-link"))
                            .disabled(!open)
                            .on_click(move |_, _, cx| {
                                opener.update(cx, |d, cx| d.open_file(id, cx))
                            }),
                    )
                    .item(
                        PopupMenuItem::new("リンクをコピー")
                            .icon(app_icon("link"))
                            .disabled(!crate::browser::is_web(&url))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(url.clone()))
                            }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new("一覧から削除")
                            .icon(app_icon("x"))
                            .disabled(running)
                            .on_click(move |_, _, cx| remover.update(cx, |d, cx| d.remove(id, cx))),
                    )
                }),
        );
    }
    let any_done = items.iter().any(|i| !i.running());
    col()
        .gap_3()
        .min_h_0()
        .when_some(error, |d, error| {
            d.child(
                div()
                    .id("downloads-error")
                    .text_sm()
                    .text_color(theme.danger)
                    .child(error)
                    .test_support(),
            )
        })
        .when(items.is_empty(), |d| {
            d.child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("ダウンロードはまだありません。"),
            )
        })
        .child(
            div()
                .id("downloads-scroll")
                .flex_1()
                .min_h_0()
                .max_h(px(420.))
                .overflow_y_scrollbar()
                .child(list),
        )
        .child(
            row()
                .gap_1()
                .justify_end()
                .child(feel::gloss(
                    button("downloads-folder")
                        .icon(app_icon("folder-open"))
                        .ghost()
                        .small()
                        .tooltip("ダウンロードのフォルダを開く")
                        .on_click(|_, _, cx| {
                            cx.open_with_system(&crate::downloads::download_dir(cx))
                        }),
                ))
                .when(any_done, |d| {
                    let downloads = downloads.clone();
                    d.child(feel::gloss(
                        button("downloads-clear")
                            .icon(app_icon("list-x"))
                            .ghost()
                            .small()
                            .tooltip("一覧を消去")
                            .on_click(move |_, _, cx| downloads.update(cx, |d, cx| d.clear(cx))),
                    ))
                }),
        )
}

impl Render for DownloadsBubble {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.open.is_none() {
            return div().into_any_element();
        }
        let theme = cx.theme().clone();
        let anchor = self.anchor.get();
        let body = list(cx);
        let panel = col()
            .id("downloads")
            .key_context(crate::shell_keys::DOWNLOADS)
            .track_focus(&self.focus)
            .on_action(
                cx.listener(|this, _: &crate::shell_keys::CloseDownloads, _, cx| {
                    this.hide(cx);
                }),
            )
            .occlude()
            .w(px(WIDTH))
            .p_3()
            .gap_2()
            .rounded(feel::lg())
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .elevate(Elevation::High, cx)
            // 外（ほかのペインやアプリのどこか）を押したら閉じる
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.hide(cx);
            }))
            .child(
                row()
                    .child(div().flex_1().text_base().child("ダウンロード"))
                    .child(feel::gloss(
                        button("downloads-close")
                            .icon(app_icon("x"))
                            .tooltip("閉じる（Esc）")
                            .ghost()
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.hide(cx);
                            })),
                    )),
            )
            .child(body)
            .test_support()
            // ボタンから下へ、ふわっと出す
            .with_animation(
                ("downloads-in", self.opens),
                Animation::new(Duration::from_millis(180)).with_easing(ease_out_quint()),
                |d, t| d.opacity(0.3 + 0.7 * t).mt(px(-8. * (1. - t))),
            );
        deferred(
            anchored()
                .position(point(anchor.right(), anchor.bottom() + px(6.)))
                .anchor(Anchor::TopRight)
                .snap_to_window_with_margin(px(8.))
                .child(panel),
        )
        .with_priority(2)
        .into_any_element()
    }
}
