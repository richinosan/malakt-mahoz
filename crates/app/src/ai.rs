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
use malakt_ai::shell::{self, Shell, Spec};
use malakt_ai::slack;
use malakt_ai::watch;
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
const MAX_WATCH_LINKS: usize = 50;
/// AI に渡す、リンク 1 つあたりの中身の文字数（タスクを整えるとき）。
const MAX_WATCH_CHARS: usize = 4000;
/// 定期チェックの間隔として選べる分数。
pub const WATCH_MINUTES: [u32; 5] = [60, 180, 360, 720, 1440];
/// 定期チェックの時刻になったかを見る間隔。眠っていた PC が起きたときも、すぐに気づく。
const WATCH_TICK: std::time::Duration = std::time::Duration::from_secs(60);
/// 最後に定期チェックを始めた時刻（UNIX 秒）を保存する設定の名前。起動し直しても間隔を数え続ける。
const WATCH_LAST_SETTING: &str = "ai.watch.last";
/// 定期チェックで進展を見つけたときに AI に渡す、最初の指示。
pub const DEFAULT_WATCH_INSTRUCTION: &str = "GitHub の進展を、関連するタスクにコメントで要約してください。\
誰が何をしたか（自分のレビューやコメントと、ほかの人の返信・レビュー・コミット）、決まったこと、次にすることを短く書いてください。\
未完了のタスクで、issue がクローズされていたりプルリクエストがマージされていたりしたら、タスクを完了にしてください。";

/// GitHub の定期チェックの設定。標準はオンで 24 時間ごと（API キーがなければ動かない）。
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
            enabled: true,
            minutes: 1440,
            instruction: DEFAULT_WATCH_INSTRUCTION.to_owned(),
        }
    }
}

/// 定期チェックで見に行くリンクと、それが書かれているタスク（ID とタイトル）。
type WatchTarget = (Link, Vec<(String, String)>);

/// 未完了のタスクに書かれている issue とプルリクエストのリンクを集める（[`MAX_WATCH_LINKS`] 個まで）。
/// 数が多いときは、最近書き換えたタスクのリンクを先に見る。
fn watch_targets(store: &malakt_core::Store) -> Result<Vec<WatchTarget>, String> {
    let filter = malakt_core::TaskFilter {
        open_only: true,
        ..Default::default()
    };
    let mut tasks = store
        .tasks(malakt_core::Scope::All, &filter)
        .map_err(|e| format!("タスクを読めませんでした: {e}"))?;
    tasks.sort_by_key(|t| std::cmp::Reverse(t.updated_at));
    let mut targets: Vec<WatchTarget> = Vec::new();
    for task in tasks {
        for link in github::find_links(&link_text(&task)) {
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
/// 会話の一覧。今の会話の ID も入れる。
/// サービスの一覧にあった、モデルの表示名（ID ごと）。
const MODEL_NAMES_SETTING: &str = "ai.model_names";
/// モデルごとに選んだ考える深さ。`{"claude:claude-fable-5-2": "low"}` の形。「標準」は入れない
const EFFORTS_SETTING: &str = "ai.efforts";
/// 前の版の、すべてのモデルで共通の考える深さ。読んだら今のモデルのものとして移す
const LEGACY_EFFORT_SETTING: &str = "ai.effort";
const MAX_MODEL_NAMES: usize = 300;
const CHATS_SETTING: &str = "ai.chats";
/// アプリが自動で始めた AI の実行の記録。
const RUNS_SETTING: &str = "ai.runs";
/// 残しておく会話の数。超えたら、古いものから消す。
pub const MAX_CHATS: usize = 30;
/// 残しておく実行の記録の数。
pub const MAX_RUNS: usize = 50;
const MAX_RUN_ITEMS: usize = 30;
const MAX_RUN_CHARS: usize = 2000;
const MAX_TITLE_CHARS: usize = 40;

/// 並列で同時に動かせる会話の数。
pub const MAX_PARALLEL: usize = 3;

/// 振り分け役への指示。
const ROUTE_PROMPT: &str = "あなたは、タスク管理アプリの AI チャットの振り分け役です。\n\
ユーザーは AI と話している途中で、AI が答え終わる前に次のメッセージを送りました。\n\
次のメッセージが、今の会話の続き（今の話題への追加、訂正、補足、質問）なら CONTINUE、\n\
今の会話と関係のない別の依頼なら NEW とだけ答えてください。ほかのことは書かないでください。";

fn coordinator_key(provider: Provider) -> String {
    format!("ai.coordinator.{}", provider.as_str())
}

/// 考え中に送られて、今の会話の続きとして待っているメッセージ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued {
    pub text: String,
    place: String,
    attached: Vec<(String, SharedString)>,
    attached_workspaces: Vec<(String, SharedString)>,
}

/// 並列で動かしている、別の会話。
pub struct Parallel {
    pub session: Entity<Session>,
    pub title: SharedString,
    pub done: bool,
    pub error: Option<SharedString>,
    _watch: Subscription,
}

fn chat_key(id: &str) -> String {
    format!("ai.chat.{id}")
}

/// 一覧に出す会話。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatInfo {
    pub id: String,
    pub title: SharedString,
    /// この会話を始めたタスク。
    pub task: Option<String>,
    pub updated: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunKind {
    Arrange,
    Track,
    Watch,
    /// 繰り返しのタスクの回を作ったときに、AI に頼んだこと。
    Repeat,
}

impl RunKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Arrange => "タスクの整理",
            Self::Track => "コメントからの更新",
            Self::Watch => "GitHub の定期チェック",
            Self::Repeat => "定期実行",
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Arrange => "arrange",
            Self::Track => "track",
            Self::Watch => "watch",
            Self::Repeat => "repeat",
        }
    }
}

/// アプリが自動で始めた AI の実行 1 回分。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub id: String,
    pub kind: RunKind,
    /// 対象のタスク（ID と題）。
    pub tasks: Vec<(String, SharedString)>,
    pub at: i64,
    /// 失敗した理由。
    pub error: Option<SharedString>,
    /// 失敗をもう見た。チャットの上には出さない。
    pub seen: bool,
    pub items: Vec<Item>,
}

fn item_json(item: &Item) -> Value {
    match item {
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
    }
}

fn item_from(v: &Value) -> Item {
    let t: SharedString = v["t"].as_str().unwrap_or_default().to_owned().into();
    match v["k"].as_str() {
        Some("user") => Item::User(t),
        Some("assistant") => Item::Assistant(t),
        Some("notice") => Item::Notice(t),
        Some("tool") => Item::Tool {
            summary: t,
            state: match v["s"].as_str() {
                Some("done") => ToolState::Done,
                Some("denied") => ToolState::Denied,
                _ => ToolState::Failed(v["d"].as_str().unwrap_or_default().to_owned().into()),
            },
        },
        _ => Item::Error(t),
    }
}

fn clip_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let mut out: String = text.chars().take(max).collect();
        out.push('…');
        out
    }
}

pub const CHAT_WIDTH: f32 = 440.;
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

/// 入力欄に添えられるタスクの数。
pub const MAX_ATTACHED_TASKS: usize = 10;
/// 入力欄に添えられるワークスペースの数。1 つでタスクの一覧ごと渡すので、タスクより少なくする。
pub const MAX_ATTACHED_WORKSPACES: usize = 3;

/// 新しいタスクを AI が整えるとき、1 回で扱うタスクの数。
const MAX_ARRANGE_TASKS: usize = 5;
/// 新しいタスクを整えるとき、タスク 1 つで読みに行くリンクの数。
const MAX_ARRANGE_LINKS: usize = 3;

/// AI が整える前のタスク。
struct NewTask {
    id: String,
    title: String,
    description: String,
    /// タスクの URL の欄。
    urls: Vec<String>,
    links: Vec<Fetch>,
    /// 繰り返しで作った回と、定時タスクで作ったタスク（前の回や雛形を写しただけなので、整えない）。
    copied: bool,
}

/// AI が中身を取りに行くリンク。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Fetch {
    GitHub(Link),
    Slack(slack::Link),
}

impl Fetch {
    fn url(&self) -> String {
        match self {
            Self::GitHub(link) => link.url(),
            Self::Slack(link) => link.url(),
        }
    }
}

/// タスクの中でリンクを探す文。タイトル、URL の欄、説明の順。
fn link_text(task: &malakt_core::Task) -> String {
    let mut text = task.title.clone();
    for url in &task.urls {
        text.push('\n');
        text.push_str(url);
    }
    text.push('\n');
    text.push_str(&task.description);
    text
}

/// 文章の中の、GitHub と Slack のリンク。
fn find_sources(text: &str) -> Vec<Fetch> {
    github::find_links(text)
        .into_iter()
        .map(Fetch::GitHub)
        .chain(slack::find_links(text).into_iter().map(Fetch::Slack))
        .collect()
}

/// タスクごとの、書かれている GitHub と Slack のリンク。新しいタスクとリンクを見分ける元にする。
fn task_links(store: &malakt_core::Store) -> Result<Vec<(NewTask, bool)>, String> {
    let tasks = store
        .tasks(malakt_core::Scope::All, &malakt_core::TaskFilter::default())
        .map_err(|e| format!("タスクを読めませんでした: {e}"))?;
    Ok(tasks
        .into_iter()
        .map(|task| {
            let links = find_sources(&link_text(&task));
            let open = task.status.is_open();
            let copied = matches!(
                task.created_by,
                malakt_core::Actor::Repeat | malakt_core::Actor::Schedule
            );
            (
                NewTask {
                    id: task.id,
                    title: task.title,
                    description: task.description,
                    urls: task.urls,
                    links,
                    copied,
                },
                open,
            )
        })
        .collect())
}

/// コメントが書かれてから、AI がタスクを更新し始めるまでの待ち時間。続けて書いた分をまとめる。
pub const TRACK_DELAY: std::time::Duration = std::time::Duration::from_secs(3);
/// コメントからタスクを更新するとき、1 回で扱うタスクの数。
const MAX_TRACK_TASKS: usize = 5;
/// コメントからタスクを更新するとき、タスク 1 つで AI に渡すコメントの数と文字数。
const MAX_TRACK_COMMENTS: usize = 12;
const MAX_TRACK_CHARS: usize = 4000;
/// 元に戻せるように覚えておくタスクの数。
const MAX_TRACK_UNDO: usize = 50;

/// 今あるコメントをすべて「もう見た」とする目印。いちばん新しい時刻と、その時刻のコメントの ID。
fn comment_mark(store: &malakt_core::Store) -> Option<(i64, Vec<String>)> {
    let latest = store.latest_comment_time().ok()?;
    let seen = store.comments_changed_since(latest).ok()?;
    Some((latest, seen.into_iter().map(|(id, _, _)| id).collect()))
}

/// AI がコメントから更新したタスクの、前と後。元に戻すときに使う。
pub struct Tracked {
    before: malakt_core::Task,
    after: malakt_core::Task,
    /// 何を変えたか（「25% → 60%」など）。
    pub label: SharedString,
}

/// AI が変えたところを短い言葉にする。`now` があれば、今も AI が変えた値のままの項目だけを数える。
/// 1 つもなければ `None`。
fn tracked_label(
    before: &malakt_core::Task,
    after: &malakt_core::Task,
    now: Option<&malakt_core::Task>,
) -> Option<String> {
    let now = now.unwrap_or(after);
    let mut parts = Vec::new();
    if before.percent != after.percent && now.percent == after.percent {
        parts.push(format!("{}% → {}%", before.percent, after.percent));
    }
    if before.status != after.status && now.status == after.status {
        parts.push(after.status.label().to_owned());
    }
    if before.due != after.due && now.due == after.due {
        parts.push(match &after.due {
            Some(due) => format!("期限 {due}"),
            None => "期限なし".to_owned(),
        });
    }
    if before.start != after.start && now.start == after.start {
        parts.push(match &after.start {
            Some(start) => format!("開始 {start}"),
            None => "開始日なし".to_owned(),
        });
    }
    if before.priority != after.priority && now.priority == after.priority {
        parts.push(format!(
            "優先度 {}",
            malakt_core::view::Priority::of(after.priority).label()
        ));
    }
    (!parts.is_empty()).then(|| parts.join("・"))
}

/// コメントからの更新で、AI が `update_task` に渡せる項目。
const TRACK_FIELDS: [&str; 6] = ["id", "percent", "status", "due", "start", "priority"];

