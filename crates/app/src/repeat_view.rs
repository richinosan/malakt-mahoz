//! タスクの詳細の「繰り返し」。押すとその場のメニューで、間隔・曜日・時刻を選ぶ。
//! 繰り返しがあるときは、回を作ったときに AI に頼むことをその場で書ける。

use crate::press::button;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::Input;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::repeat::{self, Rule, Unit, WEEKDAYS, WORKDAYS};

use crate::feel::{self, Lit};
use crate::render::{app_icon, px, remove_x, row};
use crate::state::{Detail, Editing, MainView};

/// 次の回を作る時刻のメニューに出す時（0 は日付が変わったとき）。
const HOURS: [u32; 11] = [0, 6, 7, 8, 9, 10, 12, 13, 15, 17, 18];

/// 間隔のメニューの 1 行の名前（「毎週」「2 週間ごと」）。
fn every_label(unit: Unit, every: u32) -> String {
    match (unit, every) {
        (Unit::Day, 1) => "毎日".into(),
        (Unit::Week, 1) => "毎週".into(),
        (Unit::Month, 1) => "毎月".into(),
        (Unit::Year, 1) => "毎年".into(),
        (unit, n) => format!("{n} {}ごと", unit.label()),
    }
}

/// メニューの 1 行。押すと繰り返しを `rule` にする（`None` は外す）。
fn choose(
    view: &WeakEntity<MainView>,
    label: impl Into<SharedString>,
    checked: bool,
    rule: Option<Rule>,
) -> PopupMenuItem {
    let view = view.clone();
    PopupMenuItem::new(label)
        .checked(checked)
        .on_click(move |_, _, cx| {
            let rule = rule.clone();
            // ウィンドウを閉じたあとなら、反映する先がない
            let _ = view.update(cx, |this, cx| this.set_repeat(rule, cx));
        })
}

fn repeat_menu(
    mut menu: PopupMenu,
    view: &WeakEntity<MainView>,
    base: &Rule,
    current: Option<&Rule>,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let is = |rule: &Rule| {
        current.is_some_and(|c| c.unit == rule.unit && c.every == 1 && c.weekdays == rule.weekdays)
    };
    menu = menu
        .item(choose(view, "なし", current.is_none(), None))
        .separator();
    let keep_time = |rule: Rule| Rule {
        time: base.time.clone(),
        ..rule
    };
    let presets = [
        Rule::daily(&base.from),
        Rule::workdays(&base.from),
        Rule::weekly(&base.from),
        Rule::monthly(&base.from),
        Rule::yearly(&base.from),
    ];
    for preset in presets {
        let checked = is(&preset);
        menu = menu.item(choose(
            view,
            preset.label(),
            checked,
            Some(keep_time(preset)),
        ));
    }
    let Some(current) = current.cloned() else {
        return menu;
    };
    menu = menu.separator().submenu("間隔", window, cx, {
        let (view, current) = (view.clone(), current.clone());
        move |mut menu, _, _| {
            for every in current.unit.choices().iter().copied() {
                let rule = Rule {
                    every,
                    ..current.clone()
                };
                menu = menu.item(choose(
                    &view,
                    every_label(current.unit, every),
                    current.every == every,
                    Some(rule),
                ));
            }
            menu
        }
    });
    if current.unit == Unit::Week {
        menu = menu.submenu("曜日", window, cx, {
            let (view, current) = (view.clone(), current.clone());
            move |mut menu, _, _| {
                for (day, name) in WEEKDAYS.iter().enumerate() {
                    let day = day as u8;
                    let on = current.weekdays.contains(&day);
                    let mut weekdays: Vec<u8> = current.weekdays.clone();
                    if on {
                        weekdays.retain(|d| *d != day);
                    } else {
                        weekdays.push(day);
                    }
                    // 曜日は 1 つ以上
                    let rule = (!weekdays.is_empty()).then(|| Rule {
                        weekdays,
                        ..current.clone()
                    });
                    let item = match rule {
                        Some(rule) => choose(&view, format!("{name}曜"), on, Some(rule)),
                        None => PopupMenuItem::new(format!("{name}曜")).checked(true),
                    };
                    menu = menu.item(item);
                }
                menu.separator().item(choose(
                    &view,
                    "平日",
                    current.weekdays == WORKDAYS,
                    Some(Rule {
                        weekdays: WORKDAYS.to_vec(),
                        ..current.clone()
                    }),
                ))
            }
        });
    }
    menu.submenu("時刻", window, cx, {
        let (view, current) = (view.clone(), current.clone());
        move |mut menu, _, _| {
            for hour in HOURS {
                let time = format!("{hour:02}:00");
                let value = (hour != 0).then(|| time.clone());
                menu = menu.item(choose(
                    &view,
                    format!("{hour}:00"),
                    current.time.as_deref().unwrap_or("00:00") == time,
                    Some(Rule {
                        time: value,
                        ..current.clone()
                    }),
                ));
            }
            menu
        }
    })
}

