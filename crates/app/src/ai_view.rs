//! AI チャットとシェルの画面。メインのウィンドウの右にも、別のウィンドウにも出せる。

use std::ops::Range;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::{ActiveTheme, Disableable, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_ai::keys::Source;
use malakt_ai::{Effort, Provider};

use crate::ai::{
    self, Decision, Item, KeyState, LineKind, Session, ShellPlace, ToolState, Watch, WATCH_MINUTES,
};
use crate::db::{self, Db};
use crate::state::MainView;
use crate::StopAi;

const CHAT_WIDTH: Pixels = px(380.);
const SHELL_WIDTH: Pixels = px(360.);
const SHELL_ROW: Pixels = px(20.);

/// チャットを右に置いても、タスクの画面に残す幅。これより狭いと、タスクの画面は
/// メニューをたたむ表示になる（`render.rs` の `wide`）。
pub const TASKS_MIN: Pixels = px(860.);
/// 右に置くチャットの、いちばん狭い幅。
const CHAT_MIN: Pixels = px(320.);

/// メインのウィンドウの右に置くときの幅。`None` なら全面に重ねる。
///
/// タスクの画面に `TASKS_MIN` を残せる範囲で広げる。シェルは、チャットの右に
/// 並べる幅があれば右に、なければチャットの下に置く。
pub fn docked_width(window_width: Pixels, shell_docked: bool) -> Option<Pixels> {
    let room = window_width - TASKS_MIN;
    if room < CHAT_MIN {
        None
    } else if shell_docked && room >= CHAT_MIN + SHELL_WIDTH {
        Some(room.min(CHAT_WIDTH + SHELL_WIDTH))
    } else {
        Some(room.min(CHAT_WIDTH))
    }
}

/// どこに出しているか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneMode {
    /// メインのウィンドウの右
    Docked,
    /// チャットだけのウィンドウ
    ChatWindow,
    /// シェルだけのウィンドウ
    ShellWindow,
}

pub struct AiPane {
    session: Entity<Session>,
    db: Entity<Db>,
    pub mode: PaneMode,
    /// 右に置いているときの、メインの画面。
    main: Option<WeakEntity<MainView>>,
    focus: FocusHandle,
    shell_focus: FocusHandle,
    input: Entity<TextareaState>,
    key_input: Entity<InputState>,
    model_input: Entity<InputState>,
    /// 定期確認の指示。
    watch_input: Entity<TextareaState>,
    /// モデルの入力欄に出している値。
    synced_model: String,
    pub settings_open: bool,
    chat_scroll: ScrollHandle,
    shell_scroll: UniformListScrollHandle,
    seen: (usize, usize),
    _subscriptions: Vec<Subscription>,
}

fn row() -> Div {
    div().flex().flex_row().items_center()
}

fn col() -> Div {
    div().flex().flex_col()
}

fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .px_2()
        .py_0p5()
        .rounded_md()
        .border_1()
        .text_xs()
        .cursor_pointer()
        .flex_none()
        .when(active, |d| {
            d.bg(theme.primary)
                .text_color(theme.primary_foreground)
                .border_color(theme.primary)
        })
        .when(!active, |d| {
            d.border_color(theme.border)
                .hover(|s| s.bg(theme.secondary_hover))
        })
        .child(label.into())
}

