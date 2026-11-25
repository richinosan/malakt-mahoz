//! AI チャットとシェルの画面。メインのウィンドウの右にも、別のウィンドウにも出せる。

use std::ops::Range;
use std::time::Duration;

use gpui_kit::base::TestSupportExt;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::input::{
    Copy as CopyText, Input, InputEvent, InputState, MoveDown, MoveUp, Textarea, TextareaState,
};
use gpui_kit::component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme, Disableable, Selectable, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_ai::ansi::{self, Color};
use malakt_ai::keys::Source;
use malakt_ai::{Effort, Provider};

use crate::ai::{
    self, Decision, Item, KeyState, LineKind, Run, Session, ShellPlace, ToolState, Watch,
    WATCH_MINUTES,
};
use crate::db::{self, Db};
use crate::feel::{self, Lit};
use crate::render::app_icon;
use crate::shadow::{Elevate, Elevation};
use crate::state::{DropTarget, MainView};
use crate::term::{self, TermPane};
use crate::{ClearShell, StopAi};

/// 参照する寸法を、画面全体の拡大率に合わせる。
const fn px(value: f32) -> Pixels {
    gpui_kit::px(value * crate::UI_SCALE)
}

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
/// 何もないチャットに出す、押すとそのまま送る依頼。
const SUGGESTIONS: [&str; 3] = [
    "今日やることを教えて",
    "期限が過ぎたタスクを教えて",
    "今週の予定をまとめて",
];

/// チャットの場所に出す一覧。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// 保存してある会話
    Chats,
    /// アプリが自動で始めた実行の履歴
    Runs,
}

/// 実行 1 回分の中身。失敗の理由と、AI がしたこと。
pub fn run_details(run: &Run, theme: &gpui_kit::component::Theme) -> Div {
    let mut body = col().gap_1().px_3().pb_2().text_xs();
    if let Some(error) = &run.error {
        body = body.child(div().text_color(theme.danger).child(error.clone()));
    }
    for item in &run.items {
        let (text, color) = match item {
            Item::Tool { summary, state } => (
                match state {
                    ToolState::Failed(why) => format!("{summary}: {why}").into(),
                    _ => summary.clone(),
                },
                theme.muted_foreground,
            ),
            // 失敗の理由は、上に出している
            Item::Error(text) if run.error.as_ref() == Some(text) => continue,
            Item::Error(text) => (text.clone(), theme.danger),
            Item::User(text) | Item::Assistant(text) | Item::Notice(text) => {
                (text.clone(), theme.foreground)
            }
        };
        body = body.child(div().text_color(color).child(text));
    }
    body
}

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
pub fn docked_width(
    window_width: Pixels,
    shell_docked: bool,
    (chat, shell): (f32, f32),
) -> Option<Pixels> {
    let room = window_width - TASKS_MIN;
    if room < CHAT_MIN {
        None
    } else if side_by_side(window_width, shell_docked, shell) {
        Some(room.min(px(chat) + px(shell)))
    } else {
        Some(room.min(px(chat)))
    }
}

