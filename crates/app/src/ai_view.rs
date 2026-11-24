//! AI チャットとシェルの画面。メインのウィンドウの右にも、別のウィンドウにも出せる。

use std::ops::Range;
use std::time::Duration;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{
    Copy as CopyText, Input, InputEvent, InputState, MoveDown, MoveUp, Textarea, TextareaState,
};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Disableable, Selectable, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_ai::ansi::{self, Color};
use malakt_ai::keys::Source;
use malakt_ai::{Effort, Provider};

use crate::ai::{
    self, Decision, Item, KeyState, LineKind, Session, ShellPlace, ToolState, Watch, WATCH_MINUTES,
};
use crate::db::{self, Db};
use crate::render::app_icon;
use crate::state::MainView;
use crate::{ClearShell, StopAi};

/// 参照する寸法を、画面全体の拡大率に合わせる。
const fn px(value: f32) -> Pixels {
    gpui_kit::px(value * crate::UI_SCALE)
}

const CHAT_WIDTH: Pixels = px(380.);
const SHELL_WIDTH: Pixels = px(360.);
const SHELL_ROW: Pixels = px(20.);
const TERM_PAD: Pixels = px(12.);
/// ターミナルの文字の大きさ。画面の文字の大きさ（テーマ）に対する比で決める。
fn term_font(cx: &App) -> Pixels {
    cx.theme().font_size * (13. / 16.)
}
/// ターミナルの色。画面のテーマが明るくても暗くても同じ。
const TERM_BG: u32 = 0x0f1115;
const TERM_FG: u32 = 0xd4d7dd;
const TERM_DIM: u32 = 0x7d8590;
const TERM_PROMPT: u32 = 0x7ee787;

fn term_color(color: Color) -> Hsla {
    rgb(match color {
        Color::Black => 0x484f58,
        Color::Red => 0xff7b72,
        Color::Green => 0x7ee787,
        Color::Yellow => 0xe3b341,
        Color::Blue => 0x79c0ff,
        Color::Magenta => 0xd2a8ff,
        Color::Cyan => 0x76e3ea,
        Color::White => 0xd4d7dd,
        Color::BrightBlack => 0x7d8590,
        Color::BrightRed => 0xffa198,
        Color::BrightGreen => 0xaff5b4,
        Color::BrightYellow => 0xf2cc60,
        Color::BrightBlue => 0xa5d6ff,
        Color::BrightMagenta => 0xe2c5ff,
        Color::BrightCyan => 0xb3f0ff,
        Color::BrightWhite => 0xffffff,
    })
    .into()
}

/// プロンプトに出す場所。長いときは、終わりの 2 つだけにして入力する幅を残す。
fn short_path(path: &str) -> String {
    if path.chars().count() <= 24 {
        return path.to_owned();
    }
    let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let tail = &parts[parts.len().saturating_sub(2)..];
    let short = format!("…/{}", tail.join("/"));
    if short.chars().count() <= 24 {
        short
    } else {
        // フォルダーの名前そのものが長い
        let name: Vec<char> = parts.last().copied().unwrap_or(path).chars().collect();
        let keep = &name[name.len().saturating_sub(22)..];
        format!("…{}", keep.iter().collect::<String>())
    }
}

/// 現れるときの動き。
fn appear() -> Animation {
    Animation::new(Duration::from_millis(160)).with_easing(ease_out_quint())
}

/// アイコンだけのボタン。名前はホバーで出る。
fn icon_button(id: &'static str, icon: &str, tip: &'static str) -> Button {
    Button::new(id)
        .icon(app_icon(icon))
        .tooltip(tip)
        .ghost()
        .small()
}

/// チャットを右に置いても、タスクの画面に残す幅。これより狭いと、タスクの画面は
/// メニューをたたむ表示になる（`render.rs` の `wide`）。
pub const TASKS_MIN: Pixels = px(860.);
/// 右に置くチャットの、いちばん狭い幅。
const CHAT_MIN: Pixels = px(320.);

