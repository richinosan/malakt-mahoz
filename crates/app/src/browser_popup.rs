//! アプリ内ブラウザの小さなウィンドウ。ページの中のリンクをホイールで押すと、ここで開く。
//!
//! ウィンドウはメインのウィンドウとは別に、自由に動かせる。上は別ウィンドウと同じ低い 1 行のバーで、
//! ページの題名、戻る、拡大率、タブで開く、ブラウザで開くを並べる。ページはタブと同じ一覧
//! （[`Terms::pages`]）に入るので、ログインの情報もタブと同じものを使う。タブの並びには出さない。

use crate::press::button;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::{ActiveTheme, Disableable, Placement, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::browser::{self, is_web, FindBar, PrintDialog, Step, Zoom, ZoomBadge};
use crate::db;
use crate::feel::{self, Grip};
use crate::render::{app_icon, row, window_controls};
use crate::scroll::Menus;
use crate::term::{self, Terms};
use crate::{APP_ID, UI_SCALE};

/// 参照する寸法を、画面全体の拡大率に合わせる。
const fn px(value: f32) -> Pixels {
    gpui_kit::px(value * UI_SCALE)
}

/// 上のバーの高さ。別ウィンドウのバーと同じ。
const BAR_HEIGHT: Pixels = px(30.);
/// 開いたときの大きさ。
const SIZE: Size<Pixels> = Size {
    width: px(640.),
    height: px(480.),
};
/// 2 つめからは、少しずつずらして開く。
const CASCADE: Pixels = px(28.);

/// `url` を小さなウィンドウで開く。`from` はリンクを押したページのウィンドウで、その真ん中あたりに開く。
pub fn open(url: &str, from: Option<WindowId>, cx: &mut App) {
    let terms = term::shared(cx);
    let Some(id) = terms.update(cx, |terms, cx| terms.open_popup_page(url, cx)) else {
        return;
    };
    let others = terms
        .read(cx)
        .pages
        .iter()
        .filter(|p| p.popup && p.id != id)
        .count();
    let parent = from
        .and_then(|from| cx.windows().into_iter().find(|w| w.window_id() == from))
        // 閉じたウィンドウなら、画面の真ん中に開く
        .and_then(|w| w.update(cx, |_, window, _| window.bounds()).ok());
    let shift = CASCADE * (others % 8) as f32;
    let bounds = match parent {
        Some(parent) => Bounds {
            origin: parent.center() - point(SIZE.width / 2., SIZE.height / 2.)
                + point(shift, shift),
            size: SIZE,
        },
        None => {
            let mut bounds = Bounds::centered(None, SIZE, cx);
            bounds.origin += point(shift, shift);
            bounds
        }
    };
    let label = terms.read(cx).page(id).map(|p| p.label());
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(280.), px(200.))),
        // ほかの別ウィンドウと同じく、OS のタイトルバーは出さず、中の細いバーで代える
        titlebar: Some(TitlebarOptions {
            title: label.map(|label| crate::window_title(&label).into()),
            ..gpui_kit::component::TitleBar::title_bar_options()
        }),
        #[cfg(target_os = "linux")]
        window_decorations: Some(WindowDecorations::Client),
        app_id: Some(APP_ID.into()),
        ..gpui_kit::component::TitleBar::window_options()
    };
    let opened = gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| PopupView::new(id, window, cx))
    });
    terms.update(cx, |terms, cx| match opened {
        Ok((handle, _)) => {
            if let Some(page) = terms.page_mut(id) {
                page.popup_window = Some(handle);
            }
        }
        Err(e) => {
            terms.remove_page(id, cx);
            terms.fail(
                format!("{url} を小さなウィンドウで開けませんでした: {e}"),
                cx,
            );
        }
    });
}

/// バーを右クリックしたときの、ほかのウィンドウと同じメニュー。設定はメインのウィンドウで開く。
fn window_menu(
    menu: gpui_kit::component::menu::PopupMenu,
    window: &mut Window,
    cx: &mut Context<gpui_kit::component::menu::PopupMenu>,
) -> gpui_kit::component::menu::PopupMenu {
    crate::render::window_menu(menu, None, window, cx)
}

