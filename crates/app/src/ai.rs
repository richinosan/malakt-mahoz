//! AI チャットの状態と進行。すべてのウィンドウで 1 つを共有する。描画は `ai_view` にある。
//!
//! API キーを保存して送信するまで、通信はしない。

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::*;
use malakt_ai::ansi;
use malakt_ai::github::{self, Link};
use malakt_ai::keys::{self, KeyStore, Source};
use malakt_ai::provider::{self, CatalogModel, Endpoint};
use malakt_ai::shell::{self, Spec};
use malakt_ai::wire::{self, Reply, ToolCall, Turn, Wire};
use malakt_ai::{clip_tail, tools, Effort, Provider, MAX_TOOL_OUTPUT_CHARS};
use serde_json::Value;

use crate::db::{self, Db};

/// 1 回の送信で、AI がツールを呼び続けられる回数。
const MAX_ROUNDS: usize = 40;
/// シェルの表示に残す行数。
const MAX_SHELL_LINES: usize = 4000;
/// 自分で入力したコマンドの履歴の数。
const MAX_HISTORY: usize = 200;
/// 定期チェックで 1 回に見に行くリンクの数。
const MAX_WATCH_LINKS: usize = 30;
/// 定期チェックで AI に渡す、リンク 1 つあたりの文字数。
const MAX_WATCH_CHARS: usize = 4000;
/// 定期チェックの間隔として選べる分数。
pub const WATCH_MINUTES: [u32; 4] = [15, 30, 60, 180];
/// 定期チェックで変化を見つけたときに AI に渡す、最初の指示。
pub const DEFAULT_WATCH_INSTRUCTION: &str = "GitHub の状態に合わせて、関連するタスクを更新してください。\
クローズまたはマージされていたらタスクを完了にし、新しい動きがあればタスクにコメントで要点を書いてください。";

/// GitHub の定期チェックの設定。標準はオフ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    pub enabled: bool,
    pub minutes: u32,
    /// 変化を見つけたときに AI に渡す指示。
    pub instruction: String,
}

impl Default for Watch {
    fn default() -> Self {
        Self {
            enabled: false,
            minutes: 30,
            instruction: DEFAULT_WATCH_INSTRUCTION.to_owned(),
        }
    }
}

/// 定期チェックで見に行くリンクと、それが書かれているタスク（ID とタイトル）。
type WatchTarget = (Link, Vec<(String, String)>);

/// 中身が変わったかを見分けるための値（FNV-1a）。Rust の版が変わっても同じ値になる。
fn fingerprint(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// 未完了のタスクに書かれている issue とプルリクエストのリンクを集める。
fn watch_targets(store: &malakt_core::Store) -> Result<Vec<WatchTarget>, String> {
    let filter = malakt_core::TaskFilter {
        open_only: true,
        ..Default::default()
    };
    let tasks = store
        .tasks(malakt_core::Scope::All, &filter)
        .map_err(|e| format!("タスクを読めませんでした: {e}"))?;
    let mut targets: Vec<WatchTarget> = Vec::new();
    for task in tasks {
        for link in github::find_links(&format!("{}\n{}", task.title, task.description)) {
            if !matches!(link.kind, github::Kind::Issue(_) | github::Kind::Pull(_)) {
                continue;
            }
            let entry = (task.id.clone(), task.title.clone());
            match targets.iter().position(|(l, _)| *l == link) {
                Some(ix) => {
                    if !targets[ix].1.contains(&entry) {
                        targets[ix].1.push(entry);
                    }
                }
                None if targets.len() < MAX_WATCH_LINKS => targets.push((link, vec![entry])),
                None => {}
            }
        }
    }
    Ok(targets)
}

/// 表示の状態（開いているか、別ウィンドウか、シェルの場所）を保存する設定の名前。
const UI_SETTING: &str = "ai.ui";
/// 会話を保存する設定の名前。
const CHAT_SETTING: &str = "ai.chat";
/// 保存する会話の大きさ（AI に送る分）と、画面に出す行の数。
const MAX_SAVED_CHAT_BYTES: usize = 200_000;
const MAX_SAVED_ITEMS: usize = 200;

#[derive(Default)]
struct SavedUi {
    open: bool,
    chat_window: bool,
    shell: String,
    chat_width: Option<f32>,
    shell_width: Option<f32>,
    bounds: [Option<[f32; 4]>; 2],
}

/// 右に置くチャットとシェルの、最初の幅と選べる範囲（拡大率を掛ける前の値）。
pub const CHAT_WIDTH: f32 = 380.;
pub const SHELL_WIDTH: f32 = 360.;
pub const CHAT_MIN_WIDTH: f32 = 320.;
pub const SHELL_MIN_WIDTH: f32 = 240.;
pub const PANE_MAX_WIDTH: f32 = 900.;

fn saved_bounds(v: &Value) -> Option<[f32; 4]> {
    let list = v.as_array()?;
    let mut out = [0.; 4];
    for (slot, value) in out.iter_mut().zip(list) {
        *slot = value.as_f64()? as f32;
    }
    (list.len() == 4 && out[2] >= 200. && out[3] >= 200.).then_some(out)
}

/// 新しいタスクを AI が整えるとき、1 回で扱うタスクの数。
/// いつも AI に伝える前提と指示の保存先と、長さの上限。
const CONTEXT_SETTING: &str = "ai.context";
pub const MAX_CONTEXT_CHARS: usize = 4000;
/// ワークスペースごとの分の長さの上限。
pub const MAX_WORKSPACE_CONTEXT_CHARS: usize = 2000;

/// 整えられなかった理由を、タスクの画面に出す短い言葉にする。くわしい理由はホバーで出す。
pub fn arrange_note_label(note: &str) -> &'static str {
    if note.contains("API キー") {
        "API キーがありません"
    } else if note.contains("を読み込めませんでした") {
        "リンクを読み込めませんでした"
    } else {
        "AI に接続できませんでした"
    }
}

/// ワークスペースごとの「AI に伝えること」の保存先。
pub fn workspace_context_key(workspace: &str) -> String {
    format!("ai.context.ws.{workspace}")
}

/// 長すぎる「AI に伝えること」を断る文。
pub fn workspace_context_error(text: &str) -> Option<String> {
    let count = text.trim().chars().count();
    (count > MAX_WORKSPACE_CONTEXT_CHARS).then(|| {
        format!("AI に伝えることは {MAX_WORKSPACE_CONTEXT_CHARS} 文字までです（今は {count} 文字）")
    })
}

const MAX_ARRANGE_TASKS: usize = 5;
/// 新しいタスクを整えるとき、タスク 1 つで読みに行くリンクの数。
const MAX_ARRANGE_LINKS: usize = 3;

/// AI が整える前のタスク。
struct NewTask {
    id: String,
    title: String,
    description: String,
    links: Vec<Link>,
}

/// タスクごとの、書かれている GitHub のリンク。新しいタスクとリンクを見分ける元にする。
fn task_links(store: &malakt_core::Store) -> Result<Vec<(NewTask, bool)>, String> {
    let tasks = store
        .tasks(malakt_core::Scope::All, &malakt_core::TaskFilter::default())
        .map_err(|e| format!("タスクを読めませんでした: {e}"))?;
    Ok(tasks
        .into_iter()
        .map(|task| {
            let links = github::find_links(&format!("{}\n{}", task.title, task.description));
            let open = task.status.is_open();
            (
                NewTask {
                    id: task.id,
                    title: task.title,
                    description: task.description,
                    links,
                },
                open,
            )
        })
        .collect())
}

type Headers = [(&'static str, String)];
/// 環境変数を読む。
pub type EnvFn = dyn Fn(&str) -> Option<String> + Send + Sync;
/// JSON を POST して、JSON を受け取る。
pub type PostFn = dyn Fn(&str, &Headers, &Value) -> Result<Value, String> + Send + Sync;
/// GET して、本文を受け取る。
pub type GetFn = dyn Fn(&str, &Headers) -> Result<String, String> + Send + Sync;

/// 外とのやり取り。テストでは差し替える。
#[derive(Clone)]
pub struct AiConfig {
    pub keys: Arc<dyn KeyStore>,
    pub env: Arc<EnvFn>,
    pub post: Arc<PostFn>,
    pub get: Arc<GetFn>,
    /// GitHub のリンクを取りに行くコマンド。
    pub gh: Arc<dyn Fn(&Link) -> Spec + Send + Sync>,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            keys: Arc::new(keys::Keychain),
            env: Arc::new(|name| std::env::var(name).ok()),
            post: Arc::new(malakt_ai::http::post_json),
            get: Arc::new(malakt_ai::http::get_text),
            gh: Arc::new(Link::gh),
        }
    }
}

/// 選んでいるサービスの API キーの状態。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyState {
    /// まだ調べていない
    Unknown,
    Missing,
    Present {
        masked: SharedString,
        source: Source,
    },
    Failed(SharedString),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolState {
    Running,
    Done,
    Failed(SharedString),
    /// ユーザーが許可しなかった
    Denied,
}

/// チャットに出す 1 つ分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    User(SharedString),
    Assistant(SharedString),
    Tool {
        summary: SharedString,
        state: ToolState,
    },
    Notice(SharedString),
    Error(SharedString),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Command,
    Out,
    Err,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellLine {
    pub kind: LineKind,
    /// エスケープを除いた文字。
    pub text: SharedString,
    /// 色の付いた範囲。
    pub spans: Vec<ansi::Span>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    /// このチャットの間、シェルのコマンドを確認なしで実行する
    AllowShellAlways,
    Deny,
}