impl AiPane {
    pub fn new(
        mode: PaneMode,
        main: Option<WeakEntity<MainView>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let session = ai::shared(cx);
        let db = db::shared(cx);
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("AI に頼むことを書く（Ctrl / ⌘ + Enter で送信）")
        });
        let key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("API キー")
        });
        let model_input = cx.new(|cx| InputState::new(window, cx).placeholder("モデルの ID"));
        let instruction = session.read(cx).watch.instruction.clone();
        let watch_input = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).placeholder("AI への指示");
            state.set_value(instruction, window, cx);
            state
        });
        let synced_model = session.read(cx).model().to_owned();
        model_input.update(cx, |s, cx| s.set_value(synced_model.clone(), window, cx));

        let subscriptions = vec![
            cx.observe_in(&session, window, |this, _, window, cx| {
                this.sync(window, cx)
            }),
            cx.observe(&db, |_, _, cx| cx.notify()),
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(
                    event,
                    InputEvent::PressEnter {
                        secondary: true,
                        ..
                    }
                ) {
                    this.submit(window, cx);
                }
            }),
            cx.subscribe_in(
                &key_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.save_key(window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &model_input,
                window,
                |this, input, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        let model = input.read(cx).value().to_string();
                        this.synced_model = model.trim().to_owned();
                        this.session.update(cx, |s, cx| s.set_model(&model, cx));
                    }
                },
            ),
            // ウィンドウを閉じたら、メインのウィンドウの右に戻す
            cx.on_release(move |this, cx| {
                let (session, mode) = (this.session.clone(), this.mode);
                cx.defer(move |cx| {
                    session.update(cx, |s, cx| {
                        match mode {
                            PaneMode::ChatWindow => s.chat_window = None,
                            PaneMode::ShellWindow => {
                                if matches!(s.shell_place, ShellPlace::Window(_)) {
                                    s.shell_place = ShellPlace::Docked;
                                }
                            }
                            PaneMode::Docked => {}
                        }
                        cx.notify();
                    })
                });
            }),
        ];
        // キーがあるかを調べる（キーチェーンを読むだけで、通信はしない）
        if mode != PaneMode::ShellWindow && session.read(cx).key == KeyState::Unknown {
            session.update(cx, |s, cx| s.refresh_key(cx));
        }
        let seen = {
            let s = session.read(cx);
            (s.items.len(), s.shell.len())
        };
        Self {
            session,
            db,
            mode,
            main,
            focus: cx.focus_handle(),
            shell_focus: cx.focus_handle(),
            input,
            key_input,
            model_input,
            watch_input,
            synced_model,
            settings_open: false,
            chat_scroll: ScrollHandle::new(),
            shell_scroll: UniformListScrollHandle::new(),
            seen,
            _subscriptions: subscriptions,
        }
    }

    /// チャットの変化に合わせる。新しい行が出たら、いちばん下まで送る。
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (items, lines, model) = {
            let s = self.session.read(cx);
            (s.items.len(), s.shell.len(), s.model().to_owned())
        };
        if items != self.seen.0 {
            self.chat_scroll.scroll_to_bottom();
        }
        if lines != self.seen.1 && lines > 0 {
            self.shell_scroll
                .scroll_to_item(lines - 1, ScrollStrategy::Bottom);
        }
        self.seen = (items, lines);
        if model != self.synced_model {
            self.synced_model = model.clone();
            self.model_input
                .update(cx, |s, cx| s.set_value(model, window, cx));
        }
        cx.notify();
    }

    pub fn focus_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }

    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        let place = self
            .main
            .as_ref()
            .and_then(|main| main.read_with(cx, |main, _| main.ai_place()).ok())
            .unwrap_or_else(|| "ユーザーは、チャットだけのウィンドウを開いています。".to_owned());
        let sent = self.session.update(cx, |s, cx| s.send(&text, place, cx));
        if sent {
            self.input.update(cx, |s, cx| s.set_value("", window, cx));
        }
    }

    /// 定期確認の設定を、入力欄の指示と一緒に保存する。
    fn set_watch(&mut self, change: impl FnOnce(&mut Watch), cx: &mut Context<Self>) {
        let mut watch = self.session.read(cx).watch.clone();
        watch.instruction = self.watch_input.read(cx).value().to_string();
        change(&mut watch);
        self.session.update(cx, |s, cx| s.set_watch(watch, cx));
    }

    fn save_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.key_input.read(cx).value().to_string();
        self.session.update(cx, |s, cx| s.save_key(&key, cx));
        if self.session.read(cx).error.is_none() {
            self.key_input
                .update(cx, |s, cx| s.set_value("", window, cx));
        }
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        self.session.update(cx, |s, cx| s.stop(cx));
    }

    /// 別のウィンドウに出す。
    fn detach(&mut self, what: PaneMode, cx: &mut Context<Self>) {
        match crate::open_ai_window(what, cx) {
            Ok(handle) => self.session.update(cx, |s, cx| {
                match what {
                    PaneMode::ChatWindow => s.chat_window = Some(handle),
                    PaneMode::ShellWindow => s.shell_place = ShellPlace::Window(handle),
                    PaneMode::Docked => {}
                }
                cx.notify();
            }),
            Err(e) => self.session.update(cx, |s, cx| {
                s.error = Some(format!("ウィンドウを開けませんでした: {e}").into());
                cx.notify();
            }),
        }
    }

    /// シェルのボタン。閉じていれば開き、開いていれば閉じる。別ウィンドウなら手前に出す。
    fn toggle_shell(&mut self, cx: &mut Context<Self>) {
        let place = self.session.read(cx).shell_place;
        match place {
            ShellPlace::Window(handle) => {
                let shown = handle.update(cx, |_, window, _| window.activate_window());
                if shown.is_err() {
                    // そのウィンドウはもうない
                    self.set_shell_place(ShellPlace::Docked, cx);
                }
            }
            ShellPlace::Docked => self.set_shell_place(ShellPlace::Closed, cx),
            ShellPlace::Closed => self.set_shell_place(ShellPlace::Docked, cx),
        }
    }

    fn set_shell_place(&mut self, place: ShellPlace, cx: &mut Context<Self>) {
        self.session.update(cx, |s, cx| {
            s.shell_place = place;
            cx.notify();
        });
    }

    // ---- 設定 ----

    fn render_settings(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let theme = cx.theme().clone();
        let session = self.session.read(cx);
        let provider = session.provider;
        let model = session.model().to_owned();
        let label = |text: &'static str| {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(text)
        };

        let mut providers = row().gap_1().flex_wrap();
        for p in Provider::ALL {
            providers = providers.child(
                chip(("ai-provider", p as usize), p.label(), p == provider, cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.session.update(cx, |s, cx| s.set_provider(p, cx));
                    }))
                    .test_support(),
            );
        }

        let mut models = row().gap_1().flex_wrap();
        let listed = session.listed.clone();
        let presets = provider
            .models()
            .iter()
            .map(|m| SharedString::from(*m))
            .chain(listed.iter().cloned())
            .fold(Vec::<SharedString>::new(), |mut all, m| {
                if !all.contains(&m) {
                    all.push(m);
                }
                all
            });
        for (ix, m) in presets.into_iter().enumerate() {
            let active = m.as_ref() == model;
            let chosen = m.clone();
            models = models.child(
                chip(("ai-model", ix), m, active, cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.session.update(cx, |s, cx| s.set_model(&chosen, cx));
                    }))
                    .test_support(),
            );
        }

        let mut efforts = row().gap_1().flex_wrap();
        for e in Effort::ALL {
            efforts = efforts.child(
                chip(
                    ("ai-effort", e as usize),
                    e.label(),
                    e == session.effort,
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.session.update(cx, |s, cx| s.set_effort(e, cx));
                }))
                .test_support(),
            );
        }

        let (key_text, key_color, deletable): (SharedString, Hsla, bool) = match &session.key {
            KeyState::Unknown => ("確認中…".into(), theme.muted_foreground, false),
            KeyState::Missing => ("未設定".into(), theme.muted_foreground, false),
            KeyState::Present {
                masked,
                source: Source::Keychain,
            } => (
                format!("保存済み {masked}（OS のキーチェーン）").into(),
                theme.success,
                true,
            ),
            KeyState::Present {
                masked,
                source: Source::Env,
            } => (
                format!("環境変数 {} を使用 {masked}", provider.env_var()).into(),
                theme.success,
                false,
            ),
            KeyState::Failed(e) => (e.clone(), theme.danger, false),
        };

        let watch = session.watch.clone();
        let watch_status = session.watch_status.clone();
        let busy = session.busy;
        let auto_run = session.auto_run;
        let mut auto_row = row().gap_1().flex_wrap();
        for (ix, (text, on)) in [("毎回確認する", false), ("オート実行（確認なし）", true)]
            .into_iter()
            .enumerate()
        {
            auto_row = auto_row.child(
                chip(("ai-auto", ix), text, auto_run == on, cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.session.update(cx, |s, cx| s.set_auto_run(on, cx));
                    }))
                    .test_support(),
            );
        }
        let mut watch_row = row().gap_1().flex_wrap();
        for (ix, (text, on)) in [("オフ", false), ("オン", true)].into_iter().enumerate() {
            watch_row = watch_row.child(
                chip(("ai-watch", ix), text, watch.enabled == on, cx)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.set_watch(|w| w.enabled = on, cx);
                    }))
                    .test_support(),
            );
        }
        for minutes in WATCH_MINUTES {
            let text = if minutes < 60 {
                format!("{minutes} 分ごと")
            } else {
                format!("{} 時間ごと", minutes / 60)
            };
            watch_row = watch_row.child(
                chip(
                    ("ai-watch-minutes", minutes as usize),
                    text,
                    watch.minutes == minutes,
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_watch(|w| w.minutes = minutes, cx);
                }))
                .test_support(),
            );
        }

        col()
            .id("ai-settings")
            .gap_2()
            .p_3()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(label("サービス"))
            .child(providers)
            .child(label("モデル（ID を入力して Enter でも選べます）"))
            .child(models)
            .child(
                row()
                    .gap_2()
                    .child(
                        Input::new(&self.model_input)
                            .id("ai-model-input")
                            .aria_label("モデルの ID")
                            .small()
                            .flex_1(),
                    )
                    .when(
                        provider == Provider::Claude || provider.catalog_url().is_some(),
                        |d| {
                            d.child(
                                Button::new("ai-fetch-models")
                                    .label("一覧を取得")
                                    .xsmall()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.session.update(cx, |s, cx| s.fetch_models(cx));
                                    })),
                            )
                        },
                    ),
            )
            .when(provider == Provider::Cursor, |d| {
                d.child(label(
                    "Cursor は CLI（agent コマンド）で呼び出します。空にすると Cursor の標準のモデルになります。",
                ))
            })
            .when(provider.supports_effort(), |d| {
                d.child(label("effort（考える深さ）")).child(efforts)
            })
            .child(label("API キー"))
            .child(
                row()
                    .gap_2()
                    .child(
                        div()
                            .id("ai-key-state")
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(key_color)
                            .child(key_text)
                            .test_support(),
                    )
                    .when(deletable, |d| {
                        d.child(
                            Button::new("ai-key-delete")
                                .label("削除")
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.session.update(cx, |s, cx| s.delete_key(cx));
                                })),
                        )
                    }),
            )
            .child(
                row()
                    .gap_2()
                    .child(
                        Input::new(&self.key_input)
                            .id("ai-key-input")
                            .aria_label("API キー")
                            .small()
                            .flex_1(),
                    )
                    .child(
                        Button::new("ai-key-save")
                            .label("保存")
                            .primary()
                            .xsmall()
                            .on_click(cx.listener(|this, _, window, cx| this.save_key(window, cx))),
                    ),
            )
            .child(label(
                "API キーは OS のキーチェーンに保存します。エクスポートの zip には入りません。",
            ))
            .child(label("コマンドの実行とタスクの削除"))
            .child(auto_row)
            .child(label("GitHub の定期確認"))
            .child(watch_row)
            .when_some(watch_status, |d, status| {
                d.child(
                    div()
                        .id("ai-watch-status")
                        .text_xs()
                        .child(status)
                        .test_support(),
                )
            })
            .child(label("変化があったときの AI への指示"))
            .child(
                div()
                    .id("ai-watch-instruction")
                    .child(Textarea::new(&self.watch_input).h(px(72.)))
                    .test_support(),
            )
            .child(
                row()
                    .gap_2()
                    .child(
                        Button::new("ai-watch-save")
                            .label("指示を保存")
                            .outline()
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| this.set_watch(|_| {}, cx))),
                    )
                    .child(
                        Button::new("ai-watch-now")
                            .label("今すぐ確認")
                            .outline()
                            .xsmall()
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.session.update(cx, |s, cx| {
                                    s.check_github(cx);
                                });
                            })),
                    ),
            )
            .child(label(
                "オンの間、未完了のタスクに書かれた issue とプルリクエストのリンクを、選んだ間隔で見に行きます。変化があったときだけ AI がタスクを更新します。タスクの削除とコマンドの実行は行いません。",
            ))
            .overflow_y_scroll()
            .flex_shrink(1.)
            .min_h_0()
    }

    // ---- チャット ----

    fn render_item(&self, ix: usize, item: &Item, cx: &App) -> AnyElement {
        let theme = cx.theme();
        match item {
            Item::User(text) => row()
                .justify_end()
                .child(
                    div()
                        .id(("ai-user", ix))
                        .max_w(relative(0.9))
                        .px_3()
                        .py_2()
                        .rounded_lg()
                        .bg(theme.primary)
                        .text_color(theme.primary_foreground)
                        .text_sm()
                        .child(text.clone())
                        .test_support(),
                )
                .into_any_element(),
            Item::Assistant(text) => div()
                .id(("ai-assistant", ix))
                .text_sm()
                .child(text.clone())
                .test_support()
                .into_any_element(),
            Item::Tool { summary, state } => {
                let (mark, color, detail): (&str, Hsla, Option<SharedString>) = match state {
                    ToolState::Running => ("…", theme.muted_foreground, None),
                    ToolState::Done => ("✓", theme.success, None),
                    ToolState::Failed(e) => ("×", theme.danger, Some(e.clone())),
                    ToolState::Denied => (
                        "×",
                        theme.muted_foreground,
                        Some("許可しませんでした".into()),
                    ),
                };
                col()
                    .id(("ai-tool", ix))
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .border_1()
                    .border_color(theme.border)
                    .text_xs()
                    .child(
                        row()
                            .gap_2()
                            .child(div().flex_none().text_color(color).child(mark))
                            .child(div().flex_1().min_w_0().child(summary.clone())),
                    )
                    .when_some(detail, |d, detail| {
                        d.child(div().pl_5().text_color(color).child(detail))
                    })
                    .test_support()
                    .into_any_element()
            }
            Item::Notice(text) => div()
                .id(("ai-notice", ix))
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(text.clone())
                .test_support()
                .into_any_element(),
            Item::Error(text) => div()
                .id(("ai-error", ix))
                .px_2()
                .py_1()
                .rounded_md()
                .bg(theme.danger)
                .text_color(theme.danger_foreground)
                .text_sm()
                .child(text.clone())
                .test_support()
                .into_any_element(),
        }
    }

    fn render_chat(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let session = self.session.read(cx);
        let busy = session.busy;
        let auto_run = session.auto_run;
        let shell_label = match session.shell_place {
            ShellPlace::Closed => "シェルを開く",
            ShellPlace::Docked => "シェルを閉じる",
            ShellPlace::Window(_) => "シェルを手前に",
        };
        let title: SharedString = format!(
            "{} ・ {}{}",
            session.provider.label(),
            if session.model().is_empty() {
                "標準のモデル"
            } else {
                session.model()
            },
            match session.effort.wire() {
                Some(effort) => format!(" ・ {effort}"),
                None => String::new(),
            }
        )
        .into();

        let header = col()
            .flex_none()
            .border_b_1()
            .border_color(theme.border)
            .child(
                row()
                    .h_10()
                    .px_3()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_weight(FontWeight::BOLD)
                            .truncate()
                            .child("AI チャット"),
                    )
                    .child(
                        Button::new("ai-new-chat")
                            .label("新しい会話")
                            .ghost()
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.session.update(cx, |s, cx| s.clear(cx));
                            })),
                    )
                    .when(self.mode == PaneMode::Docked, |d| {
                        d.child(
                            Button::new("ai-detach")
                                .label("別ウィンドウ")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.detach(PaneMode::ChatWindow, cx)
                                })),
                        )
                        .child(
                            Button::new("ai-close")
                                .label("閉じる")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(main) = &this.main {
                                        // メインの画面がもうなければ、閉じる先もない
                                        let _ = main.update(cx, |main, cx| {
                                            main.ai_open = false;
                                            cx.notify();
                                        });
                                    }
                                })),
                        )
                    })
                    .when(self.mode == PaneMode::ChatWindow, |d| {
                        d.child(
                            Button::new("ai-attach")
                                .label("右に戻す")
                                .ghost()
                                .xsmall()
                                .on_click(|_, window, _| window.remove_window()),
                        )
                    }),
            )
            .child(
                row()
                    .px_3()
                    .pb_2()
                    .gap_1()
                    .child(
                        div()
                            .id("ai-current")
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(title)
                            .test_support(),
                    )
                    .when(auto_run, |d| {
                        d.child(
                            div()
                                .id("ai-auto-badge")
                                .flex_none()
                                .px_2()
                                .rounded_md()
                                .bg(theme.warning)
                                .text_color(theme.warning_foreground)
                                .text_xs()
                                .child("オート実行")
                                .test_support(),
                        )
                    })
                    .child(
                        Button::new("ai-settings-toggle")
                            .label(if self.settings_open {
                                "設定を閉じる"
                            } else {
                                "設定"
                            })
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_open = !this.settings_open;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("ai-shell-toggle")
                            .label(shell_label)
                            .xsmall()
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_shell(cx))),
                    ),
            );

        let mut list = col().gap_2().p_3();
        if session.items.is_empty() {
            let hint = match &session.key {
                KeyState::Present { .. } => {
                    "タスクの追加や整理を頼めます。GitHub のリンクを書くと、中身を取ってきます。"
                }
                _ => {
                    "使うには、「設定」で API キーを保存してください。保存するまで通信はしません。"
                }
            };
            list = list.child(
                div()
                    .id("ai-empty")
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(hint)
                    .test_support(),
            );
        }
        for (ix, item) in session.items.iter().enumerate() {
            list = list.child(self.render_item(ix, item, cx));
        }
        if busy && session.pending.is_none() {
            list = list.child(
                div()
                    .id("ai-working")
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(if session.command_running() {
                        "コマンドを実行しています…"
                    } else {
                        "考えています…"
                    })
                    .test_support(),
            );
        }

        let pending = session.pending.as_ref().map(|pending| {
            col()
                .id("ai-confirm")
                .flex_none()
                .gap_2()
                .m_3()
                .p_3()
                .rounded_md()
                .border_1()
                .border_color(theme.primary)
                .child(div().text_sm().child(pending.text.clone()))
                .child(
                    row()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            Button::new("ai-allow")
                                .label("実行する")
                                .primary()
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.session
                                        .update(cx, |s, cx| s.answer(Decision::Allow, cx));
                                })),
                        )
                        .when(pending.shell, |d| {
                            d.child(
                                Button::new("ai-allow-always")
                                    .label("この会話では確認なしで実行")
                                    .small()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.session.update(cx, |s, cx| {
                                            s.answer(Decision::AllowShellAlways, cx)
                                        });
                                    })),
                            )
                        })
                        .child(Button::new("ai-deny").label("実行しない").small().on_click(
                            cx.listener(|this, _, _, cx| {
                                this.session
                                    .update(cx, |s, cx| s.answer(Decision::Deny, cx));
                            }),
                        )),
                )
                .test_support()
        });
        let error = session.error.clone();

        col()
            .id("ai-chat")
            .key_context("AiChat")
            .on_action(cx.listener(|this, _: &StopAi, _, cx| this.stop(cx)))
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(header)
            .when(self.settings_open, |d| d.child(self.render_settings(cx)))
            .when_some(error, |d, error| {
                d.child(
                    div()
                        .id("ai-settings-error")
                        .flex_none()
                        .px_3()
                        .py_1()
                        .bg(theme.danger)
                        .text_color(theme.danger_foreground)
                        .text_xs()
                        .child(error)
                        .test_support(),
                )
            })
            .child(
                div()
                    .id("ai-messages")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.chat_scroll)
                    .child(list),
            )
            .children(pending)
            .child(
                col()
                    .flex_none()
                    .gap_2()
                    .p_3()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .id("ai-input")
                            .child(Textarea::new(&self.input).h(px(84.)))
                            .test_support(),
                    )
                    .child(
                        row()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("Ctrl / ⌘ + Enter で送信"),
                            )
                            .when(busy, |d| {
                                d.child(
                                    Button::new("ai-stop")
                                        .label("止める")
                                        .danger()
                                        .small()
                                        .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                                )
                            })
                            .child(
                                Button::new("ai-send")
                                    .label("送信")
                                    .primary()
                                    .small()
                                    .disabled(busy)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.submit(window, cx)),
                                    ),
                            ),
                    ),
            )
            .test_support()
            .track_focus(&self.focus)
            .into_any_element()
    }

    // ---- シェル ----

    fn render_shell(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let session = self.session.read(cx);
        let running = session.command_running();
        let count = session.shell.len();
        let detached = self.mode == PaneMode::ShellWindow;

        col()
            .id("ai-shell")
            .key_context("AiShell")
            .on_action(cx.listener(|this, _: &StopAi, _, cx| this.stop(cx)))
            .flex_1()
            .min_w_0()
            .min_h_0()
            .bg(theme.secondary)
            .child(
                row()
                    .flex_none()
                    .h_10()
                    .px_3()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_weight(FontWeight::BOLD)
                            .truncate()
                            .child("AI のシェル"),
                    )
                    .when(running, |d| {
                        d.child(
                            Button::new("ai-shell-kill")
                                .label("強制終了（Ctrl+C）")
                                .danger()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                        )
                    })
                    .when(!detached, |d| {
                        d.child(
                            Button::new("ai-shell-detach")
                                .label("別ウィンドウ")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.detach(PaneMode::ShellWindow, cx)
                                })),
                        )
                        .child(
                            Button::new("ai-shell-close")
                                .label("閉じる")
                                .ghost()
                                .xsmall()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.set_shell_place(ShellPlace::Closed, cx)
                                })),
                        )
                    })
                    .when(detached, |d| {
                        d.child(
                            Button::new("ai-shell-attach")
                                .label("チャットの右に戻す")
                                .ghost()
                                .xsmall()
                                .on_click(|_, window, _| window.remove_window()),
                        )
                    }),
            )
            .when(count == 0, |d| {
                d.child(
                    div()
                        .id("ai-shell-empty")
                        .p_3()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child("AI が実行したコマンドと、その出力がここに出ます。")
                        .test_support(),
                )
            })
            .when(count > 0, |d| {
                d.child(
                    uniform_list(
                        "ai-shell-lines",
                        count,
                        cx.processor(move |this, range: Range<usize>, _, cx| {
                            let theme = cx.theme().clone();
                            let session = this.session.read(cx);
                            range
                                .filter_map(|ix| session.shell.get(ix).map(|line| (ix, line)))
                                .map(|(ix, line)| {
                                    let color = match line.kind {
                                        LineKind::Command => theme.primary,
                                        LineKind::Out => theme.foreground,
                                        LineKind::Err => theme.danger,
                                        LineKind::Info => theme.muted_foreground,
                                    };
                                    div()
                                        .id(("ai-shell-line", ix))
                                        .h(SHELL_ROW)
                                        .px_3()
                                        .text_xs()
                                        .text_color(color)
                                        .truncate()
                                        .child(line.text.clone())
                                        .test_support()
                                        .into_any_element()
                                })
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.shell_scroll)
                    .flex_1()
                    .min_h_0(),
                )
            })
            .test_support()
            .track_focus(&self.shell_focus)
            .into_any_element()
    }
}