/// 小さなウィンドウの中身。
pub struct PopupView {
    id: u64,
    terms: Entity<Terms>,
    /// バーの空いているところを押している。動かし始めたらウィンドウを動かす。
    bar_pressed: bool,
    /// ウィンドウの名前に出している題名。
    window_title: SharedString,
    /// ブラウザのキーの入力先。ページの中で押したキーも、ここで受ける。
    focus: FocusHandle,
    /// ページの中を探す欄。
    find: Entity<FindBar>,
    /// 印刷のダイアログ。
    print: Entity<PrintDialog>,
    _subscriptions: Vec<Subscription>,
}

impl PopupView {
    fn new(id: u64, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let terms = term::shared(cx);
        let focus = cx.focus_handle();
        let here = window.window_handle().window_id();
        terms.update(cx, |terms, _| terms.note_browser_focus(here, &focus));
        let find = cx.new(|cx| FindBar::new(move |_| Some(id), window, cx));
        let print = cx.new(|cx| PrintDialog::new(move |_| Some(id), window, cx));
        // OS のボタンや Alt+F4 で閉じるときも、ウィンドウより先にページの部品を捨てる
        window.on_window_should_close(cx, |window, cx| {
            browser::window_closing(window, cx);
            true
        });
        let subscriptions = vec![
            cx.observe(&terms, |_, _, cx| cx.notify()),
            cx.observe(&db::shared(cx), |_, _, cx| cx.notify()),
            // ウィンドウを閉じたら、ページも閉じる。タブに移したページは、そのまま残す
            cx.on_release(move |_, cx| {
                cx.defer(move |cx| {
                    term::shared(cx).update(cx, |terms, cx| {
                        if terms.page(id).is_some_and(|p| p.popup) {
                            terms.close(id, cx);
                        }
                    })
                })
            }),
        ];
        Self {
            id,
            terms,
            bar_pressed: false,
            window_title: SharedString::default(),
            focus,
            find,
            print,
            _subscriptions: subscriptions,
        }
    }