/// 実行の前の確認。
pub struct Pending {
    pub text: SharedString,
    pub shell: bool,
    answer: async_channel::Sender<Decision>,
}

/// シェルの表示の場所。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellPlace {
    Closed,
    /// チャットの右
    Docked,
    Window(AnyWindowHandle),
}

struct Output {
    stdout: String,
    /// 標準出力と標準エラーを、出た順に
    all: String,
    code: Option<i32>,
}

pub struct Session {
    db: Entity<Db>,
    config: AiConfig,
    pub provider: Provider,
    models: HashMap<Provider, String>,
    pub effort: Effort,
    pub key: KeyState,
    /// サービスから取ってきたモデルの一覧。
    pub listed: Vec<SharedString>,
    catalog: Option<(Provider, Arc<Vec<CatalogModel>>)>,
    turns: Vec<Turn>,
    pub items: Vec<Item>,
    pub busy: bool,
    pub pending: Option<Pending>,
    pub shell: Vec<ShellLine>,
    /// シェルの中身が変わるたびに増える。表示の折り返しを作り直す目印。
    pub shell_rev: u64,
    ansi: ansi::State,
    /// シェルの今の場所。`cd` で変わる。
    pub cwd: std::path::PathBuf,
    /// 自分で入力したコマンドの履歴。新しいものが後ろ。
    pub history: Vec<SharedString>,
    running: Option<shell::Handle>,
    /// 今動いているのは、シェルに自分で打ったコマンド。
    pub own_command: bool,
    /// シェルのコマンドを、確認なしで実行する（このチャットの間）。
    pub allow_shell: bool,
    /// オート実行。オンの間、コマンドの実行とタスクの削除を確認なしで行う。標準はオフ。
    pub auto_run: bool,
    /// 別ウィンドウに出したチャット。
    pub chat_window: Option<AnyWindowHandle>,
    pub shell_place: ShellPlace,
    /// 会話ごとの ID（OpenCode に送る）。
    conversation: String,
    task: Option<Task<()>>,
    /// 設定の変更などの失敗。チャットの上に出す。
    pub error: Option<SharedString>,
    pub watch: Watch,
    /// 定期チェックの、いちばん新しい結果。
    pub watch_status: Option<SharedString>,
    watch_timer: Option<Task<()>>,
    /// 今の進行が、定期チェックなどアプリから始まったものか。確認が要る操作は実行しない。
    auto: bool,
    /// 新しいタスクを AI が整える。標準はオン（API キーがなければ動かない）。
    pub arrange: bool,
    /// ユーザーが書いた、いつも AI に伝える前提と指示。
    pub context: String,
    /// AI が今整えているタスクの ID。
    pub arranging: Vec<String>,
    /// タスクを整えられなかった理由。タスクの ID ごと。
    pub arrange_notes: HashMap<String, SharedString>,
    /// もう見たタスクと、そのリンク。ここにないタスクやリンクが新しいもの。
    known: Option<HashMap<String, Vec<String>>>,
    /// 手が空いたら、新しいタスクを探し直す。
    arrange_retry: bool,
    /// チャットを出している（メインのウィンドウの右か、別ウィンドウ）。次の起動で戻す。
    pub open: bool,
    /// 起動したときに開き直す別ウィンドウ（チャット、シェル）。最初のウィンドウが受け取る。
    pub restore: Option<(bool, bool)>,
    /// 保存してある表示の状態と会話。同じなら書き込まない。
    saved_ui: String,
    saved_chat: (usize, usize, String),
    /// 右に置くチャットとシェルの幅（拡大率を掛ける前の値）。境界をドラッグして変える。
    pub chat_width: f32,
    pub shell_width: f32,
    /// 境界をドラッグしている間。終わるまで保存しない。
    pub resizing: bool,
    /// 別ウィンドウ（チャット、シェル）の位置と大きさ。x, y, 幅, 高さ。
    pub window_bounds: [Option<[f32; 4]>; 2],
    /// ユーザーが今見ている場所（ワークスペースや開いているタスク）。画面が書き込む。
    pub place: String,
    /// ユーザーが今開いているワークスペース。画面が書き込む。
    pub workspace: Option<String>,
    _subscriptions: Vec<Subscription>,
}

/// すべてのウィンドウで共有するチャット。
pub struct SharedSession(pub Entity<Session>);

impl Global for SharedSession {}

pub fn init(config: AiConfig, cx: &mut App) {
    let db = db::shared(cx);
    let session = cx.new(|cx| Session::new(db, config, cx));
    cx.set_global(SharedSession(session));
}

pub fn shared(cx: &App) -> Entity<Session> {
    cx.global::<SharedSession>().0.clone()
}

fn setting_key(provider: Provider) -> String {
    format!("ai.model.{}", provider.as_str())
}

