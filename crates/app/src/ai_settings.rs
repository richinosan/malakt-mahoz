//! AI の設定。「設定」のウィンドウ（モデル・動作・GitHub のカテゴリ）と、AI チャットのペインの
//! 設定の両方に、同じ行を出す。どちらも自分の入力欄を持つので、1 つずつ作る。

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::scroll::ScrollableElement;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Disableable, Selectable, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_ai::keys::Source;
use malakt_ai::{Effort, Provider};

use crate::ai::{self, GithubAccount, KeyState, Session, Watch, WATCH_MINUTES};
use crate::ai_view::{appear, chip, icon_button};
use crate::feel::{self, Lit};
use crate::render::app_icon;
use crate::scroll::Menus;
use crate::shadow::{Elevate, Elevation};

mod rules;

/// 参照する寸法を、画面全体の拡大率に合わせる。
const fn px(value: f32) -> Pixels {
    gpui_kit::px(value * crate::UI_SCALE)
}

/// 行の右に置く、メニューを開くボタンの幅。
const CONTROL_WIDTH: Pixels = px(220.);

fn row() -> Div {
    div().flex().flex_row().items_center()
}

fn col() -> Div {
    div().flex().flex_col()
}

/// AI の設定のまとまり。「設定」のウィンドウでは、左のカテゴリ 1 つずつになる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiGroup {
    Model,
    Behavior,
    Rules,
    Github,
}

impl AiGroup {
    pub const ALL: [AiGroup; 4] = [
        AiGroup::Model,
        AiGroup::Behavior,
        AiGroup::Rules,
        AiGroup::Github,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AiGroup::Model => "モデル",
            AiGroup::Behavior => "動作",
            AiGroup::Rules => "ルール",
            AiGroup::Github => "GitHub",
        }
    }
}

/// AI の設定の 1 行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiRow {
    Service,
    Model,
    Effort,
    Coordinator,
    Key,
    AutoRun,
    Rules,
    Arrange,
    Track,
    Shell,
    Account,
    Watch,
    WatchMinutes,
    WatchInstruction,
}

impl AiRow {
    pub const ALL: [AiRow; 14] = [
        AiRow::Service,
        AiRow::Model,
        AiRow::Effort,
        AiRow::Coordinator,
        AiRow::Key,
        AiRow::AutoRun,
        AiRow::Arrange,
        AiRow::Track,
        AiRow::Shell,
        AiRow::Rules,
        AiRow::Account,
        AiRow::Watch,
        AiRow::WatchMinutes,
        AiRow::WatchInstruction,
    ];

    pub fn group(self) -> AiGroup {
        match self {
            AiRow::Service | AiRow::Model | AiRow::Effort | AiRow::Coordinator | AiRow::Key => {
                AiGroup::Model
            }
            AiRow::AutoRun | AiRow::Arrange | AiRow::Track | AiRow::Shell => AiGroup::Behavior,
            AiRow::Rules => AiGroup::Rules,
            AiRow::Account | AiRow::Watch | AiRow::WatchMinutes | AiRow::WatchInstruction => {
                AiGroup::Github
            }
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            AiRow::Service => "サービス",
            AiRow::Model => "モデル",
            AiRow::Effort => "effort",
            AiRow::Coordinator => "振り分けのモデル",
            AiRow::Key => "API キー",
            AiRow::AutoRun => "オート実行",
            AiRow::Rules => "ルール",
            AiRow::Arrange => "新しいタスクを整える",
            AiRow::Track => "コメントから進捗を更新する",
            AiRow::Shell => "AI のシェル",
            AiRow::Account => "アカウント",
            AiRow::Watch => "GitHub の定期チェック",
            AiRow::WatchMinutes => "間隔",
            AiRow::WatchInstruction => "AI への指示",
        }
    }