    /// ブラウザのタブに移して、このウィンドウを閉じる。
    fn dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.id;
        self.terms.update(cx, |terms, cx| terms.dock_popup(id, cx));
        // 捨てたページの後片付けを、ウィンドウを閉じる前に済ませる
        crate::webview::settle();
        crate::ai_tools::reveal_shell(window, cx);
        window.remove_window();
    }

    fn step(&self, step: Step, cx: &mut App) {
        let id = self.id;
        self.terms
            .update(cx, |terms, cx| terms.page_step(id, step, cx));
    }

    fn zoom(&self, zoom: Zoom, cx: &mut App) {
        let id = self.id;
        self.terms
            .update(cx, |terms, cx| terms.zoom_page(id, zoom, cx));
    }

    /// ブラウザのタブと同じキー（[`crate::shell_keys`]）を受ける。タブを閉じるキーでは、このウィンドウを閉じる。
    fn with_keys(&self, d: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        use crate::shell_keys::*;
        d.key_context(BROWSER)
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &BrowserReload, _, cx| this.step(Step::Reload, cx)))
            .on_action(cx.listener(|this, _: &BrowserBack, _, cx| this.step(Step::Back, cx)))
            .on_action(cx.listener(|this, _: &BrowserForward, _, cx| this.step(Step::Forward, cx)))
            .on_action(cx.listener(|this, _: &FindInPage, window, cx| {
                this.find.update(cx, |find, cx| find.show(window, cx))
            }))
            .on_action(cx.listener(|this, _: &FindNext, window, cx| {
                this.find
                    .update(cx, |find, cx| find.step(false, window, cx))
            }))
            .on_action(cx.listener(|this, _: &FindPrevious, window, cx| {
                this.find.update(cx, |find, cx| find.step(true, window, cx))
            }))
            .on_action(cx.listener(|this, _: &CloseFind, _, cx| {
                if !this.find.update(cx, |find, cx| find.hide(cx)) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &PrintPage, window, cx| {
                this.print.update(cx, |print, cx| print.show(window, cx))
            }))
            .on_action(cx.listener(|this, _: &BrowserZoomIn, _, cx| this.zoom(Zoom::In, cx)))
            .on_action(cx.listener(|this, _: &BrowserZoomOut, _, cx| this.zoom(Zoom::Out, cx)))
            .on_action(cx.listener(|this, _: &BrowserZoomReset, _, cx| this.zoom(Zoom::Reset, cx)))
            .on_action(|_: &CloseTab, window, cx| {
                // ページの部品は、ウィンドウより先に捨てる
                browser::window_closing(window, cx);
                window.remove_window();
            })
            .on_action(|_: &crate::shortcuts::ToggleFullscreen, window, _| {
                window.toggle_fullscreen()
            })
    }

    /// 上の低い 1 行のバー。空いているところを掴むと、ウィンドウを動かせる。
    fn render_bar(
        &self,
        title: SharedString,
        page: Option<(bool, bool, String, Option<ZoomBadge>)>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let theme = cx.theme().clone();
        // OS がタイトルバーを描いているときは、動かすのはそちらに任せる
        #[cfg(target_os = "linux")]
        let own_bar = matches!(window.window_decorations(), Decorations::Client { .. });
        #[cfg(not(target_os = "linux"))]
        let own_bar = true;
        // ページは下に重なるので、名前はボタンの横に出す
        let tool = |id: &'static str, icon: &str, tip: &'static str, placement: Placement| {
            button(id)
                .icon(app_icon(icon))
                .tooltip(tip)
                .tooltip_placement(placement)
                .ghost()
                .xsmall()
        };
        let bar = row()
            .id("popup-bar")
            .flex_none()
            .h(BAR_HEIGHT)
            .gap_0p5()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.background)
            // 空いているところに乗せるとうっすら、つまんでいる間は少し濃くなる
            .grip(cx)
            .when(own_bar && cfg!(target_os = "windows"), |d| {
                d.window_control_area(WindowControlArea::Drag)
            })
            .when(own_bar && !cfg!(target_os = "windows"), |d| {
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
                        this.bar_pressed = true;
                        cx.notify();
                    }),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, _| this.bar_pressed = false),
                )
                .on_mouse_move(cx.listener(
                    |this, event: &MouseMoveEvent, window, _| {
                        if !this.bar_pressed {
                            return;
                        }
                        this.bar_pressed = false;
                        if event.pressed_button == Some(MouseButton::Left) {
                            window.start_window_move();
                        }
                    },
                ))
            })
            .pl_1()
            // macOS は左に信号機ボタンが乗るので、その分を空ける
            .when(cfg!(target_os = "macos"), |d| d.pl(px(80.)));
        let Some((can_back, loading, url, zoom)) = page else {
            return bar
                .child(div().flex_1().min_w_0().pl_2().truncate().child(title))
                .child(
                    row()
                        .occlude()
                        .children(crate::on_top::pin(
                            gpui_kit::component::Size::XSmall,
                            window,
                            cx,
                        ))
                        .children(window_controls(BAR_HEIGHT, false, window, cx)),
                );
        };
        let id = self.id;
        bar.child(
            // バーの判定に取られないよう、ボタンのある場所はマウスを受け止める
            row().occlude().child(feel::gloss_when(
                can_back,
                tool("popup-back", "arrow-left", "戻る", Placement::Right)
                    .disabled(!can_back)
                    .on_click(cx.listener(|this, _, _, cx| this.step(Step::Back, cx))),
            )),
        )
        .child(
            div()
                .id("popup-title")
                .flex_1()
                .min_w_0()
                .px_1()
                .text_sm()
                .text_color(if loading {
                    theme.muted_foreground
                } else {
                    theme.foreground
                })
                .truncate()
                .child(title)
                .test_support(),
        )
        .child(
            row()
                .occlude()
                .gap_0p5()
                .children(zoom.map(|zoom| {
                    let terms = self.terms.clone();
                    zoom.render(cx.reduce_motion(), theme.primary, move |_, _, cx| {
                        terms.update(cx, |terms, cx| {
                            terms.zoom_page(id, browser::Zoom::Reset, cx)
                        })
                    })
                }))
                .child(feel::gloss(
                    tool("popup-dock", "panel-top", "タブで開く", Placement::Left)
                        .on_click(cx.listener(|this, _, window, cx| this.dock(window, cx))),
                ))
                .child(feel::gloss_when(
                    is_web(&url),
                    tool(
                        "popup-external",
                        "external-link",
                        "ブラウザで開く",
                        Placement::Left,
                    )
                    .disabled(!is_web(&url))
                    .on_click(move |_, _, cx| cx.open_url(&url)),
                ))
                .children(crate::on_top::pin(
                    gpui_kit::component::Size::XSmall,
                    window,
                    cx,
                ))
                .when(own_bar, |d| {
                    d.children(window_controls(BAR_HEIGHT, false, window, cx))
                }),
        )
    }
}

