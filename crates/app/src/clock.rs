//! 時間を測る別ウィンドウ（ポモドーロとタイマー）で共有する部品。上の細いバー、残り時間の輪、
//! 終わったときに輪を光らせる動き、エラーの帯と、ウィンドウの位置と大きさを覚えること。

use std::time::Duration;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::{ActiveTheme, Placement, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

use crate::db;
use crate::feel::{self, Grip};
use crate::press::button;
use crate::render::{app_icon, row, window_controls};
use crate::{APP_ID, UI_SCALE};

/// 参照する寸法を、画面全体の拡大率に合わせる。
pub(crate) const fn px(value: f32) -> Pixels {
    gpui_kit::px(value * UI_SCALE)
}

/// 上のバーの高さ。別ウィンドウのバーと同じ。
pub(crate) const BAR_HEIGHT: Pixels = px(30.);
/// 残り時間の輪の大きさと太さ。
pub(crate) const RING: Pixels = px(188.);
const RING_WIDTH: f32 = 8. * UI_SCALE;
/// 終わったときに、輪を光らせる長さ。
pub(crate) const FLASH: Duration = Duration::from_millis(2400);
/// 時刻を見る間隔。
pub(crate) const TICK: Duration = Duration::from_secs(1);

/// バーの空いているところを押しているかを持つビュー。動かし始めたらウィンドウを動かす。
pub(crate) trait BarHold: 'static + Sized {
    fn bar_pressed(&mut self) -> &mut bool;
}

/// 上の低い 1 行のバー。左にアイコンか戻るボタン、まん中に名前、右に `right` とウィンドウの
/// ボタンを並べる。空いているところを掴むと、ウィンドウを動かせる。
pub(crate) fn bar<V: BarHold>(
    id: &'static str,
    left: AnyElement,
    title: SharedString,
    right: Vec<AnyElement>,
    window: &Window,
    cx: &mut Context<V>,
) -> Stateful<Div> {
    let theme = cx.theme().clone();
    // OS がタイトルバーを描いているときは、動かすのはそちらに任せる
    #[cfg(target_os = "linux")]
    let own_bar = matches!(window.window_decorations(), Decorations::Client { .. });
    #[cfg(not(target_os = "linux"))]
    let own_bar = true;
    row()
        .id(id)
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
                    *this.bar_pressed() = true;
                    cx.notify();
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| *this.bar_pressed() = false),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, _| {
                if !*this.bar_pressed() {
                    return;
                }
                *this.bar_pressed() = false;
                if event.pressed_button == Some(MouseButton::Left) {
                    window.start_window_move();
                }
            }))
        })
        .pl_1()
        // macOS は左に信号機ボタンが乗るので、その分を空ける
        .when(cfg!(target_os = "macos"), |d| d.pl(px(80.)))
        // バーの判定に取られないよう、ボタンのある場所はマウスを受け止める
        .child(row().occlude().child(left))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .px_1()
                .text_sm()
                .truncate()
                .child(title),
        )
        .child(
            row()
                .occlude()
                .gap_0p5()
                .children(right)
                // 常に最前面に出している間のピン
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

/// バーの左に置く、戻るボタン。
pub(crate) fn back_button<V: 'static>(
    id: &'static str,
    cx: &mut Context<V>,
    on_click: impl Fn(&mut V, &mut Window, &mut Context<V>) + 'static,
) -> AnyElement {
    feel::gloss(
        button(id)
            .icon(app_icon("arrow-left"))
            .tooltip("戻る")
            .tooltip_placement(Placement::Right)
            .ghost()
            .xsmall()
            .on_click(cx.listener(move |this, _, window, cx| on_click(this, window, cx))),
    )
    .into_any_element()
}

/// バーの左に置く、ウィンドウのアイコン。
pub(crate) fn bar_icon(name: &str) -> AnyElement {
    div()
        .px_1()
        .child(app_icon(name).small())
        .into_any_element()
}

/// 輪の下地と、進んだ割合（0〜1、上から時計回り）。中身の後ろに敷く。
pub(crate) fn ring(progress: f32, color: Hsla, track: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let center = bounds.center();
            let radius = (f32::from(bounds.size.width.min(bounds.size.height)) - RING_WIDTH) / 2.;
            paint_arc(window, center, radius, 0., 1., track);
            if progress > 0. {
                paint_arc(window, center, radius, 0., progress, color);
            }
        },
    )
    .absolute()
    .inset_0()
}

