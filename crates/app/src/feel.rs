//! 乗せたとき・押したとき・選んでいるときの塗り方。テーマの `Feel` で変わる。
//!
//! 乗せたときと押している間の面は、部品の中に敷く 1 枚の絵（`wash`）として描き、
//! 180ms で濃さを移す。動いている間だけ次のコマを頼むので、止まっている画面は描き直さない。

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::component::button::Button;
use gpui_kit::component::ActiveTheme;
use gpui_kit::*;

use crate::theme::Feel;

/// 乗せたとき・押したときに、濃さを移す時間。
const FADE: Duration = Duration::from_millis(180);

fn white(alpha: f32) -> Hsla {
    gpui_kit::white().opacity(alpha)
}

fn dark_ink(alpha: f32) -> Hsla {
    gpui_kit::black().opacity(alpha)
}

fn edge(color: Hsla, offset: f32, blur: f32, inset: bool) -> BoxShadow {
    BoxShadow {
        color,
        offset: point(px(0.), px(offset)),
        blur_radius: px(blur),
        spread_radius: px(0.),
        inset,
    }
}

/// ガラスの縁。内側を 1 周する細い光と、上の縁の少し強い光。
fn ring(dark: bool) -> Vec<BoxShadow> {
    vec![
        BoxShadow {
            spread_radius: px(1.),
            ..edge(white(if dark { 0.16 } else { 0.55 }), 0., 0., true)
        },
        edge(white(if dark { 0.3 } else { 0.85 }), 1., 0., true),
    ]
}

/// 1 枚の面の描き方。面は上から下への 2 色、影は外と内。
#[derive(Default)]
struct Look {
    fill: Option<(Hsla, Hsla)>,
    glow: Vec<BoxShadow>,
    inner: Vec<BoxShadow>,
}

impl Look {
    fn flat(color: Hsla) -> Self {
        Look {
            fill: Some((color, color)),
            ..Look::default()
        }
    }

    /// 乗せたとき。`on_top` は、中身の上に重ねて描く部品（ライブラリのボタン）。
    /// `square` は角の丸みがない部品（一覧の行）。
    fn hover(color: Hsla, on_top: bool, square: bool, cx: &App) -> Self {
        Look::hover_as(crate::theme::feel(cx), color, on_top, square, cx)
    }

    /// `feel` で描くときの、乗せたときの面。
    fn hover_as(feel: Feel, color: Hsla, on_top: bool, square: bool, cx: &App) -> Self {
        let theme = cx.theme();
        let dark = theme.mode.is_dark();
        match feel {
            Feel::Standard => Look::flat(color),
            // 端から端まである行は、面を明るくするだけ。縁の線を引くと帯に見える
            Feel::Glass if square => Look::flat(white(if dark { 0.07 } else { 0.3 })),
            Feel::Glass => {
                // 上が少し明るい面と、内側を 1 周する細い光。文字の上に重ねるときは薄くする
                let (top, bottom) = match (on_top, dark) {
                    (false, true) => (0.16, 0.07),
                    (false, false) => (0.55, 0.3),
                    (true, true) => (0.14, 0.03),
                    (true, false) => (0.3, 0.06),
                };
                Look {
                    fill: Some((white(top), white(bottom))),
                    glow: Vec::new(),
                    inner: ring(dark),
                }
            }
            Feel::Crisp => Look::flat(theme.foreground.opacity(0.08)),
            Feel::Tint => Look::flat(theme.primary.opacity(0.14)),
        }
    }

    /// 押している間。
    fn press(color: Hsla, cx: &App) -> Self {
        let theme = cx.theme();
        match crate::theme::feel(cx) {
            Feel::Standard => Look::flat(color),
            // 上が暗く下が明るい。面が沈んで見える
            Feel::Glass => Look {
                fill: Some((dark_ink(0.16), white(0.1))),
                glow: Vec::new(),
                inner: vec![edge(dark_ink(0.3), 1., 3., true)],
            },
            Feel::Crisp => Look::flat(theme.foreground.opacity(0.16)),
            Feel::Tint => Look::flat(theme.primary.opacity(0.28)),
        }
    }

