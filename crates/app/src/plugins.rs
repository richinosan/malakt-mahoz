//! プラグイン（`crates/plugins`）をアプリにつなぐ。
//!
//! - 読むフォルダー: 標準（`~/.malakt-mahoz/plugins`、設定でオフにできる）、設定で足したもの、
//!   環境変数 `MALAKT_MAHOZ_PLUGINS` のもの。前にあるものほど優先する
//! - 有効にするのは設定の「プラグイン」で、許すこと（権限）を並べた画面で承認したときだけ。
//!   ロックが有効なら、パスキーでも確かめる。承認は `plugin.toml` と WASM のハッシュで覚え、
//!   ファイルが変わったら、もう一度承認するまで動かさない
//! - コマンドは別のスレッドで動かし、プラグインが呼ぶツールは画面のスレッドでデータベースに実行する。
//!   書き込みは、ツールの呼び出し 1 回ずつ元に戻す段になり、変更履歴にはプラグインの名前が残る

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use gpui_kit::base::{Disableable, TestSupportExt};
use gpui_kit::component::button::ButtonVariants;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme, Sizable};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use malakt_ai::api::{self, Group};
use malakt_ai::tools;
use malakt_core::{Actor, Id, Store};
use malakt_plugins::{
    discover, folders, run_command, Discovery, Folder, Found, Outcome, Permission, Place, Request,
    Source, ENV_DIRS,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::db::Shared;
use crate::feel;
use crate::fonts::MonoFont;
use crate::press::button;
use crate::render::{app_icon, col, px, remove_x, row, Framed};
use crate::state::{MainView, SubModal};

/// 承認したプラグイン（ID → ハッシュと権限）を覚える設定。JSON。
pub const SETTING_APPROVED: &str = "plugins.approved";
/// 設定で足したフォルダー。JSON の文字列の配列。
pub const SETTING_DIRS: &str = "plugins.dirs";
/// 標準のフォルダーを読むか。`off` で読まない。
pub const SETTING_DEFAULT: &str = "plugins.default";

/// AI からプラグインのコマンドを実行するツール。結果はコマンドが終わってから届く。
pub const RUN_TOOL: &str = "run_plugin_command";

/// 承認したときの、プラグインのファイルのハッシュと権限。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub hash: String,
    pub permissions: Vec<String>,
}

/// プラグインの今の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Off,
    On,
    /// 有効にしたあとで、ファイルが変わった。もう一度承認するまで動かさない
    Changed,
}

impl State {
    pub fn code(self) -> &'static str {
        match self {
            State::Off => "off",
            State::On => "on",
            State::Changed => "changed",
        }
    }
}

/// 読んだプラグインの一覧。すべてのウィンドウで共有する。
#[derive(Default)]
pub struct Plugins {
    pub folders: Vec<Folder>,
    pub found: Discovery,
    pub approved: BTreeMap<String, Approval>,
    /// 設定を読めなかったときの理由
    pub error: Option<String>,
}

impl Global for Plugins {}

impl Plugins {
    pub fn state(&self, plugin: &Found) -> State {
        match self.approved.get(&plugin.manifest.id) {
            None => State::Off,
            Some(a) if a.hash == plugin.hash => State::On,
            Some(_) => State::Changed,
        }
    }

    pub fn plugin(&self, id: &str) -> Option<&Found> {
        self.found.plugins.iter().find(|p| p.manifest.id == id)
    }

    /// 有効なプラグインの、その場所に出すコマンド（プラグインの ID、コマンドの ID、名前）。
    pub fn commands(&self, place: Place) -> Vec<(String, String, String)> {
        self.found
            .plugins
            .iter()
            .filter(|p| self.state(p) == State::On)
            .flat_map(|p| {
                p.manifest
                    .commands
                    .iter()
                    .filter(move |c| c.on == place)
                    .map(move |c| (p.manifest.id.clone(), c.id.clone(), c.title.clone()))
            })
            .collect()
    }
}

/// 起動したときに 1 回読む。
pub fn init(cx: &mut App) {
    refresh(cx);
}

pub fn get(cx: &App) -> &Plugins {
    static EMPTY: std::sync::LazyLock<Plugins> = std::sync::LazyLock::new(Plugins::default);
    cx.try_global::<Plugins>().unwrap_or(&EMPTY)
}

/// プラグインの設定。
#[derive(Debug, Default, PartialEq, Eq)]
struct Settings {
    /// 標準のフォルダーを読む
    use_default: bool,
    /// 設定で足したフォルダー
    dirs: Vec<PathBuf>,
    approved: BTreeMap<String, Approval>,
}