impl Render for AiPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        if !self.db.read(cx).unlocked {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.background)
                .text_color(theme.muted_foreground)
                .child("ロックされています。メインのウィンドウで解除してください。")
                .into_any_element();
        }
        let width = window.viewport_size().width;
        let shell_docked = self.session.read(cx).shell_place == ShellPlace::Docked;
        let base = div()
            .flex()
            .bg(theme.background)
            .text_color(theme.foreground);
        match self.mode {
            PaneMode::ShellWindow => base
                .size_full()
                .flex_col()
                .child(self.render_shell(cx))
                .into_any_element(),
            PaneMode::ChatWindow => {
                // ウィンドウが狭ければ、シェルはチャットの下に置く
                let side_by_side = width >= px(640.);
                base.size_full()
                    .when(side_by_side, |d| d.flex_row())
                    .when(!side_by_side, |d| d.flex_col())
                    .child(self.render_chat(cx))
                    .when(shell_docked, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .when(side_by_side, |d| {
                                    d.w(relative(0.45))
                                        .h_full()
                                        .border_l_1()
                                        .border_color(theme.border)
                                })
                                .when(!side_by_side, |d| {
                                    d.h(relative(0.38)).border_t_1().border_color(theme.border)
                                })
                                .child(self.render_shell(cx)),
                        )
                    })
                    .into_any_element()
            }
            PaneMode::Docked => {
                let docked = docked_width(width, shell_docked);
                let side_by_side = docked.is_some_and(|w| w > CHAT_WIDTH);
                base.h_full()
                    .flex_none()
                    .occlude()
                    .when_some(docked, |d, w| {
                        d.w(w).border_l_1().border_color(theme.border)
                    })
                    .when(docked.is_none(), |d| d.absolute().inset_0())
                    .when(side_by_side, |d| d.flex_row())
                    .when(!side_by_side, |d| d.flex_col())
                    .child(self.render_chat(cx))
                    .when(shell_docked, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .when(side_by_side, |d| {
                                    d.w(SHELL_WIDTH)
                                        .flex_none()
                                        .h_full()
                                        .border_l_1()
                                        .border_color(theme.border)
                                })
                                .when(!side_by_side, |d| {
                                    d.h(relative(0.38))
                                        .flex_none()
                                        .border_t_1()
                                        .border_color(theme.border)
                                })
                                .child(self.render_shell(cx)),
                        )
                    })
                    .into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{docked_width, px};

    #[test]
    fn the_pane_fits_the_window() {
        // 狭ければ全面に重ねる
        assert_eq!(docked_width(px(800.), true), None);
        assert_eq!(docked_width(px(1179.), false), None);
        // 最初のウィンドウの幅（1180）でも、タスクの画面に 860 が残る
        assert_eq!(docked_width(px(1180.), true), Some(px(320.)));
        assert_eq!(docked_width(px(1300.), true), Some(px(380.)));
        // 広ければ、シェルをチャットの右に並べる
        assert_eq!(docked_width(px(1539.), true), Some(px(380.)));
        assert_eq!(docked_width(px(1540.), true), Some(px(680.)));
        assert_eq!(docked_width(px(1700.), true), Some(px(740.)));
        assert_eq!(docked_width(px(1700.), false), Some(px(380.)));
    }
}
