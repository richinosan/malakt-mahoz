//! 予想工数を選ぶパネル。値を押すとその場に開く。
//!
//! 上から、自由入力（打つと下に解釈を出す）、よく使う予想工数のチップ、月・日・時間・分ごとの −／＋ の順に並ぶ。
//! チップと −／＋ は押すとすぐ保存する（開いたまま続けて足せる）。選んでいるチップをもう 1 度押すと閉じる。
//! 打った文字は Enter と、欄を離れたときに保存する。Esc は書きかけを捨てて閉じ、外を押すと保存して閉じる。

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::estimate::{self, Estimate, Reading, Unit};
use malakt_core::Id;

use crate::feel;
use crate::press::button;
use crate::render::{app_icon, col, px, row};
use crate::state::MainView;

/// パネルの幅。
const PANEL_WIDTH: f32 = 320.;

/// どのタスクの予想工数か。`list` なら一覧のセル、そうでなければ詳細。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EstimateTarget {
    pub id: Id,
    pub list: bool,
}

/// 開いているパネル。
pub struct EstimatePick {
    pub target: EstimateTarget,
    /// 値のボタンの場所。外を押したかを見るときに、値のボタンは外に入れない（押すと閉じる）
    pub anchor: Rc<Cell<Bounds<Pixels>>>,
    /// 開いた回数。出てくる動きを毎回はじめから描く
    pub opens: usize,
    /// 入力欄に入れた値。欄の文字がこれと違えば書きかけ
    filled: Option<Estimate>,
}

