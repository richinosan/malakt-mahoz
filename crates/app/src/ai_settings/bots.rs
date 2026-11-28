//! AI bot の一覧。名前・インストラクション・アイコン・色・動き・覚えたことを、その場で変える。
//!
//! 「＋」で「新しい bot」を足し、その場で名前を書く。行を押すと開く。
//! 名前は Enter かほかの場所を押すと、インストラクションはほかの場所を押すと保存し、Esc で書きかけを捨てて閉じる。

use crate::press::button;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_core::bots::{AiBot, BotPatch, Motion};
use malakt_core::Store;

use super::{col, px, row, AiSettings};
use crate::bot_look;
use crate::feel::{self, Lit};
use crate::render::app_icon;

/// 絞り込んでいないときに、アイコンの先頭に並べるもの。
const BOT_ICONS: [&str; 16] = [
    "bot",
    "bot-message-square",
    "sparkles",
    "brain",
    "cat",
    "dog",
    "rabbit",
    "bird",
    "ghost",
    "smile",
    "star",
    "heart",
    "rocket",
    "zap",
    "coffee",
    "flower",
];
/// 毎日の声かけで選べる時刻（毎時ちょうど）。
const CHECK_IN_HOURS: std::ops::RangeInclusive<u32> = 6..=22;
/// アイコンを選ぶところに出す数。
const ICONS_SHOWN: usize = 48;
/// 一覧の行のアイコンの大きさ。
const ROW_ICON: Pixels = px(18.);
/// 開いた bot の、アイコンの見本の大きさ。
const PREVIEW_ICON: Pixels = px(36.);
/// 選ぶところのアイコンの大きさ。
const PICK_ICON: Pixels = px(20.);