impl MainView {
    /// 詳細の「繰り返し」の値。押すとメニュー。付いていれば、次の回を作る日時を横に出す。
    pub(crate) fn repeat_property(&self, detail: &Detail, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let current = detail.repeat.as_ref().map(|r| r.rule.clone());
        let base = self
            .repeat_base()
            .unwrap_or_else(|| Rule::daily(&malakt_core::view::today()));
        let shown = current
            .as_ref()
            .map_or_else(|| "なし".to_owned(), Rule::label);
        let view = cx.entity().downgrade();
        let menu = button("task-repeat")
            .accessibility_label(format!("繰り返し: {shown}"))
            .outline()
            .small()
            .child(
                row()
                    .min_w_0()
                    .gap_2()
                    .child(
                        app_icon("repeat")
                            .small()
                            .text_color(theme.muted_foreground),
                    )
                    .child(div().min_w_0().truncate().child(shown))
                    .child(
                        app_icon("chevron-down")
                            .small()
                            .text_color(theme.muted_foreground),
                    ),
            )
            .pop_menu(move |menu, window, cx| {
                repeat_menu(menu, &view, &base, current.as_ref(), window, cx)
            });
        row()
            .gap_2()
            .flex_wrap()
            .child(div().flex().child(menu))
            .when_some(detail.repeat.as_ref(), |d, r| {
                d.child(
                    div()
                        .id("repeat-next")
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(repeat::next_label(&r.next_at))
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new("次の回").build(window, cx)
                        }),
                )
            })
    }

    /// 詳細の「AI への指示」。繰り返しがあるときだけ出す。押すとその場で書ける。
    pub(crate) fn repeat_ai_property(
        &self,
        detail: &Detail,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let r = detail.repeat.as_ref()?;
        let theme = cx.theme().clone();
        if self.editing == Some(Editing::RepeatAi) {
            // Enter かほかの場所を押すと保存、Esc で取り消し
            return Some(
                div()
                    .w_full()
                    .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                        this.save_repeat_ai(window, cx);
                    }))
                    .child(
                        Input::new(&self.inputs.repeat_ai)
                            .id("repeat-ai-input")
                            .aria_label("AI への指示")
                            .small()
                            .bg(theme.background)
                            .border_color(theme.ring),
                    ),
            );
        }
        if r.ai.is_empty() {
            return Some(
                row().child(feel::gloss(
                    button("repeat-ai-add")
                        .icon(app_icon("bot"))
                        .ghost()
                        .xsmall()
                        .tooltip("回を作ったときに AI に頼むこと")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.start_editing(Editing::RepeatAi, window, cx)
                        })),
                )),
            );
        }
        Some(
            row()
                .gap_1()
                .items_start()
                .child(
                    div()
                        .id("repeat-ai-text")
                        .flex_1()
                        .min_w_0()
                        .mx(px(-8.))
                        .px_2()
                        .py_0p5()
                        .rounded(feel::md())
                        .text_sm()
                        .cursor_text()
                        .lit(theme.secondary_hover, cx)
                        .child(r.ai.clone())
                        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                            this.start_editing(Editing::RepeatAi, window, cx)
                        }))
                        .test_support(),
                )
                .child(
                    remove_x(
                        "repeat-ai-remove",
                        "AI への指示を外す",
                        cx.listener(|this, _: &ClickEvent, _, cx| this.clear_repeat_ai(cx)),
                        cx,
                    )
                    .text_color(theme.muted_foreground)
                    .test_support(),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::every_label;
    use malakt_core::repeat::Unit;

    #[test]
    fn interval_labels() {
        assert_eq!(every_label(Unit::Week, 1), "毎週");
        assert_eq!(every_label(Unit::Week, 2), "2 週間ごと");
        assert_eq!(every_label(Unit::Month, 3), "3 か月ごと");
        assert_eq!(every_label(Unit::Day, 10), "10 日ごと");
    }
}