impl Render for PopupView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let unlocked = db::shared(cx).read(cx).unlocked;
        let page = self.terms.read(cx).page(self.id).map(|p| {
            (
                p.label(),
                p.can_back,
                p.loading,
                p.url.clone(),
                ZoomBadge::of(p),
                p.error.clone(),
                p.shot.clone(),
                p.focused,
            )
        });
        let title = page
            .as_ref()
            .map(|p| p.0.clone())
            .unwrap_or_else(|| "ブラウザ".into());
        // タスクバーや Alt+Tab に出る名前も、ページの題名に合わせる
        if title != self.window_title {
            window.set_window_title(&crate::window_title(&title));
            self.window_title = title.clone();
        }
        let base = div()
            .id("popup")
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.background)
            .text_color(theme.foreground)
            // ここでもマウスの操作を見張る（自動でロックするまでの時間）
            .child(db::input_watcher());
        let Some((_, can_back, loading, url, zoom, error, shot, page_focused)) =
            page.filter(|_| unlocked)
        else {
            let bar = self
                .render_bar(title, None, window, cx)
                .right_menu(window_menu);
            return base
                .child(bar)
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(theme.muted_foreground)
                        .when(!unlocked, |d| {
                            d.child("ロックされています。メインのウィンドウで解除してください。")
                        }),
                )
                .child(browser::frame_end(true, window, cx))
                .test_support();
        };
        // ページの中にキーの入力先があるときも、アプリではこのウィンドウのブラウザにいることにする
        if page_focused && !self.focus.contains_focused(window, cx) {
            window.focus(&self.focus, cx);
        }
        let bar = self
            .render_bar(title, Some((can_back, loading, url, zoom)), window, cx)
            .right_menu(window_menu);
        let body = match error {
            Some(error) => div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .items_center()
                .justify_center()
                .gap_2()
                .p_4()
                .text_sm()
                .text_color(theme.danger)
                .child(div().id("popup-error").child(error).test_support())
                .child(feel::gloss(
                    button("popup-retry")
                        .icon(app_icon("rotate-cw"))
                        .label("もう一度開く")
                        .ghost()
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| this.step(Step::Reload, cx))),
                ))
                .into_any_element(),
            None => div()
                .relative()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(browser::page_view(self.id, shot, theme.background))
                .child(self.print.clone())
                .into_any_element(),
        };
        self.with_keys(base, cx)
            .child(bar)
            .child(self.find.clone())
            .child(
                div()
                    .flex_none()
                    .h(gpui_kit::px(2.))
                    .w_full()
                    .when(loading, |d| d.child(browser::loading_line(theme.primary))),
            )
            .child(body)
            // ブラウザのページを見せるかは、いちばん最後に決める
            .child(browser::frame_end(false, window, cx))
            .test_support()
    }
}