/// 設定から、読むフォルダーと承認を読む。
fn read_settings(store: &Store) -> Result<Settings, String> {
    let use_default = store
        .setting(SETTING_DEFAULT)
        .map_err(|e| e.to_string())?
        .as_deref()
        != Some("off");
    let dirs = match store.setting(SETTING_DIRS).map_err(|e| e.to_string())? {
        Some(text) => serde_json::from_str::<Vec<String>>(&text)
            .map_err(|e| format!("プラグインのフォルダーの設定を読めませんでした: {e}"))?
            .into_iter()
            .map(PathBuf::from)
            .collect(),
        None => Vec::new(),
    };
    let approved = match store.setting(SETTING_APPROVED).map_err(|e| e.to_string())? {
        Some(text) => serde_json::from_str(&text)
            .map_err(|e| format!("プラグインの承認の設定を読めませんでした: {e}"))?,
        None => BTreeMap::new(),
    };
    Ok(Settings {
        use_default,
        dirs,
        approved,
    })
}

/// フォルダーを読み直す。設定を変えたとき、設定を開いたとき、読み直しのボタンで呼ぶ。
pub fn refresh(cx: &mut App) {
    let Some(shared) = cx.try_global::<Shared>() else {
        return;
    };
    let settings = read_settings(&shared.0.read(cx).store);
    let env = std::env::var_os(ENV_DIRS);
    let plugins = match settings {
        Ok(settings) => {
            let folders = folders(settings.use_default, &settings.dirs, env.as_deref());
            Plugins {
                found: discover(&folders),
                folders,
                approved: settings.approved,
                error: None,
            }
        }
        Err(error) => Plugins {
            error: Some(error),
            ..Default::default()
        },
    };
    cx.set_global(plugins);
}

fn save_dirs(store: &mut Store, dirs: &[PathBuf]) -> malakt_core::Result<()> {
    let list: Vec<String> = dirs.iter().map(|d| d.display().to_string()).collect();
    let text =
        serde_json::to_string(&list).map_err(|e| malakt_core::Error::Invalid(e.to_string()))?;
    store.set_setting(SETTING_DIRS, (!list.is_empty()).then_some(text.as_str()))
}

fn save_approved(
    store: &mut Store,
    approved: &BTreeMap<String, Approval>,
) -> malakt_core::Result<()> {
    let text =
        serde_json::to_string(approved).map_err(|e| malakt_core::Error::Invalid(e.to_string()))?;
    store.set_setting(
        SETTING_APPROVED,
        (!approved.is_empty()).then_some(text.as_str()),
    )
}

/// プラグインがこのツールを呼べるか。呼べないなら理由。
pub fn allowed(tool: &str, permissions: &[Permission]) -> Result<(), String> {
    let Some(def) = api::tool(Group::Data, tool) else {
        return Err(format!("{tool} というツールはありません"));
    };
    if def.in_app_only || def.local_files {
        return Err(format!("{tool} はプラグインからは使えません"));
    }
    let need = if crate::mcp::reads(tool) {
        Permission::Read
    } else {
        Permission::Write
    };
    if !permissions.contains(&need) {
        return Err(format!(
            "{tool} を使うには、plugin.toml の permissions に \"{}\" が要ります",
            need.code()
        ));
    }
    Ok(())
}

/// コマンドの途中で、プラグインのスレッドから届いたものを見に行く間隔。
const POLL: std::time::Duration = std::time::Duration::from_millis(5);

/// コマンドを動かすスレッドから、画面のスレッドへ届けるもの。
enum Msg {
    Call(
        String,
        Value,
        std::sync::mpsc::Sender<Result<String, String>>,
    ),
    Done(Result<Outcome, String>),
}

/// コマンドに渡す ID。
#[derive(Debug, Clone)]
pub enum Target {
    Task(Id),
    Workspace(Id),
}

impl MainView {
    fn plugins_changed(&mut self, cx: &mut Context<Self>) {
        refresh(cx);
        cx.notify();
    }

    /// 承認の画面を出す。そのときのファイルのハッシュを覚え、承認する前に変わっていたら有効にしない。
    pub fn ask_enable_plugin(&mut self, id: &str, cx: &mut Context<Self>) -> Result<(), String> {
        refresh(cx);
        let plugin = get(cx)
            .plugin(id)
            .ok_or_else(|| format!("プラグイン {id} が見つかりません"))?;
        self.plugin_approving = Some((id.to_owned(), plugin.hash.clone()));
        self.sub_modal = Some(SubModal::EnablePlugin);
        cx.notify();
        Ok(())
    }