/// 終わったばかりなら、輪の外側を 3 回ふくらませて消す。`key` が変わるたびに動き直す。
pub(crate) fn glow(
    face: Stateful<Div>,
    key: (&'static str, u64),
    color: Hsla,
    flashing: bool,
) -> AnyElement {
    if !flashing {
        return face.into_any_element();
    }
    let glow = div()
        .absolute()
        .inset_0()
        .rounded_full()
        .border_4()
        .border_color(color);
    div()
        .relative()
        .child(face)
        .child(glow.with_animation(
            ElementId::NamedInteger(key.0.into(), key.1),
            Animation::new(FLASH / 3).repeat(),
            |glow, delta| glow.opacity(1. - delta).m(-px(10.) * delta),
        ))
        .into_any_element()
}

/// 輪の一部を描く。`from` と `to` は、上を 0 として時計回りに数えた割合（0〜1）。
fn paint_arc(
    window: &mut Window,
    center: Point<Pixels>,
    radius: f32,
    from: f32,
    to: f32,
    color: Hsla,
) {
    // 細かい線でつないで描く（1 周 180 本）
    let steps = ((to - from) * 180.).ceil().max(1.) as usize;
    let at = |t: f32| {
        let angle = std::f32::consts::TAU * t - std::f32::consts::FRAC_PI_2;
        point(
            center.x + gpui_kit::px(radius * angle.cos()),
            center.y + gpui_kit::px(radius * angle.sin()),
        )
    };
    let mut path = PathBuilder::stroke(gpui_kit::px(RING_WIDTH));
    path.move_to(at(from));
    for i in 1..=steps {
        path.line_to(at(from + (to - from) * i as f32 / steps as f32));
    }
    // 形が作れないことはない（点を並べた線）
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// ウィンドウの上に出すエラーの帯。× で閉じる。
pub(crate) fn error_row<V: 'static>(
    prefix: &'static str,
    error: SharedString,
    cx: &mut Context<V>,
    on_close: impl Fn(&mut V, &mut Context<V>) + 'static,
) -> AnyElement {
    let theme = cx.theme().clone();
    row()
        .id(SharedString::from(format!("{prefix}-error")))
        .flex_none()
        .gap_2()
        .px_2()
        .py_1()
        .text_sm()
        .text_color(theme.danger)
        .bg(theme.danger.opacity(0.08))
        .aria_label(error.clone())
        .child(div().flex_1().min_w_0().child(error))
        .child(
            button(SharedString::from(format!("{prefix}-error-close")))
                .icon(app_icon("x"))
                .tooltip("閉じる")
                .ghost()
                .xsmall()
                .on_click(cx.listener(move |this, _, _, cx| on_close(this, cx))),
        )
        .test_support()
        .into_any_element()
}

/// 別ウィンドウの開き方。前に覚えた位置と大きさ（設定の `window_key`）があればそこに、
/// なければ画面のまん中に `size` で開く。
pub(crate) fn window_options(
    name: &str,
    window_key: &str,
    size: Size<Pixels>,
    min: Size<Pixels>,
    cx: &mut App,
) -> WindowOptions {
    let saved: Option<[f32; 4]> = db::shared(cx)
        .read(cx)
        .store
        .setting(window_key)
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str(&text).ok());
    let bounds = match saved {
        Some([x, y, w, h]) => Bounds {
            origin: point(gpui_kit::px(x), gpui_kit::px(y)),
            size: gpui_kit::size(gpui_kit::px(w), gpui_kit::px(h)),
        },
        None => Bounds::centered(None, size, cx),
    };
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(min),
        // ほかの別ウィンドウと同じく、OS のタイトルバーは出さず、中の細いバーで代える
        titlebar: Some(TitlebarOptions {
            title: Some(crate::window_title(name).into()),
            ..gpui_kit::component::TitleBar::title_bar_options()
        }),
        #[cfg(target_os = "linux")]
        window_decorations: Some(WindowDecorations::Client),
        app_id: Some(APP_ID.into()),
        ..gpui_kit::component::TitleBar::window_options()
    }
}

/// ウィンドウの今の位置と大きさ。閉じるときに [`remember_closed`] で覚える。
pub(crate) fn bounds_of(window: &Window) -> [f32; 4] {
    let b = window.bounds();
    [
        f32::from(b.origin.x),
        f32::from(b.origin.y),
        f32::from(b.size.width),
        f32::from(b.size.height),
    ]
}

/// ウィンドウを閉じたときに、位置と大きさを覚える。終了で閉じるときは開いていた印を残し、
/// 次の起動で開き直す。
pub(crate) fn remember_closed(
    window_key: &str,
    open_key: &str,
    bounds: Option<[f32; 4]>,
    cx: &mut App,
) -> Result<(), malakt_core::Error> {
    let quitting = crate::session::is_quitting(cx);
    db::shared(cx).update(cx, |db, _| {
        if let Some(bounds) = bounds {
            let text = serde_json::to_string(&bounds).unwrap_or_default();
            db.store.set_setting(window_key, Some(&text))?;
        }
        if !quitting {
            db.store.set_setting(open_key, None)?;
        }
        Ok(())
    })
}

/// 前に閉じたときに開いていたか。
pub(crate) fn was_open(open_key: &str, cx: &App) -> bool {
    db::shared(cx)
        .read(cx)
        .store
        .setting(open_key)
        .ok()
        .flatten()
        .is_some()
}

/// 開いていた印を付ける。
pub(crate) fn mark_open(open_key: &str, cx: &mut App) -> Result<(), malakt_core::Error> {
    db::shared(cx).update(cx, |db, _| db.store.set_setting(open_key, Some("1")))
}