    /// 濃さ `value`（0〜1）で描く。
    fn paint(
        &self,
        bounds: Bounds<Pixels>,
        radii: Corners<Pixels>,
        value: f32,
        window: &mut Window,
    ) {
        let fade = |shadows: &[BoxShadow]| -> Vec<BoxShadow> {
            shadows
                .iter()
                .map(|shadow| BoxShadow {
                    color: shadow.color.opacity(value),
                    ..shadow.clone()
                })
                .collect()
        };
        if !self.glow.is_empty() {
            window.paint_drop_shadows(bounds, radii, &fade(&self.glow));
        }
        if let Some((top, bottom)) = self.fill {
            let fill: Background = if top == bottom {
                top.opacity(value).into()
            } else {
                linear_gradient(
                    180.,
                    linear_color_stop(top.opacity(value), 0.),
                    linear_color_stop(bottom.opacity(value), 1.),
                )
            };
            window.paint_quad(quad(
                bounds,
                radii,
                fill,
                px(0.),
                gpui_kit::transparent_black(),
                BorderStyle::default(),
            ));
        }
        if !self.inner.is_empty() {
            window.paint_inset_shadows(bounds, radii, &fade(&self.inner));
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Hover,
    Press,
}

/// 濃さの動き。向かう先が変わったら、今の濃さから移り直す。
struct Fade {
    from: f32,
    to: f32,
    since: Instant,
    /// この部品の上でボタンを押している
    held: Rc<Cell<bool>>,
}

impl Fade {
    fn value(&self, now: Instant) -> f32 {
        let t = (now.duration_since(self.since).as_secs_f32() / FADE.as_secs_f32()).min(1.);
        // 速く始まって、ゆっくり止まる
        let eased = 1. - (1. - t).powi(3);
        self.from + (self.to - self.from) * eased
    }

    fn moving(&self, now: Instant) -> bool {
        now.duration_since(self.since) < FADE
    }
}

/// 乗せたとき・押している間の面。親の部品いっぱいに敷く。
/// `radii` は親の角の丸み。`None` はライブラリのボタンの丸み。
fn wash(
    kind: Kind,
    color: Hsla,
    radii: Option<CornersRefinement<AbsoluteLength>>,
    on_top: bool,
) -> impl IntoElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, cx| {
            let hovered = hitbox.is_hovered(window);
            let now = Instant::now();
            // 同じ親の中に並ぶ部品を、場所で見分ける
            let spot = ((f32::from(bounds.origin.x) as i64 as u64) << 32)
                ^ (f32::from(bounds.origin.y) as i64 as u64 & 0xffff_ffff);
            let name = match kind {
                Kind::Hover => "wash-hover",
                Kind::Press => "wash-press",
            };
            let still = cx.reduce_motion();
            let (value, moving, held) =
                window.with_global_id(ElementId::NamedInteger(name.into(), spot), |id, window| {
                    window.with_element_state(id, |state: Option<Fade>, _| {
                        let mut state = state.unwrap_or_else(|| Fade {
                            from: 0.,
                            to: 0.,
                            since: now.checked_sub(FADE).unwrap_or(now),
                            held: Rc::default(),
                        });
                        let on = match kind {
                            Kind::Hover => hovered,
                            Kind::Press => hovered && state.held.get(),
                        };
                        let to = if on { 1. } else { 0. };
                        if to != state.to {
                            state.from = state.value(now);
                            state.to = to;
                            state.since = now;
                        }
                        let value = if still { to } else { state.value(now) };
                        let moving = !still && state.moving(now);
                        ((value, moving, state.held.clone()), state)
                    })
                });
            if moving {
                window.request_animation_frame();
            }
            if value > 0.002 {
                let radii = match &radii {
                    Some(radii) => {
                        let rem = window.rem_size();
                        let corner = |length: Option<AbsoluteLength>| {
                            length.map_or(px(0.), |length| length.to_pixels(rem))
                        };
                        Corners {
                            top_left: corner(radii.top_left),
                            top_right: corner(radii.top_right),
                            bottom_right: corner(radii.bottom_right),
                            bottom_left: corner(radii.bottom_left),
                        }
                    }
                    None => Corners::all(cx.theme().radius),
                }
                .clamp_radii_for_quad_size(bounds.size);
                let square = radii == Corners::all(px(0.));
                let look = match kind {
                    Kind::Hover => Look::hover(color, on_top, square, cx),
                    Kind::Press => Look::press(color, cx),
                };
                look.paint(bounds, radii, value, window);
            }

            // ポインターが出入りしたら、描き直す
            let view = window.current_view();
            window.on_mouse_event({
                let hitbox = hitbox.clone();
                move |_: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) != hovered {
                        cx.notify(view);
                    }
                }
            });
            if kind == Kind::Press {
                window.on_mouse_event({
                    let (hitbox, held) = (hitbox.clone(), held.clone());
                    move |event: &MouseDownEvent, phase, window, cx| {
                        if phase == DispatchPhase::Bubble
                            && event.button == MouseButton::Left
                            && hitbox.is_hovered(window)
                        {
                            held.set(true);
                            cx.notify(view);
                        }
                    }
                });
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase == DispatchPhase::Capture && held.get() {
                        held.set(false);
                        cx.notify(view);
                    }
                });
            }
        },
    )
    .absolute()
    .inset_0()
}