    pub fn note(self) -> Option<&'static str> {
        Some(match self {
            AiRow::Coordinator => "考え中に送ったメッセージを、続きか並列かに振り分ける",
            AiRow::AutoRun => "コマンド実行とタスク削除を確認なしで行う",
            AiRow::Rules => "AI がいつも従う指示",
            AiRow::Arrange => "タスクを作ると、AI がリンクを読んでタイトルと説明を整える",
            AiRow::Track => "タスクにコメントを書くと、AI が進捗・状態・期限を更新する",
            AiRow::Shell => "AI のタブで動かすシェル",
            AiRow::Account => "gh でログインしているアカウント",
            AiRow::Watch => {
                "タスクの issue・プルリクエストに進展があれば、AI がタスクにコメントする"
            }
            _ => return None,
        })
    }

    /// 検索で当たる、名前と補足のほかの言葉。
    pub fn words(self) -> &'static str {
        match self {
            AiRow::Service => "AI Claude OpenCode Cursor プロバイダー",
            AiRow::Model => "AI",
            AiRow::Effort => "AI 考える",
            AiRow::Key => "AI キー 鍵",
            AiRow::Shell => "ターミナル シェル",
            AiRow::Account => "gh ログイン",
            AiRow::Rules => "AI ルール 指示 前提 プロンプト ワークスペース",
            _ => "AI",
        }
    }

    /// 名前の下に、横いっぱいに出す行（入力欄や一覧）。
    pub fn wide(self) -> bool {
        matches!(
            self,
            AiRow::Service | AiRow::Model | AiRow::Key | AiRow::Rules | AiRow::WatchInstruction
        )
    }

    /// 今の設定で出す行。
    pub fn shown(self, cx: &App) -> bool {
        let session = ai::shared(cx).read(cx);
        match self {
            AiRow::Effort => session.provider.supports_effort(),
            AiRow::Coordinator => session.provider != Provider::Cursor,
            AiRow::WatchMinutes | AiRow::WatchInstruction => session.watch.enabled,
            _ => true,
        }
    }
}

/// AI の設定の入力欄と、モデルの一覧の開け閉め。
pub struct AiSettings {
    session: Entity<Session>,
    key_input: Entity<InputState>,
    model_input: Entity<InputState>,
    /// 定期チェックの指示。
    watch_input: Entity<TextareaState>,
    /// ルールの一覧で見ている場所。`None` はグローバル（すべてのワークスペース）。
    pub rules_scope: Option<String>,
    /// 開いている（名前と本文を書いている）ルール。
    rule_open: Option<String>,
    rule_name: Entity<InputState>,
    rule_body: Entity<TextareaState>,
    /// ルールを保存できなかった理由。
    rules_error: Option<SharedString>,
    /// モデルの一覧を開いている。
    pub model_open: bool,
    _subscriptions: Vec<Subscription>,
}