/// メインのウィンドウの右に置くときの幅。`None` ならタスクの画面の右に重ねる。
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
    /// 定期チェックの指示。
    watch_input: Entity<TextareaState>,
    /// モデルの入力欄に出している値。
    synced_model: String,
    pub settings_open: bool,
    chat_scroll: ScrollHandle,
    shell_scroll: UniformListScrollHandle,
    /// シェルの入力行。
    shell_input: Entity<InputState>,
    /// ↑↓ で見ている履歴の位置。`None` は入力中の行。
    history_at: Option<usize>,
    history_draft: String,
    /// 幅で折り返した行（シェルの行の番号と、その中の範囲）。
    shell_rows: Vec<(usize, Range<usize>)>,
    /// `shell_rows` を作ったときの、シェルの版と桁数。
    rows_key: (u64, usize),
    shell_cols: usize,
    copied: bool,
    seen: usize,
    _subscriptions: Vec<Subscription>,
}

fn row() -> Div {
    div().flex().flex_row().items_center()
}

fn col() -> Div {
    div().flex().flex_col()
}

/// いくつかの中から 1 つを選ぶボタン。選んでいるものは色とチェックで分かる。
fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    cx: &App,
) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .h_7()
        .px_2p5()
        .gap_1()
        .flex()
        .flex_none()
        .items_center()
        .rounded_md()
        .border_1()
        .text_xs()
        .cursor_pointer()
        .active(|s| s.opacity(0.6))
        .when(active, |d| {
            d.bg(theme.primary)
                .text_color(theme.primary_foreground)
                .border_color(theme.primary)
                .child(app_icon("check").size_3())
        })
        .when(!active, |d| {
            d.border_color(theme.input)
                .bg(theme.background)
                .hover(|s| s.bg(theme.secondary_hover).border_color(theme.primary))
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
        let input = cx.new(|cx| TextareaState::new(window, cx).placeholder("メッセージを入力"));
        let key_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("API キー")
        });
        let model_input = cx.new(|cx| InputState::new(window, cx).placeholder("モデル ID"));
        let instruction = session.read(cx).watch.instruction.clone();
        let watch_input = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx).placeholder("AI への指示");
            state.set_value(instruction, window, cx);
            state
        });
        let shell_input = cx.new(|cx| InputState::new(window, cx));
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
            cx.subscribe_in(
                &shell_input,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.run_line(window, cx);
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
        let seen = session.read(cx).items.len();
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
            shell_input,
            history_at: None,
            history_draft: String::new(),
            shell_rows: Vec::new(),
            rows_key: (u64::MAX, 0),
            shell_cols: 80,
            copied: false,
            seen,
            _subscriptions: subscriptions,
        }
    }

    /// チャットの変化に合わせる。新しい行が出たら、いちばん下まで送る。
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (items, model) = {
            let s = self.session.read(cx);
            (s.items.len(), s.model().to_owned())
        };
        if items != self.seen {
            self.chat_scroll.scroll_to_bottom();
        }
        self.seen = items;
        // さかのぼって読んでいる間は、新しい出力が来ても動かさない
        let follow = self.shell_at_bottom();
        if self.refresh_rows(cx) && follow {
            self.shell_to_bottom();
        }
        if model != self.synced_model {
            self.synced_model = model.clone();
            self.model_input
                .update(cx, |s, cx| s.set_value(model, window, cx));
        }
        cx.notify();
    }

    fn shell_at_bottom(&self) -> bool {
        let handle = self.shell_scroll.0.borrow().base_handle.clone();
        handle.offset().y + handle.max_offset().y <= px(4.)
    }

    fn shell_to_bottom(&self) {
        if let Some(last) = self.shell_rows.len().checked_sub(1) {
            self.shell_scroll
                .scroll_to_item(last, ScrollStrategy::Bottom);
        }
    }

    /// シェルの行を今の幅で折り返す。作り直したら `true`。
    fn refresh_rows(&mut self, cx: &App) -> bool {
        let session = self.session.read(cx);
        let key = (session.shell_rev, self.shell_cols);
        if key == self.rows_key {
            return false;
        }
        self.rows_key = key;
        self.shell_rows.clear();
        for (ix, line) in session.shell.iter().enumerate() {
            self.shell_rows.extend(
                ansi::wrap(&line.text, self.shell_cols)
                    .into_iter()
                    .map(|range| (ix, range)),
            );
        }
        true
    }

    /// シェルの幅から、1 行に入る桁数を決める。
    fn set_shell_width(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
        let family = cx.theme().mono_font_family.clone();
        let size = term_font(cx);
        let text = window.text_system();
        let cell = text
            .ch_advance(text.resolve_font(&font(family)), size)
            .unwrap_or(size * 0.6);
        // 全角の字が 2 桁より少し広い書体でも、はみ出さないように 1 桁あける
        let cols = (((width - TERM_PAD * 2.) / cell).floor() as usize)
            .saturating_sub(1)
            .max(20);
        if cols != self.shell_cols {
            let follow = self.shell_at_bottom();
            self.shell_cols = cols;
            self.refresh_rows(cx);
            if follow {
                self.shell_to_bottom();
            }
            cx.notify();
        }
    }

    /// シェルの入力行のコマンドを実行する。
    fn run_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let line = self.shell_input.read(cx).value().to_string();
        if self
            .session
            .update(cx, |s, cx| s.run_user_command(&line, cx))
        {
            self.shell_input
                .update(cx, |s, cx| s.set_value("", window, cx));
            self.history_at = None;
            self.history_draft.clear();
            self.refresh_rows(cx);
            self.shell_to_bottom();
        }
    }

    /// ↑↓ で、前に入力したコマンドを入力行に出す。
    fn history_step(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let history = self.session.read(cx).history.clone();
        let next = match (self.history_at, back) {
            (None, true) => history.len().checked_sub(1),
            (None, false) => return,
            (Some(at), true) => Some(at.saturating_sub(1)),
            (Some(at), false) => (at + 1 < history.len()).then_some(at + 1),
        };
        if self.history_at.is_none() {
            if next.is_none() {
                return;
            }
            self.history_draft = self.shell_input.read(cx).value().to_string();
        }
        self.history_at = next;
        let value = match next {
            Some(at) => history[at].to_string(),
            None => std::mem::take(&mut self.history_draft),
        };
        self.shell_input
            .update(cx, |s, cx| s.set_value(value, window, cx));
    }

    fn copy_shell(&mut self, cx: &mut Context<Self>) {
        let text = self
            .session
            .read(cx)
            .shell
            .iter()
            .map(|line| line.text.as_ref())
            .collect::<Vec<_>>()
            .join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        // コピーできたことを、少しの間ボタンに出す
        self.copied = true;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(1200))
                .await;
            // 画面がもうなければ、戻す先もない
            let _ = this.update(cx, |this, cx| {
                this.copied = false;
                cx.notify();
            });
        })
        .detach();
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
            // 別ウィンドウのチャットでも、メインのウィンドウで見ている場所を伝える
            .or_else(|| Some(self.session.read(cx).place.clone()).filter(|p| !p.is_empty()))
            .unwrap_or_else(|| "ユーザーは、チャットだけのウィンドウを開いています。".to_owned());
        let sent = self.session.update(cx, |s, cx| s.send(&text, place, cx));
        if sent {
            self.input.update(cx, |s, cx| s.set_value("", window, cx));
        }
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

    fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
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
        let arrange = session.arrange;
        let mut minutes_row = row().gap_1().flex_wrap();
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
                    watch.minutes == minutes,
                    cx,
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.set_watch(|w| w.minutes = minutes, cx);
                }))
                .test_support(),
            );
        }
        let section = || {
            col()
                .gap_2()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
        };

        col()
            .id("ai-settings")
            .gap_2()
            .p_3()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.secondary)
            .child(
                section()
                    .child(label("サービス"))
                    .child(providers)
                    .child(label("モデル"))
                    .child(models)
                    .child(
                        row()
                            .gap_1()
                            .child(
                                Input::new(&self.model_input)
                                    .id("ai-model-input")
                                    .aria_label("モデル ID")
                                    .small()
                                    .flex_1(),
                            )
                            .when(
                                provider == Provider::Claude || provider.catalog_url().is_some(),
                                |d| {
                                    d.child(
                                        icon_button(
                                            "ai-fetch-models",
                                            "refresh-cw",
                                            "モデル一覧を更新",
                                        )
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.session.update(cx, |s, cx| s.fetch_models(cx));
                                            }),
                                        ),
                                    )
                                },
                            ),
                    )
                    .when(provider.supports_effort(), |d| {
                        d.child(label("effort")).child(efforts)
                    }),
            )
            .child(
                section()
                    .child(
                        row()
                            .gap_1()
                            .child(label("API キー"))
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
                                d.child(
                                    icon_button("ai-key-delete", "trash", "API キーを削除")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.session.update(cx, |s, cx| s.delete_key(cx));
                                        })),
                                )
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
                            .child(
                                icon_button("ai-key-save", "check", "API キーを保存")
                                    .primary()
                                    .on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.save_key(window, cx)
                                        }),
                                    ),
                            ),
                    ),
            )
            .child(
                section().child(
                    Switch::new("ai-auto")
                        .checked(auto_run)
                        .label("オート実行")
                        .tooltip("コマンド実行とタスク削除を確認なしで行う")
                        .on_click(cx.listener(|this, on: &bool, _, cx| {
                            let on = *on;
                            this.session.update(cx, |s, cx| s.set_auto_run(on, cx));
                        })),
                ),
            )
            .child(
                section().child(
                    Switch::new("ai-arrange")
                        .checked(arrange)
                        .label("新しいタスクを整える")
                        .tooltip("タスクを作ると、AI がリンクを読んでタイトルと説明を整える")
                        .on_click(cx.listener(|this, on: &bool, _, cx| {
                            let on = *on;
                            this.session.update(cx, |s, cx| s.set_arrange(on, cx));
                        })),
                ),
            )
            .child(
                section()
                    .child(
                        row()
                            .gap_1()
                            .child(
                                div().flex_1().child(
                                    Switch::new("ai-watch")
                                        .checked(watch.enabled)
                                        .label("GitHub の定期チェック")
                                        .tooltip(
                                            "未完了タスクの issue・プルリクエストを定期的にチェック",
                                        )
                                        .on_click(cx.listener(|this, on: &bool, _, cx| {
                                            let on = *on;
                                            this.set_watch(|w| w.enabled = on, cx);
                                        })),
                                ),
                            )
                            .child(
                                icon_button("ai-watch-now", "play", "今すぐチェック")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.session.update(cx, |s, cx| {
                                            s.check_github(cx);
                                        });
                                    })),
                            ),
                    )
                    .when_some(watch_status, |d, status| {
                        d.child(
                            div()
                                .id("ai-watch-status")
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(status)
                                .test_support(),
                        )
                    })
                    .when(watch.enabled, |d| {
                        d.child(label("間隔"))
                            .child(minutes_row)
                            .child(label("AI への指示"))
                            .child(
                                row()
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
                                    .child(
                                        icon_button("ai-watch-save", "check", "保存")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.set_watch(|_| {}, cx)
                                            })),
                                    ),
                            )
                    }),
            )
            .overflow_y_scroll()
            .flex_1()
            .min_h_0()
            .with_animation("ai-settings-in", appear(), |el, delta| el.opacity(delta))
            .into_any_element()
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
                    ToolState::Denied => ("×", theme.muted_foreground, Some("拒否しました".into())),
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
        let (shell_tip, shell_on) = match session.shell_place {
            ShellPlace::Closed => ("シェルを開く", false),
            ShellPlace::Docked => ("シェルを閉じる", true),
            ShellPlace::Window(_) => ("シェルを表示", true),
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

        let header = row()
            .flex_none()
            .h_10()
            .pl_3()
            .pr_2()
            .gap_0p5()
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .id("ai-current")
                    .flex_1()
                    .min_w_0()
                    .text_sm()
                    .truncate()
                    .child(title)
                    .test_support(),
            )
            .when(auto_run, |d| {
                d.child(
                    row()
                        .id("ai-auto-badge")
                        .flex_none()
                        .gap_1()
                        .px_2()
                        .mr_1()
                        .h_6()
                        .rounded_md()
                        .bg(theme.warning)
                        .text_color(theme.warning_foreground)
                        .text_xs()
                        .child(app_icon("zap").size_3())
                        .child("オート実行")
                        .test_support(),
                )
            })
            .child(
                icon_button("ai-new-chat", "message-square-plus", "新しいチャット").on_click(
                    cx.listener(|this, _, _, cx| {
                        this.session.update(cx, |s, cx| s.clear(cx));
                    }),
                ),
            )
            .child(
                icon_button("ai-shell-toggle", "terminal", shell_tip)
                    .selected(shell_on)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_shell(cx))),
            )
            .child(
                icon_button("ai-settings-toggle", "settings", "設定")
                    .selected(self.settings_open)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings_open = !this.settings_open;
                        cx.notify();
                    })),
            )
            .when(self.mode == PaneMode::Docked, |d| {
                d.child(
                    icon_button("ai-detach", "picture-in-picture-2", "別ウィンドウで開く")
                        .on_click(
                            cx.listener(|this, _, _, cx| this.detach(PaneMode::ChatWindow, cx)),
                        ),
                )
                .child(icon_button("ai-close", "x", "閉じる").on_click(cx.listener(
                    |this, _, _, cx| {
                        if let Some(main) = &this.main {
                            // メインの画面がもうなければ、閉じる先もない
                            let _ = main.update(cx, |main, cx| {
                                main.ai_open = false;
                                cx.notify();
                            });
                        }
                    },
                )))
            })
            .when(self.mode == PaneMode::ChatWindow, |d| {
                d.child(
                    icon_button("ai-attach", "panel-right", "メインウィンドウに戻す")
                        .on_click(|_, window, _| window.remove_window()),
                )
            });

        let mut list = col().gap_2().p_3();
        if session.items.is_empty() && !matches!(session.key, KeyState::Present { .. }) {
            list = list.child(
                div()
                    .id("ai-empty")
                    .flex()
                    .child(
                        Button::new("ai-empty-key")
                            .icon(app_icon("settings"))
                            .label("API キーを設定")
                            .outline()
                            .small()
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.settings_open = true;
                                cx.notify();
                            })),
                    )
                    .test_support(),
            );
        }
        for (ix, item) in session.items.iter().enumerate() {
            list = list.child(div().child(self.render_item(ix, item, cx)).with_animation(
                ("ai-item-in", ix),
                appear(),
                |el, delta| el.opacity(delta),
            ));
        }
        if busy && !session.own_command && session.pending.is_none() {
            list = list.child(
                row()
                    .id("ai-working")
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(Spinner::new().small())
                    .child(if session.command_running() {
                        "コマンドを実行中"
                    } else {
                        "考え中"
                    })
                    .test_support(),
            );
        }

        let pending =
            session.pending.as_ref().map(|pending| {
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
                                    .label("許可")
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
                                        .label("このチャットでは常に許可")
                                        .small()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.session.update(cx, |s, cx| {
                                                s.answer(Decision::AllowShellAlways, cx)
                                            });
                                        })),
                                )
                            })
                            .child(Button::new("ai-deny").label("拒否").small().on_click(
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
                    // 設定を開いている間は、設定がチャットの場所を全部使う
                    .when(self.settings_open, |d| d.hidden())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.chat_scroll)
                    .child(list),
            )
            .children(pending)
            .child(
                row()
                    .flex_none()
                    .items_end()
                    .when(self.settings_open, |d| d.hidden())
                    .gap_2()
                    .p_3()
                    .border_t_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .id("ai-input")
                            .flex_1()
                            .min_w_0()
                            .child(Textarea::new(&self.input).h(px(84.)))
                            .test_support(),
                    )
                    .when(busy, |d| {
                        d.child(
                            icon_button("ai-stop", "square", "停止（Ctrl+C）")
                                .danger()
                                .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                        )
                    })
                    .when(!busy, |d| {
                        d.child(
                            icon_button("ai-send", "arrow-up", "送信（Ctrl / ⌘ + Enter）")
                                .primary()
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.submit(window, cx)),
                                ),
                        )
                    }),
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
        let prompt: SharedString = format!("{} $", short_path(&session.cwd_label())).into();
        let count = self.shell_rows.len();
        let detached = self.mode == PaneMode::ShellWindow;
        let pane = cx.entity();
        let caret_of = self.shell_input.clone();

        let header = row()
            .flex_none()
            .h_10()
            .pl_3()
            .pr_2()
            .gap_0p5()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(app_icon("terminal").size_4())
            .child(div().flex_1().min_w_0().pl_1().text_sm().child("シェル"))
            .when(running, |d| {
                d.child(
                    icon_button("ai-shell-kill", "square", "強制終了（Ctrl+C）")
                        .danger()
                        .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                )
            })
            .child(
                icon_button(
                    "ai-shell-copy",
                    if self.copied { "check" } else { "copy" },
                    "すべてコピー",
                )
                .on_click(cx.listener(|this, _, _, cx| this.copy_shell(cx))),
            )
            .child(
                icon_button("ai-shell-clear", "eraser", "クリア（Ctrl+L）").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.session.update(cx, |s, cx| s.clear_shell(cx));
                    },
                )),
            )
            .when(!detached, |d| {
                d.child(
                    icon_button(
                        "ai-shell-detach",
                        "picture-in-picture-2",
                        "別ウィンドウで開く",
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.detach(PaneMode::ShellWindow, cx))),
                )
                .child(icon_button("ai-shell-close", "x", "閉じる").on_click(
                    cx.listener(|this, _, _, cx| this.set_shell_place(ShellPlace::Closed, cx)),
                ))
            })
            .when(detached, |d| {
                d.child(
                    icon_button("ai-shell-attach", "panel-right", "チャットの横に戻す")
                        .on_click(|_, window, _| window.remove_window()),
                )
            });

        let lines = uniform_list(
            "ai-shell-lines",
            count,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                let session = this.session.read(cx);
                range
                    .filter_map(|ix| {
                        let (line, range) = this.shell_rows.get(ix)?;
                        let line = session.shell.get(*line)?;
                        Some((ix, line, range.clone()))
                    })
                    .map(|(ix, line, range)| {
                        let base = match line.kind {
                            LineKind::Command => rgb(0xffffff),
                            LineKind::Out => rgb(TERM_FG),
                            LineKind::Err => rgb(0xff7b72),
                            LineKind::Info => rgb(TERM_DIM),
                        };
                        let mut highlights = Vec::new();
                        if line.kind == LineKind::Command && range.start == 0 && range.end > 0 {
                            // 先頭の `$`
                            highlights.push((0..1, HighlightStyle::color(rgb(TERM_PROMPT).into())));
                        }
                        for (span, color) in &line.spans {
                            let start = span.start.max(range.start);
                            let end = span.end.min(range.end);
                            if start < end {
                                highlights.push((
                                    start - range.start..end - range.start,
                                    HighlightStyle::color(term_color(*color)),
                                ));
                            }
                        }
                        let text = SharedString::from(line.text[range].to_owned());
                        div()
                            .id(("ai-shell-line", ix))
                            .h(SHELL_ROW)
                            .px(TERM_PAD)
                            .flex()
                            .items_center()
                            .whitespace_nowrap()
                            .text_color(base)
                            .child(StyledText::new(text).with_highlights(highlights))
                            .test_support()
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.shell_scroll)
        // 入力行は出力のすぐ下に続く。画面がいっぱいになったら、出力の側が送られる
        .h(SHELL_ROW * count as f32)
        .flex_shrink(1.)
        .min_h_0();

        col()
            .id("ai-shell")
            .key_context("AiShell")
            .on_action(cx.listener(|this, _: &StopAi, _, cx| this.stop(cx)))
            .on_action(cx.listener(|this, _: &ClearShell, _, cx| {
                this.session.update(cx, |s, cx| s.clear_shell(cx));
            }))
            // 入力行にいても、コマンドが動いている間の Ctrl+C は強制終了にする
            .capture_action(cx.listener(|this, _: &CopyText, _, cx| {
                if this.session.read(cx).command_running() {
                    this.stop(cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                this.history_step(true, window, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                this.history_step(false, window, cx);
                cx.stop_propagation();
            }))
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(header)
            .child(
                col()
                    .id("ai-shell-screen")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .py_2()
                    .bg(rgb(TERM_BG))
                    .text_color(rgb(TERM_FG))
                    .font_family(theme.mono_font_family.clone())
                    .text_size(term_font(cx))
                    .cursor_text()
                    // どこをクリックしても、入力行に打てる
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.shell_input.update(cx, |s, cx| s.focus(window, cx));
                    }))
                    .child(
                        canvas(
                            move |bounds, window, cx| {
                                pane.update(cx, |this, cx| {
                                    this.set_shell_width(bounds.size.width, window, cx)
                                });
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(lines)
                    .child(
                        row()
                            .flex_none()
                            .h(px(28.))
                            .px(TERM_PAD)
                            .gap_2()
                            .child(
                                div()
                                    .id("ai-shell-prompt")
                                    .flex_none()
                                    .text_color(rgb(if running { TERM_DIM } else { TERM_PROMPT }))
                                    .child(prompt)
                                    .test_support(),
                            )
                            .child(
                                div()
                                    .id("ai-shell-input")
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        Input::new(&self.shell_input)
                                            .aria_label("コマンド")
                                            .appearance(false)
                                            .text_color(rgb(TERM_FG)),
                                    )
                                    .test_support(),
                            ),
                    )
                    // 入力欄のカーソルはテーマの色（明るいテーマでは黒）なので、見える色で重ねて描く
                    .child(
                        canvas(
                            |_, _, _| {},
                            move |_, _, window, cx| {
                                let state = caret_of.read(cx);
                                if !state.focus_handle(cx).is_focused(window) {
                                    return;
                                }
                                if let Some(caret) = state.prepaint_caret_geometry() {
                                    let at = caret.cursor_bounds();
                                    window.paint_quad(fill(
                                        Bounds::new(at.origin, size(px(2.), at.size.height)),
                                        rgb(TERM_FG),
                                    ));
                                }
                            },
                        )
                        .absolute()
                        .size_full(),
                    ),
            )
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
        self.refresh_rows(cx);
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
                    .when(docked.is_none(), |d| {
                        // 上のバーとタスクの画面を残して、右に重ねる
                        d.absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            .w(CHAT_WIDTH)
                            .max_w_full()
                            .border_l_1()
                            .border_color(theme.border)
                            .shadow_lg()
                    })
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
                    .with_animation("ai-pane-in", appear(), |el, delta| el.opacity(delta))
                    .into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{docked_width, px, short_path};

    #[test]
    fn a_long_place_is_shortened_in_the_prompt() {
        assert_eq!(short_path("~/work/app"), "~/work/app");
        assert_eq!(
            short_path("/private/var/folders/ab/cdefghijk/T/build"),
            "…/T/build"
        );
        let long = short_path("/a/とても長い名前のフォルダーとても長い名前のフォルダーです");
        assert_eq!(long.chars().count(), 23);
        assert!(long.starts_with('…') && long.ends_with("フォルダーです"));
    }

    #[test]
    fn the_pane_fits_the_window() {
        // 狭ければタスクの画面の右に重ねる
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