    /// 承認の画面の「有効にする」。ロックが有効なら、パスキーで確かめてから。
    pub fn confirm_enable_plugin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.lock_enabled(cx) {
            let name = self
                .plugin_approving
                .as_ref()
                .and_then(|(id, _)| get(cx).plugin(id))
                .map(|p| p.manifest.name.clone())
                .unwrap_or_default();
            self.with_passkey(
                &format!("プラグイン「{name}」を有効にする"),
                window,
                cx,
                |this, cx| this.approve_plugin(cx),
            );
        } else {
            self.approve_plugin(cx);
        }
    }

    fn approve_plugin(&mut self, cx: &mut Context<Self>) {
        let Some((id, hash)) = self.plugin_approving.clone() else {
            return;
        };
        refresh(cx);
        let plugins = get(cx);
        let Some(plugin) = plugins.plugin(&id) else {
            self.error = Some(format!("プラグイン {id} が見つかりません。").into());
            cx.notify();
            return;
        };
        if plugin.hash != hash {
            self.error = Some(
                "承認の画面を出したあとで、プラグインのファイルが変わりました。もう一度開いてください。"
                    .into(),
            );
            self.plugin_approving = None;
            self.sub_modal = None;
            cx.notify();
            return;
        }
        let name = plugin.manifest.name.clone();
        let mut approved = plugins.approved.clone();
        approved.insert(
            id,
            Approval {
                hash,
                permissions: plugin
                    .manifest
                    .permissions
                    .iter()
                    .map(|p| p.code().to_owned())
                    .collect(),
            },
        );
        if self.write(cx, |s| save_approved(s, &approved)).is_some() {
            self.plugin_approving = None;
            self.sub_modal = None;
            self.show_notice(format!("プラグイン「{name}」を有効にしました。").into());
        }
        self.plugins_changed(cx);
    }

    pub fn cancel_enable_plugin(&mut self, cx: &mut Context<Self>) {
        self.plugin_approving = None;
        self.sub_modal = None;
        cx.notify();
    }

    pub fn disable_plugin(&mut self, id: &str, cx: &mut Context<Self>) -> bool {
        let mut approved = get(cx).approved.clone();
        if approved.remove(id).is_none() {
            return true;
        }
        let saved = self.write(cx, |s| save_approved(s, &approved)).is_some();
        self.plugins_changed(cx);
        saved
    }

    /// スイッチを押したとき。オフならオンにする承認の画面を出し、オンならオフにする。
    fn toggle_plugin(&mut self, id: &str, cx: &mut Context<Self>) {
        let state = get(cx).plugin(id).map(|p| get(cx).state(p));
        match state {
            Some(State::On) => {
                self.disable_plugin(id, cx);
            }
            Some(_) => {
                if let Err(error) = self.ask_enable_plugin(id, cx) {
                    self.error = Some(error.into());
                    cx.notify();
                }
            }
            None => self.plugins_changed(cx),
        }
    }

    fn extra_plugin_dirs(&self, cx: &App) -> Result<Vec<PathBuf>, String> {
        read_settings(&self.db.read(cx).store).map(|s| s.dirs)
    }

    /// フォルダーを足す。もう入っているものは足さない。
    pub fn add_plugin_folders(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) -> bool {
        let mut dirs = match self.extra_plugin_dirs(cx) {
            Ok(dirs) => dirs,
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
                return false;
            }
        };
        for path in paths {
            if !dirs.contains(&path) {
                dirs.push(path);
            }
        }
        let saved = self.write(cx, |s| save_dirs(s, &dirs)).is_some();
        self.plugins_changed(cx);
        saved
    }

    pub fn remove_plugin_folder(&mut self, path: &Path, cx: &mut Context<Self>) -> bool {
        let mut dirs = match self.extra_plugin_dirs(cx) {
            Ok(dirs) => dirs,
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
                return false;
            }
        };
        dirs.retain(|d| d != path);
        let saved = self.write(cx, |s| save_dirs(s, &dirs)).is_some();
        self.plugins_changed(cx);
        saved
    }

    pub fn set_default_plugin_folder(&mut self, on: bool, cx: &mut Context<Self>) -> bool {
        let saved = self
            .write(cx, |s| {
                s.set_setting(SETTING_DEFAULT, (!on).then_some("off"))
            })
            .is_some();
        self.plugins_changed(cx);
        saved
    }

    /// OS のフォルダーの選択で、プラグインのフォルダーを足す。
    pub fn pick_plugin_folders(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("プラグインのフォルダーにする".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let paths = match chosen.await {
                Ok(Ok(Some(paths))) => paths,
                // 選ばずに閉じた
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(error)) => {
                    // ウィンドウを閉じたあとなら、出す先がない
                    let _ = this.update(cx, |this, cx| {
                        this.error = Some(format!("フォルダーを選べませんでした: {error}").into());
                        cx.notify();
                    });
                    return;
                }
            };
            // ウィンドウを閉じたあとなら、何もしない
            let _ = this.update(cx, |this, cx| this.add_plugin_folders(paths, cx));
        })
        .detach();
    }

    /// フォルダーを OS のファイルの画面で開く。標準のフォルダーは、まだなければ作る。
    fn open_plugin_folder(&mut self, folder: &Folder, cx: &mut Context<Self>) {
        if folder.from == Source::Default {
            if let Err(error) = std::fs::create_dir_all(&folder.path) {
                self.error = Some(
                    format!(
                        "フォルダー {} を作れませんでした: {error}",
                        folder.path.display()
                    )
                    .into(),
                );
                cx.notify();
                return;
            }
        }
        cx.open_with_system(&folder.path);
    }

    /// プラグインのコマンドを始める。結果は、終わったら返す受け口に届く。
    /// 画面から押したとき（`from_ai` が `false`）は、失敗を画面のエラーに出す。
    pub fn start_plugin_command(
        &mut self,
        plugin_id: &str,
        command_id: &str,
        target: Target,
        from_ai: bool,
        cx: &mut Context<Self>,
    ) -> Result<async_channel::Receiver<Result<Outcome, String>>, String> {
        if !self.unlocked(cx) {
            return Err("アプリがロックされています".into());
        }
        let plugins = get(cx);
        let plugin = plugins
            .plugin(plugin_id)
            .ok_or_else(|| format!("プラグイン {plugin_id} が見つかりません"))?
            .clone();
        let name = plugin.manifest.name.clone();
        let approved = match plugins.state(&plugin) {
            State::On => plugins.approved[plugin_id].hash.clone(),
            State::Off => return Err(format!("プラグイン「{name}」は無効です")),
            State::Changed => {
                return Err(format!(
                    "プラグイン「{name}」のファイルが、有効にしたあとで変わりました。設定の「プラグイン」でもう一度有効にしてください"
                ))
            }
        };
        let command = plugin
            .manifest
            .commands
            .iter()
            .find(|c| c.id == command_id)
            .ok_or_else(|| format!("プラグイン「{name}」にコマンド {command_id} はありません"))?
            .clone();
        let request = match (&target, command.on) {
            (Target::Task(id), Place::Task) => Request {
                command: command.id.clone(),
                task_id: Some(id.clone()),
                workspace_id: None,
            },
            (Target::Workspace(id), Place::Workspace) => Request {
                command: command.id.clone(),
                task_id: None,
                workspace_id: Some(id.clone()),
            },
            (_, Place::Task) => {
                return Err(format!(
                    "コマンド「{}」には task_id が要ります",
                    command.title
                ))
            }
            (_, Place::Workspace) => {
                return Err(format!(
                    "コマンド「{}」には workspace_id が要ります",
                    command.title
                ))
            }
        };
        let permissions = plugin.manifest.permissions.clone();
        // 画面のスレッドは、プラグインのスレッドから起こされずに、短い間隔で見に行く
        let (tx, rx) = std::sync::mpsc::channel::<Msg>();
        let calls = tx.clone();
        std::thread::Builder::new()
            .name(format!("plugin {plugin_id}"))
            .spawn(move || {
                let call: malakt_plugins::CallTool = Box::new(move |name, args| {
                    let (reply, answer) = std::sync::mpsc::channel();
                    calls
                        .send(Msg::Call(name.to_owned(), args.clone(), reply))
                        .map_err(|_| "アプリのウィンドウが閉じられました".to_owned())?;
                    answer
                        .recv()
                        .map_err(|_| "アプリのウィンドウが閉じられました".to_owned())?
                });
                let result = run_command(&plugin, &approved, &request, call);
                // 受け取る側がもうない（ウィンドウを閉じた）なら、結果を出す先がない
                let _ = tx.send(Msg::Done(result));
            })
            .map_err(|e| format!("プラグインを動かすスレッドを作れませんでした: {e}"))?;
        let (answer, answered) = async_channel::bounded(1);
        let title = command.title.clone();
        cx.spawn(async move |this, cx| {
            let mut done = None;
            loop {
                let msg = match rx.try_recv() {
                    Ok(msg) => msg,
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        cx.background_executor().timer(POLL).await;
                        continue;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                };
                match msg {
                    Msg::Call(tool, args, reply) => {
                        let result = this
                            .update(cx, |this, cx| {
                                this.plugin_tool(&name, &permissions, &tool, &args, cx)
                            })
                            .unwrap_or_else(|_| Err("アプリのウィンドウが閉じられました".into()));
                        // プラグインのスレッドが先に終わっていたら、返す先がない
                        let _ = reply.send(result);
                    }
                    Msg::Done(result) => {
                        done = Some(result);
                        break;
                    }
                }
            }
            let result = done.unwrap_or_else(|| Err("プラグインが途中で止まりました".into()));
            let shown = this.update(cx, |this, cx| {
                match &result {
                    Ok(outcome) => {
                        if let Some(text) = &outcome.copy {
                            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                        }
                        let notice = outcome
                            .notice
                            .clone()
                            .unwrap_or_else(|| format!("「{title}」を実行しました。"));
                        this.show_notice(notice.into());
                    }
                    Err(error) if !from_ai => {
                        this.error = Some(
                            format!("プラグイン「{name}」の「{title}」が失敗しました: {error}")
                                .into(),
                        );
                    }
                    Err(_) => {}
                }
                cx.notify();
            });
            let result = match shown {
                Ok(()) => result,
                Err(_) => result.and(Err("アプリのウィンドウが閉じられました".into())),
            };
            // AI の会話を止めたあとなら、受け取る側がない
            let _ = answer.send(result).await;
        })
        .detach();
        Ok(answered)
    }

    /// メニューから押したコマンド。
    pub fn run_plugin_from_menu(
        &mut self,
        plugin_id: &str,
        command_id: &str,
        target: Target,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self.start_plugin_command(plugin_id, command_id, target, false, cx) {
            self.error = Some(error.into());
            cx.notify();
        }
    }

    /// プラグインが呼んだツールを実行する。
    fn plugin_tool(
        &mut self,
        plugin: &str,
        permissions: &[Permission],
        tool: &str,
        args: &Value,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        if !self.unlocked(cx) {
            return Err("アプリがロックされています".into());
        }
        allowed(tool, permissions)?;
        let actor = Actor::plugin(plugin);
        self.db.update(cx, |db, cx| {
            // 自分が登録されていれば、コメントの書き手にする（AI と同じ）
            let author = db
                .store
                .people()
                .map_err(|e| e.to_string())?
                .into_iter()
                .find(|p| p.is_self)
                .map(|p| p.id);
            // 変更は、ツールの呼び出し 1 回を元に戻す 1 つの段にする（Ctrl+Z で戻せる）
            let write = tools::writes(tool);
            if write {
                db.store.undo_mark().map_err(|e| e.to_string())?;
            }
            let result = db.store.acting(actor, |store| {
                tools::execute(store, author.as_deref(), tool, args)
            });
            if write {
                let marked = db.store.undo_mark();
                cx.notify();
                marked.map_err(|e| e.to_string())?;
            }
            result
        })
    }

    // ---- AI のツール ----

    pub(crate) fn plugins_json(&mut self, cx: &mut Context<Self>) -> Value {
        refresh(cx);
        let plugins = get(cx);
        let list: Vec<Value> = plugins
            .found
            .plugins
            .iter()
            .map(|p| {
                let m = &p.manifest;
                json!({
                    "id": m.id,
                    "name": m.name,
                    "version": m.version,
                    "description": m.description,
                    "dir": p.dir.display().to_string(),
                    "state": plugins.state(p).code(),
                    "permissions": m.permissions.iter().map(|p| p.code()).collect::<Vec<_>>(),
                    "commands": m.commands.iter().map(|c| json!({
                        "id": c.id,
                        "title": c.title,
                        "on": match c.on { Place::Task => "task", Place::Workspace => "workspace" },
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        json!({
            "plugins": list,
            "folders": plugins.folders.iter().map(|f| json!({
                "path": f.path.display().to_string(),
                "from": source_code(f.from),
            })).collect::<Vec<_>>(),
            "problems": plugins.found.problems.iter().map(|p| json!({
                "path": p.path.display().to_string(),
                "text": p.text,
            })).collect::<Vec<_>>(),
        })
    }

    pub(crate) fn ai_plugin_tool(
        &mut self,
        name: &str,
        args: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Value, String> {
        let text = |key: &str| {
            args.get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("{key} が必要です"))
        };
        let flag = |key: &str| {
            args.get(key)
                .and_then(Value::as_bool)
                .ok_or_else(|| format!("{key}（true か false）が必要です"))
        };
        match name {
            "list_plugins" => Ok(self.plugins_json(cx)),
            "set_plugin_enabled" => {
                let id = text("plugin_id")?;
                if flag("enabled")? {
                    // 有効にするのは、ユーザーが承認の画面で押したときだけ
                    if !matches!(self.modal, crate::state::Modal::Appearance) {
                        self.open_appearance(window, cx);
                    }
                    self.show_appearance_section(
                        crate::state::CustomizeSection::Plugins,
                        window,
                        cx,
                    );
                    self.ask_enable_plugin(id, cx)?;
                    Ok(json!({"approval_shown": true}))
                } else {
                    refresh(cx);
                    if get(cx).plugin(id).is_none() && !get(cx).approved.contains_key(id) {
                        return Err(format!("プラグイン {id} が見つかりません"));
                    }
                    if !self.disable_plugin(id, cx) {
                        return Err(self.error_text());
                    }
                    Ok(json!({"plugin_id": id, "state": "off"}))
                }
            }
            "plugin_folder" => {
                let path = tools::expand_dir(text("path")?);
                let done = match text("action")? {
                    "add" => {
                        if !path.is_absolute() {
                            return Err("path はフォルダーの絶対パスにしてください".into());
                        }
                        self.add_plugin_folders(vec![path], cx)
                    }
                    "remove" => {
                        let dirs = self.extra_plugin_dirs(cx)?;
                        if !dirs.contains(&path) {
                            return Err(format!(
                                "{} は設定で足したフォルダーにありません",
                                path.display()
                            ));
                        }
                        self.remove_plugin_folder(&path, cx)
                    }
                    other => return Err(format!("action {other} はありません（add・remove）")),
                };
                if !done {
                    return Err(self.error_text());
                }
                Ok(self.plugins_json(cx))
            }
            "set_plugin_default_folder" => {
                if !self.set_default_plugin_folder(flag("enabled")?, cx) {
                    return Err(self.error_text());
                }
                Ok(self.plugins_json(cx))
            }
            other => Err(format!("{other} というツールはありません")),
        }
    }

    fn error_text(&self) -> String {
        self.error
            .as_ref()
            .map(|e| e.to_string())
            .unwrap_or_else(|| "保存できませんでした".into())
    }

    // ---- 画面 ----

    /// 設定の「プラグイン」。見つけたプラグインと、読めなかったもの。
    pub(crate) fn plugins_editor(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let plugins = get(cx);
        let mut list = col().gap_1();
        for (ix, plugin) in plugins.found.plugins.iter().enumerate() {
            let m = &plugin.manifest;
            let state = plugins.state(plugin);
            let id = m.id.clone();
            let mut about = format!("{}\n{}", m.id, plugin.dir.display());
            if !m.description.is_empty() {
                about = format!("{}\n{about}", m.description);
            }
            for dir in &plugin.shadowed {
                about.push_str(&format!("\n同じ ID で使っていないもの: {}", dir.display()));
            }
            let about: SharedString = about.into();
            list = list.child(
                row()
                    .id(("plugin", ix))
                    .h(px(36.))
                    .gap_2()
                    .px_2()
                    .rounded(feel::md())
                    .child(app_icon("puzzle").size_4().text_color(theme.muted_foreground))
                    .child(
                        row()
                            .id(("plugin-name", ix))
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .child(div().truncate().child(m.name.clone()))
                            .child(
                                div()
                                    .flex_none()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(m.version.clone()),
                            )
                            .tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(about.clone())
                                    .build(window, cx)
                            })
                            .test_support(),
                    )
                    .when(state == State::Changed, |d| {
                        d.child(
                            div()
                                .id(("plugin-changed", ix))
                                .flex_none()
                                .text_sm()
                                .text_color(theme.danger)
                                .child("変更あり")
                                .tooltip(|window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(
                                        "有効にしたあとでファイルが変わりました。もう一度有効にするまで動きません",
                                    )
                                    .build(window, cx)
                                })
                                .test_support(),
                        )
                    })
                    .child(
                        Switch::new(("plugin-switch", ix))
                            .checked(state == State::On)
                            .on_click(cx.listener(move |this, _: &bool, _, cx| {
                                this.toggle_plugin(&id, cx)
                            })),
                    )
                    .test_support(),
            );
        }
        if plugins.found.plugins.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("見つかりません。"),
            );
        }
        let problems = plugins
            .error
            .iter()
            .map(|e| (String::new(), e.clone()))
            .chain(
                plugins
                    .found
                    .problems
                    .iter()
                    .map(|p| (p.path.display().to_string(), p.text.clone())),
            )
            .collect::<Vec<_>>();
        col()
            .gap_3()
            .child(list)
            .when(!problems.is_empty(), |d| {
                d.child(
                    col()
                        .gap_1()
                        .px_2()
                        .children(problems.into_iter().map(|(path, text)| {
                            col()
                                .text_sm()
                                .child(div().text_color(theme.danger).child(text))
                                .when(!path.is_empty(), |d| {
                                    d.child(
                                        div()
                                            .text_color(theme.muted_foreground)
                                            .mono_font(cx)
                                            .child(path),
                                    )
                                })
                        })),
                )
            })
            .child(
                row().child(feel::gloss(
                    button("plugins-reload")
                        .icon(app_icon("refresh-cw"))
                        .framed(cx)
                        .tooltip("読み直す")
                        .on_click(cx.listener(|this, _, _, cx| this.plugins_changed(cx))),
                )),
            )
    }

    /// 設定の「プラグインのフォルダー」。
    pub(crate) fn plugin_folders_editor(&self, cx: &mut Context<Self>) -> Div {
        let theme = cx.theme().clone();
        let plugins = get(cx);
        let use_default = plugins.folders.iter().any(|f| f.from == Source::Default);
        let mut shown: Vec<Folder> = Vec::new();
        // 標準のフォルダーは、オフにしていても出して、スイッチで戻せるようにする
        if !use_default {
            if let Some(path) = malakt_plugins::default_dir() {
                shown.push(Folder {
                    path,
                    from: Source::Default,
                });
            }
        }
        shown.extend(plugins.folders.iter().cloned());
        shown.sort_by_key(|f| f.from != Source::Default);
        let mut list = col().gap_1();
        for (ix, folder) in shown.into_iter().enumerate() {
            let label = match folder.from {
                Source::Default => "標準",
                Source::Setting => "",
                Source::Env => ENV_DIRS,
            };
            let off = folder.from == Source::Default && !use_default;
            let open = folder.clone();
            let remove = folder.path.clone();
            list = list.child(
                row()
                    .h(px(32.))
                    .gap_2()
                    .px_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .mono_font(cx)
                            .when(off, |d| d.text_color(theme.muted_foreground))
                            .child(folder.path.display().to_string()),
                    )
                    .when(!label.is_empty(), |d| {
                        d.child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(label),
                        )
                    })
                    .child(feel::gloss(
                        button(("plugin-folder-open", ix))
                            .icon(app_icon("folder-open"))
                            .ghost()
                            .xsmall()
                            .tooltip("フォルダーを開く")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_plugin_folder(&open, cx)
                            })),
                    ))
                    .map(|d| match folder.from {
                        Source::Default => d.child(
                            Switch::new("plugin-default-switch")
                                .checked(use_default)
                                .on_click(cx.listener(move |this, _: &bool, _, cx| {
                                    this.set_default_plugin_folder(!use_default, cx);
                                })),
                        ),
                        Source::Setting => {
                            let view = cx.entity().downgrade();
                            d.child(remove_x(
                                ("plugin-folder-remove", ix),
                                "外す",
                                move |_, _, cx| {
                                    // 設定を閉じたあとなら、反映する先がない
                                    let _ = view.update(cx, |this, cx| {
                                        this.remove_plugin_folder(&remove, cx)
                                    });
                                },
                                cx,
                            ))
                        }
                        Source::Env => d,
                    }),
            );
        }
        col().gap_3().child(list).child(
            row().child(feel::gloss(
                button("plugin-folder-add")
                    .icon(app_icon("plus"))
                    .framed(cx)
                    .tooltip("フォルダーを足す")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.pick_plugin_folders(window, cx)),
                    ),
            )),
        )
    }

    /// 有効にする前に出す画面。名前と、許すことと、コマンドを出す場所。
    pub(crate) fn plugin_approval(&self, cx: &mut Context<Self>) -> (String, Div) {
        let theme = cx.theme().clone();
        let plugin = self
            .plugin_approving
            .as_ref()
            .and_then(|(id, _)| get(cx).plugin(id));
        let Some(plugin) = plugin else {
            return ("プラグイン".into(), col());
        };
        let m = &plugin.manifest;
        let line = |id: (&'static str, usize), icon: &'static str, text: String| {
            row()
                .id(id)
                .gap_2()
                .text_sm()
                .child(app_icon(icon).size_4().text_color(theme.muted_foreground))
                .child(div().flex_1().min_w_0().child(text))
                .test_support()
        };
        let mut lines = col().gap_2();
        for (ix, permission) in m.permissions.iter().enumerate() {
            let icon = match permission {
                Permission::Read => "eye",
                Permission::Write => "pencil",
            };
            lines = lines.child(line(
                ("plugin-permission", ix),
                icon,
                permission.label().to_owned(),
            ));
        }
        for (ix, command) in m.commands.iter().enumerate() {
            let place = match command.on {
                Place::Task => "タスク",
                Place::Workspace => "ワークスペース",
            };
            lines = lines.child(line(
                ("plugin-command", ix),
                "mouse-pointer-click",
                format!("{place}の右クリックに「{}」", command.title),
            ));
        }
        let body = col()
            .gap_4()
            .when(!m.description.is_empty(), |d| {
                d.child(div().text_sm().child(m.description.clone()))
            })
            .child(lines)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .mono_font(cx)
                    .child(plugin.dir.display().to_string()),
            )
            .child(
                row()
                    .gap_2()
                    .justify_end()
                    .child(feel::gloss(
                        button("plugin-approve-cancel")
                            .label("キャンセル")
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_enable_plugin(cx))),
                    ))
                    .child(feel::gloss(
                        button("plugin-approve")
                            .icon(app_icon("puzzle"))
                            .label("有効にする")
                            .primary()
                            .disabled(self.verifying(cx))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.confirm_enable_plugin(window, cx)
                            })),
                    )),
            );
        (format!("{} {}", m.name, m.version), body)
    }
}

