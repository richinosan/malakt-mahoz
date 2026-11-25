//! 影。面の高さを 3 段で表し、どの画面も同じ影を使う。

use gpui_kit::component::ActiveTheme;
use gpui_kit::{hsla, px, App, BoxShadow, Styled};

/// 面の高さ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Elevation {
    /// 背景に置いたカードやボタン。
    Low,
    /// 少し浮いたもの（選んだタブ、吹き出し）。
    Mid,
    /// 画面の上に重なるもの（パネル、ダイアログ、メニュー、知らせ、右下のボタン）。
    High,
}

/// その高さの影。暗い画面では背景に埋もれないよう、濃くする。
pub fn shadow(level: Elevation, cx: &App) -> Vec<BoxShadow> {
    // 背景の画像の上の面は透けるので、下に敷いた影が面の中に見えてしまう
    if level == Elevation::Low && crate::theme::backdrop(cx).is_some() {
        return Vec::new();
    }
    let dark = cx.theme().is_dark();
    let ink = |alpha: f32| hsla(0., 0., 0., if dark { alpha * 4.5 } else { alpha }.min(0.9));
    let layer = |y: f32, blur: f32, spread: f32, alpha: f32| {
        BoxShadow::new(px(0.), px(y), ink(alpha))
            .blur_radius(px(blur))
            .spread_radius(px(spread))
    };
    match level {
        Elevation::Low => vec![layer(1., 3., 0., 0.10), layer(1., 2., -1., 0.08)],
        Elevation::Mid => vec![layer(4., 10., -2., 0.12), layer(2., 4., -2., 0.08)],
        Elevation::High => vec![layer(12., 28., -6., 0.20), layer(4., 10., -4., 0.12)],
    }
}

/// 要素に高さの影を付ける。
pub trait Elevate: Styled + Sized {
    fn elevate(mut self, level: Elevation, cx: &App) -> Self {
        self.style().box_shadow = Some(shadow(level, cx));
        self
    }
}

impl<T: Styled> Elevate for T {}