/// コメントからの更新で AI が呼べるツールか。タスクを読むことと、対象のタスクの更新だけができる。
fn track_allows(name: &str, args: &Value, tracking: &[String]) -> bool {
    match name {
        "update_task" => args
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| tracking.iter().any(|t| t == id)),
        "get_task" | "list_tasks" | "list_workspaces" | "list_people" => true,
        _ => false,
    }
}

type Headers = [(&'static str, String)];
/// 環境変数を読む。
pub type EnvFn = dyn Fn(&str) -> Option<String> + Send + Sync;
/// JSON を POST して、JSON を受け取る。
pub type PostFn = dyn Fn(&str, &Headers, &Value) -> Result<Value, String> + Send + Sync;
/// GET して、本文を受け取る。
pub type GetFn = dyn Fn(&str, &Headers) -> Result<String, String> + Send + Sync;

/// ワークスペースのアイコンを選ぶときの、AI への指示。
const ICON_PROMPT: &str = "ワークスペース（タスクをまとめるフォルダ）の名前を受け取り、その名前に合うアイコンを探すための英単語を答えます。\n\
- 具体的な物や生き物を表す、小文字の英語の名詞を 5 つまで、合う順にコンマで区切って書く（例: cat, plane, book, wrench, house）。\n\
- 英単語のほかには何も書かない。";

/// 定期チェックで、GitHub のリンクから進展を読むコマンドを作る。
pub type ProgressFn = dyn Fn(&Link) -> Vec<Spec> + Send + Sync;

/// Slack のリンクから、取りに行くコマンドを作る。
pub type SlackFn = dyn Fn(&slack::Link) -> Vec<Spec> + Send + Sync;

/// 外とのやり取り。テストでは差し替える。
#[derive(Clone)]
pub struct AiConfig {
    pub keys: Arc<dyn KeyStore>,
    pub env: Arc<EnvFn>,
    pub post: Arc<PostFn>,
    pub get: Arc<GetFn>,
    /// GitHub のリンクを取りに行くコマンド。
    pub gh: Arc<dyn Fn(&Link) -> Spec + Send + Sync>,
    /// Slack のリンクを取りに行くコマンド。前のものが起動できなければ、次のものを試す。
    pub slack: Arc<SlackFn>,
    /// `gh` にログインしているアカウントを調べるコマンド（引数は、GitHub に通信してよいか）。
    pub whoami: Arc<dyn Fn(bool) -> Vec<Spec> + Send + Sync>,
    /// 定期チェックで、issue とプルリクエストの進展を読むコマンド（[`malakt_ai::watch::commands`]）。
    pub gh_progress: Arc<ProgressFn>,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            keys: Arc::new(keys::Keychain),
            env: Arc::new(|name| std::env::var(name).ok()),
            post: Arc::new(malakt_ai::http::post_json),
            get: Arc::new(malakt_ai::http::get_text),
            gh: Arc::new(Link::gh),
            slack: Arc::new(slack::Link::commands),
            whoami: Arc::new(github::whoami),
            gh_progress: Arc::new(malakt_ai::watch::commands),
        }
    }
}

/// `gh` にログインしている、ユーザーの GitHub のアカウント。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GithubAccount {
    /// まだ調べていない
    Unknown,
    Login(SharedString),
    /// `gh` がないか、ログインしていない
    Missing,
}

