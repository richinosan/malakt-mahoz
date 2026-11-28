//! AI の設定。「設定」のウィンドウ（モデル・動作・GitHub のカテゴリ）と、AI bot のペインの
//! 設定の両方に、同じ行を出す。どちらも自分の入力欄を持つので、1 つずつ作る。

use crate::press::button;
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::ButtonVariants;
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
use crate::shadow::{Elevate, Elevation};

mod bots;
mod rules;
mod schedules;

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
    Bots,
    Model,
    Behavior,
    Rules,
    Schedules,
    Github,
}

impl AiGroup {
    pub const ALL: [AiGroup; 6] = [
        AiGroup::Bots,
        AiGroup::Model,
        AiGroup::Behavior,
        AiGroup::Rules,
        AiGroup::Schedules,
        AiGroup::Github,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AiGroup::Bots => "bot",
            AiGroup::Model => "モデル",
            AiGroup::Behavior => "動作",
            AiGroup::Rules => "ルール",
            AiGroup::Schedules => "定時タスク",
            AiGroup::Github => "GitHub",
        }
    }
}

/// AI の設定の 1 行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiRow {
    Bots,
    Service,
    /// ローカル AI のサーバー
    Server,
    Model,
    Effort,
    Coordinator,
    ChatGpt,
    Key,
    AutoRun,
    Rules,
    Schedules,
    Arrange,
    Track,
    Shell,
    Account,
    Watch,
    WatchMinutes,
    WatchInstruction,
}

impl AiRow {
    pub const ALL: [AiRow; 18] = [
        AiRow::Bots,
        AiRow::Service,
        AiRow::Server,
        // キーはサービスごとなので、サービスのすぐ下に置く
        AiRow::ChatGpt,
        AiRow::Key,
        AiRow::Model,
        AiRow::Effort,
        AiRow::Coordinator,
        AiRow::AutoRun,
        AiRow::Arrange,
        AiRow::Track,
        AiRow::Shell,
        AiRow::Rules,
        AiRow::Schedules,
        AiRow::Account,
        AiRow::Watch,
        AiRow::WatchMinutes,
        AiRow::WatchInstruction,
    ];

    pub fn group(self) -> AiGroup {
        match self {
            AiRow::Bots => AiGroup::Bots,
            AiRow::Service
            | AiRow::Server
            | AiRow::Model
            | AiRow::Effort
            | AiRow::Coordinator
            | AiRow::ChatGpt
            | AiRow::Key => AiGroup::Model,
            AiRow::AutoRun | AiRow::Arrange | AiRow::Track | AiRow::Shell => AiGroup::Behavior,
            AiRow::Rules => AiGroup::Rules,
            AiRow::Schedules => AiGroup::Schedules,
            AiRow::Account | AiRow::Watch | AiRow::WatchMinutes | AiRow::WatchInstruction => {
                AiGroup::Github
            }
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            AiRow::Bots => "bot",
            AiRow::Service => "サービス",
            AiRow::Server => "サーバー",
            AiRow::Model => "モデル",
            AiRow::Effort => "考える深さ",
            AiRow::Coordinator => "振り分けのモデル",
            AiRow::ChatGpt => "ChatGPT アカウント",
            AiRow::Key => "API キー",
            AiRow::AutoRun => "オート実行",
            AiRow::Rules => "ルール",
            AiRow::Schedules => "定時タスク",
            AiRow::Arrange => "新しいタスクを整える",
            AiRow::Track => "コメントから進捗を更新する",
            AiRow::Shell => "AI のタブのシェル",
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
            AiRow::Bots => "名前・インストラクション・アイコン・動き",
            AiRow::Rules => "AI がいつも従う指示",
            AiRow::Schedules => {
                "決まった時刻に新しいタスクを作り、AI への指示があれば AI が実行する"
            }
            AiRow::Arrange => "タスクを作ると、AI がリンクを読んでタイトルと説明を整える",
            AiRow::Track => "タスクにコメントを書くと、AI が進捗・状態・期限を更新する",
            AiRow::Shell => "AI のコマンドを動かすシェル",
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
            AiRow::Bots => "AI bot ボット キャラクター 名前 インストラクション 指示 アイコン 動き アニメーション 記憶 覚えたこと",
            AiRow::Service => "AI Claude OpenCode Cursor プロバイダー ローカル Ollama LM Studio",
            AiRow::Server => {
                "AI ローカル Ollama LM Studio llama.cpp vLLM LocalAI Jan URL ポート localhost"
            }
            AiRow::Model => "AI",
            AiRow::Effort => "AI 考える",
            AiRow::ChatGpt => "AI ChatGPT OpenAI サインイン ログイン プラン SSO",
            AiRow::Key => "AI キー 鍵",
            AiRow::Shell => "ターミナル シェル",
            AiRow::Account => "gh ログイン",
            AiRow::Rules => "AI ルール 指示 前提 プロンプト ワークスペース",
            AiRow::Schedules => {
                "定時 定期 毎週 毎日 毎月 報告 繰り返し スケジュール ワークスペース"
            }
            _ => "AI",
        }
    }