impl MainView {
    /// 入力欄の、Enter と欄を離れたときの動き。`new` で 1 度だけ呼ぶ。
    pub(crate) fn subscribe_estimate_pick(
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.estimate_text_enter(window, cx),
                InputEvent::Blur => {
                    if this.estimate_pick.is_some() {
                        this.apply_estimate_text(true, window, cx);
                    }
                }
                InputEvent::Change => cx.notify(),
                _ => {}
            },
        )
    }

    /// いまの値。
    pub(crate) fn estimate_value(&self, target: &EstimateTarget) -> Option<Estimate> {
        self.detail
            .as_ref()
            .map(|d| &d.task)
            .filter(|t| t.id == target.id)
            .or_else(|| self.tasks.iter().find(|t| t.id == target.id))
            .and_then(|t| t.estimate)
    }

    /// 値を変えてすぐ保存する。保存できなかったときは `false`（エラーは画面に出る）。
    fn set_estimate_value(
        &mut self,
        target: &EstimateTarget,
        value: Option<Estimate>,
        cx: &mut Context<Self>,
    ) -> bool {
        let value = value.and_then(Estimate::non_zero);
        if self.estimate_value(target) == value {
            return true;
        }
        self.error = None;
        self.edit_task_by_id(&target.id, cx, |d| d.estimate = value);
        if self.error.is_some() {
            return false;
        }
        self.reload(cx);
        cx.notify();
        true
    }

    /// 入力欄を、いまの値に合わせる。
    fn fill_estimate_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pick) = &self.estimate_pick else {
            return;
        };
        let value = self.estimate_value(&pick.target);
        if let Some(pick) = &mut self.estimate_pick {
            pick.filled = value;
        }
        let text = value.map(|e| e.label()).unwrap_or_default();
        self.inputs
            .estimate
            .update(cx, |s, cx| s.set_value(text, window, cx));
    }

    /// AI やほかのウィンドウが値を変えたら、書きかけでない欄を合わせる。
    pub(crate) fn sync_estimate_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pick) = &self.estimate_pick else {
            return;
        };
        if self.estimate_value(&pick.target) == pick.filled || self.estimate_text_dirty(cx) {
            return;
        }
        self.fill_estimate_text(window, cx);
    }

    /// 値を押した。開いていれば閉じ、閉じていれば開く。
    pub(crate) fn toggle_estimate_pick(
        &mut self,
        target: EstimateTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .estimate_pick
            .as_ref()
            .is_some_and(|p| p.target == target)
        {
            self.close_estimate_pick(window, cx);
            return;
        }
        if self.estimate_pick.is_some() {
            self.close_estimate_pick(window, cx);
        }
        // カレンダーと同時には開かない
        if self.date_pick.is_some() {
            self.close_date_pick(window, cx);
        }
        self.estimate_opens += 1;
        self.estimate_pick = Some(EstimatePick {
            target,
            anchor: Rc::default(),
            opens: self.estimate_opens,
            filled: None,
        });
        self.fill_estimate_text(window, cx);
        // 開いた直後は欄に書ける。値のボタンからフォーカスが戻るのに負けないよう、後で移す
        let input = self.inputs.estimate.clone();
        cx.defer_in(window, move |_, window, cx| {
            input.update(cx, |s, cx| {
                s.focus(window, cx);
                s.select_all(window, cx);
            });
        });
        cx.notify();
    }

    /// 書きかけの文字を保存して閉じる。
    pub(crate) fn close_estimate_pick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.estimate_pick.is_none() {
            return;
        }
        self.apply_estimate_text(true, window, cx);
        self.estimate_pick = None;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// 閉じる（Esc）。選んだものはもう保存してあるので、書きかけの文字だけを捨てる。
    pub(crate) fn dismiss_estimate_pick(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(target) = self.estimate_pick.as_ref().map(|p| p.target.clone()) else {
            return false;
        };
        // 置いてあった詳細がもうないなら、閉じたことにして Esc を次へ回す
        let shown = target.list || self.detail.as_ref().is_some_and(|d| d.task.id == target.id);
        self.estimate_pick = None;
        if !shown {
            return false;
        }
        self.focus.focus(window, cx);
        cx.notify();
        true
    }

    /// 入力欄の文字が、いまの値と違う（書きかけ）か。
    fn estimate_text_dirty(&self, cx: &App) -> bool {
        let Some(pick) = &self.estimate_pick else {
            return false;
        };
        let text = self.inputs.estimate.read(cx).value().trim().to_owned();
        text != pick.filled.map(|e| e.label()).unwrap_or_default()
    }

    /// 入力欄の文字を値にする。読めなければ `false`（値は変えない）。
    /// `revert` なら、読めなかった文字を捨てていまの値に戻す（欄を離れたとき）。
    fn apply_estimate_text(
        &mut self,
        revert: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(target) = self.estimate_pick.as_ref().map(|p| p.target.clone()) else {
            return false;
        };
        if !self.estimate_text_dirty(cx) {
            return true;
        }
        let text = self.inputs.estimate.read(cx).value().to_string();
        let ok = match estimate::read(&text) {
            Reading::Empty => self.set_estimate_value(&target, None, cx),
            Reading::Value(e) => self.set_estimate_value(&target, Some(e), cx),
            Reading::Unreadable => false,
        };
        if ok || revert {
            self.fill_estimate_text(window, cx);
        }
        ok
    }

    /// 入力欄で Enter。読めたら閉じる。
    fn estimate_text_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.apply_estimate_text(false, window, cx) {
            cx.notify();
            return;
        }
        self.close_estimate_pick(window, cx);
    }

    /// チップを押した。選んでいるものをもう 1 度押したら閉じる。
    fn pick_estimate(&mut self, value: Estimate, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.estimate_pick.as_ref().map(|p| p.target.clone()) else {
            return;
        };
        if self.estimate_value(&target) == Some(value) {
            self.close_estimate_pick(window, cx);
            return;
        }
        self.set_estimate_value(&target, Some(value), cx);
        self.fill_estimate_text(window, cx);
    }

    /// 単位の −／＋。
    fn nudge_estimate(
        &mut self,
        unit: Unit,
        step: i32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.estimate_pick.as_ref().map(|p| p.target.clone()) else {
            return;
        };
        let next = self
            .estimate_value(&target)
            .unwrap_or_default()
            .nudge(unit, step);
        self.set_estimate_value(&target, Some(next), cx);
        self.fill_estimate_text(window, cx);
    }

    /// 「クリア」。予想工数をなくして閉じる。
    fn clear_estimate_pick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.estimate_pick.as_ref().map(|p| p.target.clone()) else {
            return;
        };
        if self.set_estimate_value(&target, None, cx) {
            self.estimate_pick = None;
            self.focus.focus(window, cx);
        }
        cx.notify();
    }

    /// 値のボタン。押すとその場にパネルが開く。開いている間は、ボタンの下にパネルを出す。
    pub(crate) fn estimate_trigger(
        &self,
        id: impl Into<ElementId>,
        target: EstimateTarget,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = self.estimate_pick.as_ref().filter(|p| p.target == target);
        let anchor = open.map(|p| p.anchor.clone());
        let theme = cx.theme().clone();
        let shown = match self.estimate_value(&target) {
            Some(e) => div().text_sm().child(e.label()),
            None if target.list => div(),
            None => div()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("未設定"),
        };
        let trigger = button(id)
            .ghost()
            .small()
            .h_auto()
            .min_h_6()
            .max_w_full()
            .py_0p5()
            .justify_start()
            .tooltip("予想工数を変更")
            .when(open.is_some(), |b| b.bg(theme.accent.opacity(0.4)))
            .child(
                row()
                    .id("value")
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .child(shown.min_w_0().truncate())
                    .test_support(),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.toggle_estimate_pick(target.clone(), window, cx)
            }));
        div()
            .relative()
            .flex()
            .max_w_full()
            .child(trigger)
            .when_some(anchor, |d, anchor| {
                d.child(
                    canvas(move |bounds, _, _| anchor.set(bounds), |_, _, _, _| {})
                        .absolute()
                        .inset_0(),
                )
                .child(
                    div().absolute().top_full().left_0().child(
                        deferred(
                            anchored()
                                .snap_to_window_with_margin(px(8.))
                                .child(div().pt_1().child(self.estimate_popover(cx))),
                        )
                        .with_priority(2),
                    ),
                )
            })
    }

    /// その場に開くパネル。外を押すと、書きかけを保存して閉じる。
    fn estimate_popover(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(pick) = &self.estimate_pick else {
            return div().into_any_element();
        };
        let theme = cx.theme().clone();
        let anchor = pick.anchor.clone();
        let opens = pick.opens;
        let value = self.estimate_value(&pick.target);
        let text = self.inputs.estimate.read(cx).value().to_string();
        let hint = {
            let line = row().min_h_4().text_xs();
            match estimate::read(&text) {
                Reading::Empty => line.text_color(theme.muted_foreground).child("未設定"),
                Reading::Value(e) => line.text_color(theme.foreground).child(e.label()),
                Reading::Unreadable => line
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.danger)
                    .child("解釈できません"),
            }
        };
        let mut chips = row().gap_1p5().flex_wrap();
        for (ix, chip) in estimate::common().into_iter().enumerate() {
            chips = chips.child(
                crate::date_pick::pill(
                    ("estimate-chip", ix),
                    chip.label(),
                    value == Some(chip),
                    false,
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.pick_estimate(chip, window, cx)
                }))
                .test_support(),
            );
        }
        let mut units = row()
            .gap_2()
            .pt_2p5()
            .border_t_1()
            .border_color(theme.border);
        let current = value.unwrap_or_default();
        for unit in Unit::ALL {
            let n = current.get(unit);
            let key = unit.key();
            let step = |sign: i32| {
                let icon = if sign < 0 { "minus" } else { "plus" };
                let tip = format!(
                    "{}{}{}",
                    unit.step(),
                    unit.label(),
                    if sign < 0 { "減らす" } else { "増やす" }
                );
                feel::gloss(
                    button(SharedString::from(format!("estimate-{key}-{icon}")))
                        .icon(app_icon(icon))
                        .tooltip(tip)
                        .ghost()
                        .xsmall()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.nudge_estimate(unit, sign, window, cx)
                        })),
                )
            };
            units = units.child(
                col()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(FontWeight::BOLD)
                            .text_color(theme.muted_foreground)
                            .child(unit.label()),
                    )
                    .child(
                        row()
                            .gap_0p5()
                            .items_center()
                            .child(step(-1))
                            .child(
                                div()
                                    .id(SharedString::from(format!("estimate-{key}")))
                                    .min_w(px(22.))
                                    .text_center()
                                    .text_sm()
                                    .when(n == 0, |d| d.text_color(theme.muted_foreground))
                                    .child(n.to_string())
                                    .test_support(),
                            )
                            .child(step(1)),
                    ),
            );
        }
        let panel = col()
            .id("estimate-pick")
            .occlude()
            .w(px(PANEL_WIDTH))
            .p_2()
            .gap_3()
            .rounded(feel::lg())
            .border_1()
            .border_color(feel::rim(cx))
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .shadow_lg()
            .on_mouse_down_out(
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    // 値のボタンを押したときは、ボタンのほうで閉じる
                    if anchor.get().contains(&event.position) {
                        return;
                    }
                    this.close_estimate_pick(window, cx);
                }),
            )
            .child(
                col()
                    .gap_1()
                    .child(
                        Input::new(&self.inputs.estimate)
                            .id("estimate-input")
                            .aria_label("予想工数")
                            .small(),
                    )
                    .child(hint),
            )
            .child(chips)
            .child(units)
            .child(
                row()
                    .pt_2()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(feel::gloss(
                        button("estimate-clear")
                            .label("クリア")
                            .outline()
                            .small()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.clear_estimate_pick(window, cx)
                            })),
                    )),
            )
            .test_support();
        if !self.motion {
            return panel.into_any_element();
        }
        // すっと下りて出る（カレンダーと同じ）
        panel
            .with_animation(
                ("estimate-pick-in", opens),
                Animation::new(std::time::Duration::from_millis(160)).with_easing(ease_out_quint()),
                |d, t| d.opacity(t).mt(px(-6.) * (1. - t)),
            )
            .into_any_element()
    }
}