fn field_label(text: &'static str, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

impl AiSettings {
    /// データベースを変える。1 回の操作を、元に戻す 1 つの段にする。
    fn write_bots<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Store) -> malakt_core::Result<R>,
    ) -> Option<R> {
        let result = crate::db::shared(cx).update(cx, |db, cx| {
            let result = db.store.undo_mark().and_then(|()| f(&mut db.store));
            let marked = db.store.undo_mark();
            cx.notify();
            result.and_then(|value| marked.map(|()| value))
        });
        match result {
            Ok(value) => {
                self.bots_error = None;
                cx.notify();
                Some(value)
            }
            Err(error) => {
                self.bots_error = Some(error.to_string().into());
                cx.notify();
                None
            }
        }
    }

    fn bot(&self, id: &str, cx: &App) -> Option<AiBot> {
        crate::db::shared(cx).read(cx).store.ai_bot(id).ok()
    }

    /// 「新しい bot」を足して開き、名前を全部選んで書けるようにする。
    /// bot を 1 つ足して開く。足した bot を返す（足せなければ None、失敗は設定の中に出る）。
    pub fn add_bot(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AiBot> {
        self.save_open_bot(cx);
        let bot = self.write_bots(cx, |s| s.create_ai_bot("", ""))?;
        self.open_bot_now(&bot, window, cx);
        let input = self.bot_name.clone();
        // メニューやボタンからフォーカスが戻ったあとに移す
        cx.defer_in(window, move |this, window, cx| {
            if this.bot_open.is_none() {
                return;
            }
            input.update(cx, |s, cx| {
                s.focus(window, cx);
                s.select_all(window, cx);
            });
        });
        Some(bot)
    }

    /// その bot を開く（AI bot の画面の「bot を編集」から）。
    pub fn open_bot(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.bot_open.as_deref() == Some(id) {
            return;
        }
        self.save_open_bot(cx);
        if let Some(bot) = self.bot(id, cx) {
            self.open_bot_now(&bot, window, cx);
        }
    }

    fn open_bot_now(&mut self, bot: &AiBot, window: &mut Window, cx: &mut Context<Self>) {
        self.bot_open = Some(bot.id.clone());
        self.bot_icons_open = false;
        let (name, text) = (bot.name.clone(), bot.instructions.clone());
        self.bot_name
            .update(cx, |s, cx| s.set_value(name, window, cx));
        self.bot_text
            .update(cx, |s, cx| s.set_value(text, window, cx));
        cx.notify();
    }

    /// 行を押した。閉じているなら開き、開いているなら保存して閉じる。
    pub fn toggle_bot(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let same = self.bot_open.as_deref() == Some(id);
        self.save_open_bot(cx);
        if same {
            self.bot_open = None;
            cx.notify();
            return;
        }
        if let Some(bot) = self.bot(id, cx) {
            self.open_bot_now(&bot, window, cx);
        }
    }

    fn save_open_bot(&mut self, cx: &mut Context<Self>) {
        self.save_bot_name(cx);
        self.save_bot_text(cx);
    }

    /// 書いた名前を保存する。空にしたときと変えていないときは、今の名前のまま。
    pub(super) fn save_bot_name(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.bot_open.clone() else {
            return;
        };
        let Some(bot) = self.bot(&id, cx) else {
            return;
        };
        let name = self.bot_name.read(cx).value().trim().to_owned();
        if name.is_empty() || name == bot.name {
            return;
        }
        self.set_bot(
            &id,
            BotPatch {
                name: Some(name),
                ..BotPatch::default()
            },
            cx,
        );
    }

    pub(super) fn save_bot_text(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.bot_open.clone() else {
            return;
        };
        let Some(bot) = self.bot(&id, cx) else {
            return;
        };
        let text = self.bot_text.read(cx).value().to_string();
        if text.trim() == bot.instructions {
            return;
        }
        self.set_bot(
            &id,
            BotPatch {
                instructions: Some(text),
                ..BotPatch::default()
            },
            cx,
        );
    }

    /// Esc。アイコンを選んでいたらそれを閉じ、そうでなければ書きかけを捨てて bot を閉じる。
    pub fn cancel_bot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bot_icons_open {
            self.bot_icons_open = false;
            cx.notify();
            return;
        }
        let Some(id) = self.bot_open.take() else {
            return;
        };
        if let Some(bot) = self.bot(&id, cx) {
            self.bot_name
                .update(cx, |s, cx| s.set_value(bot.name, window, cx));
            self.bot_text
                .update(cx, |s, cx| s.set_value(bot.instructions, window, cx));
        }
        self.bots_error = None;
        cx.notify();
    }

    fn set_bot(&mut self, id: &str, patch: BotPatch, cx: &mut Context<Self>) {
        let id = id.to_owned();
        self.write_bots(cx, |s| s.update_ai_bot(&id, &patch));
    }

    fn delete_bot(&mut self, id: &str, cx: &mut Context<Self>) {
        let id = id.to_owned();
        if self.write_bots(cx, |s| s.delete_ai_bot(&id)).is_some()
            && self.bot_open.as_deref() == Some(id.as_str())
        {
            self.bot_open = None;
        }
    }

    fn forget(&mut self, id: &str, fact: &str, cx: &mut Context<Self>) {
        let (id, fact) = (id.to_owned(), fact.to_owned());
        self.write_bots(cx, |s| s.forget_ai_bot(&id, &fact));
    }

    /// bot の一覧。
    pub fn bots_editor(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let bots = match crate::db::shared(cx).read(cx).store.ai_bots() {
            Ok(bots) => bots,
            Err(error) => {
                return div()
                    .text_sm()
                    .text_color(theme.danger)
                    .child(format!("AI bot を読めませんでした: {error}"))
                    .into_any_element();
            }
        };
        let add = feel::gloss(
            button("ai-bot-add")
                .icon(app_icon("plus"))
                .tooltip("bot を追加")
                .ghost()
                .small()
                .on_click(cx.listener(|this, _, window, cx| {
                    this.add_bot(window, cx);
                })),
        );
        let mut list = col()
            .id("ai-bots")
            .rounded(feel::md())
            .border_1()
            .border_color(theme.border)
            .overflow_hidden();
        let single = bots.len() <= 1;
        for (ix, bot) in bots.iter().enumerate() {
            list = list.child(self.bot_row(ix, bot, single, cx));
        }
        col()
            .w_full()
            .gap_2()
            .child(row().child(div().flex_1()).child(add))
            .child(list.test_support())
            .when_some(self.bots_error.clone(), |d, error| {
                d.child(
                    div()
                        .id("ai-bots-error")
                        .text_xs()
                        .text_color(theme.danger)
                        .child(error)
                        .test_support(),
                )
            })
            .into_any_element()
    }

    fn bot_row(&self, ix: usize, bot: &AiBot, single: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let open = self.bot_open.as_deref() == Some(bot.id.as_str());
        let id = bot.id.clone();
        let group = SharedString::from(format!("ai-bot-{ix}"));
        // 名前のところ。開いているときは、同じ大きさ・位置のまま入力欄になる
        let name = if open {
            div()
                .flex_1()
                .min_w_0()
                .mx(px(-8.))
                .child(
                    Input::new(&self.bot_name)
                        .id("ai-bot-name")
                        .aria_label("bot の名前")
                        .text_sm()
                        .bg(theme.background)
                        .border_color(theme.ring)
                        .px_2(),
                )
                .into_any_element()
        } else {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_sm()
                .child(bot.name.clone())
                .into_any_element()
        };
        let head = row()
            .id(("ai-bot", ix))
            .flex_1()
            .min_w_0()
            .gap_2()
            .py_1()
            .cursor_pointer()
            .child(
                div()
                    .id(("ai-bot-fold", ix))
                    .flex_none()
                    .child(
                        app_icon(if open {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        })
                        .small()
                        .text_color(theme.muted_foreground),
                    )
                    .test_support(),
            )
            .child(bot_look::avatar(
                ("ai-bot-icon", ix),
                bot,
                ROW_ICON,
                false,
                cx,
            ))
            .child(name)
            .on_click(cx.listener({
                let id = id.clone();
                move |this, _: &ClickEvent, window, cx| {
                    // 開いている間に名前の欄を押したときは、書き続ける
                    if this.bot_open.as_deref() == Some(id.as_str())
                        && this.bot_name.read(cx).focus_handle(cx).is_focused(window)
                    {
                        return;
                    }
                    this.toggle_bot(&id, window, cx)
                }
            }))
            .test_support();
        // 最後の 1 つは消せないので、消すボタンを出さない
        let delete = (!single).then(|| {
            feel::gloss(
                button(("ai-bot-delete", ix))
                    .icon(app_icon("trash"))
                    .tooltip("削除")
                    .ghost()
                    .xsmall()
                    // 乗せたときと、開いているときだけ見せる
                    .when(!open, |b| {
                        b.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.))
                    })
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| this.delete_bot(&id, cx)
                    })),
            )
        });
        let line = row()
            .group(group)
            .gap_1()
            .px_3()
            .min_h(px(36.))
            .lit(theme.muted, cx)
            .child(head)
            .children(delete);
        col()
            .when(ix > 0, |d| d.border_t_1().border_color(theme.border))
            .when(open, |d| d.bg(theme.accent.opacity(0.4)))
            .child(line)
            .when(open, |d| d.child(self.bot_body(bot, cx)))
            // Esc は、書きかけを捨てて bot を閉じる。設定のウィンドウは閉じない
            .on_action(
                cx.listener(|this, _: &crate::Dismiss, window, cx| this.cancel_bot(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &gpui_kit::base::actions::Cancel, window, cx| {
                    this.cancel_bot(window, cx)
                }),
            )
            .into_any_element()
    }

    /// 開いた bot の中身。インストラクション、アイコンと色、動き、覚えたこと。
    fn bot_body(&self, bot: &AiBot, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let id = bot.id.clone();

        // アイコンと色。見本は選んだ動きで動く
        let mut colors = row().gap_1().flex_wrap();
        let all =
            std::iter::once(None).chain(malakt_core::WORKSPACE_COLORS.iter().copied().map(Some));
        for (ix, key) in all.enumerate() {
            let selected = bot.color.as_deref() == key;
            let id = id.clone();
            colors = colors.child(
                div()
                    .id(("ai-bot-color", ix))
                    .size_7()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(feel::md())
                    .cursor_pointer()
                    .border_1()
                    .border_color(if selected {
                        theme.foreground
                    } else {
                        theme.transparent
                    })
                    .lit(theme.secondary_hover, cx)
                    .child(
                        div()
                            .size_4()
                            .rounded_full()
                            .bg(crate::render::workspace_color(key).unwrap_or(theme.foreground)),
                    )
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_bot(
                            &id,
                            BotPatch {
                                color: Some(key.map(str::to_owned)),
                                ..BotPatch::default()
                            },
                            cx,
                        )
                    }))
                    .test_support(),
            );
        }
        let preview = div()
            .id("ai-bot-icon-pick")
            .size(PREVIEW_ICON + px(12.))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .rounded(feel::md())
            .cursor_pointer()
            .border_1()
            .border_color(if self.bot_icons_open {
                theme.ring
            } else {
                theme.border
            })
            .bg(theme.background)
            .lit(theme.secondary_hover, cx)
            .child(bot_look::avatar(
                "ai-bot-preview",
                bot,
                PREVIEW_ICON,
                true,
                cx,
            ))
            .tooltip(|window, cx| {
                gpui_kit::component::tooltip::Tooltip::new("アイコンを変える").build(window, cx)
            })
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.bot_icons_open = !this.bot_icons_open;
                if this.bot_icons_open {
                    let input = this.bot_icon_query.clone();
                    input.update(cx, |s, cx| s.focus(window, cx));
                }
                cx.notify();
            }))
            .test_support();
        let look = row().gap_3().items_start().child(preview).child(colors);

        let icons = self.bot_icons_open.then(|| self.icon_picker(bot, cx));

        // 動き。ボタンの中のアイコンが、その動きで動く
        let mut motions = row().gap_1().flex_wrap();
        for (ix, motion) in Motion::ALL.into_iter().enumerate() {
            let selected = bot.motion == motion;
            let id = id.clone();
            motions = motions.child(
                col()
                    .id(("ai-bot-motion", ix))
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .rounded(feel::md())
                    .cursor_pointer()
                    .border_1()
                    .border_color(if selected {
                        theme.foreground
                    } else {
                        theme.transparent
                    })
                    .lit(theme.secondary_hover, cx)
                    .child(bot_look::look(
                        ("ai-bot-motion-look", ix),
                        bot.icon_name(),
                        bot.color.as_deref(),
                        motion,
                        PICK_ICON,
                        true,
                        cx,
                    ))
                    .child(div().text_xs().child(motion.label()))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_bot(
                            &id,
                            BotPatch {
                                motion: Some(motion),
                                ..BotPatch::default()
                            },
                            cx,
                        )
                    }))
                    .test_support(),
            );
        }

        // 覚えたこと。× で 1 つずつ消せる
        let memories: Vec<String> = bot.memories().map(str::to_owned).collect();
        let mut memory = col().id("ai-bot-memories").gap_1();
        for (ix, fact) in memories.iter().enumerate() {
            let (id, fact) = (id.clone(), fact.clone());
            memory = memory.child(
                row()
                    .gap_1()
                    .text_sm()
                    .child(div().flex_1().min_w_0().child(fact.clone()))
                    .child({
                        let forget = cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.forget(&id, &fact, cx)
                        });
                        crate::render::remove_x(("ai-bot-forget", ix), "忘れる", forget, cx)
                    }),
            );
        }

        // 毎日の声かけ。押すと時刻を選ぶメニューが開く
        let muted = theme.muted_foreground;
        let shown = match &bot.check_in {
            Some(time) => format!("毎日 {time}"),
            None => "しない".to_owned(),
        };
        let chosen = bot.check_in.clone();
        let settings = cx.entity().downgrade();
        let check_in_id = id.clone();
        let check_in = button("ai-bot-check-in")
            .outline()
            .h_8()
            .child(
                row()
                    .gap_2()
                    .child(div().child(shown))
                    .child(app_icon("chevron-down").small().text_color(muted)),
            )
            .pop_menu(move |mut menu, _, _| {
                let mut options: Vec<Option<String>> = vec![None];
                options.extend(CHECK_IN_HOURS.map(|h| Some(format!("{h:02}:00"))));
                // AI などで決めた半端な時刻も、選んだものとして並べる
                if chosen.is_some() && !options.contains(&chosen) {
                    options.push(chosen.clone());
                }
                for time in options {
                    let label = time.clone().unwrap_or_else(|| "しない".to_owned());
                    let (settings, id) = (settings.clone(), check_in_id.clone());
                    let checked = time == chosen;
                    menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                        move |_, _, cx| {
                            let time = time.clone();
                            // 設定を閉じたあとなら、反映する先がない
                            let _ = settings.update(cx, |this, cx| {
                                this.set_bot(
                                    &id,
                                    BotPatch {
                                        check_in: Some(time),
                                        ..BotPatch::default()
                                    },
                                    cx,
                                )
                            });
                        },
                    ));
                }
                menu
            });

        col()
            .id("ai-bot-body")
            .px_3()
            .pb_3()
            .gap_3()
            .child(
                col()
                    .gap_1()
                    .child(field_label("インストラクション", cx))
                    .child(
                        Textarea::new(&self.bot_text)
                            .h(px(140.))
                            .bg(theme.background),
                    ),
            )
            .child(
                col()
                    .gap_1()
                    .child(field_label("アイコン", cx))
                    .child(look)
                    .children(icons),
            )
            .child(col().gap_1().child(field_label("動き", cx)).child(motions))
            .child(
                col()
                    .gap_1()
                    .child(field_label("毎日の声かけ", cx))
                    .child(row().child(check_in)),
            )
            .when(!memories.is_empty(), |d| {
                d.child(
                    col()
                        .gap_1()
                        .child(field_label("覚えたこと", cx))
                        .child(memory.test_support()),
                )
            })
            .into_any_element()
    }

    /// アイコンを探して選ぶところ。自分で足した画像も並べる。
    fn icon_picker(&self, bot: &AiBot, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let query = self.bot_icon_query.read(cx).value().to_string();
        let found = crate::icons::search(&query, None, &BOT_ICONS, ICONS_SHOWN);
        let custom = if query.trim().is_empty() {
            match crate::db::shared(cx).read(cx).store.custom_icons() {
                Ok(names) => names,
                Err(error) => {
                    return col()
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.danger)
                                .child(format!("追加した画像を読めませんでした。{error}")),
                        )
                        .into_any_element();
                }
            }
        } else {
            Vec::new()
        };
        let chosen = bot.icon_name().to_owned();
        let mut grid = row().gap_1().flex_wrap();
        let names = custom
            .into_iter()
            .chain(found.into_iter().map(str::to_owned));
        for (ix, name) in names.enumerate() {
            let selected = chosen == name;
            let id = bot.id.clone();
            grid = grid.child(
                div()
                    .id(("ai-bot-icon-choice", ix))
                    .size(px(34.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(feel::md())
                    .cursor_pointer()
                    .border_1()
                    .border_color(if selected {
                        theme.foreground
                    } else {
                        theme.transparent
                    })
                    .lit(theme.secondary_hover, cx)
                    .child(bot_look::look(
                        ("ai-bot-icon-look", ix),
                        &name,
                        bot.color.as_deref(),
                        Motion::Still,
                        PICK_ICON,
                        false,
                        cx,
                    ))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_bot(
                            &id,
                            BotPatch {
                                icon: Some(Some(name.clone())),
                                ..BotPatch::default()
                            },
                            cx,
                        )
                    }))
                    .test_support(),
            );
        }
        col()
            .id("ai-bot-icons")
            .gap_2()
            .p_2()
            .rounded(feel::md())
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                Input::new(&self.bot_icon_query)
                    .id("ai-bot-icon-search")
                    .aria_label("アイコンを検索")
                    .small(),
            )
            .child(grid)
            .test_support()
            .into_any_element()
    }
}