/// `feel` で乗せたときの面を、乗せたままの濃さで描く（「設定」のプレビュー）。
/// 親の部品いっぱいに敷く。`radius` は親の角の丸みで、0 なら一覧の行の描き方になる。
pub(crate) fn hover_sample(feel: Feel, color: Hsla, radius: Pixels) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, cx| {
            let radii = Corners::all(radius).clamp_radii_for_quad_size(bounds.size);
            Look::hover_as(feel, color, false, radius == px(0.), cx)
                .paint(bounds, radii, 1., window);
        },
    )
    .absolute()
    .inset_0()
}

/// 乗せたときの面を、テーマに合わせて付ける。
/// 中身の後ろに敷くので、角の丸みを決めたあと、子を足す前に呼ぶ。
pub(crate) trait Lit: Styled + ParentElement + Sized {
    fn lit(mut self, color: Hsla, _cx: &App) -> Self {
        let radii = self.style().corner_radii.clone();
        self.child(wash(Kind::Hover, color, Some(radii), false))
    }
}
impl<E: Styled + ParentElement> Lit for E {}

/// 押している間の面を、テーマに合わせて付ける。呼ぶ場所は `lit` と同じ。
pub(crate) trait Sunk: Styled + ParentElement + Sized {
    fn sunk(mut self, color: Hsla, _cx: &App) -> Self {
        let radii = self.style().corner_radii.clone();
        self.child(wash(Kind::Press, color, Some(radii), false))
    }
}
impl<E: Styled + ParentElement> Sunk for E {}

/// ライブラリのボタンに、ガラスの反射と縁の光を重ねる。ガラスのテーマのときだけ箱で包む。
pub(crate) fn gloss(button: Button) -> AnyElement {
    gloss_when(true, button)
}

/// 丸いボタン（`.rounded(px(999.))`）に重ねる。ボタンの丸みは外から読めないので、ここで合わせる。
pub(crate) fn gloss_round(button: Button) -> AnyElement {
    gloss_when(true, button.rounded_full())
}