fn source_code(from: Source) -> &'static str {
    match from {
        Source::Default => "default",
        Source::Setting => "setting",
        Source::Env => "env",
    }
}

/// 右クリックのメニューに、有効なプラグインのコマンドを足す。
pub(crate) fn menu_items(
    mut menu: PopupMenu,
    commands: &[(String, String, String)],
    view: &WeakEntity<MainView>,
    target: &Target,
) -> PopupMenu {
    if commands.is_empty() {
        return menu;
    }
    menu = menu.separator();
    for (plugin, command, title) in commands {
        let (view, plugin, command, target) = (
            view.clone(),
            plugin.clone(),
            command.clone(),
            target.clone(),
        );
        menu = menu.item(
            PopupMenuItem::new(title.clone())
                .icon(app_icon("puzzle"))
                .on_click(move |_, _, cx| {
                    // ウィンドウを閉じたあとなら、反映する先がない
                    let _ = view.update(cx, |this, cx| {
                        this.run_plugin_from_menu(&plugin, &command, target.clone(), cx)
                    });
                }),
        );
    }
    menu
}

/// AI の `run_plugin_command`。結果はコマンドが終わってから届く。
pub fn run_from_ai(
    args: &Value,
    cx: &mut App,
) -> Result<async_channel::Receiver<Result<Outcome, String>>, String> {
    let text = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("{key} が必要です"))
    };
    let (plugin, command) = (
        text("plugin_id")?.to_owned(),
        text("command_id")?.to_owned(),
    );
    let target = match (
        args.get("task_id").and_then(Value::as_str),
        args.get("workspace_id").and_then(Value::as_str),
    ) {
        (Some(id), None) => Target::Task(id.to_owned()),
        (None, Some(id)) => Target::Workspace(id.to_owned()),
        (Some(_), Some(_)) => {
            return Err("task_id と workspace_id は、どちらか 1 つにしてください".into())
        }
        (None, None) => return Err("task_id か workspace_id が必要です".into()),
    };
    let (window, view) = crate::ai_tools::target(cx).ok_or("メインのウィンドウが開いていません")?;
    window
        .update(cx, |_, _, cx| {
            view.update(cx, |this, cx| {
                this.start_plugin_command(&plugin, &command, target, true, cx)
            })
        })
        .map_err(|_| "メインのウィンドウが閉じられました".to_owned())?
}