impl AiSettings {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let session = ai::shared(cx);
        let key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("API キー")
        });
        let model_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("検索、またはモデル ID を入力"));
        let instruction = session.read(cx).watch.instruction.clone();
        let watch_input = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).placeholder("AI への指示");
            state.set_value(instruction, window, cx);
            state
        });
        let rule_name = cx.new(|cx| InputState::new(window, cx));
        let rule_body = cx.new(|cx| {
            TextareaState::new(window, cx).placeholder("例: PR のタスクは優先度を高にする")
        });
        crate::term::shared(cx).update(cx, |terms, cx| terms.load_profiles(cx));
        let subscriptions = vec![
            cx.observe(&session, |_, _, cx| cx.notify()),
            cx.observe(&crate::term::shared(cx), |_, _, cx| cx.notify()),
            // AI や外のエージェントがルールを変えたら、一覧を読み直す
            cx.observe(&crate::db::shared(cx), |_, _, cx| cx.notify()),
            cx.subscribe_in(&rule_name, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.save_rule_name(cx);
                }
            }),
            cx.subscribe_in(&rule_body, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Blur) {
                    this.save_rule_body(cx);
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
                |this, input, event: &InputEvent, window, cx| match event {
                    // 絞り込んだ先頭のモデルを選ぶ。一覧になければ、入力した ID をそのまま使う
                    InputEvent::PressEnter { .. } => {
                        let typed = input.read(cx).value().trim().to_owned();
                        if typed.is_empty() {
                            return;
                        }
                        let model = this
                            .model_groups(cx)
                            .into_iter()
                            .flat_map(|(_, models)| models)
                            .next()
                            .map_or(typed, |m| m.to_string());
                        this.session.update(cx, |s, cx| s.set_model(&model, cx));
                        input.update(cx, |s, cx| s.set_value("", window, cx));
                        this.model_open = false;
                        cx.notify();
                    }
                    InputEvent::Change => cx.notify(),
                    _ => {}
                },
            ),
        ];
        Self {
            session,
            key_input,
            model_input,
            watch_input,
            rules_scope: None,
            rule_open: None,
            rule_name,
            rule_body,
            rules_error: None,
            model_open: false,
            _subscriptions: subscriptions,
        }
    }

    /// モデルの一覧を開く・閉じる。開くときは検索欄に書けるようにし、一覧がまだなければ取ってくる。
    pub fn toggle_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model_open = !self.model_open;
        if self.model_open {
            self.model_input.update(cx, |s, cx| s.focus(window, cx));
            self.session.update(cx, |s, cx| {
                if s.listed.is_empty() && matches!(s.key, KeyState::Present { .. }) {
                    s.fetch_models(cx);
                }
            });
        }
        cx.notify();
    }

    /// 定期チェックの設定を、入力欄の指示と一緒に保存する。
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

    /// 検索の言葉に合うモデル。おすすめを先に、残りは名前の頭でまとめる。
    fn model_groups(&self, cx: &App) -> Vec<(SharedString, Vec<SharedString>)> {
        let session = self.session.read(cx);
        let query = self.model_input.read(cx).value().trim().to_lowercase();
        let matches = |m: &str| query.is_empty() || m.to_lowercase().contains(&query);
        let presets: Vec<SharedString> = session
            .provider
            .models()
            .iter()
            .filter(|m| matches(m))
            .map(|m| SharedString::from(*m))
            .collect();
        let mut rest: Vec<SharedString> = Vec::new();
        let current = SharedString::from(session.model().to_owned());
        for m in session.listed.iter().chain(std::iter::once(&current)) {
            if !m.is_empty() && matches(m) && !presets.contains(m) && !rest.contains(m) {
                rest.push(m.clone());
            }
        }
        rest.sort();
        let mut groups: Vec<(SharedString, Vec<SharedString>)> = Vec::new();
        if !presets.is_empty() {
            groups.push(("おすすめ".into(), presets));
        }
        for m in rest {
            // 「glm-5.3」なら「glm」。同じ頭のモデルを 1 つにまとめる
            let family: SharedString = m
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect::<String>()
                .into();
            match groups
                .iter_mut()
                .skip_while(|(g, _)| g == "おすすめ")
                .find(|(g, _)| *g == family)
            {
                Some((_, names)) => names.push(m),
                None => groups.push((family, vec![m])),
            }
        }
        groups
    }

    /// 1 行の、右（広い行は下）に置くもの。
    pub fn control(&self, row_kind: AiRow, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let session = self.session.read(cx);
        let provider = session.provider;
        let model = session.model().to_owned();
        match row_kind {
            AiRow::Service => {
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
                providers.into_any_element()
            }
            AiRow::Model => {
                let model_open = self.model_open;
                let button = feel::gloss(
                    Button::new("ai-model-open")
                        .outline()
                        .small()
                        .w_full()
                        .justify_between()
                        .label(if model.is_empty() {
                            SharedString::from("標準のモデル")
                        } else {
                            SharedString::from(model.clone())
                        })
                        .dropdown_caret(true)
                        .selected(model_open)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.toggle_models(window, cx)),
                        ),
                );
                col()
                    .w_full()
                    .gap_2()
                    .child(button)
                    .when(model_open, |d| d.child(self.model_list(cx)))
                    .into_any_element()
            }
            AiRow::Effort => {
                let mut efforts = row().gap_1().flex_wrap();
                for e in Effort::ALL {
                    // 今のモデルで選べない値は、薄くして押せなくする
                    let allowed = session.effort_allowed(e);
                    efforts = efforts.child(
                        chip(
                            ("ai-effort", e as usize),
                            e.label(),
                            e == session.effort,
                            cx,
                        )
                        .when(!allowed, |d| d.opacity(0.4).cursor_default())
                        .when(allowed, |d| {
                            d.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.session.update(cx, |s, cx| s.set_effort(e, cx));
                            }))
                        })
                        .test_support(),
                    );
                }
                efforts.into_any_element()
            }
            AiRow::Coordinator => {
                let settings = cx.entity().downgrade();
                let set = session
                    .coordinator_set()
                    .then(|| session.coordinator_model());
                let shown: SharedString = match &set {
                    Some(id) => session.model_label(id),
                    None => "今のモデル".into(),
                };
                let models: Vec<(String, SharedString)> = provider
                    .models()
                    .iter()
                    .map(|m| (m.to_string(), session.model_label(m)))
                    .collect();
                Button::new("ai-coordinator")
                    .label(shown)
                    .outline()
                    .small()
                    .dropdown_caret(true)
                    .pop_menu(move |mut menu, _, _| {
                        let choices = std::iter::once((None, SharedString::from("今のモデル")))
                            .chain(
                                models
                                    .iter()
                                    .map(|(id, name)| (Some(id.clone()), name.clone())),
                            );
                        for (id, name) in choices {
                            let settings = settings.clone();
                            let checked = id == set;
                            menu = menu.item(PopupMenuItem::new(name).checked(checked).on_click(
                                move |_, _, cx| {
                                    // 設定を閉じたあとなら、反映する先がない
                                    let _ = settings.update(cx, |this, cx| {
                                        this.session.update(cx, |s, cx| {
                                            s.set_coordinator(id.as_deref(), cx)
                                        });
                                    });
                                },
                            ));
                        }
                        menu
                    })
                    .into_any_element()
            }
            AiRow::Key => {
                let (key_text, key_color, deletable): (SharedString, Hsla, bool) =
                    match &session.key {
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
                // 保存できなかった理由は、キーの欄のすぐ下に出す
                let error = session.error.clone();
                col()
                    .w_full()
                    .gap_2()
                    .child(
                        row()
                            .gap_1()
                            .child(
                                div()
                                    .id("ai-key-state")
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .text_color(key_color)
                                    .truncate()
                                    .child(key_text)
                                    .test_support(),
                            )
                            .when(deletable, |d| {
                                d.child(feel::gloss(
                                    icon_button("ai-key-delete", "trash", "API キーを削除")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.session.update(cx, |s, cx| s.delete_key(cx));
                                        })),
                                ))
                            }),
                    )
                    .child(
                        row()
                            .gap_1()
                            .child(
                                Input::new(&self.key_input)
                                    .id("ai-key-input")
                                    .aria_label("API キー")
                                    .small()
                                    .flex_1(),
                            )
                            .child(feel::gloss(
                                icon_button("ai-key-save", "check", "API キーを保存")
                                    .primary()
                                    .on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.save_key(window, cx)
                                        }),
                                    ),
                            )),
                    )
                    .when_some(error, |d, error| {
                        d.child(div().text_xs().text_color(theme.danger).child(error))
                    })
                    .into_any_element()
            }
            AiRow::AutoRun => Switch::new("ai-auto")
                .checked(session.auto_run)
                .on_click(cx.listener(|this, on: &bool, _, cx| {
                    let on = *on;
                    this.session.update(cx, |s, cx| s.set_auto_run(on, cx));
                }))
                .into_any_element(),
            AiRow::Rules => self.rules_editor(true, cx),
            AiRow::Arrange => Switch::new("ai-arrange")
                .checked(session.arrange)
                .on_click(cx.listener(|this, on: &bool, _, cx| {
                    let on = *on;
                    this.session.update(cx, |s, cx| s.set_arrange(on, cx));
                }))
                .into_any_element(),
            AiRow::Track => Switch::new("ai-track")
                .checked(session.track)
                .on_click(cx.listener(|this, on: &bool, _, cx| {
                    let on = *on;
                    this.session.update(cx, |s, cx| s.set_track(on, cx));
                }))
                .into_any_element(),
            AiRow::Shell => self.shell_choice(cx).into_any_element(),
            AiRow::Account => {
                let (account, signed_in): (SharedString, bool) = match session.github() {
                    GithubAccount::Login(login) => (format!("@{login}").into(), true),
                    GithubAccount::Missing => ("gh にログインしていません".into(), false),
                    GithubAccount::Unknown => ("".into(), false),
                };
                row()
                    .gap_2()
                    .child(
                        app_icon("github")
                            .size_4()
                            .text_color(theme.muted_foreground),
                    )
                    .child(
                        div()
                            .id("ai-github-account")
                            .max_w(CONTROL_WIDTH)
                            .truncate()
                            .text_sm()
                            .when(!signed_in, |d| d.text_color(theme.muted_foreground))
                            .child(account)
                            .test_support(),
                    )
                    .child(feel::gloss(
                        icon_button("ai-github-read", "refresh-cw", "gh から読み直す").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.session.update(cx, |s, cx| s.read_github(cx));
                            }),
                        ),
                    ))
                    .into_any_element()
            }
            AiRow::Watch => {
                let watch = session.watch.clone();
                let status = session.watch_status.clone();
                let busy = session.busy;
                col()
                    .items_end()
                    .gap_1()
                    .child(
                        row()
                            .gap_1()
                            .child(feel::gloss(
                                icon_button("ai-watch-now", "play", "今すぐチェック")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.session.update(cx, |s, cx| {
                                            s.check_github(cx);
                                        });
                                    })),
                            ))
                            .child(Switch::new("ai-watch").checked(watch.enabled).on_click(
                                cx.listener(|this, on: &bool, _, cx| {
                                    let on = *on;
                                    this.set_watch(|w| w.enabled = on, cx);
                                }),
                            )),
                    )
                    .when_some(status, |d, status| {
                        d.child(
                            div()
                                .id("ai-watch-status")
                                .max_w(CONTROL_WIDTH)
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(status)
                                .test_support(),
                        )
                    })
                    .into_any_element()
            }
            AiRow::WatchMinutes => {
                let current = session.watch.minutes;
                let mut minutes_row = row().gap_1().flex_wrap().justify_end();
                for minutes in WATCH_MINUTES {
                    let text = if minutes < 60 {
                        format!("{minutes} 分")
                    } else {
                        format!("{} 時間", minutes / 60)
                    };
                    minutes_row = minutes_row.child(
                        chip(
                            ("ai-watch-minutes", minutes as usize),
                            text,
                            current == minutes,
                            cx,
                        )
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.set_watch(|w| w.minutes = minutes, cx);
                        }))
                        .test_support(),
                    );
                }
                minutes_row.into_any_element()
            }
            AiRow::WatchInstruction => row()
                .w_full()
                .gap_1()
                .items_end()
                .child(
                    div()
                        .id("ai-watch-instruction")
                        .flex_1()
                        .min_w_0()
                        .child(Textarea::new(&self.watch_input).h(px(72.)))
                        .test_support(),
                )
                .child(feel::gloss(
                    icon_button("ai-watch-save", "check", "保存")
                        .on_click(cx.listener(|this, _, _, cx| this.set_watch(|_| {}, cx))),
                ))
                .into_any_element(),
        }
    }

    /// モデルを探して選ぶ一覧。
    fn model_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let session = self.session.read(cx);
        let provider = session.provider;
        let model = session.model().to_owned();
        let mut models = col()
            .id("ai-model-list")
            .max_h(px(260.))
            .overflow_y_scrollbar()
            .p_1();
        let mut ix = 0usize;
        let groups = self.model_groups(cx);
        let none = groups.is_empty();
        for (group, names) in groups {
            models = models.child(
                div()
                    .px_2()
                    .pt_2()
                    .pb_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(group),
            );
            for m in names {
                let active = m.as_ref() == model;
                let chosen = m.clone();
                models = models.child(
                    row()
                        .id(("ai-model", ix))
                        .gap_2()
                        .px_2()
                        .py_1()
                        .rounded(feel::md())
                        .items_center()
                        .text_sm()
                        .cursor_pointer()
                        .when(active, |d| d.bg(theme.accent))
                        .lit(theme.muted, cx)
                        .active(|s| s.opacity(0.6))
                        .child(
                            div()
                                .w_4()
                                .flex_none()
                                .when(active, |d| d.child(app_icon("check").size_4())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(session.model_label(&m)),
                        )
                        .tooltip({
                            let id = m.clone();
                            move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(id.clone())
                                    .build(window, cx)
                            }
                        })
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.session.update(cx, |s, cx| s.set_model(&chosen, cx));
                            this.model_input
                                .update(cx, |s, cx| s.set_value("", window, cx));
                            this.model_open = false;
                            cx.notify();
                        }))
                        .test_support(),
                );
                ix += 1;
            }
        }
        if none {
            models = models.child(
                div()
                    .px_2()
                    .py_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Enter で、入力した ID を使います"),
            );
        }
        col()
            .rounded(feel::md())
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .elevate(Elevation::Mid, cx)
            .child(
                row()
                    .gap_1()
                    .p_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        Input::new(&self.model_input)
                            .id("ai-model-input")
                            .aria_label("モデルを検索")
                            .small()
                            .flex_1(),
                    )
                    .when(
                        provider == Provider::Claude || provider.catalog_url().is_some(),
                        |d| {
                            d.child(feel::gloss(
                                icon_button("ai-fetch-models", "refresh-cw", "モデル一覧を更新")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.session.update(cx, |s, cx| s.fetch_models(cx));
                                    })),
                            ))
                        },
                    ),
            )
            .child(models)
            .with_animation("ai-model-list-in", appear(), |el, delta| el.opacity(delta))
            .into_any_element()
    }

    /// 「AI のシェル」。AI のタブで動かすシェルを、ターミナルで開けるシェルから選ぶ。
    fn shell_choice(&self, cx: &mut Context<Self>) -> Div {
        let terms = crate::term::shared(cx).read(cx);
        let chosen = terms.ai_profile.clone();
        let mut options: Vec<(SharedString, bool, Option<String>)> = vec![(
            format!("標準（{}）", malakt_ai::shell::Shell::standard().name()).into(),
            chosen.is_none(),
            None,
        )];
        for profile in terms.profiles() {
            options.push((
                profile.name.clone().into(),
                chosen.as_deref() == Some(profile.id.as_str()),
                Some(profile.id.clone()),
            ));
        }
        let shown = terms.ai_shell().name();
        let muted = cx.theme().muted_foreground;
        let settings = cx.entity().downgrade();
        // 押すと、選べるシェルのメニューが開く
        let menu = Button::new("shell-choice")
            .outline()
            .w_full()
            .h_8()
            .child(
                row()
                    .min_w_0()
                    .gap_2()
                    .child(div().flex_1().min_w_0().truncate().child(shown))
                    .child(app_icon("chevron-down").small().text_color(muted)),
            )
            .pop_menu(move |mut menu, _, _| {
                for (label, checked, id) in options.iter().cloned() {
                    let settings = settings.clone();
                    menu = menu.item(PopupMenuItem::new(label).checked(checked).on_click(
                        move |_, _, cx| {
                            // 設定を閉じたあとなら、反映する先がない
                            let _ = settings.update(cx, |_, cx| {
                                crate::term::shared(cx)
                                    .update(cx, |terms, cx| terms.set_ai_profile(id.as_deref(), cx))
                            });
                        },
                    ));
                }
                menu
            });
        row()
            .gap_1()
            .child(div().w(CONTROL_WIDTH).flex().child(menu))
            .child(
                Button::new("shell-choice-reset")
                    .icon(app_icon("rotate-ccw"))
                    .ghost()
                    .small()
                    .tooltip("標準に戻す")
                    // 場所は取っておき、標準のままのときは見せない
                    .when(chosen.is_none(), |b| b.invisible().disabled(true))
                    .on_click(cx.listener(|_, _, _, cx| {
                        crate::term::shared(cx)
                            .update(cx, |terms, cx| terms.set_ai_profile(None, cx))
                    })),
            )
    }

    /// AI チャットのペインに出す設定。まとまりごとに見出しとカードを並べる。
    pub fn render_all(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let mut all = col().gap_4();
        for group in AiGroup::ALL {
            let rows: Vec<AiRow> = AiRow::ALL
                .into_iter()
                .filter(|r| r.group() == group && r.shown(cx))
                .collect();
            let mut card = crate::render::setting_card(cx);
            for (ix, row_kind) in rows.into_iter().enumerate() {
                let control = self.control(row_kind, cx);
                card = card.child(crate::render::setting_line(
                    row_kind.name(),
                    row_kind.note(),
                    control,
                    row_kind.wide(),
                    ix == 0,
                    &theme,
                ));
            }
            all = all.child(
                col()
                    .gap_2()
                    .child(
                        div()
                            .px_1()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(group.label()),
                    )
                    .child(card),
            );
        }
        all
    }
}

impl Render for AiSettings {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.render_all(cx)
    }
}