/// `enabled` が偽（押せないボタン）のときは、何も重ねない。
pub(crate) fn gloss_when(enabled: bool, mut button: Button) -> AnyElement {
    if !enabled || crate::theme::feel_now() != Feel::Glass {
        return button.into_any_element();
    }
    // 置き場所を決める指定は、外側の箱へ移す。箱の大きさはボタンと同じになる
    let inner = button.style();
    // 丸みを自分で決めているボタン（丸いボタン）は、その丸みで描く
    let radii = inner
        .corner_radii
        .top_left
        .is_some()
        .then(|| inner.corner_radii.clone());
    // 「親いっぱい」の幅と高さは箱にも付ける（ボタンは箱いっぱいになる）
    let relative = |length: Option<Length>| {
        length.filter(|length| matches!(length, Length::Definite(DefiniteLength::Fraction(_))))
    };
    let outer = StyleRefinement {
        margin: std::mem::take(&mut inner.margin),
        inset: std::mem::take(&mut inner.inset),
        position: inner.position.take(),
        flex_grow: inner.flex_grow.take(),
        flex_shrink: inner.flex_shrink.take().or(Some(0.)),
        flex_basis: inner.flex_basis.take(),
        align_self: inner.align_self.take(),
        visibility: inner.visibility,
        size: SizeRefinement {
            width: relative(inner.size.width),
            height: relative(inner.size.height),
        },
        ..StyleRefinement::default()
    };
    let mut frame = div();
    *frame.style() = outer;
    frame
        .flex()
        .child(button.flex_grow(1.))
        .child(wash(Kind::Hover, white(1.), radii.clone(), true))
        .child(wash(Kind::Press, white(1.), radii, true))
        .into_any_element()
}

/// 枠つきのボタンの、乗せたときと押している間の色。部品が 1 色しか受け取らないので、面の色だけを変える。
pub(crate) fn button(hover: Hsla, active: Hsla, cx: &App) -> (Hsla, Hsla) {
    let theme = cx.theme();
    match crate::theme::feel(cx) {
        Feel::Standard => (hover, active),
        // 反射と光は `gloss` が重ねる
        Feel::Glass => (theme.background, theme.background),
        Feel::Crisp => (
            theme.background.blend(theme.foreground.opacity(0.08)),
            theme.background.blend(theme.foreground.opacity(0.16)),
        ),
        Feel::Tint => (
            theme.background.blend(theme.primary.opacity(0.14)),
            theme.background.blend(theme.primary.opacity(0.28)),
        ),
    }
}

/// 選んでいる行や項目の面。角の丸みを決めたあとに呼ぶ。
pub(crate) fn selected<E: Styled>(mut el: E, color: Hsla, cx: &App) -> E {
    let theme = cx.theme();
    let dark = theme.mode.is_dark();
    let square = el.style().corner_radii.top_left.is_none();
    match crate::theme::feel(cx) {
        Feel::Standard => el.bg(color),
        Feel::Glass if square => el.bg(white(if dark { 0.1 } else { 0.4 })),
        Feel::Glass => {
            let (top, bottom) = if dark { (0.18, 0.1) } else { (0.7, 0.45) };
            el.bg(linear_gradient(
                180.,
                linear_color_stop(white(top), 0.),
                linear_color_stop(white(bottom), 1.),
            ))
            .shadow(ring(dark))
        }
        Feel::Crisp => el.bg(theme.foreground.opacity(0.12)),
        Feel::Tint => el.bg(theme.primary.opacity(0.2)),
    }
}

/// 部品の角の丸み。浮かせたパネルの中では、パネルの角に合わせて大きくする。
pub(crate) fn md() -> AbsoluteLength {
    if crate::theme::floating_now() {
        px(crate::theme::FLOAT_RADIUS).into()
    } else {
        rems(0.375).into()
    }
}

/// カードの角の丸み。
pub(crate) fn lg() -> AbsoluteLength {
    if crate::theme::floating_now() {
        px(crate::theme::FLOAT_RADIUS_LG).into()
    } else {
        rems(0.5).into()
    }
}

/// 浮かせたパネルの影。ガラスのときは、上の縁の光と、下に落ちるやわらかい影にする。
pub(crate) fn panel_shadow(cx: &App) -> Option<Vec<BoxShadow>> {
    let dark = cx.theme().mode.is_dark();
    (crate::theme::feel(cx) == Feel::Glass).then(|| {
        vec![
            edge(white(if dark { 0.14 } else { 0.6 }), 1., 0., true),
            edge(dark_ink(if dark { 0.3 } else { 0.1 }), 8., 24., false),
        ]
    })
}

/// 浮かせたパネルの縁。ガラスのときは細い光の線にする。
pub(crate) fn rim(cx: &App) -> Hsla {
    match crate::theme::feel(cx) {
        Feel::Glass => white(if cx.theme().mode.is_dark() { 0.22 } else { 0.7 }),
        _ => cx.theme().border,
    }
}