/// `gh` の返事を待つ長さ。過ぎたら止める。
const WHOAMI_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// コマンドを順に試して、最初に読めたアカウントの名前を返す。シェルの画面には出さない。
fn read_login(specs: &[Spec]) -> Option<String> {
    specs.iter().find_map(|spec| {
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = shell::spawn(spec, move |event| {
            // 待つのをやめたあとの出力は、送れなくてよい
            let _ = tx.send(event);
        })
        .ok()?;
        let until = std::time::Instant::now() + WHOAMI_TIMEOUT;
        let mut stdout = String::new();
        loop {
            match rx.recv_timeout(until.saturating_duration_since(std::time::Instant::now())) {
                Ok(shell::Event::Stdout(line)) => {
                    stdout.push_str(&line);
                    stdout.push('\n');
                }
                Ok(shell::Event::Stderr(_)) => {}
                Ok(shell::Event::Exit(Some(0))) => return github::parse_login(&stdout),
                Ok(shell::Event::Exit(_)) => return None,
                Err(_) => {
                    // 止められなくても、このコマンドの結果は使わない
                    let _ = handle.kill();
                    return None;
                }
            }
        }
    })
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
    /// 今のモデルの考える深さ。モデルを変えると、そのモデルで最後に選んだものになる
    pub effort: Effort,
    /// モデルごとに選んだ考える深さ。キーは `effort_key`
    efforts: HashMap<String, Effort>,
    pub key: KeyState,
    github: GithubAccount,
    /// アカウントを読みに行った回数。古い結果を使わないために数える。
    github_read: u64,
    /// サービスから取ってきたモデルの一覧。
    pub listed: Vec<SharedString>,
    /// サービスの一覧にあった、モデルの表示名。
    model_names: HashMap<String, String>,
    catalog: Option<(Provider, Arc<Vec<CatalogModel>>)>,
    turns: Vec<Turn>,
    pub items: Vec<Item>,
    pub busy: bool,
    pub pending: Option<Pending>,
    pub shell: Vec<ShellLine>,
    /// シェルの中身が変わるたびに増える。表示の折り返しを作り直す目印。
    pub shell_rev: u64,
    /// 古くなって消したシェルの行の数。行を `shell_dropped + 番号` で数えると、消しても変わらない。
    pub shell_dropped: usize,
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
    /// AI のコマンドが動くシェル。メッセージを送るたびに、開いているタブから決めて AI に伝える。
    pub turn_shell: Shell,
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
    /// 定期チェックの時計の起点（UNIX 秒と、そのときのアプリの時計）。
    watch_epoch: (i64, std::time::Instant),
    /// 今の進行が、定期チェックなどアプリから始まったものか。確認が要る操作は実行しない。
    auto: bool,
    /// 新しいタスクを AI が整える。標準はオン（API キーがなければ動かない）。
    pub arrange: bool,
    /// AI が今整えているタスクの ID。
    pub arranging: Vec<String>,
    /// タスクを整えられなかった理由。タスクの ID ごと。
    pub arrange_notes: HashMap<String, SharedString>,
    /// コメントが書かれたら、AI が進捗などを更新する。標準はオン（API キーがなければ動かない）。
    pub track: bool,
    /// AI が今コメントから更新しているタスクの ID。
    pub tracking: Vec<String>,
    /// コメントから更新できなかった理由。タスクの ID ごと。
    pub track_notes: HashMap<String, SharedString>,
    /// AI がコメントから更新したタスクの、前と後。タスクの ID ごと。
    pub tracked: HashMap<String, Tracked>,
    /// コメントが書かれて、更新を待っているタスクの ID。
    track_waiting: Vec<String>,
    /// この時刻までのコメントは、もう見た。同じ時刻のコメントは ID で見分ける。
    comment_mark: Option<(i64, Vec<String>)>,
    track_timer: Option<Task<()>>,
    /// 手が空いたら、待っているタスクを更新する。
    track_retry: bool,
    /// もう見たタスクと、そのリンク。ここにないタスクやリンクが新しいもの。
    known: Option<HashMap<String, Vec<String>>>,
    /// ユーザーが最初のタイトルを書いている途中のタスク。書き終えるまで整えない。
    holding: Vec<String>,
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
    /// 入力欄に添えたタスク（ID と題）。送るときに、中身を AI に渡す。
    pub attached: Vec<(String, SharedString)>,
    /// 入力欄に添えたワークスペース（ID と名前）。送るときに、サブワークスペースとタスクを AI に渡す。
    pub attached_workspaces: Vec<(String, SharedString)>,
    /// ユーザーが今開いているワークスペース。画面が書き込む。
    pub workspace: Option<String>,
    /// 並列の会話を動かすための、裏の会話。保存も自動の処理もしない。
    worker: bool,
    /// 振り分けに使うモデル（サービスごと）。なければ今のモデル。
    coordinators: HashMap<Provider, String>,
    /// 振り分けを待っているメッセージ。
    pub routing: Vec<SharedString>,
    /// 今の会話の続きとして、答え終わるのを待っているメッセージ。
    pub queued: Vec<Queued>,
    /// 並列で動かしている会話。
    pub parallel: Vec<Parallel>,
    /// 保存してある会話。新しいものが前。
    pub chats: Vec<ChatInfo>,
    /// 今の会話を始めたタスク（ID と題）。
    pub chat_task: Option<(String, SharedString)>,
    /// 古い形で保存してある会話を、まだ新しい形に移していない。
    legacy_chat: bool,
    /// アプリが自動で始めた実行の、始まる前の会話の長さ。終わったら、ここまで戻す。
    auto_mark: Option<usize>,
    /// 自動の実行の間、よけておく会話。AI には、その実行の分だけを送る。
    held_turns: Vec<Turn>,
    auto_kind: RunKind,
    auto_tasks: Vec<(String, SharedString)>,
    auto_error: Option<SharedString>,
    /// アプリが自動で始めた実行の記録。新しいものが後ろ。
    pub runs: Vec<Run>,
    runs_dirty: bool,
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

/// モデルごとの考える深さのキー。同じ ID のモデルでも、サービスが違えば別に覚える。
fn effort_key(provider: Provider, model: &str) -> String {
    format!("{}:{model}", provider.as_str())
}

impl Session {
    fn new(db: Entity<Db>, config: AiConfig, cx: &mut Context<Self>) -> Self {
        Self::create(db, config, false, cx)
    }

    fn create(db: Entity<Db>, config: AiConfig, worker: bool, cx: &mut Context<Self>) -> Self {
        let mut error = None;
        let watch;
        let auto_run;
        let ui: SavedUi;
        let chat: Option<Value>;
        let mut chats = Vec::new();
        let mut model_names = HashMap::new();
        let mut current = None;
        let mut legacy_chat = false;
        let mut runs = Vec::new();
        let arrange;
        let track;
        let mut migrate_effort = false;
        let mut efforts_error = None;
        let (provider, effort, efforts, models, coordinators) = {
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
            let models: HashMap<Provider, String> = Provider::ALL
                .into_iter()
                .filter_map(|p| read(&setting_key(p)).map(|m| (p, m)))
                .collect();
            let model = models
                .get(&provider)
                .map(String::as_str)
                .unwrap_or_else(|| provider.default_model());
            let mut efforts: HashMap<String, Effort> = HashMap::new();
            match read(EFFORTS_SETTING) {
                Some(v) => match serde_json::from_str::<HashMap<String, String>>(&v) {
                    Ok(saved) => {
                        efforts = saved
                            .into_iter()
                            .filter_map(|(k, e)| Effort::parse(&e).map(|e| (k, e)))
                            .collect();
                    }
                    Err(e) => {
                        efforts_error =
                            Some(format!("モデルごとの考える深さを読めませんでした: {e}").into())
                    }
                },
                // 前の版で選んでいた深さは、今のモデルのものにする
                None => {
                    if let Some(e) = read(LEGACY_EFFORT_SETTING).and_then(|v| Effort::parse(&v)) {
                        if e != Effort::Default && provider.supports_effort() {
                            efforts.insert(effort_key(provider, model), e);
                        }
                        migrate_effort = true;
                    }
                }
            }
            let effort = efforts
                .get(&effort_key(provider, model))
                .copied()
                .unwrap_or(Effort::Default);
            let coordinators = Provider::ALL
                .into_iter()
                .filter_map(|p| {
                    read(&coordinator_key(p))
                        .filter(|m| !m.is_empty())
                        .map(|m| (p, m))
                })
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
            if let Some(names) =
                read(MODEL_NAMES_SETTING).and_then(|v| serde_json::from_str(&v).ok())
            {
                model_names = names;
            }
            if let Some(index) =
                read(CHATS_SETTING).and_then(|v| serde_json::from_str::<Value>(&v).ok())
            {
                current = index["current"].as_str().map(str::to_owned);
                for v in index["chats"].as_array().into_iter().flatten() {
                    if let Some(id) = v["id"].as_str() {
                        chats.push(ChatInfo {
                            id: id.to_owned(),
                            title: v["title"].as_str().unwrap_or_default().to_owned().into(),
                            task: v["task"].as_str().map(str::to_owned),
                            updated: v["updated"].as_i64().unwrap_or(0),
                        });
                    }
                }
            }
            let saved = match &current {
                Some(id) => read(&chat_key(id)),
                None => {
                    // 会話を 1 つだけ保存していたころの形
                    let old = read(CHAT_SETTING);
                    legacy_chat = old.is_some();
                    old
                }
            };
            chat = saved.and_then(|v| serde_json::from_str::<Value>(&v).ok());
            for v in read(RUNS_SETTING)
                .and_then(|v| serde_json::from_str::<Value>(&v).ok())
                .as_ref()
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                runs.push(Run {
                    id: v["id"].as_str().unwrap_or_default().to_owned(),
                    kind: match v["kind"].as_str() {
                        Some("track") => RunKind::Track,
                        Some("watch") => RunKind::Watch,
                        Some("repeat") => RunKind::Repeat,
                        _ => RunKind::Arrange,
                    },
                    tasks: v["tasks"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|t| {
                            (
                                t["id"].as_str().unwrap_or_default().to_owned(),
                                t["title"].as_str().unwrap_or_default().to_owned().into(),
                            )
                        })
                        .collect(),
                    at: v["at"].as_i64().unwrap_or(0),
                    error: v["error"].as_str().map(|e| e.to_owned().into()),
                    seen: v["seen"].as_bool().unwrap_or(true),
                    items: v["items"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(item_from)
                        .collect(),
                });
            }
            auto_run = read("ai.auto_run").as_deref() == Some("1");
            arrange = read("ai.arrange").as_deref() != Some("0");
            track = read("ai.track").as_deref() != Some("0");
            let defaults = Watch::default();
            watch = Watch {
                enabled: read("ai.watch.enabled").as_deref() != Some("0"),
                minutes: read("ai.watch.minutes")
                    .and_then(|v| v.parse().ok())
                    .filter(|m| WATCH_MINUTES.contains(m))
                    .unwrap_or(defaults.minutes),
                instruction: read("ai.watch.instruction")
                    .filter(|v| !v.trim().is_empty())
                    .unwrap_or(defaults.instruction),
            };
            (provider, effort, efforts, models, coordinators)
        };
        if efforts_error.is_some() {
            error = efforts_error;
        }
        let mut this = Self {
            db,
            config,
            provider,
            models,
            effort,
            efforts,
            key: KeyState::Unknown,
            github: GithubAccount::Unknown,
            github_read: 0,
            listed: Vec::new(),
            model_names,
            catalog: None,
            turns: Vec::new(),
            items: Vec::new(),
            busy: false,
            pending: None,
            shell: Vec::new(),
            shell_rev: 0,
            shell_dropped: 0,
            ansi: ansi::State::default(),
            cwd: dirs::home_dir()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| ".".into()),
            history: Vec::new(),
            running: None,
            own_command: false,
            allow_shell: false,
            turn_shell: Shell::standard(),
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
            watch_epoch: (unix_now(), cx.background_executor().now()),
            auto: false,
            arrange,
            arranging: Vec::new(),
            arrange_notes: HashMap::new(),
            track,
            tracking: Vec::new(),
            track_notes: HashMap::new(),
            tracked: HashMap::new(),
            track_waiting: Vec::new(),
            comment_mark: None,
            track_timer: None,
            track_retry: false,
            known: None,
            holding: Vec::new(),
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
            attached: Vec::new(),
            attached_workspaces: Vec::new(),
            workspace: None,
            worker,
            coordinators,
            routing: Vec::new(),
            queued: Vec::new(),
            parallel: Vec::new(),
            chats,
            chat_task: None,
            legacy_chat,
            auto_mark: None,
            held_turns: Vec::new(),
            auto_kind: RunKind::Arrange,
            auto_tasks: Vec::new(),
            auto_error: None,
            runs,
            runs_dirty: false,
            _subscriptions: Vec::new(),
        };
        if worker {
            return this;
        }
        if migrate_effort {
            this.migrate_effort(cx);
        }
        this._subscriptions = vec![
            cx.observe(&this.db.clone(), |this, _, cx| {
                this.tasks_changed(cx);
                this.comments_changed(cx);
                this.repeat_jobs(cx);
            }),
            cx.observe_self(|this, cx| this.persist(cx)),
            cx.observe_self(|this, cx| {
                if this.arrange_retry && !this.busy {
                    this.arrange_retry = false;
                    this.tasks_changed(cx);
                }
            }),
            cx.observe_self(|this, cx| {
                // 答え終わったら、続きとして待っているメッセージを送る
                if !this.busy && !this.queued.is_empty() {
                    let mut next = this.queued.remove(0);
                    next.attached.append(&mut this.attached);
                    this.attached = next.attached;
                    next.attached_workspaces
                        .append(&mut this.attached_workspaces);
                    this.attached_workspaces = next.attached_workspaces;
                    if !this.send(&next.text, next.place, cx) && this.error.is_none() {
                        this.error = Some(
                            format!("「{}」を送れませんでした", clip_chars(&next.text, 20)).into(),
                        );
                    }
                }
            }),
            cx.observe_self(|this, cx| {
                if this.track_retry && !this.busy {
                    this.track_retry = false;
                    this.track_flush(cx);
                }
            }),
            cx.observe_self(|this, cx| this.repeat_jobs(cx)),
        ];
        if let Some(chat) = chat {
            this.load_chat(&chat);
        }
        if let Some(id) = current {
            this.conversation = id;
        }
        this.chat_task = this.linked_task();
        this.saved_ui = this.ui_json();
        this.saved_chat = this.chat_mark();
        this.restart_watch(cx);
        // 今あるタスクは、もう見たものにする
        this.tasks_changed(cx);
        this.comments_changed(cx);
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
        if !self.busy && mark != self.saved_chat && self.save_chat(cx) {
            self.saved_chat = mark;
        }
        if self.runs_dirty {
            let runs: Vec<Value> = self
                .runs
                .iter()
                .map(|run| {
                    serde_json::json!({
                        "id": run.id,
                        "kind": run.kind.as_str(),
                        "tasks": run.tasks.iter().map(|(id, title)| {
                            serde_json::json!({"id": id, "title": title})
                        }).collect::<Vec<_>>(),
                        "at": run.at,
                        "error": run.error,
                        "seen": run.seen,
                        "items": run.items.iter().map(item_json).collect::<Vec<_>>(),
                    })
                })
                .collect();
            if self.save_setting(RUNS_SETTING, &Value::Array(runs).to_string(), cx) {
                self.runs_dirty = false;
            }
        }
    }

    /// 今の会話を始めたタスク。一覧に書いてあるものから。
    fn linked_task(&self) -> Option<(String, SharedString)> {
        let info = self.chats.iter().find(|c| c.id == self.conversation)?;
        Some((info.task.clone()?, info.title.clone()))
    }

    fn remove_setting(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let result = self.db.update(cx, |db, _| db.store.set_setting(key, None));
        match result {
            Ok(()) => true,
            Err(e) => {
                self.error = Some(format!("AI の会話を消せませんでした: {e}").into());
                cx.notify();
                false
            }
        }
    }

    fn save_index(&mut self, cx: &mut Context<Self>) -> bool {
        let chats: Vec<Value> = self
            .chats
            .iter()
            .map(|c| {
                serde_json::json!({
                    "id": c.id, "title": c.title, "task": c.task, "updated": c.updated,
                })
            })
            .collect();
        let index = serde_json::json!({"current": self.conversation, "chats": chats}).to_string();
        self.save_setting(CHATS_SETTING, &index, cx)
    }

    /// 今の会話を保存し、一覧のいちばん前に置く。空の会話は一覧に出さない。
    fn save_chat(&mut self, cx: &mut Context<Self>) -> bool {
        if self.items.is_empty() {
            return self.save_index(cx);
        }
        let id = self.conversation.clone();
        let chat = self.chat_json();
        if !self.save_setting(&chat_key(&id), &chat, cx) {
            return false;
        }
        let title: SharedString = match &self.chat_task {
            Some((_, title)) => title.clone(),
            None => self
                .items
                .iter()
                .find_map(|item| match item {
                    Item::User(text) => Some(clip_chars(
                        text.lines().next().unwrap_or_default(),
                        MAX_TITLE_CHARS,
                    )),
                    _ => None,
                })
                .unwrap_or_else(|| "新しいチャット".to_owned())
                .into(),
        };
        self.chats.retain(|c| c.id != id);
        self.chats.insert(
            0,
            ChatInfo {
                id,
                title,
                task: self.chat_task.as_ref().map(|(id, _)| id.clone()),
                updated: malakt_core::now_ms(),
            },
        );
        if !self.trim_chats(cx) {
            return false;
        }
        if self.legacy_chat && self.remove_setting(CHAT_SETTING, cx) {
            self.legacy_chat = false;
        }
        self.save_index(cx)
    }

    /// 古い会話から消すので、増え続けない。
    fn trim_chats(&mut self, cx: &mut Context<Self>) -> bool {
        while self.chats.len() > MAX_CHATS {
            let last = self.chats[self.chats.len() - 1].id.clone();
            if !self.remove_setting(&chat_key(&last), cx) {
                return false;
            }
            self.chats.pop();
        }
        true
    }

    /// 振り分けのモデルを設定してあるか。
    pub fn coordinator_set(&self) -> bool {
        self.coordinators.contains_key(&self.provider)
    }

    /// 振り分けに使うモデル。設定していなければ、今のモデル。
    pub fn coordinator_model(&self) -> String {
        self.coordinators
            .get(&self.provider)
            .cloned()
            .unwrap_or_else(|| self.model().to_owned())
    }

    /// 振り分けに使うモデルを設定する。`None` なら、今のモデルを使う。
    pub fn set_coordinator(&mut self, model: Option<&str>, cx: &mut Context<Self>) {
        let key = coordinator_key(self.provider);
        if !self.save_setting(&key, model.unwrap_or_default(), cx) {
            return;
        }
        match model {
            Some(model) => self.coordinators.insert(self.provider, model.to_owned()),
            None => self.coordinators.remove(&self.provider),
        };
        cx.notify();
    }

    /// 考え中に送られたメッセージを、今の会話の続きにするか、並列の別の会話にするか、AI に決めさせる。
    fn route(&mut self, queued: Queued, cx: &mut Context<Self>) {
        let text: SharedString = queued.text.clone().into();
        self.routing.push(text.clone());
        // 今の会話の終わりのほうを、振り分けの手がかりに渡す
        let mut recent: Vec<String> = self
            .shown_items()
            .iter()
            .rev()
            .filter_map(|item| match item {
                Item::User(t) => Some(format!("ユーザー: {}", clip_chars(t, 300))),
                Item::Assistant(t) => Some(format!("AI: {}", clip_chars(t, 300))),
                _ => None,
            })
            .take(6)
            .collect();
        recent.reverse();
        let (provider, model, config) =
            (self.provider, self.coordinator_model(), self.config.clone());
        cx.spawn(async move |this, cx| {
            let result = Self::route_run(
                &this,
                &config,
                provider,
                &model,
                &recent.join("\n"),
                &queued.text,
                cx,
            )
            .await;
            // チャットがもうなければ、送る先もない
            let _ = this.update(cx, |this, cx| {
                if let Some(at) = this.routing.iter().position(|t| *t == text) {
                    this.routing.remove(at);
                }
                match result {
                    Ok(true) if this.running_parallel() < MAX_PARALLEL => {
                        this.start_parallel(queued, cx)
                    }
                    Ok(_) => this.queued.push(queued),
                    Err(error) => {
                        this.error = Some(
                            format!("振り分けられなかったので、続きとして送ります: {error}").into(),
                        );
                        this.queued.push(queued);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 振り分け役に聞く。別の会話にするなら `true`。
    async fn route_run(
        this: &WeakEntity<Self>,
        config: &AiConfig,
        provider: Provider,
        model: &str,
        recent: &str,
        text: &str,
        cx: &mut AsyncApp,
    ) -> Result<bool, String> {
        let found = {
            let config = config.clone();
            cx.background_executor()
                .spawn(async move { keys::lookup(&*config.keys, provider, |n| (config.env)(n)) })
                .await?
        };
        let Some((key, _)) = found else {
            return Err("API キーが設定されていません".into());
        };
        let turns = [Turn::User(format!(
            "今の会話:\n{recent}\n\n次のメッセージ:\n{text}"
        ))];
        let request = wire::Request {
            model,
            effort: Effort::Default,
            system: ROUTE_PROMPT,
            turns: &turns,
            tools: &[],
            fallbacks: false,
            prefix_ttl: None,
        };
        let conversation = malakt_core::new_id();
        let (_, reply) =
            Self::request(this, config, provider, &key, &conversation, request, cx).await?;
        if let Some(notice) = reply.notice {
            return Err(notice);
        }
        Ok(reply.text.to_uppercase().contains("NEW"))
    }

    /// 動いている並列の会話の数。
    pub fn running_parallel(&self) -> usize {
        self.parallel.iter().filter(|p| !p.done).count()
    }

    /// 別の会話を裏で始める。終わったら、会話の一覧に足す。
    fn start_parallel(&mut self, queued: Queued, cx: &mut Context<Self>) {
        let (db, config) = (self.db.clone(), self.config.clone());
        let copy = (
            self.provider,
            self.models.clone(),
            self.effort,
            self.efforts.clone(),
            self.key.clone(),
            self.github.clone(),
            self.workspace.clone(),
            self.catalog.clone(),
            self.model_names.clone(),
        );
        let worker = cx.new(|cx| {
            let mut w = Session::create(db, config, true, cx);
            (
                w.provider,
                w.models,
                w.effort,
                w.efforts,
                w.key,
                w.github,
                w.workspace,
                w.catalog,
                w.model_names,
            ) = copy;
            w.attached = queued.attached.clone();
            w.attached_workspaces = queued.attached_workspaces.clone();
            w
        });
        let sent = worker.update(cx, |w, cx| w.send(&queued.text, queued.place.clone(), cx));
        if !sent {
            let why = worker.read(cx).error.clone();
            self.error = Some(
                format!(
                    "「{}」を並列で送れませんでした{}",
                    clip_chars(&queued.text, 20),
                    why.map(|e| format!(": {e}")).unwrap_or_default()
                )
                .into(),
            );
            return;
        }
        let watch = cx.observe(&worker, |this, worker, cx| {
            this.parallel_changed(&worker, cx)
        });
        self.parallel.push(Parallel {
            session: worker,
            title: clip_chars(
                queued.text.lines().next().unwrap_or_default(),
                MAX_TITLE_CHARS,
            )
            .into(),
            done: false,
            error: None,
            _watch: watch,
        });
    }

    /// 並列の会話が答え終わったら、会話の一覧に足す。
    fn parallel_changed(&mut self, worker: &Entity<Session>, cx: &mut Context<Self>) {
        let Some(ix) = self
            .parallel
            .iter()
            .position(|p| p.session == *worker && !p.done)
        else {
            return;
        };
        let w = worker.read(cx);
        if w.busy {
            return;
        }
        let (id, json) = (w.conversation.clone(), w.chat_json());
        let error = w.items.iter().rev().find_map(|item| match item {
            Item::Error(e) => Some(e.clone()),
            _ => None,
        });
        let title = self.parallel[ix].title.clone();
        if self.save_setting(&chat_key(&id), &json, cx) {
            self.chats.retain(|c| c.id != id);
            self.chats.insert(
                0,
                ChatInfo {
                    id,
                    title,
                    task: None,
                    updated: malakt_core::now_ms(),
                },
            );
            if self.trim_chats(cx) {
                self.save_index(cx);
            }
        }
        let p = &mut self.parallel[ix];
        p.done = true;
        p.error = error;
        cx.notify();
    }

    /// 終わった並列の会話を開く。
    pub fn open_parallel(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(p) = self.parallel.get(ix).filter(|p| p.done) else {
            return;
        };
        let id = p.session.read(cx).conversation.clone();
        self.parallel.remove(ix);
        self.open_chat(&id, cx);
    }

    /// 並列の会話の表示を消す。動いている途中なら止める。
    pub fn dismiss_parallel(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.parallel.len() {
            let p = self.parallel.remove(ix);
            if !p.done {
                p.session.update(cx, |s, cx| s.stop(cx));
            }
            cx.notify();
        }
    }

    /// 続きとして待っているメッセージを取り消す。
    pub fn unqueue(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.queued.len() {
            self.queued.remove(ix);
            cx.notify();
        }
    }

    /// 今の会話の ID。
    pub fn chat_id(&self) -> &str {
        &self.conversation
    }

    /// 会話に出すもの。アプリが自動で始めた実行は、会話に混ぜない。
    pub fn shown_items(&self) -> &[Item] {
        let end = self
            .auto_mark
            .map_or(self.items.len(), |items| items.min(self.items.len()));
        &self.items[..end]
    }

    /// 今動いている、アプリが自動で始めた実行。
    pub fn auto_running(&self) -> Option<(RunKind, &[(String, SharedString)])> {
        self.auto_mark
            .map(|_| (self.auto_kind, self.auto_tasks.as_slice()))
    }

    /// あるタスクについての実行の記録。新しいものが前。
    pub fn runs_for(&self, task: &str) -> Vec<&Run> {
        self.runs
            .iter()
            .rev()
            .filter(|run| run.tasks.iter().any(|(id, _)| id == task))
            .collect()
    }

    /// 失敗を見たことにして、チャットの上から外す。
    pub fn dismiss_run(&mut self, id: &str, cx: &mut Context<Self>) {
        for run in &mut self.runs {
            if run.id == id && !run.seen {
                run.seen = true;
                self.runs_dirty = true;
            }
        }
        cx.notify();
    }

    /// 失敗した実行を、もう一度行う。
    pub fn retry_run(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(run) = self.runs.iter().find(|run| run.id == id).cloned() else {
            return;
        };
        self.dismiss_run(id, cx);
        match run.kind {
            RunKind::Arrange => {
                for (task, _) in &run.tasks {
                    self.retry_arrange(task, cx);
                }
            }
            RunKind::Track => {
                for (task, _) in &run.tasks {
                    self.retry_track(task, cx);
                }
            }
            RunKind::Watch => {
                self.check_github(cx);
            }
            RunKind::Repeat => {
                for (task, title) in &run.tasks {
                    // 今まさに実行している指示は、そのまま。止めたあとに勝手に動き出さない
                    let running = self.busy
                        && self.auto
                        && self.auto_kind == RunKind::Repeat
                        && self.auto_tasks.iter().any(|(id, _)| id == task);
                    if running {
                        continue;
                    }
                    match self.db.update(cx, |db, _| db.store.retry_repeat_ai(task)) {
                        Ok(true) => {}
                        Ok(false) => {
                            self.error = Some(
                                format!("「{title}」の AI への指示は、もう残っていません").into(),
                            )
                        }
                        Err(e) => {
                            self.error = Some(
                                format!("「{title}」の AI への指示を読めませんでした: {e}").into(),
                            )
                        }
                    }
                }
                self.repeat_jobs(cx);
            }
        }
    }

    fn begin_auto(&mut self, kind: RunKind, tasks: Vec<(String, SharedString)>) {
        self.busy = true;
        self.auto = true;
        self.auto_mark = Some(self.items.len());
        self.held_turns = std::mem::take(&mut self.turns);
        self.auto_kind = kind;
        self.auto_tasks = tasks;
        self.auto_error = None;
    }

    /// 自動の実行で増えた分を会話から外し、記録に移す。
    fn end_auto(&mut self) {
        let Some(items) = self.auto_mark.take() else {
            return;
        };
        let made = self.items.split_off(items.min(self.items.len()));
        self.turns = std::mem::take(&mut self.held_turns);
        let tasks = std::mem::take(&mut self.auto_tasks);
        let error = self.auto_error.take();
        let items: Vec<Item> = made
            .into_iter()
            .filter(|item| !matches!(item, Item::Notice(_)))
            .map(|item| match item {
                Item::Assistant(t) => Item::Assistant(clip_chars(&t, MAX_RUN_CHARS).into()),
                Item::Error(t) => Item::Error(clip_chars(&t, MAX_RUN_CHARS).into()),
                Item::Tool {
                    summary,
                    state: ToolState::Running,
                } => Item::Tool {
                    summary,
                    state: ToolState::Failed("中断しました".into()),
                },
                other => other,
            })
            .collect();
        // 何も起きなかった定期チェックは、記録しない
        if items.is_empty() && error.is_none() && tasks.is_empty() {
            return;
        }
        let skip = items.len().saturating_sub(MAX_RUN_ITEMS);
        self.runs.push(Run {
            id: malakt_core::new_id(),
            kind: self.auto_kind,
            tasks,
            at: malakt_core::now_ms(),
            seen: error.is_none(),
            error,
            items: items[skip..].to_vec(),
        });
        let over = self.runs.len().saturating_sub(MAX_RUNS);
        self.runs.drain(..over);
        self.runs_dirty = true;
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
        let items: Vec<Value> = self.items[skip..].iter().map(item_json).collect();
        serde_json::json!({"id": self.conversation, "items": items, "turns": turns}).to_string()
    }

    fn load_chat(&mut self, chat: &Value) {
        let text = |v: &Value| v["t"].as_str().unwrap_or_default().to_owned();
        for v in chat["items"].as_array().into_iter().flatten() {
            self.items.push(item_from(v));
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

    /// この実行で使うルール。グローバルのものは最初の指示に、ワークスペースのものはメッセージに添える。
    /// チャットでは今開いているワークスペース、自動の処理では扱うタスクのワークスペースのルールを使う。
    fn rules(&self, cx: &App) -> Result<(String, String), String> {
        let store = &self.db.read(cx).store;
        let fail = |e: malakt_core::Error| format!("AI のルールを読めませんでした: {e}");
        let mut workspaces: Vec<String> = Vec::new();
        if self.auto {
            for (id, _) in &self.auto_tasks {
                // 消えたタスクは扱わないので、そのワークスペースのルールも要らない
                if let Ok(task) = store.task(id) {
                    workspaces.extend(task.workspace_id);
                }
            }
        } else {
            workspaces.extend(self.workspace.clone());
        }
        let ids: Vec<&str> = workspaces.iter().map(String::as_str).collect();
        Ok((
            tools::global_rules(store, self.auto).map_err(fail)?,
            tools::workspace_rules(store, &ids, self.auto).map_err(fail)?,
        ))
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

    /// 追加したばかりのタスクを、ユーザーがタイトルを書き終えるまで整えずに待つ。
    pub fn hold(&mut self, id: String) {
        if !self.holding.contains(&id) {
            self.holding.push(id);
        }
    }

    /// タイトルを書き終えたタスクを、整える対象に戻す。
    pub fn release(&mut self, id: &str, cx: &mut Context<Self>) {
        let before = self.holding.len();
        self.holding.retain(|held| held != id);
        if before != self.holding.len() {
            self.tasks_changed(cx);
        }
    }

    /// まだ見ていないタスクと、新しくリンクが書かれたタスク。
    fn new_tasks(&self, all: Vec<(NewTask, bool)>) -> Vec<NewTask> {
        let Some(known) = &self.known else {
            return Vec::new();
        };
        all.into_iter()
            .filter(|(task, open)| {
                *open
                    && !self.holding.contains(&task.id)
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
            // タイトルを書いている途中のタスクは、書き終わってから整える
            if !waiting.contains(&task.id) && !self.holding.contains(&task.id) {
                known.insert(task.id, task.links.iter().map(Fetch::url).collect());
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
                    .map(|(task, _)| (task.id, task.links.iter().map(Fetch::url).collect()))
                    .collect(),
            );
            return;
        };
        // 消えたタスクの分は忘れるので、増え続けない
        known.retain(|id, _| all.iter().any(|(task, _)| task.id == *id));
        // 繰り返しで作った回は、前の回のリンクを写しただけなので、もう見たものにする
        for (task, _) in all.iter().filter(|(task, _)| task.copied) {
            known
                .entry(task.id.clone())
                .or_insert_with(|| task.links.iter().map(Fetch::url).collect());
        }
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
        self.begin_auto(
            RunKind::Arrange,
            tasks
                .iter()
                .map(|task| (task.id.clone(), task.title.clone().into()))
                .collect(),
        );
        self.arranging = tasks.iter().map(|task| task.id.clone()).collect();
        if let Some(known) = &mut self.known {
            for task in &tasks {
                known.insert(task.id.clone(), task.links.iter().map(Fetch::url).collect());
            }
        }
        let ids = self.arranging.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = Self::arrange_run(&this, tasks, cx).await;
            // チャットがなくなっていれば、知らせる先もない
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(notes) => {
                        if !notes.is_empty() {
                            let all: Vec<&str> = notes.iter().map(|(_, n)| n.as_ref()).collect();
                            this.auto_error = Some(all.join("\n").into());
                        }
                        this.arrange_notes.extend(notes)
                    }
                    Err(error) => {
                        let note: SharedString =
                            format!("AI がタスクを整えられませんでした: {error}").into();
                        this.auto_error = Some(note.clone());
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
             - 説明には、リンクの URL と要点を書く。タスクの URL の欄にあるリンクは、説明に URL を書き写さず、要点だけを書く。ユーザーが書いた文は消さない。\n\
             - 中身から分かる範囲で、優先度や期限を入れる。期限の時刻が分かるときは、時刻も入れる。\n\
             - 直すところがなければ、何も変えない。\n\
             - タスクを新しく作らない。消さない。\n\
             - タイトルと説明には、タスクの中身だけを書く。読めなかったリンクや、確かめられなかったことについての注記を書かない。\n",
        );
        let mut any = false;
        for task in &tasks {
            let mut section = format!("\n## タスク（id: {}）\nタイトル: {}\n", task.id, task.title);
            if !task.urls.is_empty() {
                section.push_str(&format!("URL: {}\n", task.urls.join(" ")));
            }
            section.push_str(&format!(
                "説明: {}\n",
                if task.description.trim().is_empty() {
                    "（なし）"
                } else {
                    task.description.as_str()
                }
            ));
            let mut read = 0;
            for link in task.links.iter().take(MAX_ARRANGE_LINKS) {
                let url = link.url();
                match Self::fetch(this, &config, link, cx).await {
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

    // ---- コメントからタスクを更新する ----

    pub fn set_track(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.track == on || !self.save_setting("ai.track", if on { "1" } else { "0" }, cx) {
            return;
        }
        self.track = on;
        // オンにしたときは、今あるコメントをもう見たものにして、次に書かれたものから始める
        self.comment_mark = None;
        self.track_waiting.clear();
        self.track_timer = None;
        self.track_notes.clear();
        self.comments_changed(cx);
        cx.notify();
    }

    /// AI がコメントから変えて、今もその値のままの項目（「0% → 60%・進行中」など）。
    /// 元に戻せるものがなければ `None`。
    pub fn tracked_for(&self, task: &malakt_core::Task) -> Option<String> {
        let tracked = self.tracked.get(&task.id)?;
        tracked_label(&tracked.before, &tracked.after, Some(task))
    }

    /// AI が `update_task` で変えた直後に、変える前の値を控える。
    fn record_tracked(&mut self, before: malakt_core::Task, cx: &App) {
        let Ok(after) = self.db.read(cx).store.task(&before.id) else {
            return;
        };
        // 続けて更新したときは、最初の値まで戻せるようにする
        let before = match self.tracked.remove(&before.id) {
            Some(old) if old.after.updated_at == before.updated_at => old.before,
            _ => before,
        };
        if let Some(label) = tracked_label(&before, &after, None) {
            let label = label.into();
            self.tracked.insert(
                after.id.clone(),
                Tracked {
                    before,
                    after,
                    label,
                },
            );
        }
        // 覚えておく数には上限を付け、古い更新から忘れる
        while self.tracked.len() > MAX_TRACK_UNDO {
            let oldest = self
                .tracked
                .iter()
                .min_by_key(|(_, t)| t.after.updated_at)
                .map(|(id, _)| id.clone());
            match oldest {
                Some(id) => self.tracked.remove(&id),
                None => break,
            };
        }
    }

    /// 更新できなかったタスクを、もう一度コメントから更新する。
    pub fn retry_track(&mut self, id: &str, cx: &mut Context<Self>) {
        self.track_notes.remove(id);
        if !self.track_waiting.iter().any(|t| t == id) {
            self.track_waiting.push(id.to_owned());
        }
        self.track_timer = None;
        self.track_flush(cx);
        cx.notify();
    }

    /// AI がコメントから変えたところを、前の値に戻す。そのあとにユーザーが変えたところは残す。
    pub fn undo_track(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(tracked) = self.tracked.remove(id) else {
            return;
        };
        let (before, after) = (&tracked.before, &tracked.after);
        let result = self.db.update(cx, |db, cx| {
            let now = db.store.task(id)?;
            let mut draft = malakt_core::TaskDraft::from(&now);
            if now.status == after.status {
                draft.status = before.status;
            }
            if now.percent == after.percent {
                draft.percent = before.percent;
            }
            if now.due == after.due {
                draft.due = before.due.clone();
            }
            if now.start == after.start {
                draft.start = before.start.clone();
            }
            if now.priority == after.priority {
                draft.priority = before.priority;
            }
            let task = db.store.update_task(id, &draft);
            cx.notify();
            task
        });
        if let Err(e) = result {
            self.error = Some(format!("元に戻せませんでした: {e}").into());
            self.tracked.insert(id.to_owned(), tracked);
        }
        cx.notify();
    }

    /// 新しく書かれた・書き直されたコメントを見つけたら、少し待ってから AI にタスクを更新させる。
    fn comments_changed(&mut self, cx: &mut Context<Self>) {
        if !self.track {
            self.comment_mark = None;
            return;
        }
        let db = self.db.read(cx);
        if !db.unlocked {
            return;
        }
        let Some((mark, seen)) = &self.comment_mark else {
            // 今あるコメントは、もう見たものにする
            self.comment_mark = comment_mark(&db.store);
            return;
        };
        let Ok(mut changed) = db.store.comments_changed_since(*mark) else {
            return;
        };
        changed.retain(|(id, _, at)| at > mark || !seen.contains(id));
        let Some(latest) = changed.iter().map(|(_, _, at)| *at).max() else {
            return;
        };
        let mut seen = if latest == *mark {
            seen.clone()
        } else {
            Vec::new()
        };
        for (id, task, at) in changed {
            if at == latest {
                seen.push(id);
            }
            if !self.track_waiting.contains(&task) {
                self.track_waiting.push(task);
            }
        }
        self.comment_mark = Some((latest, seen));
        // 続けて書いている間は待ち、手が止まってから 1 回だけ送る
        self.track_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TRACK_DELAY).await;
            // チャットがなくなっていれば、更新する先もない
            let _ = this.update(cx, |this, cx| this.track_flush(cx));
        }));
    }

    /// 待っているタスクを AI に渡す。API キーがなければ通信しない。
    fn track_flush(&mut self, cx: &mut Context<Self>) {
        if self.track_waiting.is_empty() {
            return;
        }
        if self.busy {
            self.track_retry = true;
            return;
        }
        match &self.key {
            // 調べ終わったら、もう一度ここに来る
            KeyState::Unknown => self.refresh_key(cx),
            KeyState::Missing | KeyState::Failed(_) => self.track_waiting.clear(),
            KeyState::Present { .. } => {
                let take = self.track_waiting.len().min(MAX_TRACK_TASKS);
                let ids: Vec<String> = self.track_waiting.drain(..take).collect();
                self.track_retry = !self.track_waiting.is_empty();
                self.track_tasks(ids, cx);
            }
        }
    }

    fn track_tasks(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        // 未完了のタスクだけを見る。消えたタスクは飛ばす
        let tasks: Vec<(malakt_core::Task, Vec<malakt_core::Comment>)> = {
            let store = &self.db.read(cx).store;
            ids.iter()
                .filter_map(|id| {
                    let task = store.task(id).ok().filter(|t| t.status.is_open())?;
                    let comments = store.comments(id).ok().filter(|c| !c.is_empty())?;
                    Some((task, comments))
                })
                .collect()
        };
        if tasks.is_empty() {
            return;
        }
        self.begin_auto(
            RunKind::Track,
            tasks
                .iter()
                .map(|(task, _)| (task.id.clone(), task.title.clone().into()))
                .collect(),
        );
        self.tracking = tasks.iter().map(|(task, _)| task.id.clone()).collect();
        for id in &self.tracking {
            self.track_notes.remove(id);
        }
        let ids = self.tracking.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = Self::track_run(&this, tasks, cx).await;
            // チャットがなくなっていれば、知らせる先もない
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    let note: SharedString =
                        format!("AI がコメントからタスクを更新できませんでした: {error}").into();
                    this.auto_error = Some(note.clone());
                    this.items.push(Item::Error(note.clone()));
                    for id in ids {
                        this.track_notes.insert(id, note.clone());
                    }
                }
                this.finish();
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// コメントを AI に渡し、進捗などを更新させる。
    async fn track_run(
        this: &WeakEntity<Self>,
        tasks: Vec<(malakt_core::Task, Vec<malakt_core::Comment>)>,
        cx: &mut AsyncApp,
    ) -> Result<(), String> {
        let (config, place) = this
            .read_with(cx, |s, _| (s.config.clone(), s.place.clone()))
            .map_err(gone)?;
        let titles: Vec<String> = tasks
            .iter()
            .map(|(t, _)| format!("「{}」", t.title))
            .collect();
        let mut message = String::from(
            "（アプリからの自動のメッセージです）\n\
             次のタスクにコメントが書かれました。コメントから読み取れることを、それぞれのタスクに update_task で反映してください。\n\
             - percent（進捗率、0〜100）を、説明とコメントの流れから見積もる。作業の何割が終わったかが分かるときだけ変える。\n\
             - 着手したと読み取れれば status を in_process に、終わったと読み取れれば completed に、やめたと読み取れれば cancelled にする。\n\
             - 期限や開始日、優先度が変わったと読み取れれば、due、start、priority を変える。\n\
             - 読み取れることがなければ、何も変えない。推測で変えない。\n\
             - タイトルと説明は変えない。コメントを書かない。タスクを作らない。消さない。\n\
             - ユーザーに聞き返さない。\n",
        );
        for (task, comments) in &tasks {
            message.push_str(&format!(
                "\n## タスク（id: {}）\nタイトル: {}\n状態: {}\n進捗: {}%\n期限: {}\n説明: {}\n### コメント（古い順）\n",
                task.id,
                task.title,
                task.status.as_str(),
                task.percent,
                task.due.as_deref().unwrap_or("（なし）"),
                if task.description.trim().is_empty() {
                    "（なし）".to_owned()
                } else {
                    clip_tail(&task.description, MAX_TRACK_CHARS)
                }
            ));
            let mut ordered: Vec<&malakt_core::Comment> = comments.iter().collect();
            ordered.sort_by_key(|c| c.created_at);
            let skip = ordered.len().saturating_sub(MAX_TRACK_COMMENTS);
            let mut text = String::new();
            for comment in &ordered[skip..] {
                text.push_str(&format!(
                    "- {}: {}\n",
                    malakt_core::view::format_datetime(comment.updated_at),
                    comment.body
                ));
            }
            message.push_str(&clip_tail(&text, MAX_TRACK_CHARS));
        }
        this.update(cx, |s, cx| {
            s.items.push(Item::Notice(
                format!(
                    "{} のコメントから、AI がタスクを更新します。",
                    titles.join("、")
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
            &format!("コメントからの更新（アプリが自動で始めました）。{place}"),
            cx,
        )
        .await
    }

    // ---- ワークスペースのアイコンを選ぶ ----

    /// 名前を付けたばかりのワークスペースのアイコンを、AI に選ばせる。チャットには出さない。
    /// API キーがなければ通信せず、アイコンは最初のままにする。
    pub fn suggest_icon(&mut self, workspace: String, name: String, cx: &mut Context<Self>) {
        // Cursor は CLI をシェルで動かすので、チャットの外からは使わない
        if !self.db.read(cx).unlocked || self.provider == Provider::Cursor {
            return;
        }
        let config = self.config.clone();
        cx.spawn(async move |this, cx| {
            let result = Self::icon_run(&this, &config, &workspace, &name, cx).await;
            // チャットがなくなっていれば、知らせる先もない
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.items.push(Item::Error(
                        format!("「{name}」のアイコンを AI が選べませんでした: {error}").into(),
                    ));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    async fn icon_run(
        this: &WeakEntity<Self>,
        config: &AiConfig,
        workspace: &str,
        name: &str,
        cx: &mut AsyncApp,
    ) -> Result<(), String> {
        let (provider, model) = this
            .read_with(cx, |s, _| (s.provider, s.model().to_owned()))
            .map_err(gone)?;
        let found = {
            let config = config.clone();
            cx.background_executor()
                .spawn(async move { keys::lookup(&*config.keys, provider, |n| (config.env)(n)) })
                .await?
        };
        let Some((key, _)) = found else {
            return Ok(());
        };
        let turns = [Turn::User(format!("ワークスペースの名前: {name}"))];
        let request = wire::Request {
            model: &model,
            effort: Effort::Default,
            system: ICON_PROMPT,
            turns: &turns,
            tools: &[],
            fallbacks: false,
            prefix_ttl: None,
        };
        let conversation = malakt_core::new_id();
        let (_, reply) =
            Self::request(this, config, provider, &key, &conversation, request, cx).await?;
        if let Some(notice) = reply.notice {
            return Err(notice);
        }
        let words: Vec<&str> = reply
            .text
            .split([',', '、', '\n'])
            .map(str::trim)
            .filter(|word| !word.is_empty())
            .collect();
        let Some(icon) = crate::icons::pick(&words) else {
            return Err(format!(
                "AI の答え「{}」に合うアイコンがありません",
                reply.text.trim()
            ));
        };
        this.update(cx, |s, cx| {
            s.db.update(cx, |db, cx| {
                // 待っている間に消えた、名前が変わった、ユーザーがアイコンを選んだ、のどれかなら変えない
                let Ok(current) = db.store.workspace(workspace) else {
                    return Ok(());
                };
                if current.name != name || current.icon.is_some() {
                    return Ok(());
                }
                db.store
                    .set_workspace_icon(workspace, Some(icon), current.color.as_deref())
                    .map_err(|e| e.to_string())?;
                cx.notify();
                Ok(())
            })
        })
        .map_err(gone)?
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

    // ---- 繰り返しのタスクの、AI への指示 ----

    /// 繰り返しで作った回に AI への指示があれば、手が空いているときに 1 件ずつ実行する。
    /// ロック中も動く。API キーがなければ通信せず、失敗として記録に残す（やり直せる）。
    fn repeat_jobs(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let waiting = match self.db.read(cx).store.repeat_ai_jobs() {
            Ok(jobs) => jobs.iter().any(|job| !job.failed),
            Err(e) => {
                let text: SharedString = format!("AI に頼むことを読めませんでした: {e}").into();
                if self.error.as_ref() != Some(&text) {
                    self.error = Some(text);
                    cx.notify();
                }
                return;
            }
        };
        if !waiting {
            return;
        }
        if self.key == KeyState::Unknown {
            // 調べ終わったら、もう一度ここに来る
            self.refresh_key(cx);
            return;
        }
        let job = match self.db.update(cx, |db, _| db.store.take_repeat_ai()) {
            Ok(Some(job)) => job,
            Ok(None) => return,
            Err(e) => {
                self.error = Some(format!("AI に頼むことを読めませんでした: {e}").into());
                cx.notify();
                return;
            }
        };
        self.begin_auto(
            RunKind::Repeat,
            vec![(job.task_id.clone(), job.title.clone().into())],
        );
        let missing = match &self.key {
            KeyState::Present { .. } => None,
            KeyState::Failed(e) => Some(format!("API キーを読めませんでした: {e}")),
            _ => Some(format!(
                "{} の API キーが設定されていないので、AI への指示を実行できませんでした",
                self.provider.label()
            )),
        };
        if let Some(error) = missing {
            self.repeat_failed(&job, error, cx);
            self.finish();
            cx.notify();
            return;
        }
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = Self::repeat_run(&this, &job, cx).await;
            // チャットがなくなっていれば、知らせる先もない
            let _ = this.update(cx, |this, cx| {
                match result {
                    Err(error) => this.repeat_failed(&job, error, cx),
                    Ok(()) => {
                        if let Err(e) = this
                            .db
                            .update(cx, |db, _| db.store.finish_repeat_ai(&job.task_id))
                        {
                            this.error = Some(
                                format!(
                                    "「{}」の実行済みの AI への指示を消せませんでした: {e}",
                                    job.title
                                )
                                .into(),
                            );
                        }
                    }
                }
                this.finish();
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// 実行できなかった指示を記録に残し、やり直せるように待ち行列に戻す。
    fn repeat_failed(
        &mut self,
        job: &malakt_core::repeat::AiJob,
        error: String,
        cx: &mut Context<Self>,
    ) {
        let note: SharedString = format!("「{}」の定期実行: {error}", job.title).into();
        self.auto_error = Some(note.clone());
        self.items.push(Item::Error(note));
        if let Err(e) = self.db.update(cx, |db, _| {
            db.store.fail_repeat_ai(&job.task_id, &job.prompt)
        }) {
            self.error =
                Some(format!("「{}」の AI への指示を残せませんでした: {e}", job.title).into());
        }
    }

    async fn repeat_run(
        this: &WeakEntity<Self>,
        job: &malakt_core::repeat::AiJob,
        cx: &mut AsyncApp,
    ) -> Result<(), String> {
        let config = this.read_with(cx, |s, _| s.config.clone()).map_err(gone)?;
        let message = format!(
            "（アプリの定期実行からの自動のメッセージです）\n\
             繰り返しのタスク「{title}」（id: {id}）の新しい回を、アプリが作りました。\
             次の指示を実行して、結果を add_comment でこのタスクにコメントしてください。\
             タスクの中身は get_task で、前の回は list_tasks で同じタイトルを探して読めます。\n\n\
             指示: {prompt}\n",
            title = job.title,
            id = job.task_id,
            prompt = job.prompt,
        );
        this.update(cx, |s, cx| {
            s.items.push(Item::Notice(
                format!(
                    "繰り返しのタスク「{}」の回を作りました。AI が指示を実行します。",
                    job.title
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
            "定期実行（ユーザーは画面を見ていないことがあります）",
            cx,
        )
        .await
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
        // 時刻になったかを短い間隔で見る。間隔ぶん待つタイマーにすると、起動し直すたびに数え直しになり、
        // PC が眠っていた間も進まない
        self.watch_timer = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(WATCH_TICK).await;
            let alive = this.update(cx, |s, cx| {
                if s.watch_due(cx) {
                    s.check_github(cx);
                }
            });
            if alive.is_err() {
                break;
            }
        }));
    }

    /// 今の時刻（UNIX 秒）。テストではアプリの時計を進めると進む。
    fn watch_now(&self, cx: &App) -> i64 {
        let (wall, at) = self.watch_epoch;
        let passed = cx.background_executor().now().saturating_duration_since(at);
        unix_now().max(wall + passed.as_secs() as i64)
    }

    /// 最後に定期チェックを始めた時刻（UNIX 秒）。
    fn watch_last(&self, cx: &App) -> Option<i64> {
        // 読めなければ、まだ一度もチェックしていないものとして扱う（次のチェックで書き直す）
        self.db
            .read(cx)
            .store
            .setting(WATCH_LAST_SETTING)
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
    }

    /// 定期チェックの時刻になったか。
    fn watch_due(&self, cx: &App) -> bool {
        if !self.watch.enabled {
            return false;
        }
        let interval = i64::from(self.watch.minutes) * 60;
        self.watch_last(cx)
            .is_none_or(|last| self.watch_now(cx) - last >= interval)
    }

    /// タスクに書かれている GitHub のリンクを見に行き、前回から進展があれば AI がタスクにコメントする。
    /// ロック中も動く（画面を見ていない間の進展も、タスクに残す）。
    /// チャットが動いている間は何もしない（手が空いてから次の時刻の確認で見る）。
    pub fn check_github(&mut self, cx: &mut Context<Self>) -> bool {
        if self.busy {
            return false;
        }
        let now = self.watch_now(cx);
        let last = self.watch_last(cx);
        if !self.save_setting(WATCH_LAST_SETTING, &now.to_string(), cx) {
            return false;
        }
        self.begin_auto(RunKind::Watch, Vec::new());
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = Self::watch_run(&this, last, cx).await;
            // チャットがなくなっていれば、知らせる先もない
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(status) => this.watch_status = Some(status.into()),
                    Err(error) => {
                        this.watch_status = Some(
                            format!("{} のチェックに失敗しました: {error}", now_label()).into(),
                        );
                        let note: SharedString = format!("GitHub の定期チェック: {error}").into();
                        this.auto_error = Some(note.clone());
                        this.items.push(Item::Error(note));
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
        self.end_auto();
        self.settle();
        self.busy = false;
        self.own_command = false;
        self.auto = false;
        self.pending = None;
        self.arranging.clear();
        self.tracking.clear();
    }

    /// 1 つのリンクの今の様子を、`gh` で読む。
    async fn watch_read(
        this: &WeakEntity<Self>,
        config: &AiConfig,
        link: &Link,
        cx: &mut AsyncApp,
    ) -> Result<watch::Snapshot, WatchError> {
        let mut outputs = Vec::new();
        for spec in (config.gh_progress)(link) {
            let program = spec.display.clone();
            match Self::run_command(this, spec, cx).await {
                Ok(output) if output.code == Some(0) => outputs.push(output.stdout),
                Ok(output) => {
                    return Err(WatchError::Link(format!(
                        "gh が失敗しました（終了コード {}）: {}",
                        output.code.map_or("なし".to_owned(), |c| c.to_string()),
                        clip_tail(output.all.trim(), 500)
                    )))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(WatchError::NoGh);
                }
                Err(e) => {
                    return Err(WatchError::Link(format!(
                        "{program} を起動できませんでした: {e}"
                    )))
                }
            }
        }
        watch::read(link, &outputs).map_err(WatchError::Link)
    }

    async fn watch_run(
        this: &WeakEntity<Self>,
        last: Option<i64>,
        cx: &mut AsyncApp,
    ) -> Result<String, String> {
        let (config, provider, instruction, me) = this
            .read_with(cx, |s, _| {
                let me = match &s.github {
                    GithubAccount::Login(login) => Some(login.to_string()),
                    _ => None,
                };
                (
                    s.config.clone(),
                    s.provider,
                    s.watch.instruction.clone(),
                    me,
                )
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
                let seen: HashMap<String, Value> = store
                    .setting("ai.watch.seen")
                    .map_err(|e| format!("前回の結果を読めませんでした: {e}"))?
                    .and_then(|v| serde_json::from_str(&v).ok())
                    .unwrap_or_default();
                Ok::<_, String>((watch_targets(store)?, seen))
            })
            .map_err(gone)??;

        let mut changed = Vec::new();
        let mut failed = Vec::new();
        let mut now: HashMap<String, Value> = HashMap::new();
        for (link, tasks) in &targets {
            let url = link.url();
            match Self::watch_read(this, &config, link, cx).await {
                Ok(snapshot) => {
                    let progress = match seen.get(&url).and_then(watch::Mark::from_json) {
                        Some(mark) => snapshot.since(&mark),
                        // 初めて見るリンクは、前回のチェックより新しい動きだけ
                        None => snapshot.after(last),
                    };
                    if let Some(progress) = progress {
                        changed.push((url.clone(), tasks.clone(), progress));
                    }
                    now.insert(url, snapshot.mark().to_json());
                }
                Err(WatchError::NoGh) => {
                    return Err("gh コマンドが見つかりませんでした。GitHub CLI を入れて、gh auth login でログインしてください".into());
                }
                Err(WatchError::Link(error)) => {
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
            "{} にチェックしました。リンク {} 件中、進展 {} 件。",
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

        let me_note = match &me {
            Some(login) => format!("「自分」はユーザー本人（GitHub の @{login}）です。\n"),
            None => String::new(),
        };
        let mut message = format!(
            "（アプリの定期チェックからの自動のメッセージです）\n\
             タスクに書かれている GitHub のリンクに、前回のチェックから進展がありました。\n{me_note}\n指示: {instruction}\n"
        );
        for (url, tasks, progress) in &changed {
            message.push_str(&format!("\n## {url}\n関連するタスク:\n"));
            for (id, title) in tasks {
                message.push_str(&format!("- {title}（id: {id}）\n"));
            }
            message.push_str(&progress.describe(me.as_deref(), |secs| {
                malakt_core::view::format_datetime(secs.saturating_mul(1000))
            }));
        }
        let urls: Vec<&str> = changed.iter().map(|(url, _, _)| url.as_str()).collect();
        this.update(cx, |s, cx| {
            s.watch_status = Some(status.clone().into());
            s.auto_tasks = changed
                .iter()
                .flat_map(|(_, tasks, _)| tasks.iter())
                .map(|(id, title)| (id.clone(), title.clone().into()))
                .collect();
            s.items.push(Item::Notice(
                format!(
                    "GitHub の定期チェック: {} に進展がありました。AI がタスクにコメントします。",
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

    /// モデルの、画面に出す名前。サービスの一覧にあればその名前、なければ ID から作る。
    pub fn model_label(&self, id: &str) -> SharedString {
        match self.model_names.get(id) {
            Some(name) => name.clone().into(),
            None => provider::display_name(id).into(),
        }
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
        self.effort = self.saved_effort();
        self.refresh_key(cx);
        cx.notify();
    }

    pub fn set_model(&mut self, model: &str, cx: &mut Context<Self>) {
        let model = model.trim();
        if model == self.model() || !self.save_setting(&setting_key(self.provider), model, cx) {
            return;
        }
        self.models.insert(self.provider, model.to_owned());
        self.effort = self.saved_effort();
        cx.notify();
    }

    /// 前の版の共通の考える深さを、モデルごとの形で保存し直す。
    fn migrate_effort(&mut self, cx: &mut Context<Self>) {
        let efforts = self.efforts.clone();
        let error = self.error.clone();
        if self.save_efforts(&efforts, cx) {
            self.error = error;
            if let Err(e) = self.db.update(cx, |db, _| {
                db.store.set_setting(LEGACY_EFFORT_SETTING, None)
            }) {
                self.error = Some(format!("AI の設定を保存できませんでした: {e}").into());
            }
        }
    }

    /// 今のモデルで最後に選んだ考える深さ。選んでいなければ「標準」。
    fn saved_effort(&self) -> Effort {
        if !self.provider.supports_effort() {
            return Effort::Default;
        }
        self.efforts
            .get(&effort_key(self.provider, self.model()))
            .copied()
            .unwrap_or(Effort::Default)
    }

    /// モデルごとの考える深さを保存する。保存できなければ `false`（エラーは画面に出る）。
    fn save_efforts(&mut self, efforts: &HashMap<String, Effort>, cx: &mut Context<Self>) -> bool {
        let saved: std::collections::BTreeMap<&str, &str> = efforts
            .iter()
            .map(|(k, e)| (k.as_str(), e.as_str()))
            .collect();
        let json = serde_json::to_string(&saved).expect("文字列だけの表は JSON にできる");
        self.save_setting(EFFORTS_SETTING, &json, cx)
    }

    /// OpenCode のモデルの一覧を読んでおく。モデルごとに選べる effort を、選ぶ前に出すため。
    /// API キーがなければ通信しない。
    fn load_catalog(&mut self, cx: &mut Context<Self>) {
        let provider = self.provider;
        let Some(url) = provider.catalog_url() else {
            return;
        };
        if !matches!(self.key, KeyState::Present { .. })
            || self.catalog.as_ref().is_some_and(|(p, _)| *p == provider)
        {
            return;
        }
        let config = self.config.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { fetch_catalog(&config, url) })
                .await;
            // チャットがもうなければ、知らせる先もない
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(catalog) => this.catalog = Some((provider, Arc::new(catalog))),
                    Err(e) => {
                        this.error = Some(format!("モデルの一覧を取得できませんでした: {e}").into())
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 今のモデルで、その effort を選べるか。モデルの一覧をまだ読んでいなければ、選べるものとして扱う。
    pub fn effort_allowed(&self, effort: Effort) -> bool {
        let Some(wire) = effort.wire() else {
            return true;
        };
        if !self.provider.supports_effort() {
            return false;
        }
        let Some((provider, catalog)) = &self.catalog else {
            return true;
        };
        if *provider != self.provider {
            return true;
        }
        match catalog.iter().find(|m| m.id == self.model()) {
            Some(entry) => entry.efforts.iter().any(|e| e == wire),
            None => true,
        }
    }

    /// 今のモデルの考える深さを選ぶ。モデルごとに覚え、そのモデルに戻すとこの深さになる。
    pub fn set_effort(&mut self, effort: Effort, cx: &mut Context<Self>) {
        if self.effort == effort {
            return;
        }
        let mut efforts = self.efforts.clone();
        let key = effort_key(self.provider, self.model());
        if effort == Effort::Default {
            efforts.remove(&key);
        } else {
            efforts.insert(key, effort);
        }
        if !self.save_efforts(&efforts, cx) {
            return;
        }
        self.efforts = efforts;
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
                this.track_retry = true;
                // 設定に名前がなかったアカウントを、GitHub に聞く
                if matches!(this.key, KeyState::Present { .. })
                    && this.github == GithubAccount::Missing
                {
                    this.read_github(cx);
                }
                this.load_catalog(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// ユーザーの GitHub のアカウント。
    pub fn github(&self) -> &GithubAccount {
        &self.github
    }

    /// ユーザーの GitHub のアカウントを、`gh` から読み直す。
    /// API キーがなければ、`gh` の設定を読むだけで、GitHub には通信しない。
    pub fn read_github(&mut self, cx: &mut Context<Self>) {
        let specs = (self.config.whoami)(matches!(self.key, KeyState::Present { .. }));
        self.github_read += 1;
        let read = self.github_read;
        cx.spawn(async move |this, cx| {
            let login = cx
                .background_executor()
                .spawn(async move { read_login(&specs) })
                .await;
            // チャットがもうなければ、知らせる先もない
            let _ = this.update(cx, |this, cx| {
                // あとから読み直していたら、そちらの結果を使う
                if this.github_read != read {
                    return;
                }
                this.github = match login {
                    Some(login) => GithubAccount::Login(login.into()),
                    None => GithubAccount::Missing,
                };
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
                            .map(|m| (m.id.clone(), m.name.clone()))
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
                        .filter_map(|m| {
                            Some((
                                m["id"].as_str()?.to_owned(),
                                m["display_name"].as_str().map(str::to_owned),
                            ))
                        })
                        .collect())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.provider != provider {
                    return;
                }
                match result {
                    Ok(models) => {
                        this.error = None;
                        let mut changed = false;
                        for (id, name) in &models {
                            if let Some(name) = name.as_ref().filter(|name| !name.is_empty()) {
                                changed |= this.model_names.get(id) != Some(name);
                                this.model_names.insert(id.clone(), name.clone());
                            }
                        }
                        this.listed = models
                            .into_iter()
                            .map(|(id, _)| SharedString::from(id))
                            .collect();
                        // 次の起動でも名前を出せるように残す。一覧にあるモデルの分だけ
                        if changed {
                            if this.model_names.len() > MAX_MODEL_NAMES {
                                let listed = this.listed.clone();
                                this.model_names
                                    .retain(|id, _| listed.iter().any(|have| have == id));
                            }
                            match serde_json::to_string(&this.model_names) {
                                Ok(json) => {
                                    this.save_setting(MODEL_NAMES_SETTING, &json, cx);
                                }
                                Err(e) => {
                                    this.error = Some(
                                        format!("モデルの名前を保存できませんでした: {e}").into(),
                                    )
                                }
                            }
                        }
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
    fn reset_chat(&mut self) {
        self.turns.clear();
        self.items.clear();
        self.allow_shell = false;
        self.attached.clear();
        self.attached_workspaces.clear();
        self.conversation = malakt_core::new_id();
        self.chat_task = None;
        self.error = None;
    }

    /// 新しい会話を始める。今の会話は一覧に残る。
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.stop(cx);
        self.persist(cx);
        self.reset_chat();
        cx.notify();
    }

    /// 保存してある会話を開く。
    pub fn open_chat(&mut self, id: &str, cx: &mut Context<Self>) {
        if id == self.conversation {
            return;
        }
        let Some(info) = self.chats.iter().find(|c| c.id == id).cloned() else {
            return;
        };
        let chat = match self.db.read(cx).store.setting(&chat_key(id)) {
            Ok(Some(text)) => serde_json::from_str::<Value>(&text).map_err(|e| e.to_string()),
            Ok(None) => Err("保存した中身がありません".to_owned()),
            Err(e) => Err(e.to_string()),
        };
        let chat = match chat {
            Ok(chat) => chat,
            Err(error) => {
                self.error =
                    Some(format!("会話「{}」を開けませんでした: {error}", info.title).into());
                cx.notify();
                return;
            }
        };
        self.stop(cx);
        self.persist(cx);
        self.reset_chat();
        self.load_chat(&chat);
        self.conversation = info.id.clone();
        self.chat_task = info.task.map(|task| (task, info.title));
        self.saved_chat = self.chat_mark();
        self.save_index(cx);
        cx.notify();
    }

    /// 保存してある会話を消す。
    pub fn delete_chat(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.remove_setting(&chat_key(id), cx) {
            return;
        }
        self.chats.retain(|c| c.id != id);
        if id == self.conversation {
            self.stop(cx);
            self.reset_chat();
            self.saved_chat = self.chat_mark();
        }
        self.save_index(cx);
        cx.notify();
    }

    /// あるタスクについての会話を開く。まだなければ、タスクを添えた新しい会話を始める。
    pub fn chat_about_task(&mut self, id: &str, title: SharedString, cx: &mut Context<Self>) {
        if self.chat_task.as_ref().is_some_and(|(task, _)| task == id) {
            return;
        }
        let have = self
            .chats
            .iter()
            .find(|c| c.task.as_deref() == Some(id))
            .map(|c| c.id.clone());
        match have {
            Some(chat) => self.open_chat(&chat, cx),
            None => {
                self.clear(cx);
                self.chat_task = Some((id.to_owned(), title.clone()));
                self.attach_task(id, title, cx);
            }
        }
    }

    /// タスクを入力欄に添える。同じタスクは 1 つだけ。添えてあれば（前から添えてあったものも）`true`。
    pub fn attach_task(&mut self, id: &str, title: SharedString, cx: &mut Context<Self>) -> bool {
        if self.attached.iter().any(|(have, _)| have == id) {
            return true;
        }
        let room = self.attached.len() < MAX_ATTACHED_TASKS;
        if room {
            self.attached.push((id.to_owned(), title));
        } else {
            self.error = Some(format!("添えられるタスクは {MAX_ATTACHED_TASKS} 件までです").into());
        }
        cx.notify();
        room
    }

    /// 添えたタスクを外す。
    pub fn detach_task(&mut self, id: &str, cx: &mut Context<Self>) {
        self.attached.retain(|(have, _)| have != id);
        cx.notify();
    }

    /// ワークスペースを入力欄に添える。同じワークスペースは 1 つだけ。添えてあれば（前から添えてあったものも）`true`。
    pub fn attach_workspace(
        &mut self,
        id: &str,
        name: SharedString,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.attached_workspaces.iter().any(|(have, _)| have == id) {
            return true;
        }
        let room = self.attached_workspaces.len() < MAX_ATTACHED_WORKSPACES;
        if room {
            self.attached_workspaces.push((id.to_owned(), name));
        } else {
            self.error = Some(
                format!("添えられるワークスペースは {MAX_ATTACHED_WORKSPACES} 件までです").into(),
            );
        }
        cx.notify();
        room
    }

    /// 添えたワークスペースを外す。
    pub fn detach_workspace(&mut self, id: &str, cx: &mut Context<Self>) {
        self.attached_workspaces.retain(|(have, _)| have != id);
        cx.notify();
    }

    /// 送信する。`place` は、ユーザーが今開いている場所の説明。
    pub fn send(&mut self, text: &str, place: String, cx: &mut Context<Self>) -> bool {
        let text = text.trim().to_owned();
        if text.is_empty() || !self.db.read(cx).unlocked {
            return false;
        }
        // 考え中でも送れる。今の会話の続きか、並列の別の会話かを振り分ける
        if self.busy && !self.worker {
            let queued = Queued {
                text,
                place,
                attached: std::mem::take(&mut self.attached),
                attached_workspaces: std::mem::take(&mut self.attached_workspaces),
            };
            // 自動の処理やシェルのコマンドの間、Cursor のときは、終わるのを待って送る
            if self.auto || self.own_command || self.provider == Provider::Cursor {
                self.queued.push(queued);
            } else {
                self.route(queued, cx);
            }
            cx.notify();
            return true;
        }
        if self.busy {
            return false;
        }
        // 添えたタスクの中身を読む。読めないものがあれば、送らずにそのことを出す
        let mut extra = String::new();
        let mut shown = String::new();
        for (id, title) in &self.attached {
            let read = self.db.update(cx, |db, _| {
                tools::execute(
                    &mut db.store,
                    None,
                    "get_task",
                    &serde_json::json!({"id": id}),
                )
            });
            match read {
                Ok(task) => {
                    extra.push_str(&format!("\n\n[添えたタスク「{title}」]\n{task}"));
                    shown.push_str(&format!("「{title}」\n"));
                }
                Err(error) => {
                    self.error =
                        Some(format!("添えたタスク「{title}」を読めませんでした: {error}").into());
                    cx.notify();
                    return false;
                }
            }
        }
        // 添えたワークスペースは、サブワークスペースとタスクの一覧ごと渡す（多いときは一部と件数）
        for (id, name) in &self.attached_workspaces {
            let read = tools::workspace_context(&self.db.read(cx).store, id, false);
            match read {
                Ok(context) => {
                    extra.push_str(&format!("\n\n[添えたワークスペース「{name}」]\n{context}"));
                    shown.push_str(&format!("「{name}」\n"));
                }
                Err(error) => {
                    self.error = Some(
                        format!("添えたワークスペース「{name}」を読めませんでした: {error}").into(),
                    );
                    cx.notify();
                    return false;
                }
            }
        }
        self.attached.clear();
        self.attached_workspaces.clear();
        self.busy = true;
        self.error = None;
        self.items.push(Item::User(format!("{shown}{text}").into()));
        self.task = Some(cx.spawn(async move |this, cx| {
            let result = Self::run(&this, text, extra, place, cx).await;
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
        self.queued.clear();
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
        if self.auto {
            // 自動の実行は、会話に何も残さない
            self.auto_error = Some("中止しました".into());
            self.finish();
            cx.notify();
            return;
        }
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
            self.shell_dropped += extra;
        }
    }

    /// シェルの表示を消す（Ctrl+L）。実行中のコマンドはそのまま。
    pub fn clear_shell(&mut self, cx: &mut Context<Self>) {
        self.shell_dropped += self.shell.len();
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
        let shell = crate::term::ai_shell(cx);
        self.task = Some(cx.spawn(async move |this, cx| {
            // 起動できなかったことは、シェルに出ている
            let _ = Self::shell_line(&this, &command, &shell, cx).await;
            // チャットがなくなっていれば、知らせる先もない
            let _ = this.update(cx, |this, cx| {
                this.finish();
                cx.notify();
            });
        }));
        cx.notify();
        true
    }

    /// シェルの 1 行を、`shell` で今の場所で実行する。`cd` はシェルの場所を変える。
    async fn shell_line(
        this: &WeakEntity<Self>,
        command: &str,
        shell: &Shell,
        cx: &mut AsyncApp,
    ) -> std::io::Result<Output> {
        let dropped = || std::io::Error::other("チャットが閉じられました");
        let (cwd, ai_shell) = this
            .read_with(cx, |s, cx| (s.cwd.clone(), crate::term::ai_shell(cx)))
            .map_err(|_| dropped())?;
        // WSL の中の場所は、アプリからは移れない。`cd` もシェルに任せる
        if let Some(target) = cd_target(command).filter(|_| !shell.is_wsl()) {
            let next = resolve_dir(&cwd, target);
            let moved = next.as_ref().is_ok_and(|dir| dir.is_dir());
            let mut output = Output {
                stdout: String::new(),
                all: String::new(),
                code: Some(if moved { 0 } else { 1 }),
            };
            this.update(cx, |s, cx| {
                s.push_shell(LineKind::Command, &format!("{} {command}", shell.mark()));
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
        let mut spec = shell.spec(command);
        spec.cwd = Some(cwd);
        // AI のタブと違うシェル（開いているターミナルのタブのシェル）なら、どのシェルかを出す
        if *shell != ai_shell {
            spec.mark = format!("[{}] {}", shell.label(), shell.mark());
        }
        // シェルから起動した Claude Code などが、このアプリのコマンドとデータを見つけられるようにする
        spec.env.extend(crate::mcp::shell_env());
        Self::run_command(this, spec, cx).await
    }

    // ---- 進行 ----

    async fn run(
        this: &WeakEntity<Self>,
        text: String,
        extra: String,
        place: String,
        cx: &mut AsyncApp,
    ) -> Result<(), String> {
        let config = this.read_with(cx, |s, _| s.config.clone()).map_err(gone)?;

        // メッセージに GitHub や Slack のリンクがあれば、先に中身を取ってきて添える
        let mut message = text.clone();
        message.push_str(&extra);
        for link in find_sources(&text) {
            match Self::fetch(this, &config, &link, cx).await {
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
                let (global, mut extra) = s.rules(cx)?;
                if let GithubAccount::Login(login) = &s.github {
                    extra = format!("{}\n{extra}", github::account_note(login));
                }
                // コマンドが動くシェルは、ユーザーが今開いているタブで決まる
                let (shell, in_tab) = crate::term::command_shell(cx);
                extra = format!("{}\n{extra}", tools::shell_note(&shell, in_tab));
                s.turn_shell = shell;
                let note = tools::turn_note(&malakt_core::view::now(), place, &extra);
                if let Some(Turn::User(message)) = s.turns.last_mut() {
                    message.push_str(&note);
                }
                Ok::<_, String>(tools::system_prompt(&global))
            })
            .map_err(gone)??;
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
                            summary: crate::ai_tools::summary(&call.name, &call.args)
                                .unwrap_or_else(|| tools::summary(&call.name, &call.args))
                                .into(),
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
        let (turns, auto) = this
            .read_with(cx, |s, _| (s.turns.clone(), s.auto))
            .map_err(gone)?;
        let mut definitions = tools::definitions();
        // 画面と設定を変えるツールは、ユーザーが頼んだ会話にだけ渡す
        if !auto {
            definitions.extend(crate::ai_tools::definitions());
        }
        let request = wire::Request {
            model,
            effort,
            system,
            turns: &turns,
            tools: &definitions,
            fallbacks: false,
            prefix_ttl: None,
        };
        Self::request(this, config, provider, key, conversation, request, cx).await
    }

    /// AI に 1 回問い合わせる。
    async fn request(
        this: &WeakEntity<Self>,
        config: &AiConfig,
        provider: Provider,
        key: &str,
        conversation: &str,
        mut request: wire::Request<'_>,
        cx: &mut AsyncApp,
    ) -> Result<(Wire, Reply), String> {
        let (model, effort) = (request.model, request.effort);

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
        request.prefix_ttl = endpoint.prefix_ttl;
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
        let app_tool = crate::ai_tools::is_app_tool(name);
        let confirm = if app_tool {
            crate::ai_tools::needs_confirmation(name, &call.args)
        } else {
            tools::needs_confirmation(name)
        };
        // 並列の会話では、確認が要る操作をしない（確認を出す場所がない）
        let (auto, from_app) = this
            .read_with(cx, |s, _| (s.auto || s.worker, s.auto))
            .map_err(gone)?;
        if from_app && app_tool {
            return Ok(Err(ToolError::Failed(
                "アプリが自動で始めた実行からは、画面と設定は変えられません".into(),
            )));
        }
        if auto && confirm {
            // 確認に答える人がいないので、定期チェックからは実行しない
            return Ok(Err(ToolError::Failed(
                "定期チェックからは、タスクの削除とコマンドの実行はできません".into(),
            )));
        }
        if confirm {
            let (tx, rx) = async_channel::bounded(1);
            let asked = this
                .update(cx, |s, cx| {
                    if s.auto_run || name == tools::RUN_SHELL && s.allow_shell {
                        return false;
                    }
                    let text = if app_tool {
                        crate::ai_tools::confirmation_text(name, &call.args)
                    } else {
                        tools::confirmation_text(
                            &s.db.read(cx).store,
                            name,
                            &call.args,
                            &s.turn_shell,
                        )
                    };
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
        let tracking = this
            .read_with(cx, |s, _| s.tracking.clone())
            .map_err(gone)?;
        if !tracking.is_empty() && !track_allows(name, &call.args, &tracking) {
            return failed(
                "コメントからの更新では、コメントが書かれたタスクの更新だけができます".into(),
            );
        }
        if name == crate::browser::READ_TOOL {
            // ページの文字は、OS のブラウザの部品が読み終わってから届く
            let asked = cx.update(|cx| crate::browser::read_page(&call.args, cx));
            return match asked {
                Ok(answer) => match answer.recv().await {
                    Ok(text) => Ok(Ok(text)),
                    Err(_) => failed("ページを読む前に、タブが閉じられました".into()),
                },
                Err(e) => failed(e),
            };
        }
        if crate::ai_tools::is_view_tool(name) {
            let result = cx.update(|cx| crate::ai_tools::run_view_tool(name, &call.args, cx));
            return Ok(result.map_err(ToolError::Failed));
        }
        if crate::ai_tools::is_session_tool(name) {
            let result = this
                .update(cx, |s, cx| s.run_setting_tool(name, &call.args, cx))
                .map_err(gone)?;
            return Ok(result.map_err(ToolError::Failed));
        }
        match name {
            tools::RUN_SHELL => {
                let Some(command) = call.args.get("command").and_then(Value::as_str) else {
                    return failed("command が必要です".into());
                };
                let shell = this
                    .read_with(cx, |s, _| s.turn_shell.clone())
                    .map_err(gone)?;
                match Self::shell_line(this, command, &shell, cx).await {
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
            tools::SLACK_FETCH => {
                let url = call
                    .args
                    .get("url")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let Some(link) = slack::parse_link(url) else {
                    return failed(format!(
                        "Slack のメッセージへのリンクとして読めません: {url}"
                    ));
                };
                match Self::fetch_slack(this, config, &link, cx).await {
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
                        // コメントからの更新では、進捗・状態・期限・開始日・優先度だけを変えさせる
                        let cleaned = if s.tracking.is_empty() || name != "update_task" {
                            cleaned
                        } else {
                            let mut args = call.args.clone();
                            if let Some(map) = args.as_object_mut() {
                                map.retain(|key, _| TRACK_FIELDS.contains(&key.as_str()));
                            }
                            Some(args)
                        };
                        let args = cleaned.as_ref().unwrap_or(&call.args);
                        // コメントからの更新では、AI が変える前の値を控えて、元に戻せるようにする
                        let before = (!s.tracking.is_empty() && name == "update_task")
                            .then(|| args.get("id").and_then(Value::as_str))
                            .flatten()
                            .and_then(|id| s.db.read(cx).store.task(id).ok());
                        // 変更履歴には、どの AI が、どの自動の実行でしたかを残す
                        let actor = malakt_core::Actor::ai(
                            s.provider.label(),
                            s.auto.then(|| s.auto_kind.label()),
                        );
                        let result = s.db.update(cx, |db, cx| {
                            // 自分が登録されていれば、コメントの書き手にする
                            let author = db
                                .store
                                .people()
                                .ok()
                                .and_then(|p| p.into_iter().find(|p| p.is_self))
                                .map(|p| p.id);
                            // 変更は、ツールの呼び出し 1 回を元に戻す 1 つの段にする（Ctrl+Z で戻せる）
                            let write = tools::writes(name);
                            let marked = if write {
                                db.store.undo_mark().map_err(|e| e.to_string())
                            } else {
                                Ok(())
                            };
                            let result = marked.and_then(|()| {
                                db.store.acting(actor, |store| {
                                    tools::execute(store, author.as_deref(), name, args)
                                })
                            });
                            if write {
                                if let Err(e) = db.store.undo_mark() {
                                    cx.notify();
                                    return result.and(Err(e.to_string()));
                                }
                            }
                            cx.notify();
                            result
                        });
                        s.absorb(&waiting, cx);
                        if let Some(before) = before {
                            s.record_tracked(before, cx);
                        }
                        // AI が書いたコメントは、もう見たものにする
                        if s.comment_mark.is_some() {
                            s.comment_mark = comment_mark(&s.db.read(cx).store);
                        }
                        result
                    })
                    .map_err(gone)?;
                Ok(result.map_err(ToolError::Failed))
            }
        }
    }

    async fn fetch(
        this: &WeakEntity<Self>,
        config: &AiConfig,
        source: &Fetch,
        cx: &mut AsyncApp,
    ) -> Result<String, String> {
        match source {
            Fetch::GitHub(link) => Self::fetch_github(this, config, link, cx).await,
            Fetch::Slack(link) => Self::fetch_slack(this, config, link, cx).await,
        }
    }

    /// Slack のメッセージと、そのスレッドの返信を Slack CLI で取ってくる。
    async fn fetch_slack(
        this: &WeakEntity<Self>,
        config: &AiConfig,
        link: &slack::Link,
        cx: &mut AsyncApp,
    ) -> Result<String, String> {
        for spec in (config.slack)(link) {
            let output = match Self::run_command(this, spec, cx).await {
                Ok(output) => output,
                // このコマンドがなければ、次の呼び方を試す
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("Slack CLI を起動できませんでした: {e}")),
            };
            if output.code != Some(0) {
                return Err(format!(
                    "Slack CLI が失敗しました（終了コード {}）: {}",
                    output.code.map_or("なし".to_owned(), |c| c.to_string()),
                    clip_tail(output.all.trim(), 1000)
                ));
            }
            return link.format_response(&output.stdout, |secs| {
                malakt_core::view::format_datetime(secs.saturating_mul(1000))
            });
        }
        Err("Slack CLI（slack-cli）が見つかりませんでした".into())
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
            let mark = if spec.mark.is_empty() {
                "$"
            } else {
                &spec.mark
            };
            s.push_shell(LineKind::Command, &format!("{mark} {}", spec.display));
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

/// 今の UNIX 秒。
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// 定期チェックで、1 つのリンクを読めなかった理由。
enum WatchError {
    /// `gh` が入っていない。ほかのリンクも読めないので、そこで止める
    NoGh,
    Link(String),
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