impl Session {
    fn new(db: Entity<Db>, config: AiConfig, cx: &mut Context<Self>) -> Self {
        let mut error = None;
        let watch;
        let auto_run;
        let ui: SavedUi;
        let chat: Option<Value>;
        let arrange;
        let context;
        let (provider, effort, models) = {
            let store = &db.read(cx).store;
            let mut read = |key: &str| match store.setting(key) {
                Ok(value) => value,
                Err(e) => {
                    error = Some(format!("AI の設定を読めませんでした: {e}").into());
                    None
                }
            };
            let provider = read("ai.provider")
                .and_then(|v| Provider::parse(&v))
                .unwrap_or(Provider::Claude);
            let effort = read("ai.effort")
                .and_then(|v| Effort::parse(&v))
                .unwrap_or(Effort::Default);
            let models = Provider::ALL
                .into_iter()
                .filter_map(|p| read(&setting_key(p)).map(|m| (p, m)))
                .collect();
            ui = read(UI_SETTING)
                .and_then(|v| serde_json::from_str::<Value>(&v).ok())
                .map(|v| SavedUi {
                    open: v["open"].as_bool().unwrap_or(false),
                    chat_window: v["chat"] == "window",
                    shell: v["shell"].as_str().unwrap_or("docked").to_owned(),
                    chat_width: v["cw"].as_f64().map(|w| w as f32),
                    shell_width: v["sw"].as_f64().map(|w| w as f32),
                    bounds: [
                        saved_bounds(&v["chat_bounds"]),
                        saved_bounds(&v["shell_bounds"]),
                    ],
                })
                .unwrap_or_default();
            chat = read(CHAT_SETTING).and_then(|v| serde_json::from_str::<Value>(&v).ok());
            auto_run = read("ai.auto_run").as_deref() == Some("1");
            arrange = read("ai.arrange").as_deref() != Some("0");
            context = read(CONTEXT_SETTING).unwrap_or_default();
            let defaults = Watch::default();
            watch = Watch {
                enabled: read("ai.watch.enabled").as_deref() == Some("1"),
                minutes: read("ai.watch.minutes")
                    .and_then(|v| v.parse().ok())
                    .filter(|m| WATCH_MINUTES.contains(m))
                    .unwrap_or(defaults.minutes),
                instruction: read("ai.watch.instruction")
                    .filter(|v| !v.trim().is_empty())
                    .unwrap_or(defaults.instruction),
            };
            (provider, effort, models)
        };
        let mut this = Self {
            db,
            config,
            provider,
            models,
            effort,
            key: KeyState::Unknown,
            listed: Vec::new(),
            catalog: None,
            turns: Vec::new(),
            items: Vec::new(),
            busy: false,
            pending: None,
            shell: Vec::new(),
            shell_rev: 0,
            ansi: ansi::State::default(),
            cwd: dirs::home_dir()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| ".".into()),
            history: Vec::new(),
            running: None,
            own_command: false,
            allow_shell: false,
            auto_run,
            chat_window: None,
            shell_place: if ui.shell == "closed" {
                ShellPlace::Closed
            } else {
                ShellPlace::Docked
            },
            conversation: malakt_core::new_id(),
            task: None,
            error,
            watch,
            watch_status: None,
            watch_timer: None,
            auto: false,
            arrange,
            context,
            arranging: Vec::new(),
            arrange_notes: HashMap::new(),
            known: None,
            arrange_retry: false,
            open: ui.open,
            restore: (ui.open || ui.shell == "window")
                .then_some((ui.open && ui.chat_window, ui.shell == "window")),
            saved_ui: String::new(),
            saved_chat: (0, 0, String::new()),
            chat_width: ui
                .chat_width
                .unwrap_or(CHAT_WIDTH)
                .clamp(CHAT_MIN_WIDTH, PANE_MAX_WIDTH),
            shell_width: ui
                .shell_width
                .unwrap_or(SHELL_WIDTH)
                .clamp(SHELL_MIN_WIDTH, PANE_MAX_WIDTH),
            resizing: false,
            window_bounds: ui.bounds,
            place: String::new(),
            workspace: None,
            _subscriptions: Vec::new(),
        };
        this._subscriptions = vec![
            cx.observe(&this.db.clone(), |this, _, cx| this.tasks_changed(cx)),
            cx.observe_self(|this, cx| this.persist(cx)),
            cx.observe_self(|this, cx| {
                if this.arrange_retry && !this.busy {
                    this.arrange_retry = false;
                    this.tasks_changed(cx);
                }
            }),
        ];
        if let Some(chat) = chat {
            this.load_chat(&chat);
        }
        this.saved_ui = this.ui_json();
        this.saved_chat = this.chat_mark();
        this.restart_watch(cx);
        // 今あるタスクは、もう見たものにする
        this.tasks_changed(cx);
        this
    }

    // ---- 次の起動で戻すもの ----

    pub fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.open != open {
            self.open = open;
            cx.notify();
        }
    }

    /// ドラッグ中の幅。範囲に収める。
    pub fn set_widths(&mut self, chat: f32, shell: f32, cx: &mut Context<Self>) {
        let chat = chat.clamp(CHAT_MIN_WIDTH, PANE_MAX_WIDTH).round();
        let shell = shell.clamp(SHELL_MIN_WIDTH, PANE_MAX_WIDTH).round();
        if (chat, shell) != (self.chat_width, self.shell_width) {
            self.chat_width = chat;
            self.shell_width = shell;
            cx.notify();
        }
    }

    fn ui_json(&self) -> String {
        serde_json::json!({
            "open": self.open || self.chat_window.is_some(),
            "chat": if self.chat_window.is_some() { "window" } else { "docked" },
            "shell": match self.shell_place {
                ShellPlace::Closed => "closed",
                ShellPlace::Docked => "docked",
                ShellPlace::Window(_) => "window",
            },
            "cw": self.chat_width,
            "sw": self.shell_width,
            "chat_bounds": self.window_bounds[0],
            "shell_bounds": self.window_bounds[1],
        })
        .to_string()
    }

    fn chat_mark(&self) -> (usize, usize, String) {
        (
            self.items.len(),
            self.turns.len(),
            self.conversation.clone(),
        )
    }

    /// 表示の状態と会話を保存する。変わっていなければ書き込まない。
    fn persist(&mut self, cx: &mut Context<Self>) {
        if !self.db.read(cx).unlocked {
            return;
        }
        // 起動してすぐ、別ウィンドウを開き直す前の状態では上書きしない
        if self.restore.is_none() && !self.resizing {
            let ui = self.ui_json();
            if ui != self.saved_ui && self.save_setting(UI_SETTING, &ui, cx) {
                self.saved_ui = ui;
            }
        }
        let mark = self.chat_mark();
        if !self.busy && mark != self.saved_chat {
            let chat = self.chat_json();
            if self.save_setting(CHAT_SETTING, &chat, cx) {
                self.saved_chat = mark;
            }
        }
    }

    /// 会話の終わりのほうを、決めた大きさまで。古いものから落とすので、増え続けない。
    fn chat_json(&self) -> String {
        let mut turns: Vec<Value> = Vec::new();
        let mut size = 0;
        // AI に送る会話は、ユーザーの発言から始まる形で切る
        let mut start = self.turns.len();
        for (ix, turn) in self.turns.iter().enumerate().rev() {
            let value = match turn {
                Turn::User(text) => serde_json::json!({"k": "user", "t": text}),
                Turn::Assistant { text, calls, .. } => serde_json::json!({
                    "k": "assistant",
                    "t": text,
                    "calls": calls.iter().map(|c| serde_json::json!({
                        "id": c.id, "name": c.name, "args": c.args,
                    })).collect::<Vec<_>>(),
                }),
                Turn::Tool {
                    id,
                    name,
                    output,
                    is_error,
                } => serde_json::json!({
                    "k": "tool", "id": id, "name": name, "t": output, "e": is_error,
                }),
            };
            size += value.to_string().len();
            if size > MAX_SAVED_CHAT_BYTES {
                break;
            }
            turns.push(value);
            if matches!(turn, Turn::User(_)) {
                start = ix;
            }
        }
        turns.reverse();
        let turns = turns.split_off(turns.len() - (self.turns.len() - start));
        let skip = self.items.len().saturating_sub(MAX_SAVED_ITEMS);
        let items: Vec<Value> = self.items[skip..]
            .iter()
            .map(|item| match item {
                Item::User(t) => serde_json::json!({"k": "user", "t": t}),
                Item::Assistant(t) => serde_json::json!({"k": "assistant", "t": t}),
                Item::Notice(t) => serde_json::json!({"k": "notice", "t": t}),
                Item::Error(t) => serde_json::json!({"k": "error", "t": t}),
                Item::Tool { summary, state } => {
                    let (state, detail) = match state {
                        ToolState::Done => ("done", None),
                        ToolState::Denied => ("denied", None),
                        ToolState::Failed(e) => ("failed", Some(e.to_string())),
                        ToolState::Running => ("failed", Some("中断しました".to_owned())),
                    };
                    serde_json::json!({"k": "tool", "t": summary, "s": state, "d": detail})
                }
            })
            .collect();
        serde_json::json!({"id": self.conversation, "items": items, "turns": turns}).to_string()
    }

    fn load_chat(&mut self, chat: &Value) {
        let text = |v: &Value| v["t"].as_str().unwrap_or_default().to_owned();
        for v in chat["items"].as_array().into_iter().flatten() {
            let t: SharedString = text(v).into();
            self.items.push(match v["k"].as_str() {
                Some("user") => Item::User(t),
                Some("assistant") => Item::Assistant(t),
                Some("notice") => Item::Notice(t),
                Some("tool") => Item::Tool {
                    summary: t,
                    state: match v["s"].as_str() {
                        Some("done") => ToolState::Done,
                        Some("denied") => ToolState::Denied,
                        _ => {
                            ToolState::Failed(v["d"].as_str().unwrap_or_default().to_owned().into())
                        }
                    },
                },
                _ => Item::Error(t),
            });
        }
        for v in chat["turns"].as_array().into_iter().flatten() {
            self.turns.push(match v["k"].as_str() {
                Some("user") => Turn::User(text(v)),
                Some("assistant") => Turn::Assistant {
                    text: text(v),
                    calls: v["calls"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|c| ToolCall {
                            id: c["id"].as_str().unwrap_or_default().to_owned(),
                            name: c["name"].as_str().unwrap_or_default().to_owned(),
                            args: c["args"].clone(),
                        })
                        .collect(),
                    raw: None,
                },
                _ => Turn::Tool {
                    id: v["id"].as_str().unwrap_or_default().to_owned(),
                    name: v["name"].as_str().unwrap_or_default().to_owned(),
                    output: text(v),
                    is_error: v["e"].as_bool().unwrap_or(false),
                },
            });
        }
        if let Some(id) = chat["id"].as_str().filter(|id| !id.is_empty()) {
            self.conversation = id.to_owned();
        }
    }

    /// 今開いているワークスペースについての、AI に伝える前提と指示。
    fn workspace_context(&self, cx: &App) -> String {
        let store = &self.db.read(cx).store;
        let Some(id) = &self.workspace else {
            return String::new();
        };
        // 読めなければ添えない（次の操作でデータベースのエラーとして出る）
        let own = store
            .setting(&workspace_context_key(id))
            .ok()
            .flatten()
            .unwrap_or_default();
        if own.trim().is_empty() {
            return String::new();
        }
        let name = store.workspace(id).map(|w| w.name).unwrap_or_default();
        format!(
            "ワークスペース「{name}」についての前提と指示:\n{}",
            own.trim()
        )
    }

    /// いつも AI に伝える前提と指示を保存する。
    pub fn set_context(&mut self, text: &str, cx: &mut Context<Self>) {
        let text = text.trim();
        let count = text.chars().count();
        if count > MAX_CONTEXT_CHARS {
            self.error = Some(
                format!("いつも伝えることは {MAX_CONTEXT_CHARS} 文字までです（今は {count} 文字）")
                    .into(),
            );
            cx.notify();
            return;
        }
        if text == self.context || !self.save_setting(CONTEXT_SETTING, text, cx) {
            return;
        }
        self.context = text.to_owned();
        cx.notify();
    }

    // ---- 新しいタスクを整える ----

    pub fn set_arrange(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.arrange == on || !self.save_setting("ai.arrange", if on { "1" } else { "0" }, cx) {
            return;
        }
        self.arrange = on;
        // オンにしたときは、今あるタスクをもう見たものにして、次に作られたものから始める
        self.known = None;
        self.arrange_notes.clear();
        self.tasks_changed(cx);
        cx.notify();
    }

    /// 整えられなかったタスクを、もう一度整える。
    pub fn retry_arrange(&mut self, id: &str, cx: &mut Context<Self>) {
        self.arrange_notes.remove(id);
        // まだ見ていないタスクに戻すと、次の確認で整える
        if let Some(known) = &mut self.known {
            known.remove(id);
        }
        self.tasks_changed(cx);
        cx.notify();
    }

    /// まだ見ていないタスクと、新しくリンクが書かれたタスク。
    fn new_tasks(&self, all: Vec<(NewTask, bool)>) -> Vec<NewTask> {
        let Some(known) = &self.known else {
            return Vec::new();
        };
        all.into_iter()
            .filter(|(task, open)| {
                *open
                    && known.get(&task.id).is_none_or(|seen| {
                        task.links.iter().any(|link| !seen.contains(&link.url()))
                    })
            })
            .map(|(task, _)| task)
            .collect()
    }

    /// AI がツールで書き換えた分を、もう見たものにする。ユーザーが作ってまだ
    /// 整えていないタスクは、そのまま残す。
    fn absorb(&mut self, waiting: &[String], cx: &App) {
        let Some(known) = &mut self.known else {
            return;
        };
        let Ok(all) = task_links(&self.db.read(cx).store) else {
            return;
        };
        for (task, _) in all {
            if !waiting.contains(&task.id) {
                known.insert(task.id, task.links.iter().map(Link::url).collect());
            }
        }
    }

    /// 新しいタスクを見つけたら、AI に整えさせる。API キーがなければ通信しない。
    fn tasks_changed(&mut self, cx: &mut Context<Self>) {
        if !self.arrange {
            self.known = None;
            return;
        }
        if !self.db.read(cx).unlocked {
            return;
        }
        let all = match task_links(&self.db.read(cx).store) {
            Ok(all) => all,
            Err(_) => return,
        };
        let Some(known) = &mut self.known else {
            self.known = Some(
                all.into_iter()
                    .map(|(task, _)| (task.id, task.links.iter().map(Link::url).collect()))
                    .collect(),
            );
            return;
        };
        // 消えたタスクの分は忘れるので、増え続けない
        known.retain(|id, _| all.iter().any(|(task, _)| task.id == *id));
        let mut tasks = self.new_tasks(all);
        tasks.truncate(MAX_ARRANGE_TASKS);
        let before = self.arrange_notes.len();
        self.arrange_notes
            .retain(|id, _| !tasks.iter().any(|task| task.id == *id) || self.busy);
        if tasks.is_empty() {
            if before != self.arrange_notes.len() {
                cx.notify();
            }
            return;
        }
        if self.busy {
            self.arrange_retry = true;
            return;
        }
        match &self.key {
            KeyState::Unknown => {
                // 調べ終わったら、もう一度ここに来る
                self.refresh_key(cx);
            }
            KeyState::Missing | KeyState::Failed(_) => {
                let note: SharedString = match &self.key {
                    KeyState::Failed(e) => format!("API キーを読めませんでした: {e}").into(),
                    _ => format!(
                        "{} の API キーがないので、AI はリンクを読み込めません",
                        self.provider.label()
                    )
                    .into(),
                };
                let mut changed = before != self.arrange_notes.len();
                // リンクのないタスクには、何も出さない
                for task in tasks.iter().filter(|task| !task.links.is_empty()) {
                    changed |= self.arrange_notes.insert(task.id.clone(), note.clone())
                        != Some(note.clone());
                }
                if changed {
                    cx.notify();
                }
            }
            KeyState::Present { .. } => self.arrange_tasks(tasks, cx),
        }
    }

    fn arrange_tasks(&mut self, tasks: Vec<NewTask>, cx: &mut Context<Self>) {
        self.busy = true;
        self.auto = true;
        self.arranging = tasks.iter().map(|task| task.id.clone()).collect();
        if let Some(known) = &mut self.known {
            for task in &tasks {
                known.insert(task.id.clone(), task.links.iter().map(Link::url).collect());
            }
        }
        let ids = self.arranging.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = Self::arrange_run(&this, tasks, cx).await;
            // チャットがなくなっていれば、知らせる先もない
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(notes) => this.arrange_notes.extend(notes),
                    Err(error) => {
                        let note: SharedString =
                            format!("AI がタスクを整えられませんでした: {error}").into();
                        this.items.push(Item::Error(note.clone()));
                        for id in ids {
                            this.arrange_notes.insert(id, note.clone());
                        }
                    }
                }
                this.finish();
                this.arrange_retry = true;
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// リンクの中身を読み、AI にタスクを整えさせる。返すのは、読めなかったリンクの知らせ。
    async fn arrange_run(
        this: &WeakEntity<Self>,
        tasks: Vec<NewTask>,
        cx: &mut AsyncApp,
    ) -> Result<Vec<(String, SharedString)>, String> {
        let (config, place) = this
            .read_with(cx, |s, _| (s.config.clone(), s.place.clone()))
            .map_err(gone)?;
        let titles: Vec<String> = tasks.iter().map(|t| format!("「{}」", t.title)).collect();
        this.update(cx, |s, cx| {
            s.items.push(Item::Notice(
                format!("新しいタスク {} を AI が整えます。", titles.join("、")).into(),
            ));
            cx.notify();
        })
        .map_err(gone)?;
        let mut notes = Vec::new();
        let mut message = String::from(
            "（アプリからの自動のメッセージです）\n\
             次のタスクが新しく作られたか、新しいリンクが書かれました。それぞれのタスクを update_task で整えてください。\n\
             - タイトルは、中身が一目で分かる短い言葉にする。URL が入っているタイトルは、リンク先の題を元に付け直す（「〜のレビュー」のようにユーザーが書いた目的は残す）。\n\
             - 説明には、リンクの URL と要点を書く。ユーザーが書いた文は消さない。\n\
             - 中身から分かる範囲で、優先度や期限を入れる。\n\
             - 直すところがなければ、何も変えない。\n\
             - タスクを新しく作らない。消さない。\n\
             - タイトルと説明には、タスクの中身だけを書く。読めなかったリンクや、確かめられなかったことについての注記を書かない。\n",
        );
        let mut any = false;
        for task in &tasks {
            let mut section = format!(
                "\n## タスク（id: {}）\nタイトル: {}\n説明: {}\n",
                task.id,
                task.title,
                if task.description.trim().is_empty() {
                    "（なし）"
                } else {
                    task.description.as_str()
                }
            );
            let mut read = 0;
            for link in task.links.iter().take(MAX_ARRANGE_LINKS) {
                let url = link.url();
                match Self::fetch_github(this, &config, link, cx).await {
                    Ok(content) => {
                        read += 1;
                        section.push_str(&format!(
                            "### {url} の中身\n{}\n",
                            clip_tail(&content, MAX_WATCH_CHARS)
                        ));
                    }
                    Err(error) => {
                        let note: SharedString =
                            format!("{url} を読み込めませんでした: {error}").into();
                        this.update(cx, |s, cx| {
                            s.items.push(Item::Error(note.clone()));
                            cx.notify();
                        })
                        .map_err(gone)?;
                        notes.push((task.id.clone(), note));
                    }
                }
            }
            // リンクを 1 つも読めなかったタスクは、AI に渡さず元のまま残す
            if read > 0 || task.links.is_empty() {
                message.push_str(&section);
                any = true;
            }
        }
        if !any {
            return Ok(notes);
        }
        this.update(cx, |s, cx| {
            s.turns.push(Turn::User(message));
            cx.notify();
        })
        .map_err(gone)?;
        Self::converse(
            this,
            &config,
            &format!("新しいタスクの整理（アプリが自動で始めました）。{place}"),
            cx,
        )
        .await?;
        Ok(notes)
    }

    /// オート実行を切り替える。待っている確認があれば、オンにしたときに実行へ進める。
    pub fn set_auto_run(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.auto_run == on || !self.save_setting("ai.auto_run", if on { "1" } else { "0" }, cx)
        {
            return;
        }
        self.auto_run = on;
        if on && self.pending.is_some() {
            self.answer(Decision::Allow, cx);
        }
        cx.notify();
    }

    // ---- GitHub の定期チェック ----

    /// 定期チェックの設定を保存し、間隔を数え直す。
    pub fn set_watch(&mut self, watch: Watch, cx: &mut Context<Self>) {
        if !WATCH_MINUTES.contains(&watch.minutes) || watch.instruction.trim().is_empty() {
            self.error = Some("定期チェックの間隔と指示を入れてください".into());
            cx.notify();
            return;
        }
        if watch == self.watch {
            return;
        }
        let saved = self.save_setting(
            "ai.watch.enabled",
            if watch.enabled { "1" } else { "0" },
            cx,
        ) && self.save_setting("ai.watch.minutes", &watch.minutes.to_string(), cx)
            && self.save_setting("ai.watch.instruction", watch.instruction.trim(), cx);
        if !saved {
            return;
        }
        self.watch = Watch {
            instruction: watch.instruction.trim().to_owned(),
            ..watch
        };
        self.restart_watch(cx);
        cx.notify();
    }

    fn restart_watch(&mut self, cx: &mut Context<Self>) {
        self.watch_timer = None;
        if !self.watch.enabled {
            return;
        }
        let every = std::time::Duration::from_secs(u64::from(self.watch.minutes) * 60);
        self.watch_timer = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(every).await;
            if this.update(cx, |s, cx| s.check_github(cx)).is_err() {
                break;
            }
        }));
    }

    /// タスクに書かれている GitHub のリンクを見に行き、変化があれば AI に渡す。
    /// チャットが動いている間とロック中は、何もしない（次の回に見る）。
    pub fn check_github(&mut self, cx: &mut Context<Self>) -> bool {
        if self.busy || !self.db.read(cx).unlocked {
            return false;
        }
        self.busy = true;
        self.auto = true;
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = Self::watch_run(&this, cx).await;
            // チャットがなくなっていれば、知らせる先もない
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(status) => this.watch_status = Some(status.into()),
                    Err(error) => {
                        this.watch_status = Some(
                            format!("{} のチェックに失敗しました: {error}", now_label()).into(),
                        );
                        this.items.push(Item::Error(
                            format!("GitHub の定期チェック: {error}").into(),
                        ));
                    }
                }
                this.finish();
                cx.notify();
            });
        }));
        cx.notify();
        true
    }

    /// 進行の後始末。
    fn finish(&mut self) {
        self.settle();
        self.busy = false;
        self.own_command = false;
        self.auto = false;
        self.pending = None;
        self.arranging.clear();
    }

    async fn watch_run(this: &WeakEntity<Self>, cx: &mut AsyncApp) -> Result<String, String> {
        let (config, provider, instruction) = this
            .read_with(cx, |s, _| {
                (s.config.clone(), s.provider, s.watch.instruction.clone())
            })
            .map_err(gone)?;
        // キーがなければ、GitHub にも AI にも通信しない
        let has_key = {
            let config = config.clone();
            cx.background_executor()
                .spawn(async move { keys::lookup(&*config.keys, provider, |n| (config.env)(n)) })
                .await?
                .is_some()
        };
        if !has_key {
            return Ok(format!(
                "{} の API キーが設定されていないので、{} のチェックをスキップしました。",
                provider.label(),
                now_label()
            ));
        }
        let (targets, mut seen) = this
            .read_with(cx, |s, cx| {
                let store = &s.db.read(cx).store;
                let seen: HashMap<String, String> = store
                    .setting("ai.watch.seen")
                    .map_err(|e| format!("前回の結果を読めませんでした: {e}"))?
                    .and_then(|v| serde_json::from_str(&v).ok())
                    .unwrap_or_default();
                Ok::<_, String>((watch_targets(store)?, seen))
            })
            .map_err(gone)??;

        let mut changed = Vec::new();
        let mut failed = Vec::new();
        let mut now: HashMap<String, String> = HashMap::new();
        for (link, tasks) in &targets {
            let url = link.url();
            match Self::fetch_github(this, &config, link, cx).await {
                Ok(content) => {
                    let print = fingerprint(&content);
                    // 初めて見るリンクは、次からの比べる元にするだけ
                    if seen.get(&url).is_some_and(|old| *old != print) {
                        changed.push((url.clone(), tasks.clone(), content));
                    }
                    now.insert(url, print);
                }
                Err(error) => {
                    // 取れなかったものは、前の値のまま次に比べる
                    if let Some(old) = seen.remove(&url) {
                        now.insert(url.clone(), old);
                    }
                    failed.push(format!("{url}: {error}"));
                }
            }
        }
        // 今あるリンクの分だけを残すので、増え続けない
        let json = serde_json::to_string(&now).map_err(|e| e.to_string())?;
        this.update(cx, |s, cx| {
            s.db.update(cx, |db, _| {
                db.store.set_setting("ai.watch.seen", Some(&json))
            })
            .map_err(|e| format!("チェック結果を保存できませんでした: {e}"))
        })
        .map_err(gone)??;

        let mut status = format!(
            "{} をチェックしました。リンク {} 件中、更新 {} 件。",
            now_label(),
            targets.len(),
            changed.len()
        );
        if !failed.is_empty() {
            status.push_str(&format!(" 取得できなかったもの: {}", failed.join(" / ")));
        }
        if changed.is_empty() {
            return Ok(status);
        }

        let mut message = format!(
            "（アプリの定期チェックからの自動のメッセージです）\n\
             タスクに書かれている GitHub のリンクに変化がありました。\n\n指示: {instruction}\n"
        );
        for (url, tasks, content) in &changed {
            message.push_str(&format!("\n## {url}\n関連するタスク:\n"));
            for (id, title) in tasks {
                message.push_str(&format!("- {title}（id: {id}）\n"));
            }
            message.push_str(&format!(
                "今の中身:\n{}\n",
                clip_tail(content, MAX_WATCH_CHARS)
            ));
        }
        let urls: Vec<&str> = changed.iter().map(|(url, _, _)| url.as_str()).collect();
        this.update(cx, |s, cx| {
            s.watch_status = Some(status.clone().into());
            s.items.push(Item::Notice(
                format!(
                    "GitHub の定期チェック: {} に更新がありました。AI がタスクに反映します。",
                    urls.join("、")
                )
                .into(),
            ));
            s.turns.push(Turn::User(message));
            cx.notify();
        })
        .map_err(gone)?;
        Self::converse(
            this,
            &config,
            "定期チェック（ユーザーは画面を見ていないことがあります）",
            cx,
        )
        .await?;
        Ok(status)
    }

    pub fn model(&self) -> &str {
        self.models
            .get(&self.provider)
            .map(String::as_str)
            .unwrap_or_else(|| self.provider.default_model())
    }

    pub fn command_running(&self) -> bool {
        self.running.is_some()
    }

    // ---- 設定 ----

    fn save_setting(&mut self, key: &str, value: &str, cx: &mut Context<Self>) -> bool {
        let result = self
            .db
            .update(cx, |db, _| db.store.set_setting(key, Some(value)));
        match result {
            Ok(()) => {
                self.error = None;
                true
            }
            Err(e) => {
                self.error = Some(format!("AI の設定を保存できませんでした: {e}").into());
                cx.notify();
                false
            }
        }
    }

    pub fn set_provider(&mut self, provider: Provider, cx: &mut Context<Self>) {
        if self.provider == provider || !self.save_setting("ai.provider", provider.as_str(), cx) {
            return;
        }
        self.provider = provider;
        self.listed.clear();
        if !provider.supports_effort() {
            self.effort = Effort::Default;
        }
        self.refresh_key(cx);
        cx.notify();
    }

    pub fn set_model(&mut self, model: &str, cx: &mut Context<Self>) {
        let model = model.trim();
        if model == self.model() || !self.save_setting(&setting_key(self.provider), model, cx) {
            return;
        }
        self.models.insert(self.provider, model.to_owned());
        cx.notify();
    }

    pub fn set_effort(&mut self, effort: Effort, cx: &mut Context<Self>) {
        if self.effort == effort || !self.save_setting("ai.effort", effort.as_str(), cx) {
            return;
        }
        self.effort = effort;
        cx.notify();
    }

    /// 選んでいるサービスのキーがあるかを調べ直す。
    pub fn refresh_key(&mut self, cx: &mut Context<Self>) {
        let (config, provider) = (self.config.clone(), self.provider);
        self.key = KeyState::Unknown;
        cx.spawn(async move |this, cx| {
            // キーチェーンは OS の確認を待つことがあるので、画面のスレッドでは読まない
            let found = cx
                .background_executor()
                .spawn(async move { keys::lookup(&*config.keys, provider, |n| (config.env)(n)) })
                .await;
            // チャットがもうなければ、知らせる先もない
            let _ = this.update(cx, |this, cx| {
                if this.provider != provider {
                    return;
                }
                this.key = match found {
                    Ok(Some((key, source))) => KeyState::Present {
                        masked: keys::masked(&key).into(),
                        source,
                    },
                    Ok(None) => KeyState::Missing,
                    Err(e) => KeyState::Failed(e.into()),
                };
                this.arrange_retry = true;
                cx.notify();
            });
        })
        .detach();
    }

    pub fn save_key(&mut self, key: &str, cx: &mut Context<Self>) {
        let key = key.trim();
        if key.is_empty() {
            return;
        }
        match self.config.keys.set(self.provider, key) {
            Ok(()) => {
                self.error = None;
                self.refresh_key(cx);
            }
            Err(e) => self.error = Some(e.into()),
        }
        cx.notify();
    }

    pub fn delete_key(&mut self, cx: &mut Context<Self>) {
        match self.config.keys.delete(self.provider) {
            Ok(()) => {
                self.error = None;
                self.refresh_key(cx);
            }
            Err(e) => self.error = Some(e.into()),
        }
        cx.notify();
    }

    /// サービスにモデルの一覧を聞く（Claude と OpenCode）。
    pub fn fetch_models(&mut self, cx: &mut Context<Self>) {
        let (config, provider) = (self.config.clone(), self.provider);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if let Some(url) = provider.catalog_url() {
                        let catalog = fetch_catalog(&config, url)?;
                        return Ok(catalog
                            .iter()
                            .filter(|m| m.api.is_some())
                            .map(|m| m.id.clone())
                            .collect::<Vec<_>>());
                    }
                    if provider != Provider::Claude {
                        return Err(format!(
                            "{} のモデルの一覧は取得できません。モデルの名前を入力してください",
                            provider.label()
                        ));
                    }
                    let (key, _) = keys::lookup(&*config.keys, provider, |n| (config.env)(n))?
                        .ok_or("API キーが設定されていません")?;
                    let headers = [
                        ("x-api-key", key),
                        ("anthropic-version", "2026-10-21".to_owned()),
                    ];
                    let text =
                        (config.get)("https://api.anthropic.com/v1/models?limit=100", &headers)?;
                    let value: Value = serde_json::from_str(&text)
                        .map_err(|e| format!("モデルの一覧が JSON として読めません: {e}"))?;
                    Ok(value["data"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|m| m["id"].as_str().map(str::to_owned))
                        .collect())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.provider != provider {
                    return;
                }
                match result {
                    Ok(ids) => {
                        this.error = None;
                        this.listed = ids.into_iter().map(SharedString::from).collect();
                    }
                    Err(e) => {
                        this.error = Some(format!("モデルの一覧を取得できませんでした: {e}").into())
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---- 会話 ----

    /// 新しい会話にする。
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.stop(cx);
        self.turns.clear();
        self.items.clear();
        self.allow_shell = false;
        self.conversation = malakt_core::new_id();
        self.error = None;
        cx.notify();
    }

    /// 送信する。`place` は、ユーザーが今開いている場所の説明。
    pub fn send(&mut self, text: &str, place: String, cx: &mut Context<Self>) -> bool {
        let text = text.trim().to_owned();
        if self.busy || text.is_empty() || !self.db.read(cx).unlocked {
            return false;
        }
        self.busy = true;
        self.error = None;
        self.items.push(Item::User(text.clone().into()));
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = Self::run(&this, text, place, cx).await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.items.push(Item::Error(error.into()));
                }
                this.finish();
                cx.notify();
            });
        }));
        cx.notify();
        true
    }

    /// 止める。実行中のコマンドも強制終了する。
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let killed = self.kill_command();
        // 自分で打ったコマンドは、止まればそのまま終わる。チャットには何も書かない
        if !self.busy || self.own_command {
            if killed {
                cx.notify();
            }
            return;
        }
        // タスクを捨てると、待っているところで止まる
        self.task = None;
        self.finish();
        for item in &mut self.items {
            if let Item::Tool { state, .. } = item {
                if *state == ToolState::Running {
                    *state = ToolState::Failed("中止しました".into());
                }
            }
        }
        self.items.push(Item::Notice("中止しました。".into()));
        cx.notify();
    }

    /// 実行中のコマンドを強制終了する。
    fn kill_command(&mut self) -> bool {
        let Some(handle) = self.running.take() else {
            return false;
        };
        match handle.kill() {
            Ok(()) => self.push_shell(LineKind::Info, "強制終了しました"),
            Err(e) => {
                self.push_shell(LineKind::Err, &format!("強制終了できませんでした: {e}"));
                self.error = Some(format!("コマンドを強制終了できませんでした: {e}").into());
            }
        }
        true
    }

    /// 結果のないツールの呼び出しに、中止の結果を付ける。
    /// 付けないと、次に送るときに API が会話を受け付けない。
    fn settle(&mut self) {
        let mut answered: Vec<&str> = Vec::new();
        let mut open: Vec<(String, String)> = Vec::new();
        for turn in &self.turns {
            match turn {
                Turn::Tool { id, .. } => answered.push(id),
                Turn::Assistant { calls, .. } => {
                    open.extend(calls.iter().map(|c| (c.id.clone(), c.name.clone())))
                }
                Turn::User(_) => {}
            }
        }
        open.retain(|(id, _)| !answered.contains(&id.as_str()));
        for (id, name) in open {
            self.turns.push(Turn::Tool {
                id,
                name,
                output: "ユーザーが中止しました".into(),
                is_error: true,
            });
        }
    }

    /// 確認に答える。
    pub fn answer(&mut self, decision: Decision, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        if decision == Decision::AllowShellAlways {
            self.allow_shell = true;
        }
        // 待っている側がもう止まっていれば、答えは要らない
        let _ = pending.answer.try_send(decision);
        cx.notify();
    }

    fn push_shell(&mut self, kind: LineKind, text: &str) {
        // 色は出力の間だけ引き継ぐ。コマンドや知らせの行で元に戻す
        if !matches!(kind, LineKind::Out | LineKind::Err) {
            self.ansi = ansi::State::default();
        }
        let (text, spans) = ansi::parse(text, &mut self.ansi);
        self.shell_rev += 1;
        self.shell.push(ShellLine {
            kind,
            text: text.into(),
            spans,
        });
        if self.shell.len() > MAX_SHELL_LINES {
            let extra = self.shell.len() - MAX_SHELL_LINES;
            self.shell.drain(..extra);
        }
    }

    /// シェルの表示を消す（Ctrl+L）。実行中のコマンドはそのまま。
    pub fn clear_shell(&mut self, cx: &mut Context<Self>) {
        self.shell.clear();
        self.shell_rev += 1;
        cx.notify();
    }

    /// プロンプトに出す今の場所。ホームは `~` にする。
    pub fn cwd_label(&self) -> String {
        match dirs::home_dir() {
            Some(home) => match self.cwd.strip_prefix(&home) {
                Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
                Ok(rest) => format!("~/{}", rest.display()).replace('\\', "/"),
                Err(_) => self.cwd.display().to_string(),
            },
            None => self.cwd.display().to_string(),
        }
    }

    /// シェルに自分で入力したコマンドを実行する。AI が動いている間は実行しない。
    pub fn run_user_command(&mut self, command: &str, cx: &mut Context<Self>) -> bool {
        let command = command.trim().to_owned();
        if self.busy || command.is_empty() || !self.db.read(cx).unlocked {
            return false;
        }
        if self.history.last().map(|h| h.as_ref()) != Some(command.as_str()) {
            self.history.push(command.clone().into());
            if self.history.len() > MAX_HISTORY {
                self.history.remove(0);
            }
        }
        self.busy = true;
        self.own_command = true;
        self.task = Some(cx.spawn(async move |this, cx| {
            // 起動できなかったことは、シェルに出ている
            let _ = Self::shell_line(&this, &command, cx).await;
            // チャットがなくなっていれば、知らせる先もない
            let _ = this.update(cx, |this, cx| {
                this.finish();
                cx.notify();
            });
        }));
        cx.notify();
        true
    }

    /// シェルの 1 行を、今の場所で実行する。`cd` はシェルの場所を変える。
    async fn shell_line(
        this: &WeakEntity<Self>,
        command: &str,
        cx: &mut AsyncApp,
    ) -> std::io::Result<Output> {
        let dropped = || std::io::Error::other("チャットが閉じられました");
        let cwd = this
            .read_with(cx, |s, _| s.cwd.clone())
            .map_err(|_| dropped())?;
        if let Some(target) = cd_target(command) {
            let next = resolve_dir(&cwd, target);
            let moved = next.as_ref().is_ok_and(|dir| dir.is_dir());
            let mut output = Output {
                stdout: String::new(),
                all: String::new(),
                code: Some(if moved { 0 } else { 1 }),
            };
            this.update(cx, |s, cx| {
                s.push_shell(LineKind::Command, &format!("$ {command}"));
                match next {
                    Ok(dir) if moved => {
                        output.all = format!("今の場所: {}", dir.display());
                        s.cwd = dir;
                    }
                    _ => {
                        output.all = format!("cd: {target}: フォルダが見つかりません");
                        s.push_shell(LineKind::Err, &output.all.clone());
                    }
                }
                cx.notify();
            })
            .map_err(|_| dropped())?;
            return Ok(output);
        }
        let mut spec = Spec::shell(command);
        spec.cwd = Some(cwd);
        Self::run_command(this, spec, cx).await
    }

    // ---- 進行 ----

    async fn run(
        this: &WeakEntity<Self>,
        text: String,
        place: String,
        cx: &mut AsyncApp,
    ) -> Result<(), String> {
        let config = this.read_with(cx, |s, _| s.config.clone()).map_err(gone)?;

        // メッセージに GitHub のリンクがあれば、先に中身を取ってきて添える
        let mut message = text.clone();
        for link in github::find_links(&text) {
            match Self::fetch_github(this, &config, &link, cx).await {
                Ok(content) => message.push_str(&format!("\n\n[{} の中身]\n{content}", link.url())),
                Err(error) => {
                    let note = format!("{} を取得できませんでした: {error}", link.url());
                    message.push_str(&format!("\n\n[{note}]"));
                    this.update(cx, |s, cx| {
                        s.items.push(Item::Error(note.into()));
                        cx.notify();
                    })
                    .map_err(gone)?;
                }
            }
        }
        this.update(cx, |s, _| s.turns.push(Turn::User(message)))
            .map_err(gone)?;
        Self::converse(this, &config, &place, cx).await
    }

    /// 最後のメッセージに AI が答え終わるまで、ツールの実行を挟みながら進める。
    async fn converse(
        this: &WeakEntity<Self>,
        config: &AiConfig,
        place: &str,
        cx: &mut AsyncApp,
    ) -> Result<(), String> {
        let (provider, model, effort, conversation) = this
            .read_with(cx, |s, _| {
                (
                    s.provider,
                    s.model().to_owned(),
                    s.effort,
                    s.conversation.clone(),
                )
            })
            .map_err(gone)?;
        let key = {
            let config = config.clone();
            cx.background_executor()
                .spawn(async move { keys::lookup(&*config.keys, provider, |n| (config.env)(n)) })
                .await?
        }
        .ok_or_else(|| {
            format!(
                "{} の API キーが設定されていません。「設定」で API キーを保存してください。",
                provider.label()
            )
        })?
        .0;

        // 最初の指示は会話の間変えない。日付と今の画面は、最後のメッセージに添える
        let system = this
            .update(cx, |s, cx| {
                let note =
                    tools::turn_note(&malakt_core::view::today(), place, &s.workspace_context(cx));
                if let Some(Turn::User(message)) = s.turns.last_mut() {
                    message.push_str(&note);
                }
                tools::system_prompt(&s.context)
            })
            .map_err(gone)?;
        let mut recovered = false;
        for _ in 0..MAX_ROUNDS {
            let result = Self::complete(
                this,
                config,
                provider,
                &model,
                effort,
                &key,
                &conversation,
                &system,
                cx,
            )
            .await;
            let (wire, reply) = match result {
                // 設定やアプリの更新で最初の指示が変わると、前の考えのブロックをサーバーが断る。
                // 考えのブロックを外して、1 度だけやり直す
                Err(error) if !recovered && wire::is_thinking_mismatch(&error) => {
                    recovered = true;
                    let stripped = this
                        .update(cx, |s, _| wire::strip_thinking(&mut s.turns))
                        .map_err(gone)?;
                    if !stripped {
                        return Err(error);
                    }
                    continue;
                }
                result => result?,
            };
            let calls = reply.calls.clone();
            let first_tool = this
                .update(cx, |s, cx| {
                    if !reply.text.trim().is_empty() {
                        s.items
                            .push(Item::Assistant(reply.text.trim().to_owned().into()));
                    }
                    if let Some(notice) = &reply.notice {
                        s.items.push(Item::Notice(notice.clone().into()));
                    }
                    let first = s.items.len();
                    for call in &reply.calls {
                        s.items.push(Item::Tool {
                            summary: tools::summary(&call.name, &call.args).into(),
                            state: ToolState::Running,
                        });
                    }
                    s.turns.push(reply.into_turn(wire));
                    cx.notify();
                    first
                })
                .map_err(gone)?;
            if calls.is_empty() {
                return Ok(());
            }
            for (offset, call) in calls.iter().enumerate() {
                let (state, output) = match Self::call_tool(this, config, call, cx).await? {
                    Ok(output) => (ToolState::Done, Ok(output)),
                    Err(ToolError::Denied) => {
                        (ToolState::Denied, Err("ユーザーが拒否しました".to_owned()))
                    }
                    Err(ToolError::Failed(e)) => (ToolState::Failed(e.clone().into()), Err(e)),
                };
                this.update(cx, |s, cx| {
                    if let Some(Item::Tool { state: slot, .. }) =
                        s.items.get_mut(first_tool + offset)
                    {
                        *slot = state;
                    }
                    let (output, is_error) = match output {
                        Ok(output) => (output, false),
                        Err(error) => (error, true),
                    };
                    s.turns.push(Turn::Tool {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        output: clip_tail(&output, MAX_TOOL_OUTPUT_CHARS),
                        is_error,
                    });
                    cx.notify();
                })
                .map_err(gone)?;
            }
        }
        Err(format!(
            "ツールの呼び出しが {MAX_ROUNDS} 回続いたので止めました。続けるときは、もう一度送ってください。"
        ))
    }

    /// AI に 1 回聞く。
    #[allow(clippy::too_many_arguments)]
    async fn complete(
        this: &WeakEntity<Self>,
        config: &AiConfig,
        provider: Provider,
        model: &str,
        effort: Effort,
        key: &str,
        conversation: &str,
        system: &str,
        cx: &mut AsyncApp,
    ) -> Result<(Wire, Reply), String> {
        let turns = this.read_with(cx, |s, _| s.turns.clone()).map_err(gone)?;
        let definitions = tools::definitions();
        let mut request = wire::Request {
            model,
            effort,
            system,
            turns: &turns,
            tools: &definitions,
            fallbacks: false,
        };

        if provider == Provider::Cursor {
            let Value::String(prompt) = wire::body(Wire::Cursor, &request) else {
                return Err("Cursor に渡す文章を作れませんでした".into());
            };
            let output = Self::run_command(this, cursor_spec(model, key, prompt)?, cx)
                .await
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        "Cursor の CLI（agent コマンド）が見つかりません。https://cursor.com/docs/cli の手順で入れてください".to_owned()
                    } else {
                        format!("Cursor の CLI を起動できませんでした: {e}")
                    }
                })?;
            if output.code != Some(0) {
                return Err(format!(
                    "Cursor の CLI が失敗しました（終了コード {}）。\n{}",
                    output.code.map_or("なし".to_owned(), |c| c.to_string()),
                    clip_tail(output.all.trim(), 2000)
                ));
            }
            let line = output.stdout.lines().rev().find(|l| !l.trim().is_empty());
            let value: Value = serde_json::from_str(line.unwrap_or_default())
                .map_err(|e| format!("Cursor の CLI の出力が JSON として読めません: {e}"))?;
            return wire::parse(Wire::Cursor, &value).map(|reply| (Wire::Cursor, reply));
        }

        let endpoint: Endpoint = match provider.catalog_url() {
            Some(url) => {
                let cached = this
                    .read_with(cx, |s, _| {
                        s.catalog
                            .as_ref()
                            .filter(|(p, _)| *p == provider)
                            .map(|(_, c)| c.clone())
                    })
                    .map_err(gone)?;
                let catalog = match cached {
                    Some(catalog) => catalog,
                    None => {
                        let config = config.clone();
                        let catalog = Arc::new(
                            cx.background_executor()
                                .spawn(async move { fetch_catalog(&config, url) })
                                .await?,
                        );
                        this.update(cx, |s, _| s.catalog = Some((provider, catalog.clone())))
                            .map_err(gone)?;
                        catalog
                    }
                };
                provider::opencode_endpoint(&catalog, model, effort, key, conversation)?
            }
            None => provider::endpoint(provider, model, key)?,
        };
        request.fallbacks = endpoint.fallbacks;
        let body = wire::body(endpoint.wire, &request);
        let post = config.post.clone();
        let wire = endpoint.wire;
        let response = cx
            .background_executor()
            .spawn(async move { post(&endpoint.url, &endpoint.headers, &body) })
            .await?;
        wire::parse(wire, &response).map(|reply| (wire, reply))
    }

    /// ツールを 1 つ実行する。外側の `Err` は、チャットがなくなったとき。
    async fn call_tool(
        this: &WeakEntity<Self>,
        config: &AiConfig,
        call: &ToolCall,
        cx: &mut AsyncApp,
    ) -> Result<Result<String, ToolError>, String> {
        let name = call.name.as_str();
        let auto = this.read_with(cx, |s, _| s.auto).map_err(gone)?;
        if auto && tools::needs_confirmation(name) {
            // 確認に答える人がいないので、定期チェックからは実行しない
            return Ok(Err(ToolError::Failed(
                "定期チェックからは、タスクの削除とコマンドの実行はできません".into(),
            )));
        }
        if tools::needs_confirmation(name) {
            let (tx, rx) = async_channel::bounded(1);
            let asked = this
                .update(cx, |s, cx| {
                    if s.auto_run || name == tools::RUN_SHELL && s.allow_shell {
                        return false;
                    }
                    let text = tools::confirmation_text(&s.db.read(cx).store, name, &call.args);
                    s.pending = Some(Pending {
                        text: text.into(),
                        shell: name == tools::RUN_SHELL,
                        answer: tx,
                    });
                    cx.notify();
                    true
                })
                .map_err(gone)?;
            // 答える側がなくなったら、許可されなかったものとして扱う
            if asked && rx.recv().await.unwrap_or(Decision::Deny) == Decision::Deny {
                return Ok(Err(ToolError::Denied));
            }
        }
        let failed = |e: String| Ok(Err(ToolError::Failed(e)));
        match name {
            tools::RUN_SHELL => {
                let Some(command) = call.args.get("command").and_then(Value::as_str) else {
                    return failed("command が必要です".into());
                };
                match Self::shell_line(this, command, cx).await {
                    Ok(output) => Ok(Ok(format!(
                        "終了コード: {}\n{}",
                        output
                            .code
                            .map_or("なし（強制終了）".to_owned(), |c| c.to_string()),
                        output.all
                    ))),
                    Err(e) => failed(format!("コマンドを起動できませんでした: {e}")),
                }
            }
            tools::GITHUB_FETCH => {
                let url = call
                    .args
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let Some(link) = github::parse_link(url) else {
                    return failed(format!("GitHub のリンクとして読めません: {url}"));
                };
                match Self::fetch_github(this, config, &link, cx).await {
                    Ok(content) => Ok(Ok(content)),
                    Err(e) => failed(e),
                }
            }
            _ => {
                let result = this
                    .update(cx, |s, cx| {
                        // ユーザーが作って、まだ整えていないタスク
                        let waiting: Vec<String> = task_links(&s.db.read(cx).store)
                            .map(|all| s.new_tasks(all).into_iter().map(|t| t.id).collect())
                            .unwrap_or_default();
                        // 自動の整理では、読めなかったことの注記をタスクに書かせない
                        let cleaned = (!s.arranging.is_empty())
                            .then(|| without_fetch_notes(&s.db.read(cx).store, &call.args));
                        let args = cleaned.as_ref().unwrap_or(&call.args);
                        let result = s.db.update(cx, |db, cx| {
                            // 自分が登録されていれば、コメントの書き手にする
                            let author = db
                                .store
                                .people()
                                .ok()
                                .and_then(|p| p.into_iter().find(|p| p.is_self))
                                .map(|p| p.id);
                            let result =
                                tools::execute(&mut db.store, author.as_deref(), name, args);
                            cx.notify();
                            result
                        });
                        s.absorb(&waiting, cx);
                        result
                    })
                    .map_err(gone)?;
                Ok(result.map_err(ToolError::Failed))
            }
        }
    }

    /// GitHub のリンクの中身を取ってくる。`gh` がなければ、公開の API を読む。
    async fn fetch_github(
        this: &WeakEntity<Self>,
        config: &AiConfig,
        link: &Link,
        cx: &mut AsyncApp,
    ) -> Result<String, String> {
        match Self::run_command(this, (config.gh)(link), cx).await {
            Ok(output) if output.code == Some(0) && !output.stdout.trim().is_empty() => {
                Ok(output.stdout)
            }
            Ok(output) if output.code == Some(0) => Err("gh の出力が空でした".into()),
            Ok(output) => Err(format!(
                "gh が失敗しました（終了コード {}）: {}",
                output.code.map_or("なし".to_owned(), |c| c.to_string()),
                clip_tail(output.all.trim(), 1000)
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let (url, accept) = link.api_request();
                this.update(cx, |s, cx| {
                    s.push_shell(
                        LineKind::Info,
                        &format!("gh コマンドがないので、{url} を直接読みます"),
                    );
                    cx.notify();
                })
                .map_err(gone)?;
                let get = config.get.clone();
                let headers = [("accept", accept.to_owned())];
                let body = cx
                    .background_executor()
                    .spawn(async move { get(&url, &headers) })
                    .await?;
                Ok(link.format_api_response(&body))
            }
            Err(e) => Err(format!("gh を起動できませんでした: {e}")),
        }
    }

    /// コマンドを実行し、出力をシェルに出しながら終わるまで待つ。
    async fn run_command(
        this: &WeakEntity<Self>,
        spec: Spec,
        cx: &mut AsyncApp,
    ) -> std::io::Result<Output> {
        let dropped = || std::io::Error::other("チャットが閉じられました");
        let (tx, rx) = async_channel::unbounded();
        this.update(cx, |s, cx| {
            s.push_shell(LineKind::Command, &format!("$ {}", spec.display));
            cx.notify();
        })
        .map_err(|_| dropped())?;
        let handle = match shell::spawn(&spec, move |event| {
            // 受け取る側が中止で止まっていれば、送れなくてよい
            let _ = tx.send_blocking(event);
        }) {
            Ok(handle) => handle,
            Err(e) => {
                this.update(cx, |s, cx| {
                    s.push_shell(LineKind::Err, &format!("起動できませんでした: {e}"));
                    cx.notify();
                })
                .map_err(|_| dropped())?;
                return Err(e);
            }
        };
        this.update(cx, |s, cx| {
            s.running = Some(handle);
            cx.notify();
        })
        .map_err(|_| dropped())?;

        let mut output = Output {
            stdout: String::new(),
            all: String::new(),
            code: None,
        };
        let mut finished = false;
        while !finished {
            let Ok(first) = rx.recv().await else { break };
            // たまっている分は、まとめて 1 回で画面に出す
            let mut events = vec![first];
            while events.len() < 500 {
                match rx.try_recv() {
                    Ok(event) => events.push(event),
                    Err(_) => break,
                }
            }
            this.update(cx, |s, cx| {
                for event in events {
                    match event {
                        shell::Event::Stdout(line) => {
                            s.push_shell(LineKind::Out, &line);
                            output.stdout.push_str(&line);
                            output.stdout.push('\n');
                            output.all.push_str(&line);
                            output.all.push('\n');
                        }
                        shell::Event::Stderr(line) => {
                            s.push_shell(LineKind::Err, &line);
                            output.all.push_str(&line);
                            output.all.push('\n');
                        }
                        shell::Event::Exit(code) => {
                            output.code = code;
                            finished = true;
                            s.running = None;
                            if code != Some(0) {
                                let text = match code {
                                    Some(code) => format!("終了コード {code}"),
                                    None => "シグナルで終了".to_owned(),
                                };
                                s.push_shell(LineKind::Info, &text);
                            }
                        }
                    }
                }
                cx.notify();
            })
            .map_err(|_| dropped())?;
        }
        Ok(output)
    }
}

