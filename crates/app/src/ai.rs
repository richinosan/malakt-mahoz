//! AI チャットの状態と進行。すべてのウィンドウで 1 つを共有する。描画は `ai_view` にある。
//!
//! API キーを保存して送信するまで、通信はしない。

use std::collections::HashMap;
use std::sync::Arc;

use gpui_kit::*;
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
    pub text: SharedString,
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
    running: Option<shell::Handle>,
    /// シェルのコマンドを、確認なしで実行する（このチャットの間）。
    pub allow_shell: bool,
    /// 別ウィンドウに出したチャット。
    pub chat_window: Option<AnyWindowHandle>,
    pub shell_place: ShellPlace,
    /// 会話ごとの ID（OpenCode に送る）。
    conversation: String,
    task: Option<Task<()>>,
    /// 設定の変更などの失敗。チャットの上に出す。
    pub error: Option<SharedString>,
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
            (provider, effort, models)
        };
        Self {
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
            running: None,
            allow_shell: false,
            chat_window: None,
            shell_place: ShellPlace::Docked,
            conversation: malakt_core::new_id(),
            task: None,
            error,
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
                this.settle();
                this.busy = false;
                this.pending = None;
                cx.notify();
            });
        }));
        cx.notify();
        true
    }

    /// 止める。実行中のコマンドも強制終了する。
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        let killed = self.kill_command();
        if !self.busy {
            if killed {
                cx.notify();
            }
            return;
        }
        // タスクを捨てると、待っているところで止まる
        self.task = None;
        self.busy = false;
        self.pending = None;
        self.settle();
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
        self.shell.push(ShellLine {
            kind,
            text: text.to_owned().into(),
        });
        if self.shell.len() > MAX_SHELL_LINES {
            let extra = self.shell.len() - MAX_SHELL_LINES;
            self.shell.drain(..extra);
        }
    }

    // ---- 進行 ----

    async fn run(
        this: &WeakEntity<Self>,
        text: String,
        place: String,
        cx: &mut AsyncApp,
    ) -> Result<(), String> {
        let (config, provider, model, effort, conversation) = this
            .read_with(cx, |s, _| {
                (
                    s.config.clone(),
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

        let system = tools::system_prompt(&malakt_core::view::today(), &place);
        for _ in 0..MAX_ROUNDS {
            let (wire, reply) = Self::complete(
                this,
                &config,
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
                let (state, output) = match Self::call_tool(this, &config, call, cx).await? {
                    Ok(output) => (ToolState::Done, Ok(output)),
                    Err(ToolError::Denied) => (
                        ToolState::Denied,
                        Err("ユーザーが許可しませんでした".to_owned()),
                    ),
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
        if tools::needs_confirmation(name) {
            let (tx, rx) = async_channel::bounded(1);
            let asked = this
                .update(cx, |s, cx| {
                    if name == tools::RUN_SHELL && s.allow_shell {
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
                    return failed("command が要ります".into());
                };
                match Self::run_command(this, Spec::shell(command), cx).await {
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
