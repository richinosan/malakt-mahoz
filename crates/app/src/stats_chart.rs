//! 統計のダイアログの棒グラフ。
//!
//! 棒は終わったタスクの数で、予想工数に収まったもの（下）・超えたもの・予想工数のないもの（上）に分けて積む。
//! 色だけで分けないよう、凡例と、棒に乗せたときの説明に名前と数を出す。

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::ActiveTheme;
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::timing::Bar;

use crate::render::{col, px, row};

/// 棒を描く部分の高さ。
const PLOT_HEIGHT: f32 = 88.;
/// 下に出す目盛りの名前の数の上限。棒が多いときは間引く。
const MAX_LABELS: usize = 8;

/// 3 つの分け方の色（収まった・超えた・予想工数なし）。
fn colors(cx: &App) -> [Hsla; 3] {
    let theme = cx.theme();
    [
        theme.success,
        theme.danger,
        theme.muted_foreground.opacity(0.35),
    ]
}

fn swatch(color: Hsla) -> Div {
    div().size(px(10.)).flex_none().rounded(px(2.)).bg(color)
}

/// 凡例。グラフの上に 1 つだけ出す。
pub(crate) fn legend(cx: &App) -> Div {
    let muted = cx.theme().muted_foreground;
    let mut out = row().gap_4().flex_wrap().text_xs().text_color(muted);
    for (color, name) in
        colors(cx)
            .into_iter()
            .zip(["予想工数に収まった", "予想工数を超えた", "予想工数なし"])
    {
        out = out.child(
            row()
                .gap_1p5()
                .items_center()
                .child(swatch(color))
                .child(name),
        );
    }
    out
}

/// 棒に乗せたときの説明。
fn tip(bar: &Bar) -> String {
    let mut parts = Vec::new();
    if bar.within > 0 {
        parts.push(format!("収まった {}", bar.within));
    }
    if bar.over > 0 {
        parts.push(format!("超えた {}", bar.over));
    }
    if bar.unestimated > 0 {
        parts.push(format!("予想工数なし {}", bar.unestimated));
    }
    if parts.is_empty() {
        format!("{}: 0 件", bar.label)
    } else {
        format!("{}: {} 件（{}）", bar.label, bar.total(), parts.join("・"))
    }
}

/// 棒グラフ 1 つ。`id` は棒ごとの要素の名前（`(id, 何本目)`）にも使う。
pub(crate) fn bar_chart(id: &'static str, title: &'static str, bars: &[Bar], cx: &App) -> Div {
    let theme = cx.theme();
    let [within, over, unestimated] = colors(cx);
    let max = bars.iter().map(Bar::total).max().unwrap_or(0).max(1);
    // 目盛りの名前は、最後（今）から数えて等しく間引いたもの
    let every = bars.len().div_ceil(MAX_LABELS).max(1);
    let labelled = |ix: usize| (bars.len() - 1 - ix).is_multiple_of(every);
    let mut plot = row()
        .h(px(PLOT_HEIGHT))
        .items_end()
        .gap(px(2.))
        .border_b_1()
        .border_color(theme.border);
    let mut labels = row()
        .gap(px(2.))
        .text_xs()
        .text_color(theme.muted_foreground);
    for (ix, bar) in bars.iter().enumerate() {
        let height = |n: usize| px(PLOT_HEIGHT * n as f32 / max as f32);
        // 上から: 予想工数なし・超えた・収まった（区切りは 2px の隙間）
        let stack = col()
            .w_full()
            .gap(px(2.))
            .rounded_t(px(4.))
            .overflow_hidden()
            .when(bar.unestimated > 0, |d| {
                d.child(div().h(height(bar.unestimated)).bg(unestimated))
            })
            .when(bar.over > 0, |d| {
                d.child(div().h(height(bar.over)).bg(over))
            })
            .when(bar.within > 0, |d| {
                d.child(div().h(height(bar.within)).bg(within))
            });
        let text = tip(bar);
        plot = plot.child(
            div()
                .id((id, ix))
                .flex_1()
                .min_w_0()
                .h_full()
                .flex()
                .flex_col()
                .justify_end()
                .rounded_t(px(4.))
                .hover(|d| d.bg(theme.muted.opacity(0.6)))
                .child(stack)
                .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
                .test_support(),
        );
        // 名前は棒より広いことがあるので、棒の真ん中に置いてはみ出させる
        labels = labels.child(div().flex_1().min_w_0().flex().justify_center().when(
            labelled(ix),
            |d| {
                d.child(
                    div()
                        .flex_none()
                        .whitespace_nowrap()
                        .child(bar.label.clone()),
                )
            },
        ));
    }
    let top = bars.iter().map(Bar::total).max().unwrap_or(0);
    col()
        .flex_1()
        .min_w(px(220.))
        .gap_1p5()
        .child(
            row()
                .items_baseline()
                .gap_2()
                .child(div().flex_1().text_sm().child(title))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(format!("最大 {top} 件")),
                ),
        )
        .child(plot)
        .child(labels)
}