enum ToolError {
    Denied,
    Failed(String),
}

/// チャットがなくなった（アプリの終了）。出す先がないので、文は空でよい。
/// `cd` だけのコマンドなら、移る先（引数がなければホーム）。
fn cd_target(command: &str) -> Option<&str> {
    let rest = command.trim().strip_prefix("cd")?;
    if rest.is_empty() {
        return Some("~");
    }
    // `cd x && make` のように続きがあるものは、シェルに任せる
    if !rest.starts_with(char::is_whitespace) || rest.contains(['&', ';', '|', '>', '<', '`', '$'])
    {
        return None;
    }
    let target = rest.trim();
    Some(
        target
            .strip_prefix('"')
            .and_then(|t| t.strip_suffix('"'))
            .or_else(|| target.strip_prefix('\'').and_then(|t| t.strip_suffix('\'')))
            .unwrap_or(target),
    )
}

fn resolve_dir(cwd: &std::path::Path, target: &str) -> std::io::Result<std::path::PathBuf> {
    let path = if target == "~" || target.starts_with("~/") || target.starts_with("~\\") {
        let home = dirs::home_dir()
            .ok_or_else(|| std::io::Error::other("ホームのフォルダが分かりません"))?;
        home.join(target.get(2..).unwrap_or_default())
    } else {
        cwd.join(target)
    };
    // `..` をたどって、実際の場所にする
    path.canonicalize()
}