/// シェルをチャットの右に並べる幅があるか。
fn side_by_side(window_width: Pixels, shell_docked: bool, shell: f32) -> bool {
    shell_docked && window_width - TASKS_MIN >= CHAT_MIN + px(shell)
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
    /// ターミナルの画面。
    term: Entity<TermPane>,
    /// シェルを、チャットの場所まで広げている。
    pub shell_wide: bool,
    input: Entity<TextareaState>,
    key_input: Entity<InputState>,
    model_input: Entity<InputState>,
    /// 定期チェックの指示。
    watch_input: Entity<TextareaState>,
    context_input: Entity<TextareaState>,
    /// モデルの入力欄に出している値。
    /// 設定で、モデルの一覧を開いている。
    model_open: bool,
    pub settings_open: bool,
    /// チャットの場所に出している一覧。
    pub side: Option<Side>,
    /// 実行履歴で、中身を開いている実行。
    run_open: Option<String>,
    /// ドラッグしている境界。
    resize: Option<Resize>,
    /// 開いて見せている、ツール実行のまとまり（最初の行の番号）。
    open_tools: std::collections::HashSet<usize>,
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
        .rounded(feel::md())
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

/// ドラッグで動かす境界。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resize {
    /// タスクの画面とチャットの間
    Chat,
    /// チャットとシェルの間
    Shell,
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
                .submit_on_enter(true)
                // 1 行から始め、書いた分だけ伸びる
                .auto_grow(1, 8)
                .placeholder("メッセージを入力（Shift+Enter で改行）")
        });
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
        let context = session.read(cx).context.clone();
        let context_input = cx.new(|cx| {
            let mut state =
                TextareaState::new(window, cx).placeholder("例: PR のタスクは優先度を高にする");
            state.set_value(context, window, cx);
            state
        });
        let shell_input = cx.new(|cx| InputState::new(window, cx));
        term::shared(cx).update(cx, |terms, cx| terms.load_profiles(cx));

        let subscriptions = vec![
            cx.observe(&session, |this, _, cx| this.sync(cx)),
            cx.observe(&term::shared(cx), |_, _, cx| cx.notify()),
            cx.observe(&db, |_, _, cx| cx.notify()),
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| {
                // Enter で送り、Shift+Enter で改行する
                if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
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
        // 別ウィンドウの位置と大きさを覚えて、次に開くときに戻す
        let slot = match mode {
            PaneMode::ChatWindow => Some(0),
            PaneMode::ShellWindow => Some(1),
            PaneMode::Docked => None,
        };
        if let Some(slot) = slot {
            cx.observe_window_bounds(window, move |this, window, cx| {
                let b = window.bounds();
                let now = Some([
                    f32::from(b.origin.x),
                    f32::from(b.origin.y),
                    f32::from(b.size.width),
                    f32::from(b.size.height),
                ]);
                this.session.update(cx, |s, cx| {
                    if s.window_bounds[slot] != now {
                        s.window_bounds[slot] = now;
                        cx.notify();
                    }
                });
            })
            .detach();
        }
        // キーがあるかを調べる（キーチェーンを読むだけで、通信はしない）
        if mode != PaneMode::ShellWindow && session.read(cx).key == KeyState::Unknown {
            session.update(cx, |s, cx| s.refresh_key(cx));
        }
        let seen = session.read(cx).shown_items().len();
        Self {
            session,
            db,
            mode,
            main,
            focus: cx.focus_handle(),
            shell_focus: cx.focus_handle(),
            term: cx.new(TermPane::new),
            shell_wide: false,
            input,
            key_input,
            model_input,
            watch_input,
            context_input,
            model_open: false,
            settings_open: false,
            side: None,
            run_open: None,
            resize: None,
            open_tools: std::collections::HashSet::new(),
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

    /// 境界のドラッグ。`from_right` は、ウィンドウの右端からマウスまでの幅。
    fn drag_resize(
        &mut self,
        what: Resize,
        from_right: f32,
        window_width: Pixels,
        cx: &mut Context<Self>,
    ) {
        self.session.update(cx, |s, cx| {
            let together = side_by_side(
                window_width,
                s.shell_place == ShellPlace::Docked,
                s.shell_width,
            );
            // タスクの画面に残す幅より先には広げない
            let room = f32::from(window_width - TASKS_MIN) / crate::UI_SCALE;
            match what {
                // 左の境界。シェルが右に並んでいれば、その分を引いたのがチャットの幅
                Resize::Chat => {
                    let shell = if together { s.shell_width } else { 0. };
                    let (chat, shell_width) = (from_right.min(room) - shell, s.shell_width);
                    s.set_widths(chat, shell_width, cx);
                }
                // チャットとシェルの境界。全体の幅は変えずに、分け方を変える
                Resize::Shell => {
                    let total = s.chat_width + s.shell_width;
                    let shell = from_right.clamp(ai::SHELL_MIN_WIDTH, total - ai::CHAT_MIN_WIDTH);
                    s.set_widths(total - shell, shell, cx);
                }
            }
        });
    }

    /// ドラッグを終える。幅はここで保存される。
    fn finish_resize(&mut self, cx: &mut Context<Self>) {
        if self.resize.take().is_some() {
            self.session.update(cx, |s, cx| {
                s.resizing = false;
                cx.notify();
            });
            cx.notify();
        }
    }

    /// チャットの変化に合わせる。新しい行が出たら、いちばん下まで送る。
    fn sync(&mut self, cx: &mut Context<Self>) {
        let items = self.session.read(cx).shown_items().len();
        if items != self.seen {
            self.chat_scroll.scroll_to_bottom();
        }
        if items < self.seen {
            // 新しいチャットでは、行の番号が最初からになる
            self.open_tools.clear();
        }
        self.seen = items;
        // さかのぼって読んでいる間は、新しい出力が来ても動かさない
        let follow = self.shell_at_bottom();
        if self.refresh_rows(cx) && follow {
            self.shell_to_bottom();
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

    /// ターミナルの画面。
    pub fn term_pane(&self) -> Entity<TermPane> {
        self.term.clone()
    }

    pub fn focus_input(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }

    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        if self.send_text(&text, cx) {
            self.input.update(cx, |s, cx| s.set_value("", window, cx));
        }
    }

    /// 今見ている場所を添えて、AI に送る。
    fn send_text(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let place = self
            .main
            .as_ref()
            .and_then(|main| main.read_with(cx, |main, _| main.ai_place()).ok())
            // 別ウィンドウのチャットでも、メインのウィンドウで見ている場所を伝える
            .or_else(|| Some(self.session.read(cx).place.clone()).filter(|p| !p.is_empty()))
            .unwrap_or_else(|| "ユーザーは、チャットだけのウィンドウを開いています。".to_owned());
        self.session.update(cx, |s, cx| s.send(text, place, cx))
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

        let model_open = self.model_open;
        let mut models = col()
            .id("ai-model-list")
            .max_h(px(260.))
            .overflow_y_scroll()
            .p_1();
        let mut ix = 0usize;
        let groups = if model_open {
            self.model_groups(cx)
        } else {
            Vec::new()
        };
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
        let track = session.track;
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
                .rounded(feel::lg())
                .border_1()
                .border_color(theme.border)
                .bg(theme.background)
                .elevate(Elevation::Low, cx)
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
                    .child(
                        feel::gloss(Button::new("ai-model-open")
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
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.model_open = !this.model_open;
                                if this.model_open {
                                    this.model_input
                                        .update(cx, |s, cx| s.focus(window, cx));
                                    // 一覧がまだなければ、開いたときに取ってくる
                                    this.session.update(cx, |s, cx| {
                                        if s.listed.is_empty()
                                            && matches!(s.key, KeyState::Present { .. })
                                        {
                                            s.fetch_models(cx);
                                        }
                                    });
                                }
                                cx.notify();
                            }))),
                    )
                    .when(model_open, |d| {
                        d.child(
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
                                                || provider.catalog_url().is_some(),
                                            |d| {
                                                d.child(
                                                    feel::gloss(icon_button(
                                                        "ai-fetch-models",
                                                        "refresh-cw",
                                                        "モデル一覧を更新",
                                                    )
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.session
                                                            .update(cx, |s, cx| s.fetch_models(cx));
                                                    }))),
                                                )
                                            },
                                        ),
                                )
                                .child(models)
                                .with_animation("ai-model-list-in", appear(), |el, delta| {
                                    el.opacity(delta)
                                }),
                        )
                    })
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
                                    feel::gloss(icon_button("ai-key-delete", "trash", "API キーを削除")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.session.update(cx, |s, cx| s.delete_key(cx));
                                        }))),
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
                                feel::gloss(icon_button("ai-key-save", "check", "API キーを保存")
                                    .primary()
                                    .on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.save_key(window, cx)
                                        }),
                                    )),
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
                section().child(label("いつも AI に伝えること")).child(
                    row()
                        .gap_1()
                        .items_end()
                        .child(
                            div()
                                .id("ai-context")
                                .flex_1()
                                .min_w_0()
                                .child(Textarea::new(&self.context_input).h(px(72.)))
                                .test_support(),
                        )
                        .child(
                            feel::gloss(icon_button("ai-context-save", "check", "保存").on_click(cx.listener(|this, _, _, cx| {
                                let text = this.context_input.read(cx).value().to_string();
                                this.session.update(cx, |s, cx| s.set_context(&text, cx));
                            }))),
                        ),
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
                section().child(
                    Switch::new("ai-track")
                        .checked(track)
                        .label("コメントから進捗を更新する")
                        .tooltip("タスクにコメントを書くと、AI が進捗・状態・期限を更新する")
                        .on_click(cx.listener(|this, on: &bool, _, cx| {
                            let on = *on;
                            this.session.update(cx, |s, cx| s.set_track(on, cx));
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
                                feel::gloss(icon_button("ai-watch-now", "play", "今すぐチェック")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.session.update(cx, |s, cx| {
                                            s.check_github(cx);
                                        });
                                    }))),
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
                                        feel::gloss(icon_button("ai-watch-save", "check", "保存")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.set_watch(|_| {}, cx)
                                            }))),
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

    /// チャットからタスクを開く。メインのウィンドウに置いたチャットでだけ動く。
    fn open_task(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(main) = &self.main {
            // メインの画面がなくなっていれば、開く先がない
            let _ = main.update(cx, |main, cx| main.open_panel(id, window, cx));
        }
    }

    fn run_title(run_kind: ai::RunKind, tasks: &[(String, SharedString)]) -> String {
        let titles: Vec<&str> = tasks.iter().map(|(_, title)| title.as_ref()).collect();
        if titles.is_empty() {
            run_kind.label().to_owned()
        } else {
            format!("{}: {}", run_kind.label(), titles.join("、"))
        }
    }

    /// 入力欄の上に出す、動いている実行と失敗した実行。
    fn render_tray(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = cx.theme().clone();
        let session = self.session.read(cx);
        let running = session
            .auto_running()
            .map(|(kind, tasks)| (Self::run_title(kind, tasks), tasks.first().cloned()));
        let failed: Vec<Run> = session
            .runs
            .iter()
            .rev()
            .filter(|run| run.error.is_some() && !run.seen)
            .take(3)
            .cloned()
            .collect();
        if running.is_none() && failed.is_empty() {
            return None;
        }
        let line = || {
            row()
                .gap_2()
                .items_center()
                .h_7()
                .px_2()
                .rounded(feel::md())
                .text_xs()
        };
        let mut tray = col().id("ai-tray").gap_1().pb_2();
        if let Some((title, task)) = running {
            tray = tray.child(
                line()
                    .id("ai-running")
                    .bg(theme.muted)
                    .text_color(theme.muted_foreground)
                    .cursor_pointer()
                    .lit(theme.accent, cx)
                    .child(Spinner::new().small())
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some((id, _)) = &task {
                            this.open_task(id, window, cx);
                        }
                    }))
                    .test_support(),
            );
        }
        for (ix, run) in failed.into_iter().enumerate() {
            let detail = run.error.clone().unwrap_or_default();
            let task = run.tasks.first().map(|(id, _)| id.clone());
            let (retry, dismiss) = (run.id.clone(), run.id.clone());
            tray = tray.child(
                line()
                    .bg(theme.danger.opacity(0.12))
                    .child(
                        row()
                            .id(("ai-failed", ix))
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .items_center()
                            .cursor_pointer()
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(theme.danger)
                                    .child(app_icon("circle-x").size_3p5()),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(Self::run_title(run.kind, &run.tasks)),
                            )
                            .tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(detail.clone())
                                    .build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, window, cx| match &task {
                                Some(id) => this.open_task(id, window, cx),
                                None => {
                                    this.side = Some(Side::Runs);
                                    cx.notify();
                                }
                            }))
                            .test_support(),
                    )
                    .child(feel::gloss(
                        Button::new(("ai-failed-retry", ix))
                            .icon(app_icon("refresh-cw"))
                            .ghost()
                            .xsmall()
                            .tooltip("もう一度実行")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.session.update(cx, |s, cx| s.retry_run(&retry, cx));
                            })),
                    ))
                    .child(feel::gloss(
                        Button::new(("ai-failed-dismiss", ix))
                            .icon(app_icon("x"))
                            .ghost()
                            .xsmall()
                            .tooltip("表示を消す")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.session.update(cx, |s, cx| s.dismiss_run(&dismiss, cx));
                            })),
                    )),
            );
        }
        Some(tray.test_support().into_any_element())
    }

    /// 保存してある会話の一覧。
    fn render_chats(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let session = self.session.read(cx);
        let now = malakt_core::now_ms();
        let mut list = col().gap_1().p_2();
        if session.chats.is_empty() {
            list = list.child(
                div()
                    .id("ai-chats-empty")
                    .pt_10()
                    .text_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("会話はまだありません")
                    .test_support(),
            );
        }
        for (ix, chat) in session.chats.iter().enumerate() {
            let (open, delete) = (chat.id.clone(), chat.id.clone());
            let active = session.chat_id() == chat.id;
            list = list.child(
                row()
                    .gap_2()
                    .items_center()
                    .px_2()
                    .h_10()
                    .rounded(feel::md())
                    .when(active, |d| d.bg(theme.muted))
                    .lit(theme.muted, cx)
                    .child(
                        row()
                            .id(("ai-chat-row", ix))
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .gap_2()
                            .items_center()
                            .cursor_pointer()
                            .child(
                                div().flex_none().text_color(theme.muted_foreground).child(
                                    app_icon(if chat.task.is_some() {
                                        "list-todo"
                                    } else {
                                        "sparkles"
                                    })
                                    .size_4(),
                                ),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .child(chat.title.clone()),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(malakt_core::view::format_relative(chat.updated, now)),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.session.update(cx, |s, cx| s.open_chat(&open, cx));
                                this.side = None;
                                this.focus_input(window, cx);
                                cx.notify();
                            }))
                            .test_support(),
                    )
                    .child(feel::gloss(
                        Button::new(("ai-chat-delete", ix))
                            .icon(app_icon("trash"))
                            .ghost()
                            .xsmall()
                            .tooltip("会話を消す")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.session.update(cx, |s, cx| s.delete_chat(&delete, cx));
                            })),
                    )),
            );
        }
        div()
            .id("ai-chats-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(list)
            .test_support()
            .into_any_element()
    }

    /// アプリが自動で始めた実行の履歴。新しいものが上。
    fn render_runs(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let session = self.session.read(cx);
        let now = malakt_core::now_ms();
        let mut list = col().gap_1().p_2();
        if session.runs.is_empty() {
            list = list.child(
                div()
                    .id("ai-runs-empty")
                    .pt_10()
                    .text_center()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("実行はまだありません")
                    .test_support(),
            );
        }
        for (ix, run) in session.runs.iter().rev().enumerate() {
            let failed = run.error.is_some();
            let opened = self.run_open.as_deref() == Some(run.id.as_str());
            let (toggle, retry) = (run.id.clone(), run.id.clone());
            let task = run.tasks.first().map(|(id, _)| id.clone());
            let mut entry = col()
                .rounded(feel::md())
                .hover(|s| s.bg(theme.muted))
                .child(
                    row()
                        .gap_2()
                        .items_center()
                        .px_2()
                        .h_10()
                        .child(
                            row()
                                .id(("ai-run-row", ix))
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .gap_2()
                                .items_center()
                                .cursor_pointer()
                                .child(
                                    div()
                                        .flex_none()
                                        .text_color(if failed {
                                            theme.danger
                                        } else {
                                            theme.success
                                        })
                                        .child(
                                            app_icon(if failed {
                                                "circle-x"
                                            } else {
                                                "circle-check"
                                            })
                                            .size_4(),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .child(Self::run_title(run.kind, &run.tasks)),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(malakt_core::view::format_relative(run.at, now)),
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.run_open =
                                        if this.run_open.as_deref() == Some(toggle.as_str()) {
                                            None
                                        } else {
                                            Some(toggle.clone())
                                        };
                                    cx.notify();
                                }))
                                .test_support(),
                        )
                        .when(failed, |d| {
                            d.child(feel::gloss(
                                Button::new(("ai-run-retry", ix))
                                    .icon(app_icon("refresh-cw"))
                                    .ghost()
                                    .xsmall()
                                    .tooltip("もう一度実行")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.session.update(cx, |s, cx| s.retry_run(&retry, cx));
                                    })),
                            ))
                        })
                        .when_some(task.filter(|_| self.main.is_some()), |d, task| {
                            d.child(feel::gloss(
                                Button::new(("ai-run-task", ix))
                                    .icon(app_icon("list-todo"))
                                    .ghost()
                                    .xsmall()
                                    .tooltip("タスクを開く")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_task(&task, window, cx);
                                    })),
                            ))
                        }),
                );
            if opened {
                entry = entry.child(
                    run_details(run, &theme)
                        .id(("ai-run-detail", ix))
                        .test_support(),
                );
            }
            list = list.child(entry);
        }
        div()
            .id("ai-runs-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .child(list)
            .test_support()
            .into_any_element()
    }

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
                        .rounded(feel::lg())
                        .bg(theme.primary)
                        .text_color(theme.primary_foreground)
                        .elevate(Elevation::Low, cx)
                        .text_sm()
                        .child(text.clone())
                        .test_support(),
                )
                .into_any_element(),
            // AI の返答は、枠も印も付けずにペインの幅いっぱいに出す
            Item::Assistant(text) => row()
                .items_start()
                .child(
                    div()
                        .id(("ai-assistant", ix))
                        .flex_1()
                        .min_w_0()
                        .px_1()
                        .py_1()
                        .text_sm()
                        // ドラッグで選んでコピーできる
                        .child(TextView::markdown(("ai-text", ix), text.clone()).selectable(true))
                        .test_support()
                        // 右クリックで、返答を丸ごとコピーする
                        .context_menu({
                            let text = text.clone();
                            move |menu, _, _| {
                                let text = text.clone();
                                menu.item(PopupMenuItem::new("返答をコピー").on_click(
                                    move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            text.to_string(),
                                        ));
                                    },
                                ))
                            }
                        }),
                )
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
            // アプリからの知らせは、真ん中に小さく
            Item::Notice(text) => row()
                .justify_center()
                .child(
                    div()
                        .id(("ai-notice", ix))
                        .max_w(relative(0.9))
                        .px_3()
                        .py_1()
                        .rounded(feel::lg())
                        .border_1()
                        .border_color(theme.border)
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(text.clone())
                        .test_support(),
                )
                .into_any_element(),
            Item::Error(text) => div()
                .id(("ai-error", ix))
                .px_2()
                .py_1()
                .rounded(feel::md())
                .bg(theme.danger)
                .text_color(theme.danger_foreground)
                .text_sm()
                .child(text.clone())
                .test_support()
                .into_any_element(),
        }
    }

    /// 続けて実行したツールのまとまり。押すと、1 つずつの中身が開く。
    fn render_tools(
        &self,
        start: usize,
        tools: &[Item],
        pane: WeakEntity<Self>,
        cx: &App,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let open = self.open_tools.contains(&start);
        let state_of = |item: &Item| match item {
            Item::Tool { state, .. } => Some(state.clone()),
            _ => None,
        };
        let failed = tools.iter().any(|item| {
            matches!(
                state_of(item),
                Some(ToolState::Failed(_) | ToolState::Denied)
            )
        });
        let running = tools
            .iter()
            .any(|item| state_of(item) == Some(ToolState::Running));
        let last = match tools.last() {
            Some(Item::Tool { summary, .. }) => summary.clone(),
            _ => SharedString::default(),
        };
        let lines: Vec<AnyElement> = if open {
            tools
                .iter()
                .enumerate()
                .map(|(offset, item)| self.render_item(start + offset, item, cx))
                .collect()
        } else {
            Vec::new()
        };
        col()
            .rounded(feel::md())
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .elevate(Elevation::Low, cx)
            .child(
                row()
                    .id(("ai-tools", start))
                    .gap_2()
                    .px_2()
                    .py_1()
                    .items_center()
                    .text_xs()
                    .cursor_pointer()
                    .rounded(feel::md())
                    .lit(theme.muted, cx)
                    .active(|s| s.opacity(0.6))
                    .child(
                        app_icon(if open {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        })
                        .size_3()
                        .flex_none(),
                    )
                    .child(if running {
                        Spinner::new().small().into_any_element()
                    } else {
                        app_icon("wrench")
                            .size_3()
                            .flex_none()
                            .text_color(if failed {
                                theme.danger
                            } else {
                                theme.muted_foreground
                            })
                            .into_any_element()
                    })
                    .child(
                        div()
                            .flex_none()
                            .child(format!("ツール {} 件", tools.len())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.muted_foreground)
                            .child(last),
                    )
                    .on_click(move |_: &ClickEvent, _, cx| {
                        // チャットを閉じたあとなら、開く先がない
                        let _ = pane.update(cx, |this, cx| {
                            if !this.open_tools.remove(&start) {
                                this.open_tools.insert(start);
                            }
                            cx.notify();
                        });
                    })
                    .test_support(),
            )
            .when(open, |d| {
                d.child(
                    col()
                        .border_t_1()
                        .border_color(theme.border)
                        .children(lines)
                        .with_animation(("ai-tools-in", start), appear(), |el, delta| {
                            el.opacity(delta)
                        }),
                )
            })
            .into_any_element()
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

    /// サービス・モデル・effort を、押してその場で選ぶボタン。
    fn render_pills(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let session = self.session.read(cx);
        let provider = session.provider;
        let model = session.model().to_owned();
        let effort = session.effort;
        let models: Vec<SharedString> = provider
            .models()
            .iter()
            .map(|m| SharedString::from(*m))
            .chain((!model.is_empty()).then(|| SharedString::from(model.clone())))
            .fold(Vec::new(), |mut all, m| {
                if !all.contains(&m) {
                    all.push(m);
                }
                all
            });
        let pane = cx.entity().downgrade();
        let labels: Vec<SharedString> = models.iter().map(|m| session.model_label(m)).collect();
        let pill = |id: &'static str, label: SharedString, tip: SharedString| {
            Button::new(id)
                .ghost()
                .small()
                .label(label)
                .dropdown_caret(true)
                .tooltip(tip)
        };
        let mut pills = Vec::new();
        pills.push({
            let pane = pane.clone();
            pill(
                "ai-pick-provider",
                provider.label().into(),
                "サービス".into(),
            )
            .dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
                for p in Provider::ALL {
                    let pane = pane.clone();
                    menu = menu.item(
                        PopupMenuItem::new(p.label())
                            .checked(p == provider)
                            .on_click(move |_, _, cx| {
                                // チャットを閉じたあとなら、反映する先がない
                                let _ = pane.update(cx, |this, cx| {
                                    this.session.update(cx, |s, cx| s.set_provider(p, cx));
                                });
                            }),
                    );
                }
                menu
            })
            .into_any_element()
        });
        pills.push({
            let pane = pane.clone();
            // 名前を出し、ID はホバーで出す
            let (shown, tip): (SharedString, SharedString) = if model.is_empty() {
                ("標準のモデル".into(), "モデル".into())
            } else {
                (session.model_label(&model), model.clone().into())
            };
            div()
                .min_w_0()
                .overflow_hidden()
                .child(pill("ai-pick-model", shown, tip).dropdown_menu_with_anchor(
                    Anchor::BottomLeft,
                    move |mut menu, _, _| {
                        for (m, label) in models.iter().zip(&labels) {
                            let m = m.clone();
                            let pane = pane.clone();
                            let chosen = m.clone();
                            menu = menu.item(
                                PopupMenuItem::new(label.clone())
                                    .checked(m.as_ref() == model)
                                    .on_click(move |_, _, cx| {
                                        let _ = pane.update(cx, |this, cx| {
                                            this.session
                                                .update(cx, |s, cx| s.set_model(&chosen, cx));
                                        });
                                    }),
                            );
                        }
                        let pane = pane.clone();
                        menu.separator()
                            .item(
                                PopupMenuItem::new("ほかのモデル…").on_click(move |_, _, cx| {
                                    let _ = pane.update(cx, |this, cx| {
                                        this.settings_open = true;
                                        this.model_open = true;
                                        cx.notify();
                                    });
                                }),
                            )
                    },
                ))
                .into_any_element()
        });
        if provider.supports_effort() {
            pills.push(
                pill("ai-pick-effort", effort.label().into(), "effort".into())
                    .dropdown_menu_with_anchor(Anchor::BottomLeft, move |mut menu, _, _| {
                        for e in Effort::ALL {
                            let pane = pane.clone();
                            menu = menu.item(
                                PopupMenuItem::new(e.label()).checked(e == effort).on_click(
                                    move |_, _, cx| {
                                        let _ = pane.update(cx, |this, cx| {
                                            this.session.update(cx, |s, cx| s.set_effort(e, cx));
                                        });
                                    },
                                ),
                            );
                        }
                        menu
                    })
                    .into_any_element(),
            );
        }
        pills
    }

    fn render_chat(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let session = self.session.read(cx);
        let busy = session.busy;
        let auto_run = session.auto_run;
        let shell_label = match session.shell_place {
            ShellPlace::Closed => "シェルを開く",
            ShellPlace::Docked => "シェルを閉じる",
            ShellPlace::Window(_) => "シェルを表示",
        };
        let mode = self.mode;
        let pane = cx.entity().downgrade();
        let close = (mode == PaneMode::Docked).then(|| {
            feel::gloss(icon_button("ai-close", "x", "閉じる").on_click(cx.listener(
                |this, _, _, cx| {
                    if let Some(main) = &this.main {
                        // メインの画面がもうなければ、閉じる先もない
                        let _ = main.update(cx, |main, cx| {
                            main.ai_open = false;
                            cx.notify();
                        });
                        this.session.update(cx, |s, cx| s.set_open(false, cx));
                    }
                },
            )))
        });
        let bar = || {
            row()
                .flex_none()
                .h_10()
                .pl_3()
                .pr_2()
                .gap_0p5()
                .border_b_1()
                .border_color(theme.border)
        };
        let badge = auto_run.then(|| {
            row()
                .id("ai-auto-badge")
                .flex_none()
                .gap_1()
                .px_2()
                .mr_1()
                .h_6()
                .rounded(feel::md())
                .bg(theme.warning)
                .text_color(theme.warning_foreground)
                .text_xs()
                .child(app_icon("zap").size_3())
                .child("オート実行")
                .test_support()
        });
        let header = if self.settings_open {
            // 設定の間は、戻るボタンと「設定」だけにする
            bar()
                .pl_2()
                .child(feel::gloss(
                    icon_button("ai-settings-toggle", "arrow-left", "チャットに戻る").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.settings_open = false;
                            cx.notify();
                        }),
                    ),
                ))
                .child(div().flex_1().min_w_0().pl_1().text_sm().child("AI の設定"))
                .children(badge)
                .children(close)
        } else if let Some(side) = self.side {
            bar()
                .pl_2()
                .child(feel::gloss(
                    icon_button("ai-side-back", "arrow-left", "チャットに戻る").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.side = None;
                            cx.notify();
                        }),
                    ),
                ))
                .child(div().flex_1().min_w_0().pl_1().text_sm().child(match side {
                    Side::Chats => "会話",
                    Side::Runs => "AI の実行履歴",
                }))
                .children(badge)
                .children(close)
        } else {
            bar()
                .child(app_icon("sparkles").size_4())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .pl_1()
                        .text_sm()
                        .child("AI チャット"),
                )
                .children(badge)
                .child(feel::gloss(
                    icon_button("ai-chats", "history", "会話の一覧").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.side = Some(Side::Chats);
                            cx.notify();
                        },
                    )),
                ))
                .child(feel::gloss(
                    icon_button("ai-new-chat", "message-square-plus", "新しいチャット").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.session.update(cx, |s, cx| s.clear(cx));
                        }),
                    ),
                ))
                .child(
                    // 残りの操作は、名前の付いたメニューにまとめる
                    icon_button("ai-menu", "ellipsis", "メニュー").dropdown_menu_with_anchor(
                        Anchor::TopRight,
                        move |menu, _, _| {
                            let item = |label: &'static str,
                                        icon: &str,
                                        run: fn(&mut AiPane, &mut Window, &mut Context<AiPane>)| {
                                let pane = pane.clone();
                                PopupMenuItem::new(label).icon(app_icon(icon)).on_click(
                                    move |_, window, cx| {
                                        // チャットを閉じたあとなら、反映する先がない
                                        let _ = pane.update(cx, |this, cx| run(this, window, cx));
                                    },
                                )
                            };
                            menu.item(item("設定", "settings", |this, _, cx| {
                                this.settings_open = true;
                                cx.notify();
                            }))
                            .item(item(shell_label, "terminal", |this, _, cx| {
                                this.toggle_shell(cx)
                            }))
                            .item(if mode == PaneMode::ChatWindow {
                                item(
                                    "メインウィンドウに戻す",
                                    "panel-right",
                                    |_, window, _| window.remove_window(),
                                )
                            } else {
                                item(
                                    "別ウィンドウで開く",
                                    "picture-in-picture-2",
                                    |this, _, cx| this.detach(PaneMode::ChatWindow, cx),
                                )
                            })
                            .item(item(
                                "AI の実行履歴",
                                "list-todo",
                                |this, _, cx| {
                                    this.side = Some(Side::Runs);
                                    cx.notify();
                                },
                            ))
                        },
                    ),
                )
                .children(close)
        };

        let mut list = col().gap_2().p_3();
        if session.shown_items().is_empty() {
            let has_key = matches!(session.key, KeyState::Present { .. });
            let mut start = col().id("ai-empty").items_center().gap_2().pt_10().child(
                div()
                    .text_color(theme.muted_foreground)
                    .child(app_icon("sparkles").size_8()),
            );
            if has_key {
                // 押すと、そのまま送る
                for (ix, text) in SUGGESTIONS.into_iter().enumerate() {
                    start = start.child(feel::gloss(
                        Button::new(("ai-suggest", ix))
                            .label(text)
                            .outline()
                            .small()
                            .rounded(px(999.))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.send_text(text, cx);
                            })),
                    ));
                }
            } else {
                start = start.child(feel::gloss(
                    Button::new("ai-empty-key")
                        .icon(app_icon("settings"))
                        .label("API キーを設定")
                        .outline()
                        .small()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.settings_open = true;
                            cx.notify();
                        })),
                ));
            }
            list = list.child(start.test_support());
        }
        let items = session.shown_items();
        let pane = cx.entity().downgrade();
        let mut ix = 0;
        while ix < items.len() {
            // 続けて実行したツールは、1 つにまとめて畳む
            let start = ix;
            let mut end = ix;
            while end < items.len() && matches!(items[end], Item::Tool { .. }) {
                end += 1;
            }
            let element = if end - ix >= 2 {
                ix = end;
                self.render_tools(start, &items[start..end], pane.clone(), cx)
            } else {
                ix += 1;
                self.render_item(start, &items[start], cx)
            };
            list = list.child(div().child(element).with_animation(
                ("ai-item-in", start),
                appear(),
                |el, delta| el.opacity(delta),
            ));
        }
        // 自動の実行は、入力欄の上に出す
        if busy
            && !session.own_command
            && session.pending.is_none()
            && session.auto_running().is_none()
        {
            list = list.child(
                row()
                    .id("ai-working")
                    .gap_2()
                    .items_center()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .px_1()
                    .child(Spinner::new().small())
                    .child(if session.command_running() {
                        "コマンドを実行中"
                    } else {
                        "考え中"
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
                .rounded(feel::md())
                .border_1()
                .border_color(theme.primary)
                .bg(theme.background)
                .elevate(Elevation::Mid, cx)
                .child(div().text_sm().child(pending.text.clone()))
                .child(
                    row()
                        .gap_2()
                        .flex_wrap()
                        .child(feel::gloss(
                            Button::new("ai-allow")
                                .label("許可")
                                .primary()
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.session
                                        .update(cx, |s, cx| s.answer(Decision::Allow, cx));
                                })),
                        ))
                        .when(pending.shell, |d| {
                            d.child(feel::gloss(
                                Button::new("ai-allow-always")
                                    .label("このチャットでは常に許可")
                                    .small()
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.session.update(cx, |s, cx| {
                                            s.answer(Decision::AllowShellAlways, cx)
                                        });
                                    })),
                            ))
                        })
                        .child(feel::gloss(
                            Button::new("ai-deny")
                                .label("拒否")
                                .small()
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.session
                                        .update(cx, |s, cx| s.answer(Decision::Deny, cx));
                                })),
                        )),
                )
                .test_support()
        });
        let error = session.error.clone();
        let tray = self.render_tray(cx);
        let pills = self.render_pills(cx);
        let attached = self.session.read(cx).attached.clone();
        let dropping = self.main.as_ref().is_some_and(|main| {
            main.read_with(cx, |main, _| {
                main.task_drag
                    .as_ref()
                    .and_then(|d| d.over.as_ref())
                    .is_some_and(|(target, _)| *target == DropTarget::Chat)
            })
            .unwrap_or(false)
        });

        col()
            .id("ai-chat")
            .key_context("AiChat")
            .on_action(cx.listener(|this, _: &StopAi, _, cx| this.stop(cx)))
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(header)
            .when(self.settings_open, |d| d.child(self.render_settings(cx)))
            .when_some(self.side.filter(|_| !self.settings_open), |d, side| {
                d.child(match side {
                    Side::Chats => self.render_chats(cx),
                    Side::Runs => self.render_runs(cx),
                })
            })
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
                    .when(self.settings_open || self.side.is_some(), |d| d.hidden())
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.chat_scroll)
                    .child(list),
            )
            .children(pending)
            .child(
                div()
                    .flex_none()
                    .when(self.settings_open || self.side.is_some(), |d| d.hidden())
                    .px_3()
                    .pb_3()
                    .pt_1()
                    .children(tray)
                    .child(
                        // 入力欄と、サービス・モデル・effort・送信を 1 つの枠にまとめる
                        col()
                            .id("ai-composer")
                            .rounded(feel::lg())
                            .border_1()
                            .border_color(theme.input)
                            .bg(theme.background)
                            // ドラッグ中のタスクが上にある間は、色を付ける
                            .when(dropping, |d| {
                                d.border_color(theme.primary)
                                    .bg(theme.primary.opacity(0.15))
                            })
                            .elevate(Elevation::Mid, cx)
                            .p_1()
                            .gap_1()
                            .when(!attached.is_empty(), |d| {
                                d.child(row().gap_1().flex_wrap().px_1().pt_1().children(
                                    attached.iter().enumerate().map(|(ix, (id, title))| {
                                        let id = id.clone();
                                        row()
                                            .id(("ai-attached", ix))
                                            .max_w_full()
                                            .gap_1()
                                            .pl_2()
                                            .pr_0p5()
                                            .h_6()
                                            .items_center()
                                            .rounded(feel::md())
                                            .bg(theme.secondary)
                                            .text_xs()
                                            .child(app_icon("list-todo").size_3().flex_none())
                                            .child(div().min_w_0().truncate().child(title.clone()))
                                            .child(feel::gloss(
                                                Button::new(("ai-attached-remove", ix))
                                                    .icon(app_icon("x"))
                                                    .ghost()
                                                    .xsmall()
                                                    .tooltip("外す")
                                                    .on_click(cx.listener(
                                                        move |this, _, _, cx| {
                                                            this.session.update(cx, |s, cx| {
                                                                s.detach_task(&id, cx)
                                                            });
                                                        },
                                                    )),
                                            ))
                                            .test_support()
                                    }),
                                ))
                            })
                            .child(
                                // 入力欄と送信を 1 行に置く
                                row()
                                    .gap_1()
                                    .items_end()
                                    .pr_1()
                                    .pb_1()
                                    .child(
                                        div()
                                            .id("ai-input")
                                            .mb(px(-4.))
                                            .flex_1()
                                            .min_w_0()
                                            .child(Textarea::new(&self.input).appearance(false))
                                            .test_support(),
                                    )
                                    .when(busy, |d| {
                                        d.child(feel::gloss(
                                            icon_button("ai-stop", "square", "停止（Ctrl+C）")
                                                .danger()
                                                .on_click(
                                                    cx.listener(|this, _, _, cx| this.stop(cx)),
                                                ),
                                        ))
                                    })
                                    .when(!busy, |d| {
                                        d.child(feel::gloss(
                                            icon_button("ai-send", "arrow-up", "送信（Enter）")
                                                .primary()
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.submit(window, cx)
                                                })),
                                        ))
                                    }),
                            ),
                    )
                    // サービス・モデル・effort は、入力欄の下の細い行に
                    .child(row().pt_1().min_w_0().children(pills)),
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
        // 見ているのが AI のシェルか、ターミナルか
        let on_ai = term::shared(cx).read(cx).active_tab().is_none();
        // 失敗を先に出す。なければ、うまくいったことの知らせ
        let (term_error, term_failed) = {
            let terms = term::shared(cx).read(cx);
            match (&terms.error, &terms.notice) {
                (Some(error), _) => (Some(error.clone()), true),
                (None, notice) => (notice.clone(), false),
            }
        };

        let header = row()
            .flex_none()
            .h_10()
            .pl_3()
            .pr_2()
            .gap_0p5()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(self.render_term_tabs(cx))
            .when(running && on_ai, |d| {
                d.child(feel::gloss(
                    icon_button("ai-shell-kill", "square", "強制終了（Ctrl+C）")
                        .danger()
                        .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                ))
            })
            .when(on_ai, |d| {
                d.child(feel::gloss(
                    icon_button(
                        "ai-shell-copy",
                        if self.copied { "check" } else { "copy" },
                        "すべてコピー",
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.copy_shell(cx))),
                ))
                .child(feel::gloss(
                    icon_button("ai-shell-clear", "eraser", "クリア（Ctrl+L）").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.session.update(cx, |s, cx| s.clear_shell(cx));
                        }),
                    ),
                ))
            })
            .when(!detached, |d| {
                d.child(feel::gloss(
                    icon_button(
                        "ai-shell-wide",
                        if self.shell_wide {
                            "minimize-2"
                        } else {
                            "maximize-2"
                        },
                        if self.shell_wide {
                            "元の大きさに戻す"
                        } else {
                            "チャットの場所まで広げる"
                        },
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.shell_wide = !this.shell_wide;
                        cx.notify();
                    })),
                ))
                .child(feel::gloss(
                    icon_button(
                        "ai-shell-detach",
                        "picture-in-picture-2",
                        "別ウィンドウで開く",
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.detach(PaneMode::ShellWindow, cx))),
                ))
                .child(feel::gloss(
                    icon_button("ai-shell-close", "x", "閉じる").on_click(
                        cx.listener(|this, _, _, cx| this.set_shell_place(ShellPlace::Closed, cx)),
                    ),
                ))
            })
            .when(detached, |d| {
                d.child(feel::gloss(
                    icon_button("ai-shell-attach", "panel-right", "チャットの横に戻す")
                        .on_click(|_, window, _| window.remove_window()),
                ))
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
            .when_some(term_error, |d, error| {
                d.child(
                    row()
                        .id("term-error")
                        .flex_none()
                        .px_3()
                        .py_1()
                        .gap_2()
                        .text_xs()
                        .text_color(if term_failed {
                            theme.danger
                        } else {
                            theme.muted_foreground
                        })
                        .child(div().flex_1().min_w_0().child(error))
                        .child(feel::gloss(
                            Button::new("term-error-close")
                                .icon(app_icon("x"))
                                .tooltip("閉じる")
                                .ghost()
                                .xsmall()
                                .on_click(|_, _, cx| {
                                    term::shared(cx).update(cx, |terms, cx| {
                                        terms.error = None;
                                        terms.notice = None;
                                        cx.notify();
                                    })
                                }),
                        ))
                        .test_support(),
                )
            })
            .when(!on_ai, |d| d.child(self.term.clone()))
            .when(on_ai, |d| {
                d.child(
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
                                        .text_color(rgb(if running {
                                            TERM_DIM
                                        } else {
                                            TERM_PROMPT
                                        }))
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
            })
            .test_support()
            .track_focus(&self.shell_focus)
            .into_any_element()
    }

    /// シェルの上に並べるタブ。AI のシェルと、開いているターミナル。
    fn render_term_tabs(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let shared = term::shared(cx);
        let terms = shared.read(cx);
        let active = terms.active_tab().map(|tab| tab.id);
        let tab = |id: ElementId, icon: &str, label: SharedString, on: bool| {
            row()
                .id(id)
                .h_7()
                .min_w_0()
                .max_w(px(150.))
                .pl_2()
                .pr_1()
                .gap_1()
                .rounded(feel::md())
                .text_xs()
                .cursor_pointer()
                .active(|s| s.opacity(0.6))
                .when(on, |d| d.bg(theme.secondary))
                .when(!on, |d| {
                    d.text_color(theme.muted_foreground)
                        .lit(theme.secondary_hover, cx)
                })
                .child(app_icon(icon).size_3p5().flex_none())
                .child(div().min_w_0().truncate().child(label))
        };
        let mut tabs = row().flex_1().min_w_0().gap_0p5().overflow_hidden().child(
            tab(
                "term-tab-ai".into(),
                "sparkles",
                "AI".into(),
                active.is_none(),
            )
            .pr_2()
            .on_click({
                let shared = shared.clone();
                move |_, _, cx| shared.update(cx, |terms, cx| terms.activate(None, cx))
            })
            .test_support(),
        );
        for item in &terms.tabs {
            let id = item.id;
            tabs = tabs.child(
                tab(
                    ("term-tab", id as usize).into(),
                    "terminal",
                    item.label(),
                    active == Some(id),
                )
                .on_click({
                    let shared = shared.clone();
                    move |_, _, cx| shared.update(cx, |terms, cx| terms.activate(Some(id), cx))
                })
                .child(
                    div()
                        .id(("term-close", id as usize))
                        .flex_none()
                        .size_5()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_sm()
                        .lit(theme.muted, cx)
                        .active(|s| s.opacity(0.6))
                        .child(app_icon("x").size_3())
                        .on_click({
                            let shared = shared.clone();
                            move |_, _, cx| {
                                cx.stop_propagation();
                                shared.update(cx, |terms, cx| terms.close(id, cx));
                            }
                        })
                        .test_support(),
                )
                .test_support(),
            );
        }
        let profiles: Vec<(SharedString, SharedString)> = terms
            .profiles()
            .iter()
            .map(|p| (p.id.clone().into(), p.name.clone().into()))
            .collect();
        let chosen: Option<SharedString> = terms.default_profile().map(|p| p.id.clone().into());
        let new_tip: SharedString = match terms.default_profile() {
            Some(profile) => format!("新しいターミナル（{}）", profile.name).into(),
            None => "新しいターミナル".into(),
        };
        row()
            .flex_1()
            .min_w_0()
            .gap_0p5()
            .child(tabs)
            .child(feel::gloss(
                Button::new("term-new")
                    .icon(app_icon("plus"))
                    .tooltip(new_tip)
                    .ghost()
                    .small()
                    .on_click({
                        let shared = shared.clone();
                        move |_, _, cx| {
                            shared.update(cx, |terms, cx| {
                                terms.open(None, None, cx);
                            })
                        }
                    }),
            ))
            .child(
                icon_button("term-shells", "chevron-down", "シェルを選ぶ")
                    .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                        for (id, name) in &profiles {
                            let (shared, id) = (shared.clone(), id.clone());
                            menu = menu.item(
                                PopupMenuItem::new(name.clone())
                                    // 「＋」で開くシェルに印を付ける
                                    .icon(app_icon(if chosen.as_ref() == Some(&id) {
                                        "check"
                                    } else {
                                        "terminal"
                                    }))
                                    .on_click(move |_, _, cx| {
                                        shared.update(cx, |terms, cx| {
                                            terms.open(Some(&id), None, cx);
                                        })
                                    }),
                            );
                        }
                        menu
                    }),
            )
            .when(active.is_some(), |d| {
                d.child(
                    icon_button("term-mcp", "plug", "MCP に登録").dropdown_menu_with_anchor(
                        Anchor::TopRight,
                        |menu, _, _| {
                            use crate::mcp_register::Agent;
                            let typed = |name: &'static str, agent: Agent| {
                                PopupMenuItem::new(name)
                                    .icon(app_icon("terminal"))
                                    .on_click(move |_, _, cx| {
                                        term::shared(cx).update(cx, |terms, cx| {
                                            terms.type_mcp_command(agent, cx);
                                        })
                                    })
                            };
                            menu.item(typed("Claude Code", Agent::Claude))
                                .item(typed("Codex", Agent::Codex))
                                .item(
                                    PopupMenuItem::new("Cursor")
                                        .icon(app_icon("plug"))
                                        .on_click(|_, _, cx| {
                                            term::shared(cx)
                                                .update(cx, |terms, cx| terms.add_to_cursor(cx))
                                        }),
                                )
                        },
                    ),
                )
            })
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
        // シェルを広げている間は、チャットを出さない
        let wide = self.shell_wide && shell_docked;
        // タスクをドラッグしている間は、チャットが落とし先になる
        let dragging = self.main.as_ref().is_some_and(|main| {
            main.read_with(cx, |main, _| {
                main.task_drag.as_ref().is_some_and(|d| d.moved)
            })
            .unwrap_or(false)
        });
        let base = div()
            .flex()
            .bg(theme.background)
            .text_color(theme.foreground)
            .when(dragging, |d| {
                d.on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                    if let Some(main) = &this.main {
                        // メインの画面がもうなければ、落とす先もない
                        let _ = main.update(cx, |main, cx| {
                            // チャットの上では、メインの画面にマウスの動きが届かない
                            main.drag_task(event.position, cx);
                            main.drag_task_over(DropTarget::Chat, event.position, cx)
                        });
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        if let Some(main) = &this.main {
                            let _ = main.update(cx, |main, cx| main.finish_task_drag(cx));
                        }
                    }),
                )
            });
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
                    .when(!wide, |d| d.child(self.render_chat(cx)))
                    .when(shell_docked, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .when(wide, |d| d.flex_1().min_w_0().min_h_0())
                                .when(side_by_side && !wide, |d| {
                                    d.w(relative(0.45))
                                        .h_full()
                                        .border_l_1()
                                        .border_color(theme.border)
                                })
                                .when(!side_by_side && !wide, |d| {
                                    d.h(relative(0.38)).border_t_1().border_color(theme.border)
                                })
                                .child(self.render_shell(cx)),
                        )
                    })
                    .into_any_element()
            }
            PaneMode::Docked => {
                let widths = {
                    let s = self.session.read(cx);
                    (s.chat_width, s.shell_width)
                };
                let docked = docked_width(width, shell_docked, widths);
                let side_by_side = docked.is_some() && side_by_side(width, shell_docked, widths.1);
                let resize = self.resize;
                let pane = cx.entity().downgrade();
                let edge_pane = pane.clone();
                let edge = |id: &'static str, what: Resize| {
                    div()
                        .id(id)
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(-3.))
                        .w(px(6.))
                        .cursor_col_resize()
                        .when(resize == Some(what), |d| d.bg(theme.ring))
                        .hover(|s| s.bg(theme.ring))
                        .on_mouse_down(MouseButton::Left, {
                            let pane = edge_pane.clone();
                            move |event: &MouseDownEvent, _, cx| {
                                cx.stop_propagation();
                                let twice = event.click_count >= 2;
                                // チャットを閉じたあとなら、変える先がない
                                let _ = pane.update(cx, |this, cx| {
                                    if twice {
                                        // ダブルクリックで最初の幅に戻す
                                        this.session.update(cx, |s, cx| {
                                            s.set_widths(ai::CHAT_WIDTH, ai::SHELL_WIDTH, cx)
                                        });
                                    } else {
                                        this.resize = Some(what);
                                        this.session.update(cx, |s, _| s.resizing = true);
                                        cx.notify();
                                    }
                                });
                            }
                        })
                        .test_support()
                };
                base.h_full()
                    .flex_none()
                    .occlude()
                    .when_some(docked, |d, w| {
                        // タスクの画面の上に乗って見えるように、左に影を落とす
                        d.relative()
                            .w(w)
                            .border_l_1()
                            .border_color(theme.border)
                            .elevate(Elevation::High, cx)
                    })
                    .when(docked.is_none(), |d| {
                        // 上のバーとタスクの画面を残して、右に重ねる
                        d.absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            // 重ねるときは、タスクの画面が左に見える幅までにする
                            .w(px(widths.0.min(ai::CHAT_MIN_WIDTH + 60.)))
                            .max_w_full()
                            .border_l_1()
                            .border_color(theme.border)
                            .elevate(Elevation::High, cx)
                    })
                    .when(side_by_side, |d| d.flex_row())
                    .when(!side_by_side, |d| d.flex_col())
                    // 左の境界をドラッグして幅を変える
                    .child(edge("ai-resize", Resize::Chat))
                    // ドラッグしている間は、ウィンドウのどこにマウスがあっても幅を追う
                    .when_some(resize, |d, what| {
                        d.child(
                            canvas(
                                |_, _, _| (),
                                move |_, _, window, _| {
                                    let moved = pane.clone();
                                    window.on_mouse_event(
                                        move |event: &MouseMoveEvent, _, window, cx| {
                                            let window_width = window.viewport_size().width;
                                            let from_right =
                                                f32::from(window_width - event.position.x)
                                                    / crate::UI_SCALE;
                                            let held =
                                                event.pressed_button == Some(MouseButton::Left);
                                            // チャットを閉じたあとなら、変える先がない
                                            let _ = moved.update(cx, |this, cx| {
                                                if held {
                                                    this.drag_resize(
                                                        what,
                                                        from_right,
                                                        window_width,
                                                        cx,
                                                    );
                                                } else {
                                                    this.finish_resize(cx);
                                                }
                                            });
                                        },
                                    );
                                    let released = pane.clone();
                                    window.on_mouse_event(move |_: &MouseUpEvent, _, _, cx| {
                                        let _ =
                                            released.update(cx, |this, cx| this.finish_resize(cx));
                                    });
                                },
                            )
                            .absolute()
                            .size_0(),
                        )
                    })
                    .when(!wide, |d| d.child(self.render_chat(cx)))
                    .when(shell_docked, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .when(wide, |d| d.flex_1().min_w_0().min_h_0())
                                .when(side_by_side && !wide, |d| {
                                    d.w(px(widths.1))
                                        .flex_none()
                                        .h_full()
                                        .relative()
                                        .border_l_1()
                                        .border_color(theme.border)
                                        // チャットとシェルの境界
                                        .child(edge("ai-shell-resize", Resize::Shell))
                                })
                                .when(!side_by_side && !wide, |d| {
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
        assert_eq!(docked_width(px(800.), true, (380., 360.)), None);
        assert_eq!(docked_width(px(1179.), false, (380., 360.)), None);
        // 最初のウィンドウの幅（1180）でも、タスクの画面に 860 が残る
        assert_eq!(docked_width(px(1180.), true, (380., 360.)), Some(px(320.)));
        assert_eq!(docked_width(px(1300.), true, (380., 360.)), Some(px(380.)));
        // 広ければ、シェルをチャットの右に並べる
        assert_eq!(docked_width(px(1539.), true, (380., 360.)), Some(px(380.)));
        assert_eq!(docked_width(px(1540.), true, (380., 360.)), Some(px(680.)));
        assert_eq!(docked_width(px(1700.), true, (380., 360.)), Some(px(740.)));
        assert_eq!(docked_width(px(1700.), false, (380., 360.)), Some(px(380.)));
    }
}
