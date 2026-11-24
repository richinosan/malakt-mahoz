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
    /// 今の進行が、定期チェックから始まったものか。確認が要る操作は実行しない。
    auto: bool,
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
            auto_run = read("ai.auto_run").as_deref() == Some("1");
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
            shell_place: ShellPlace::Docked,
            conversation: malakt_core::new_id(),
            task: None,
            error,
            watch,
            watch_status: None,
            watch_timer: None,
            auto: false,
        };
        this.restart_watch(cx);
        this
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

        let system = tools::system_prompt(&malakt_core::view::today(), place);
        for _ in 0..MAX_ROUNDS {
            let (wire, reply) = Self::complete(
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
            .await?;
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
                        s.db.update(cx, |db, cx| {
                            // 自分が登録されていれば、コメントの書き手にする
                            let author = db
                                .store
                                .people()
                                .ok()
                                .and_then(|p| p.into_iter().find(|p| p.is_self))
                                .map(|p| p.id);
                            let result =
                                tools::execute(&mut db.store, author.as_deref(), name, &call.args);
                            cx.notify();
                            result
                        })
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
            Ok(output) if output.code == Some(0) => Ok(output.stdout),
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