/// 「11/24 13:05」のような、今の時刻。
fn now_label() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64);
    malakt_core::view::format_datetime(ms)
        .chars()
        .skip(5)
        .collect()
}

/// 読めなかった・確かめられなかったことを書いた文の目印。
const FETCH_NOTE_MARKS: &[&str] = &[
    "取得でき",
    "取得に失敗",
    "取得失敗",
    "読み込めな",
    "読み込めません",
    "読めな",
    "読めません",
    "アクセスでき",
    "確認できな",
    "確認できません",
    "未確認",
];

fn is_fetch_note(text: &str) -> bool {
    FETCH_NOTE_MARKS.iter().any(|mark| text.contains(mark))
}

/// AI がタスクに書こうとしたタイトルと説明から、読めなかったことの注記を除く。
/// 元のタスクにあった文は残す。
fn without_fetch_notes(store: &malakt_core::Store, args: &Value) -> Value {
    let mut args = args.clone();
    let before = args
        .get("id")
        .and_then(Value::as_str)
        .and_then(|id| store.task(id).ok());
    let (old_title, old_description) = before.map(|t| (t.title, t.description)).unwrap_or_default();
    let Some(map) = args.as_object_mut() else {
        return args;
    };
    if map
        .get("title")
        .and_then(Value::as_str)
        .is_some_and(|title| is_fetch_note(title) && title != old_title)
    {
        map.remove("title");
    }
    if let Some(description) = map.get("description").and_then(Value::as_str) {
        let kept: Vec<&str> = description
            .lines()
            .filter(|line| !is_fetch_note(line) || old_description.lines().any(|old| old == *line))
            .collect();
        let kept = kept.join("\n").trim_end().to_owned();
        map.insert("description".into(), Value::String(kept));
    }
    args
}