    /// 名前の下に、横いっぱいに出す行（入力欄や一覧）。
    pub fn wide(self) -> bool {
        matches!(
            self,
            AiRow::Bots
                | AiRow::Service
                | AiRow::Server
                | AiRow::Model
                | AiRow::Rules
                | AiRow::Schedules
                | AiRow::WatchInstruction
        )
    }

    /// 今の設定で出す行。
    pub fn shown(self, cx: &App) -> bool {
        let session = ai::shared(cx).read(cx);
        match self {
            AiRow::Server => session.provider == Provider::Local,
            AiRow::Effort => session.provider.supports_effort(),
            AiRow::Coordinator => session.provider != Provider::Cursor,
            AiRow::ChatGpt => session.provider == Provider::OpenAi,
            // ChatGPT のアカウントでサインインしている間は、API キーを使わない
            AiRow::Key => !session.on_chatgpt_plan(),
            AiRow::WatchMinutes | AiRow::WatchInstruction => session.watch.enabled,
            _ => true,
        }
    }
}

/// AI の設定の入力欄と、モデルの一覧の開け閉め。
pub struct AiSettings {
    session: Entity<Session>,
    key_input: Entity<InputState>,
    /// ローカル AI のサーバーの URL。
    local_input: Entity<InputState>,
    /// キーの入力欄が、任意のキー（ローカル AI）の案内になっている。
    key_optional: bool,
    /// 保存済みの API キーを、新しいキーに変えている。
    key_editing: bool,
    model_input: Entity<InputState>,
    /// 定期チェックの指示。
    watch_input: Entity<TextareaState>,
    /// 開いている（中身を変えている）bot。
    pub bot_open: Option<String>,
    bot_name: Entity<InputState>,
    bot_text: Entity<TextareaState>,
    /// bot のアイコンを探す欄。
    bot_icon_query: Entity<InputState>,
    /// bot のアイコンを選ぶところを開いている。
    bot_icons_open: bool,
    /// bot を保存できなかった理由。
    bots_error: Option<SharedString>,
    /// ルールの一覧で見ている場所。`None` はグローバル（すべてのワークスペース）。
    pub rules_scope: Option<String>,
    /// 開いている（名前と本文を書いている）ルール。
    rule_open: Option<String>,
    rule_name: Entity<InputState>,
    rule_body: Entity<TextareaState>,
    /// ルールを保存できなかった理由。
    rules_error: Option<SharedString>,
    /// 定時タスクの一覧で見ている場所。`None` は未所属。
    pub schedules_scope: Option<String>,
    /// 開いている（中身を変えている）定時タスク。
    schedule_open: Option<String>,
    schedule_title: Entity<InputState>,
    schedule_ai: Entity<TextareaState>,
    /// 開いた定時タスクの、雛形の説明・URL・カテゴリ・サブタスクを書く欄。
    schedule_inputs: schedules::TemplateInputs,
    /// 定時タスクを保存できなかった理由。
    schedules_error: Option<SharedString>,
    /// モデルの一覧を開いている。
    pub model_open: bool,
    _subscriptions: Vec<Subscription>,
}

