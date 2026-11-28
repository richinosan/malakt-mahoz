//! AI bot のアイコン。選んだアイコンと色で描き、選んだ動き（[`Motion`]）で動かす。
//!
//! 動くのは、考えている間と、会話が空のときだけ。ほかのときは止めておく。

use std::f32::consts::PI;
use std::time::Duration;

use gpui_kit::component::Icon;
use gpui_kit::*;
use malakt_core::bots::{AiBot, Motion};

/// 1 回の動きの長さ（ミリ秒）。
fn period(motion: Motion) -> u64 {
    match motion {
        Motion::Still => 1000,
        Motion::Float => 2400,
        Motion::Bounce => 900,
        Motion::Sway => 1600,
        Motion::Spin => 1800,
        Motion::Pulse => 1200,
    }
}

/// 動きの途中（`t` は 0〜1 で繰り返す）の形。`size` はアイコンの大きさ。
pub fn transform(motion: Motion, t: f32, size: Pixels) -> Transformation {
    let wave = (2. * PI * t).sin();
    match motion {
        Motion::Still => Transformation::default(),
        Motion::Float => Transformation::translate(point(px(0.), size * (-0.12 * wave))),
        Motion::Bounce => {
            Transformation::translate(point(px(0.), size * (-0.25 * (PI * t).sin().abs())))
        }
        Motion::Sway => Transformation::rotate(radians(0.3 * wave)),
        Motion::Spin => Transformation::rotate(radians(2. * PI * t)),
        Motion::Pulse => {
            let s = 1. + 0.14 * wave;
            Transformation::scale(gpui_kit::size(s, s))
        }
    }
}

/// 画像のアイコンを動かすときの、上へのずれ。画像は傾けたり大きさを変えたりできないので、
/// 上下の動き（ふわふわ・ぴょんぴょん）だけにする。
fn lift(motion: Motion, t: f32, size: Pixels) -> Pixels {
    match motion {
        Motion::Float => size * (0.12 * (2. * PI * t).sin()),
        Motion::Bounce => size * (0.25 * (PI * t).sin().abs()),
        _ => px(0.),
    }
}

/// bot のアイコン。`moving` のときだけ、bot の動きで動く。
pub fn avatar(
    id: impl Into<ElementId>,
    bot: &AiBot,
    size: Pixels,
    moving: bool,
    cx: &App,
) -> AnyElement {
    look(
        id,
        bot.icon_name(),
        bot.color.as_deref(),
        bot.motion,
        size,
        moving,
        cx,
    )
}

/// アイコンの名前・色・動きから描く。設定で動きを選ぶボタンにも使う。
pub fn look(
    id: impl Into<ElementId>,
    icon: &str,
    color: Option<&str>,
    motion: Motion,
    size: Pixels,
    moving: bool,
    cx: &App,
) -> AnyElement {
    let moving = moving && motion != Motion::Still;
    let animation = Animation::new(Duration::from_millis(period(motion))).repeat();
    let custom = malakt_core::icon::is_custom(icon)
        .then(|| crate::icons::custom_image(icon, cx))
        .flatten();
    // 動いてもまわりが動かないように、同じ大きさの箱に入れる
    let frame = div()
        .size(size)
        .flex_none()
        .flex()
        .items_center()
        .justify_center();
    if let Some(image) = custom {
        let image = img(image).size(size).object_fit(ObjectFit::Contain);
        if !moving {
            return frame.child(image).into_any_element();
        }
        return frame
            .child(
                div()
                    .relative()
                    .child(image)
                    .with_animation(id, animation, move |d, t| d.top(-lift(motion, t, size))),
            )
            .into_any_element();
    }
    let mut icon = Icon::default()
        .path(crate::state::workspace_icon_path(Some(icon)))
        .size(size)
        .flex_none();
    if let Some(color) = crate::render::workspace_color(color) {
        icon = icon.text_color(color);
    }
    if !moving {
        return frame.child(icon).into_any_element();
    }
    frame
        .child(icon.with_animation(id, animation, move |icon, t| {
            icon.transform(transform(motion, t, size))
        }))
        .into_any_element()
}