fn gone<E>(_: E) -> String {
    String::new()
}

fn fetch_catalog(config: &AiConfig, url: &str) -> Result<Vec<CatalogModel>, String> {
    let text = (config.get)(url, &[])?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| format!("OpenCode のモデルの一覧が JSON として読めません: {e}"))?;
    provider::parse_catalog(&value)
}

/// Cursor の CLI を、質問に答えるだけのモードで呼ぶ。
fn cursor_spec(model: &str, key: &str, prompt: String) -> Result<Spec, String> {
    // CLI が自分でファイルを触っても困らないように、空のフォルダで動かす
    let dir = std::env::temp_dir().join("malakt-mahoz-cursor");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("{} を作れませんでした: {e}", dir.display()))?;
    let mut args: Vec<String> = ["-p", "--output-format", "json", "--mode", "ask"]
        .map(String::from)
        .into();
    if !model.is_empty() {
        args.extend(["--model".to_owned(), model.to_owned()]);
    }
    let display = format!(
        "agent {} （会話 {} 文字）",
        args.join(" "),
        prompt.chars().count()
    );
    args.push(prompt);
    let mut spec = Spec::direct("agent", args);
    spec.display = display;
    spec.env = vec![("CURSOR_API_KEY".into(), key.to_owned())];
    spec.cwd = Some(dir);
    Ok(spec)
}

#[cfg(test)]
mod tests {
    use super::cursor_spec;

    #[test]
    fn cursor_runs_in_ask_mode_and_hides_the_prompt_and_key() {
        let spec = cursor_spec("gpt-x", "secret-key", "長い会話".into()).unwrap();
        assert_eq!(spec.program, "agent");
        assert_eq!(
            spec.args,
            [
                "-p",
                "--output-format",
                "json",
                "--mode",
                "ask",
                "--model",
                "gpt-x",
                "長い会話"
            ]
        );
        assert!(spec.display.contains("会話 4 文字"));
        assert!(!spec.display.contains("長い会話"));
        assert!(!spec.display.contains("secret-key"));
        assert!(spec
            .env
            .contains(&("CURSOR_API_KEY".into(), "secret-key".into())));
        assert!(spec.cwd.unwrap().ends_with("malakt-mahoz-cursor"));
        // モデルが空なら、Cursor の標準のモデルに任せる
        let spec = cursor_spec("", "k", "p".into()).unwrap();
        assert!(!spec.args.contains(&"--model".to_owned()));
    }
}