impl AiSettings {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let session = ai::shared(cx);
        let optional = !session.read(cx).provider.needs_key();
        let key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(if optional { "任意" } else { "" })
        });
        let local_url = session.read(cx).local_url.clone();
        let local_input = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("自動で探す");
            state.set_value(local_url, window, cx);
            state
        });
        let model_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("検索、またはモデル ID を入力"));
        let instruction = session.read(cx).watch.instruction.clone();
        let watch_input = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).placeholder("AI への指示");
            state.set_value(instruction, window, cx);
            state
        });
        let bot_name = cx.new(|cx| InputState::new(window, cx));
        let bot_text = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("例: 敬語で短く答える。タスクの期限には厳しく")
        });
        let bot_icon_query = cx.new(|cx| InputState::new(window, cx).placeholder("検索"));
        let rule_name = cx.new(|cx| InputState::new(window, cx));
        let rule_body = cx.new(|cx| {
            TextareaState::new(window, cx).placeholder("例: PR のタスクは優先度を高にする")
        });
        let schedule_title = cx.new(|cx| InputState::new(window, cx));
        let schedule_ai = cx.new(|cx| {
            TextareaState::new(window, cx)
                .submit_on_enter(true)
                .auto_grow(2, 8)
                .placeholder("例: 先週完了したタスクから、定例の報告を下書きする")
        });
        let schedule_inputs = schedules::TemplateInputs::new(window, cx);
        crate::term::shared(cx).update(cx, |terms, cx| terms.load_profiles(cx));
        let mut subscriptions = schedule_inputs.subscribe(window, cx);
        subscriptions.extend([
            cx.subscribe_in(
                &schedule_title,
                window,
                |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        this.save_schedule_title(cx);
                    }
                },
            ),
            cx.subscribe_in(
                &schedule_ai,
                window,
                |this, _, event: &InputEvent, _, cx| {
                    if matches!(
                        event,
                        InputEvent::PressEnter { shift: false, .. } | InputEvent::Blur
                    ) {
                        this.save_schedule_ai(cx);
                    }
                },
            ),
            cx.observe(&session, |_, _, cx| cx.notify()),
            // ローカル AI のキーは任意。サービスに合わせて、入力欄の案内を変える
            cx.observe_in(&session, window, |this, session, window, cx| {
                let optional = !session.read(cx).provider.needs_key();
                if this.key_optional != optional {
                    this.key_optional = optional;
                    let placeholder = if optional { "任意" } else { "" };
                    this.key_input
                        .update(cx, |s, cx| s.set_placeholder(placeholder, window, cx));
                }
            }),
            cx.observe(&crate::term::shared(cx), |_, _, cx| cx.notify()),
            // AI や外のエージェントがルールを変えたら、一覧を読み直す
            cx.observe(&crate::db::shared(cx), |_, _, cx| cx.notify()),
            cx.subscribe_in(&bot_name, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.save_bot_name(cx);
                }
            }),
            cx.subscribe_in(&bot_text, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Blur) {
                    this.save_bot_text(cx);
                }
            }),
            cx.subscribe_in(
                &bot_icon_query,
                window,
                |_, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        cx.notify();
                    }
                },
            ),
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
                &local_input,
                window,
                |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        this.save_local_url(cx);
                    }
                },
            ),
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
        ]);
        Self {
            session,
            key_input,
            local_input,
            key_optional: optional,
            key_editing: false,
            model_input,
            watch_input,
            bot_open: None,
            bot_name,
            bot_text,
            bot_icon_query,
            bot_icons_open: false,
            bots_error: None,
            rules_scope: None,
            rule_open: None,
            rule_name,
            rule_body,
            rules_error: None,
            schedules_scope: None,
            schedule_open: None,
            schedule_title,
            schedule_ai,
            schedule_inputs,
            schedules_error: None,
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
                // ローカル AI は、開くたびに読み直す（入れたばかりのモデルも出す）
                let local = s.provider == Provider::Local && !s.local_searching;
                if local || (s.listed.is_empty() && matches!(s.key, KeyState::Present { .. })) {
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

    /// 入力欄の URL を、ローカル AI のサーバーにする。空なら手元のポートを探す。
    fn save_local_url(&mut self, cx: &mut Context<Self>) {
        let url = self.local_input.read(cx).value().to_string();
        self.session.update(cx, |s, cx| s.set_local_url(&url, cx));
    }

    /// 見つけたサーバーを選ぶ。
    fn pick_server(&mut self, base: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.local_input
            .update(cx, |s, cx| s.set_value(base.to_owned(), window, cx));
        self.session.update(cx, |s, cx| s.set_local_url(base, cx));
    }

    /// ChatGPT のアカウントの行。サインインしていればメールアドレスとサインアウト、
    /// していなければ「ChatGPT で続ける」。
    fn chatgpt_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let session = self.session.read(cx);
        let error = session.chatgpt_error.clone();
        let errors = |d: Div| {
            d.children(error.clone().map(|e| {
                div()
                    .id("ai-chatgpt-error")
                    .text_xs()
                    .text_color(theme.danger)
                    .child(e)
                    .test_support()
            }))
        };
        if matches!(session.key, KeyState::Unknown) {
            return div()
                .id("ai-chatgpt-state")
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("確認中…")
                .test_support()
                .into_any_element();
        }
        let body = match (&session.chatgpt, &session.key) {
            (ai::ChatGptSignIn::Waiting(_), _) => row()
                .gap_1()
                .child(
                    button("ai-chatgpt-waiting")
                        .label("ブラウザでサインイン中")
                        .outline()
                        .small()
                        .loading(true)
                        .disabled(true)
                        // 待っている間は押せない。やめるときは ✕
                        .on_click(|_, _, _| {}),
                )
                .child(feel::gloss(
                    icon_button("ai-chatgpt-cancel", "x", "キャンセル").on_click(cx.listener(
                        |this, _, _, cx| this.session.update(cx, |s, _| s.cancel_chatgpt()),
                    )),
                )),
            (
                chatgpt,
                KeyState::Present {
                    masked,
                    source: Source::ChatGpt,
                },
            ) => row()
                .gap_1()
                .child(
                    row()
                        .id("ai-chatgpt-state")
                        .gap_2()
                        .pr_1()
                        .child(
                            app_icon("circle-user-round")
                                .size_4()
                                .text_color(theme.muted_foreground),
                        )
                        .child(div().text_sm().child(masked.clone()))
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new("ChatGPT のプランを使用中")
                                .build(window, cx)
                        })
                        .test_support(),
                )
                .child(feel::gloss(
                    icon_button("ai-chatgpt-sign-out", "log-out", "サインアウト")
                        .disabled(matches!(chatgpt, ai::ChatGptSignIn::SigningOut))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.session.update(cx, |s, cx| s.sign_out_chatgpt(cx));
                        })),
                )),
            _ => row().child(
                button("ai-chatgpt-sign-in")
                    .label("ChatGPT で続ける")
                    .outline()
                    .small()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.session.update(cx, |s, cx| s.sign_in_chatgpt(cx));
                    })),
            ),
        };
        errors(col().items_end().gap_1().child(body)).into_any_element()
    }

    fn save_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.key_input.read(cx).value().to_string();
        self.session.update(cx, |s, cx| s.save_key(&key, cx));
        if self.session.read(cx).error.is_none() {
            self.key_input
                .update(cx, |s, cx| s.set_value("", window, cx));
            self.key_editing = false;
            cx.notify();
        }
    }

    /// キーの入力欄に書けるようにする（「API キーを設定」から来たとき）。保存済みなら何もしない。
    pub fn focus_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.session.read(cx).key, KeyState::Present { .. }) {
            self.key_input.update(cx, |s, cx| s.focus(window, cx));
        }
    }

    /// 保存済みのキーを、新しいキーに変え始める。
    fn edit_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.key_editing = true;
        self.key_input.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    /// 書きかけの新しいキーを捨てて、保存済みのキーの表示に戻す。
    fn cancel_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.key_editing = false;
        self.key_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    /// 検索の言葉に合うモデル。おすすめを先に、残りは名前の頭でまとめる。
    fn model_groups(&self, cx: &App) -> Vec<(SharedString, Vec<SharedString>)> {
        let session = self.session.read(cx);
        let query = self.model_input.read(cx).value().trim().to_lowercase();
        // ID でも、画面に出す名前（「haiku 5.5」など）でも探せる
        let matches = |m: &str| {
            query.is_empty()
                || m.to_lowercase().contains(&query)
                || session.model_label(m).to_lowercase().contains(&query)
        };
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
            AiRow::Server => {
                // 見つけたサーバーと、URL で選んだサーバー
                let mut servers: Vec<malakt_ai::local::Server> = session.local_found.clone();
                if let Some(used) = &session.local_server {
                    if !servers.contains(used) {
                        servers.push(used.clone());
                    }
                }
                let mut chips = row().gap_1().flex_wrap();
                for (ix, server) in servers.into_iter().enumerate() {
                    let active = session.local_server.as_ref() == Some(&server);
                    let address: SharedString = server.address().to_owned().into();
                    let base = server.base.clone();
                    chips = chips.child(
                        chip(("ai-local-server", ix), server.kind.label(), active, cx)
                            .tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(address.clone())
                                    .build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                this.pick_server(&base, window, cx)
                            }))
                            .test_support(),
                    );
                }
                let searching = session.local_searching;
                col()
                    .w_full()
                    .gap_2()
                    .child(
                        row()
                            .gap_1()
                            .child(
                                Input::new(&self.local_input)
                                    .id("ai-local-url")
                                    .aria_label("サーバーの URL")
                                    .small()
                                    .flex_1(),
                            )
                            .child(if searching {
                                div()
                                    .size_6()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(gpui_kit::component::spinner::Spinner::new().small())
                                    .into_any_element()
                            } else {
                                feel::gloss(
                                    icon_button("ai-local-search", "refresh-cw", "探し直す")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.session.update(cx, |s, cx| s.search_local(cx));
                                        })),
                                )
                                .into_any_element()
                            }),
                    )
                    .child(chips)
                    .when_some(session.local_error.clone(), |d, error| {
                        d.child(
                            div()
                                .id("ai-local-error")
                                .text_xs()
                                .text_color(theme.danger)
                                .child(error)
                                .test_support(),
                        )
                    })
                    .into_any_element()
            }
            AiRow::Model => {
                let model_open = self.model_open;
                let button = feel::gloss(
                    button("ai-model-open")
                        .outline()
                        .small()
                        .w_full()
                        .justify_between()
                        // 一覧と同じ名前を出し、ID はホバーで出す
                        .label(if model.is_empty() {
                            SharedString::from("標準のモデル")
                        } else {
                            session.model_label(&model)
                        })
                        .when(!model.is_empty(), |b| b.tooltip(model.clone()))
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
                button("ai-coordinator")
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
            AiRow::ChatGpt => self.chatgpt_row(cx),
            AiRow::Key => {
                // 保存済みなら、伏せたキーを 1 行で出す。入力欄は、未設定のときと変えるときだけ
                let saved = match &session.key {
                    KeyState::Present { masked, source }
                        if !self.key_editing && *source != Source::None =>
                    {
                        Some((masked.clone(), *source))
                    }
                    _ => None,
                };
                if matches!(session.key, KeyState::Unknown) {
                    return div()
                        .id("ai-key-state")
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("確認中…")
                        .test_support()
                        .into_any_element();
                }
                if let Some((masked, source)) = saved {
                    let (icon, place): (&str, SharedString) = match source {
                        Source::Keychain => ("key-round", "OS のキーチェーン".into()),
                        Source::Env | Source::None => (
                            "variable",
                            format!("環境変数 {}", provider.env_var()).into(),
                        ),
                        // ChatGPT のアカウントは「ChatGPT アカウント」の行に出す（この行は出さない）
                        Source::ChatGpt => ("circle-user-round", "ChatGPT アカウント".into()),
                    };
                    return row()
                        .gap_1()
                        .child(
                            row()
                                .id("ai-key-state")
                                .gap_2()
                                .pr_1()
                                .child(app_icon(icon).size_4().text_color(theme.muted_foreground))
                                .child(div().text_sm().child(masked))
                                .tooltip(move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(place.clone())
                                        .build(window, cx)
                                })
                                .test_support(),
                        )
                        .child(feel::gloss(
                            icon_button("ai-key-edit", "pen", "API キーを変更").on_click(
                                cx.listener(|this, _, window, cx| this.edit_key(window, cx)),
                            ),
                        ))
                        .when(source == Source::Keychain, |d| {
                            d.child(feel::gloss(
                                icon_button("ai-key-delete", "trash", "API キーを削除").on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.session.update(cx, |s, cx| s.delete_key(cx));
                                    }),
                                ),
                            ))
                        })
                        .into_any_element();
                }
                // 保存済みのキーがあるときだけ、変えるのをキャンセルできる
                let editing = self.key_editing
                    && matches!(&session.key, KeyState::Present { source, .. } if *source != Source::None);
                // 読めなかった理由と保存できなかった理由は、入力欄のすぐ下に出す
                let errors: Vec<SharedString> = [
                    match &session.key {
                        KeyState::Failed(e) => Some(e.clone()),
                        _ => None,
                    },
                    session.error.clone(),
                ]
                .into_iter()
                .flatten()
                .collect();
                // 狭くて名前の下に回ったときは、横いっぱいに広げる
                col()
                    .flex_1()
                    .min_w(CONTROL_WIDTH)
                    .gap_1()
                    .child(
                        row()
                            .id("ai-key-editor")
                            .w_full()
                            .gap_1()
                            // Esc は、書きかけの新しいキーを捨てるだけ（設定は閉じない）
                            .when(editing, |d| {
                                d.key_context("AiKeyEditor").on_action(cx.listener(
                                    |this, _: &crate::CancelKeyEdit, window, cx| {
                                        this.cancel_key(window, cx)
                                    },
                                ))
                            })
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
                            ))
                            .when(editing, |d| {
                                d.child(feel::gloss(
                                    icon_button("ai-key-cancel", "x", "キャンセル").on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.cancel_key(window, cx)
                                        }),
                                    ),
                                ))
                            }),
                    )
                    .children(
                        errors
                            .into_iter()
                            .map(|error| div().text_xs().text_color(theme.danger).child(error)),
                    )
                    .into_any_element()
            }
            AiRow::AutoRun => crate::render::on_off(
                session.auto_run,
                Switch::new("ai-auto")
                    .checked(session.auto_run)
                    .on_click(cx.listener(|this, on: &bool, _, cx| {
                        let on = *on;
                        this.session.update(cx, |s, cx| s.set_auto_run(on, cx));
                    })),
                cx,
            ),
            AiRow::Bots => self.bots_editor(cx),
            AiRow::Rules => self.rules_editor(true, cx),
            AiRow::Schedules => self.schedules_editor(true, cx),
            AiRow::Arrange => crate::render::on_off(
                session.arrange,
                Switch::new("ai-arrange")
                    .checked(session.arrange)
                    .on_click(cx.listener(|this, on: &bool, _, cx| {
                        let on = *on;
                        this.session.update(cx, |s, cx| s.set_arrange(on, cx));
                    })),
                cx,
            ),
            AiRow::Track => crate::render::on_off(
                session.track,
                Switch::new("ai-track")
                    .checked(session.track)
                    .on_click(cx.listener(|this, on: &bool, _, cx| {
                        let on = *on;
                        this.session.update(cx, |s, cx| s.set_track(on, cx));
                    })),
                cx,
            ),
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
                            .child(crate::render::on_off(
                                watch.enabled,
                                Switch::new("ai-watch").checked(watch.enabled).on_click(
                                    cx.listener(|this, on: &bool, _, cx| {
                                        let on = *on;
                                        this.set_watch(|w| w.enabled = on, cx);
                                    }),
                                ),
                                cx,
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
                        // ローカル AI のモデルは、ツールと画像を使えるかを印で出す
                        .children(session.model_abilities(&m).into_iter().map(|(icon, _)| {
                            app_icon(icon).size_3p5().text_color(theme.muted_foreground)
                        }))
                        .tooltip({
                            let abilities: Vec<&str> = session
                                .model_abilities(&m)
                                .into_iter()
                                .map(|(_, name)| name)
                                .collect();
                            let tip: SharedString = if abilities.is_empty() {
                                m.clone()
                            } else {
                                format!("{m}（{}）", abilities.join("・")).into()
                            };
                            move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(tip.clone())
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
                        provider == Provider::Claude
                            || provider.catalog_url().is_some()
                            || session.on_chatgpt_plan(),
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

    /// 「AI のタブのシェル」。AI のタブで動かすシェルを、ターミナルで開けるシェルから選ぶ。
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
        let menu = button("shell-choice")
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
                button("shell-choice-reset")
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

    /// AI bot のペインに出す設定。まとまりごとに見出しとカードを並べる。
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