/// AI に返す、コマンドの結果。
pub fn outcome_json(outcome: &Outcome) -> String {
    json!({"notice": outcome.notice, "copied": outcome.copy}).to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use malakt_core::Store;
    use malakt_plugins::Permission;

    use super::{
        allowed, read_settings, save_approved, save_dirs, Approval, Settings, SETTING_APPROVED,
        SETTING_DEFAULT, SETTING_DIRS,
    };

    #[test]
    fn plugins_call_only_the_tools_their_permissions_allow() {
        let read = [Permission::Read];
        let both = [Permission::Read, Permission::Write];
        assert!(allowed("get_task", &read).is_ok());
        assert!(allowed("list_tasks", &read).is_ok());
        assert!(allowed("update_task", &read)
            .unwrap_err()
            .contains("\"write\""));
        assert!(allowed("update_task", &both).is_ok());
        // 読むだけに見えないものは、書き込みとして扱う
        assert!(allowed("pomodoro", &read).is_err());
        assert!(allowed("get_task", &[Permission::Write]).is_err());
        // PC のファイルを読むもの、アプリの中の AI だけのものは呼べない
        for tool in [
            "add_attachment",
            "add_schedule_attachment",
            "run_shell",
            "github_fetch",
        ] {
            assert!(
                allowed(tool, &both).unwrap_err().contains("使えません"),
                "{tool}"
            );
        }
        assert!(allowed("set_panels", &both)
            .unwrap_err()
            .contains("ありません"));
    }

    #[test]
    fn settings_are_read_back() {
        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(
            read_settings(&store).unwrap(),
            Settings {
                use_default: true,
                ..Default::default()
            }
        );
        let mut approved = BTreeMap::new();
        approved.insert(
            "a".to_owned(),
            Approval {
                hash: "h".into(),
                permissions: vec!["read".into()],
            },
        );
        save_approved(&mut store, &approved).unwrap();
        save_dirs(&mut store, &[PathBuf::from("/p"), PathBuf::from("/日本語")]).unwrap();
        store.set_setting(SETTING_DEFAULT, Some("off")).unwrap();
        let read = read_settings(&store).unwrap();
        assert!(!read.use_default);
        assert_eq!(read.dirs, [PathBuf::from("/p"), PathBuf::from("/日本語")]);
        assert_eq!(read.approved, approved);
        // 空にしたら設定ごと消す
        save_dirs(&mut store, &[]).unwrap();
        save_approved(&mut store, &BTreeMap::new()).unwrap();
        assert_eq!(store.setting(SETTING_DIRS).unwrap(), None);
        assert_eq!(store.setting(SETTING_APPROVED).unwrap(), None);
        store.set_setting(SETTING_DIRS, Some("{")).unwrap();
        assert!(read_settings(&store).unwrap_err().contains("フォルダー"));
    }
}
