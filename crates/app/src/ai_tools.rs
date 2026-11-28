//! アプリの画面と設定を、AI チャットから操作するためのツール。
//!
//! データベースだけで済む操作（タスク、ワークスペース、担当者、コメントなど）は
//! `malakt_ai::tools` にあり、MCP でつなぐ外のエージェントにも同じものを出す。
//! ここにあるのは、画面（今見ている場所、一覧の並び、パネル、ペイン）と、
//! カスタマイズ、AI の設定、ロック、エクスポートのように、アプリの中でしかできない操作。

use gpui_kit::*;
use malakt_ai::api::{self, Group};
use malakt_ai::wire::ToolDef;
use malakt_core::view::{self, AssigneeFilter, DueFormat, Priority, Sort, Tab};
use malakt_core::{archive, Status};
use serde_json::{json, Value};

use crate::ai::{Session, Watch, WATCH_MINUTES};
use crate::state::{Column, MainView, Modal, Nav};
use crate::theme::{self, Choice, Density, Feel, Layout, Preset, Scene, Slot};

fn mark(name: &str, pick: fn(&api::Tool) -> bool) -> bool {
    api::tool(Group::App, name).is_some_and(pick)
}

/// 画面を動かすツールか（api/app.tsp の `@view`）。メインのウィンドウで実行する。
pub fn is_view_tool(name: &str) -> bool {
    mark(name, |t| t.view)
}

/// AI の設定を変えるツールか（api/app.tsp の `@session`）。チャットで実行する。
pub fn is_session_tool(name: &str) -> bool {
    mark(name, |t| t.session)
}

pub fn is_app_tool(name: &str) -> bool {
    is_view_tool(name)
        || is_session_tool(name)
        || name == crate::browser::READ_TOOL
        || name == crate::plugins::RUN_TOOL
}

const SORTS: [(&str, Sort); 8] = [
    ("due", Sort::Due),
    ("priority", Sort::Priority),
    ("updated", Sort::Updated),
    ("created", Sort::Created),
    ("title", Sort::Title),
    ("status", Sort::Status),
    ("assignee", Sort::Assignee),
    ("manual", Sort::Manual),
];

const COLUMNS: [(&str, Column); 5] = [
    ("status", Column::Status),
    ("priority", Column::Priority),
    ("assignee", Column::Assignee),
    ("due", Column::Due),
    ("updated", Column::Updated),
];

const MODES: [(&str, Choice); 3] = [
    ("system", Choice::System),
    ("light", Choice::Light),
    ("dark", Choice::Dark),
];

const DENSITIES: [(&str, Density); 3] = [
    ("roomy", Density::Roomy),
    ("standard", Density::Standard),
    ("compact", Density::Compact),
];

const FEELS: [(&str, Feel); 4] = [
    ("standard", Feel::Standard),
    ("glass", Feel::Glass),
    ("crisp", Feel::Crisp),
    ("tint", Feel::Tint),
];

const DUE_FORMATS: [(&str, DueFormat); 2] =
    [("date", DueFormat::Date), ("relative", DueFormat::Relative)];

const SCENES: [(&str, Scene); 4] = [
    ("none", Scene::None),
    ("sky", Scene::Sky),
    ("forest", Scene::Forest),
    ("dusk", Scene::Dusk),
];

const PRIORITIES: [(&str, Priority); 4] = [
    ("none", Priority::None),
    ("high", Priority::High),
    ("mid", Priority::Mid),
    ("low", Priority::Low),
];

fn key_of<T: PartialEq + Copy>(table: &[(&'static str, T)], value: T) -> &'static str {
    table
        .iter()
        .find(|(_, v)| *v == value)
        .map_or("", |(k, _)| k)
}

fn parse_in<T: Clone>(table: &[(&'static str, T)], key: &str, what: &str) -> Result<T, String> {
    table
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.clone())
        .ok_or_else(|| {
            let keys: Vec<_> = table.iter().map(|(k, _)| *k).collect();
            format!("{what} は {} のどれかです: {key}", keys.join(" / "))
        })
}

fn names<T>(table: &[(&'static str, T)]) -> Vec<&'static str> {
    table.iter().map(|(k, _)| *k).collect()
}

/// AI に見せる、アプリのツールの一覧。定義は api/app.tsp。
pub fn definitions() -> Vec<ToolDef> {
    api::definitions(Group::App)
}

/// 実行の前にユーザーの確認が要るか。
/// いつも確認するもの（`@confirm`）は api/app.tsp に、中身で決まるものはここに書く。
pub fn needs_confirmation(name: &str, args: &Value) -> bool {
    if mark(name, |t| t.confirm) {
        return true;
    }
    match name {
        "export_data" => export_path(args).is_ok_and(|p| p.exists()),
        "set_ai_settings" => args.get("auto_run").and_then(Value::as_bool) == Some(true),
        _ => false,
    }
}

/// 確認に出す文。
pub fn confirmation_text(name: &str, args: &Value) -> String {
    let arg = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or_default();
    match name {
        "delete_theme" => format!("テーマ「{}」を削除します。元に戻せません。", arg("name")),
        "import_data" => format!(
            "{} を取り込みます。ID が同じタスクなどは、ファイルの中身で上書きされます。",
            arg("path")
        ),
        "export_data" => format!(
            "{} を上書きします。",
            export_path(args).map_or_else(|e| e, |p| p.display().to_string())
        ),
        "set_ai_settings" => {
            "オート実行をオンにします。オンの間、コマンドの実行と削除を確認なしで行います。".into()
        }
        other => format!("{other} を実行します。"),
    }
}

/// 画面に出す、ツールの呼び出しの短い説明。アプリのツールでなければ `None`。
pub fn summary(name: &str, args: &Value) -> Option<String> {
    let arg = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or_default();
    Some(match name {
        "get_screen" => "画面の状態を取得".into(),
        "navigate" => "画面を移動".into(),
        "open_task" => "タスクを開く".into(),
        "close_task" => "タスクを閉じる".into(),
        "set_list_view" => "一覧の表示を変更".into(),
        "set_columns" => "表の列を変更".into(),
        "set_panels" => "画面の部分を変更".into(),
        "pane" => "ペインを操作".into(),
        "open_dialog" => "ダイアログを開く".into(),
        "open_window" => match arg("kind") {
            "pomodoro" => "ポモドーロのウィンドウを開く".into(),
            _ => "新しいウィンドウを開く".into(),
        },
        "open_terminal" => "ターミナルを開く".into(),
        "browser" => match arg("action") {
            "open" => format!("ブラウザで開く: {}", arg("url")),
            "navigate" => format!("ブラウザで移動: {}", arg("url")),
            "open_external" => "OS のブラウザで開く".into(),
            "zoom_in" | "zoom_out" | "zoom_reset" => "ブラウザの拡大率を変更".into(),
            _ => "ブラウザを操作".into(),
        },
        "read_browser" => "ブラウザのページを読む".into(),
        "lock" => "ロックを操作".into(),
        "set_app_options" => "アプリの設定を変更".into(),
        "update_now" => "アップデートして再起動".into(),
        "export_data" => "エクスポート".into(),
        "import_data" => format!("インポート: {}", arg("path")),
        "undo" => "元に戻す".into(),
        "redo" => "やり直す".into(),
        "find_icons" => format!("アイコンを検索: {}", arg("query")),
        "set_workspace_icon" => "ワークスペースのアイコンを変更".into(),
        "get_customization" => "カスタマイズを取得".into(),
        "list_fonts" => "書体の一覧を取得".into(),
        "apply_theme" => format!("テーマを変更: {}", arg("name")),
        "save_theme" => format!("テーマを保存: {}", arg("name")),
        "rename_theme" => format!("テーマの名前を変更: {}", arg("new_name")),
        "delete_theme" => format!("テーマを削除: {}", arg("name")),
        "apply_layout" => format!("レイアウトを変更: {}", arg("name")),
        "set_appearance" => "見た目を変更".into(),
        "get_ai_settings" => "AI の設定を取得".into(),
        "set_ai_settings" => "AI の設定を変更".into(),
        "check_github" => "GitHub を確認".into(),
        "list_plugins" => "プラグインの一覧を取得".into(),
        "run_plugin_command" => format!("プラグインのコマンドを実行: {}", arg("command_id")),
        "set_plugin_enabled" => match args.get("enabled").and_then(Value::as_bool) {
            Some(true) => format!("プラグインを有効にする: {}", arg("plugin_id")),
            _ => format!("プラグインを無効にする: {}", arg("plugin_id")),
        },
        "plugin_folder" => format!("プラグインのフォルダーを変更: {}", arg("path")),
        "set_plugin_default_folder" => "プラグインの標準のフォルダーを変更".into(),
        _ => return None,
    })
}

fn export_path(args: &Value) -> Result<std::path::PathBuf, String> {
    match args.get("path").and_then(Value::as_str).map(str::trim) {
        Some(path) if !path.is_empty() => {
            let path = malakt_ai::tools::expand_dir(path);
            if path.is_dir() {
                Ok(path.join(default_export_name()))
            } else {
                Ok(path)
            }
        }
        _ => Ok(dirs::document_dir()
            .or_else(dirs::home_dir)
            .ok_or("書き出す場所が決まりません。path を渡してください")?
            .join(default_export_name())),
    }
}

fn default_export_name() -> String {
    format!("malakt-mahoz-{}.zip", view::today())
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn bool_arg(args: &Value, key: &str) -> Result<Option<bool>, String> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(other) => Err(format!("{key} が正しくありません: {other}")),
    }
}

/// 数か null。null は `Some(None)`。
fn width_arg(args: &Value, key: &str, min: f32, max: f32) -> Result<Option<Option<f32>>, String> {
    match args.get(key) {
        None => Ok(None),
        Some(Value::Null) => Ok(Some(None)),
        Some(v) => {
            let n = v
                .as_f64()
                .ok_or_else(|| format!("{key} が正しくありません: {v}"))?
                as f32;
            if !(min..=max).contains(&n) {
                return Err(format!("{key} は {min}〜{max} です: {n}"));
            }
            Ok(Some(Some(n.round())))
        }
    }
}

fn required<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    str_arg(args, key)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{key} が必要です"))
}

// ---- どのウィンドウで動かすか ----

/// 開いているメインのウィンドウ。AI の操作は、いちばん最近使ったものに届く。
#[derive(Default)]
struct Views {
    list: Vec<(AnyWindowHandle, WeakEntity<MainView>)>,
}

impl Global for Views {}

/// メインのウィンドウを登録する。手前に出たときにも呼び、いちばん後ろを最近のものにする。
pub fn register(window: AnyWindowHandle, view: WeakEntity<MainView>, cx: &mut App) {
    let views = cx.default_global::<Views>();
    views
        .list
        .retain(|(_, v)| v.entity_id() != view.entity_id() && v.upgrade().is_some());
    views.list.push((window, view));
}

/// 操作を届けるウィンドウ。手前にあるメインのウィンドウか、いちばん最近使ったもの。
/// ほかのウィンドウの中に置かれたペインは、そのウィンドウのメインのビューが受け持つ。
pub(crate) fn target(cx: &mut App) -> Option<(AnyWindowHandle, Entity<MainView>)> {
    let active = cx.active_window();
    let list: Vec<(AnyWindowHandle, Entity<MainView>)> = {
        let views = cx.default_global::<Views>();
        views.list.retain(|(_, v)| v.upgrade().is_some());
        views
            .list
            .iter()
            .filter_map(|(w, v)| Some((*w, v.upgrade()?)))
            .collect()
    };
    let list: Vec<_> = list
        .into_iter()
        .filter(|(_, v)| !v.read(cx).embedded)
        .collect();
    list.iter()
        .find(|(w, _)| Some(*w) == active)
        .or_else(|| list.last())
        .cloned()
}

/// シェルの場所（ターミナルとブラウザのタブ）を見える所に出す。`window` のメインの画面か、
/// なければ最近使ったメインのウィンドウで出す。
pub fn reveal_shell(window: &mut Window, cx: &mut App) {
    let here = window.window_handle();
    let views: Vec<Entity<MainView>> = cx
        .default_global::<Views>()
        .list
        .iter()
        .filter(|(w, _)| *w == here)
        .filter_map(|(_, v)| v.upgrade())
        .collect();
    let own = views.into_iter().find(|v| !v.read(cx).embedded);
    match own {
        Some(view) => view.update(cx, |view, cx| view.reveal_shell(window, cx)),
        None => {
            if let Some((other, view)) = target(cx) {
                // ウィンドウを閉じていれば、出す先がない
                let _ = other.update(cx, |_, window, cx| {
                    view.update(cx, |view, cx| view.reveal_shell(window, cx))
                });
            }
        }
    }
}

/// 画面のツールを、メインのウィンドウで実行する。
pub fn run_view_tool(name: &str, args: &Value, cx: &mut App) -> Result<String, String> {
    let (window, view) = target(cx).ok_or("メインのウィンドウが開いていません")?;
    window
        .update(cx, |_, window, cx| {
            view.update(cx, |this, cx| this.run_ai_tool(name, args, window, cx))
        })
        .map_err(|_| "メインのウィンドウが閉じられました".to_owned())?
}

// ---- 画面 ----

fn nav_json(nav: &Nav) -> Value {
    match nav {
        Nav::Home => json!("home"),
        Nav::All => json!("all"),
        Nav::Unfiled => json!("unfiled"),
        Nav::Workspace(id) => json!({"workspace_id": id}),
    }
}

fn status_name(status: Status) -> &'static str {
    status.as_str()
}

impl MainView {
    /// 今のペインのビュー（分けて増えたペインなら、その中身）。
    fn ai_pane_view(&self) -> Option<Entity<MainView>> {
        self.focused_child()
    }

    /// AI のツールを 1 つ実行する。返すのは AI に渡す文字列。
    pub fn run_ai_tool(
        &mut self,
        name: &str,
        args: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        // 一覧と詳細の操作は、今のペインに届ける
        if matches!(
            name,
            "navigate" | "open_task" | "close_task" | "set_list_view" | "set_columns"
        ) {
            if let Some(child) = self.ai_pane_view() {
                let result = child.update(cx, |c, cx| c.pane_tool(name, args, window, cx));
                cx.notify();
                return result;
            }
            return self.pane_tool(name, args, window, cx);
        }
        let value = match name {
            "get_screen" => self.screen_json(cx),
            "set_panels" => self.ai_set_panels(args, window, cx)?,
            "pane" => self.ai_pane(args, window, cx)?,
            "open_dialog" => self.ai_open_dialog(args, window, cx)?,
            "list_plugins"
            | "set_plugin_enabled"
            | "plugin_folder"
            | "set_plugin_default_folder" => self.ai_plugin_tool(name, args, window, cx)?,
            "open_window" => match str_arg(args, "kind").unwrap_or("main") {
                "main" => {
                    crate::open_main_window(cx)
                        .map_err(|e| format!("ウィンドウを開けませんでした: {e}"))?;
                    json!({"opened": true})
                }
                "pomodoro" => {
                    // ウィンドウの更新中なので、終わってから開く
                    cx.defer(|cx| {
                        if let Err(e) = crate::pomodoro::open(cx) {
                            crate::pomodoro::shared(cx).update(cx, |p, cx| {
                                p.fail(format!("ポモドーロのウィンドウを開けませんでした: {e}"), cx)
                            });
                        }
                    });
                    json!({"opened": "pomodoro"})
                }
                other => return Err(format!("kind は main / pomodoro のどちらかです: {other}")),
            },
            "open_terminal" => self.ai_open_terminal(args, window, cx)?,
            "browser" => {
                let mut reveal = false;
                let value = crate::browser::run_tool(args, &mut reveal, cx)?;
                if reveal {
                    self.reveal_shell(window, cx);
                }
                value
            }
            "lock" => match required(args, "action")? {
                "lock_now" => {
                    crate::db::lock_now(cx);
                    json!({"locked": true})
                }
                "enable" => {
                    self.enable_lock(window, cx);
                    json!({"asked": "OS の認証が出ています。通ると有効になります"})
                }
                "disable" => {
                    self.disable_lock(window, cx);
                    json!({"asked": "OS の認証が出ています。通ると無効になります"})
                }
                "set_options" => self.ai_set_lock_options(args, cx)?,
                other => return Err(format!("action が正しくありません: {other}")),
            },
            "set_app_options" => self.ai_set_options(args, cx)?,
            "update_now" => {
                if crate::update::ready_version(cx).is_none() {
                    return Err("入れ替えられる新しい版はまだありません".into());
                }
                crate::update::install_and_restart(cx)?;
                json!({"restarting": true})
            }
            "export_data" => {
                let path = export_path(args)?;
                let manifest = archive::export_to_path(&self.db.read(cx).store, &path)
                    .map_err(|e| e.to_string())?;
                self.show_notice(
                    format!(
                        "{} 件のタスクを {} に書き出しました。",
                        manifest.counts.tasks,
                        path.display()
                    )
                    .into(),
                );
                cx.notify();
                json!({"path": path.display().to_string(), "tasks": manifest.counts.tasks})
            }
            "import_data" => {
                let path = malakt_ai::tools::expand_dir(required(args, "path")?);
                let manifest = self
                    .write(cx, |s| archive::import_from_path(s, &path))
                    .ok_or_else(|| self.take_error("取り込めませんでした"))?;
                self.show_notice(
                    format!("{} 件のタスクを取り込みました。", manifest.counts.tasks).into(),
                );
                json!({"tasks": manifest.counts.tasks})
            }
            "undo" | "redo" => {
                let redo = name == "redo";
                let done = self.db.update(cx, |db, cx| {
                    let done = if redo {
                        db.store.redo()
                    } else {
                        db.store.undo()
                    };
                    cx.notify();
                    done
                });
                let done = done.map_err(|e| e.to_string())?;
                if done {
                    self.show_notice(
                        if redo {
                            "やり直しました。"
                        } else {
                            "元に戻しました。"
                        }
                        .into(),
                    );
                }
                cx.notify();
                json!({"done": done})
            }
            "find_icons" => {
                let limit = args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .map_or(30, |n| n.clamp(1, 100) as usize);
                json!(crate::icons::search(
                    required(args, "query")?,
                    None,
                    &[],
                    limit
                ))
            }
            "set_workspace_icon" => {
                let id = required(args, "id")?;
                let ws = self
                    .db
                    .read(cx)
                    .store
                    .workspace(id)
                    .map_err(|e| e.to_string())?;
                let pick = |key: &str, now: Option<String>| -> Result<Option<String>, String> {
                    match args.get(key) {
                        None => Ok(now),
                        Some(Value::Null) => Ok(None),
                        Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
                        Some(Value::String(s)) => Ok(Some(s.trim().to_owned())),
                        Some(other) => Err(format!("{key} が正しくありません: {other}")),
                    }
                };
                let mut icon = pick("icon", ws.icon)?;
                let color = pick("color", ws.color)?;
                if let Some(path) = str_arg(args, "image_path").filter(|p| !p.trim().is_empty()) {
                    if args.get("icon").is_some() {
                        return Err("icon と image_path は一緒に渡せません".into());
                    }
                    let path = malakt_ai::tools::expand_dir(path.trim());
                    let png = crate::icons::shrink_file(&path)?;
                    icon = Some(
                        self.write(cx, |s| s.add_custom_icon(&png))
                            .ok_or_else(|| self.take_error("画像をアイコンにできませんでした"))?,
                    );
                }
                if let Some(icon) = &icon {
                    if !malakt_core::icon::is_custom(icon) && crate::icons::svg(icon).is_none() {
                        return Err(format!(
                            "アイコン「{icon}」はありません。find_icons で探してください"
                        ));
                    }
                }
                let saved = self
                    .write(cx, |s| {
                        s.set_workspace_icon(id, icon.as_deref(), color.as_deref())
                    })
                    .ok_or_else(|| self.take_error("アイコンを変えられませんでした"))?;
                json!({"id": saved.id, "icon": saved.icon, "color": saved.color})
            }
            "get_customization" => self.customization_json(cx),
            "list_fonts" => {
                let query = str_arg(args, "query").unwrap_or_default().to_lowercase();
                let fonts: Vec<String> = theme::fonts(cx)
                    .into_iter()
                    .filter(|f| f.to_lowercase().contains(query.trim()))
                    .take(200)
                    .collect();
                json!(fonts)
            }
            "apply_theme" => {
                let preset = self.find_theme(required(args, "name")?, cx)?;
                theme::apply_preset(&preset, window, cx)?;
                self.after_look_change(window, cx);
                json!({"applied": preset.name})
            }
            "save_theme" => {
                let name = required(args, "name")?.trim().to_owned();
                let mut presets = theme::own_presets(cx)?;
                if presets.iter().any(|p| p.name == name) {
                    return Err(format!("「{name}」という名前のテーマがもうあります"));
                }
                presets.push(theme::current(&name, cx));
                self.ai_store_presets(presets, cx)?;
                json!({"saved": name})
            }
            "rename_theme" => {
                let name = required(args, "name")?;
                let new_name = required(args, "new_name")?.trim().to_owned();
                let mut presets = theme::own_presets(cx)?;
                if presets.iter().any(|p| p.name == new_name) {
                    return Err(format!("「{new_name}」という名前のテーマがもうあります"));
                }
                let preset = presets
                    .iter_mut()
                    .find(|p| p.name == name)
                    .ok_or_else(|| format!("自分のテーマに「{name}」はありません"))?;
                preset.name = new_name.clone();
                self.ai_store_presets(presets, cx)?;
                json!({"renamed": new_name})
            }
            "delete_theme" => {
                let name = required(args, "name")?;
                let mut presets = theme::own_presets(cx)?;
                let before = presets.len();
                presets.retain(|p| p.name != name);
                if presets.len() == before {
                    return Err(format!("自分のテーマに「{name}」はありません"));
                }
                self.ai_store_presets(presets, cx)?;
                json!({"deleted": name})
            }
            "apply_layout" => {
                let name = required(args, "name")?;
                let layout = Layout::PATTERNS
                    .iter()
                    .find(|l| l.name == name || l.name.replace(' ', "") == name.replace(' ', ""))
                    .ok_or_else(|| {
                        let all: Vec<_> = Layout::PATTERNS.iter().map(|l| l.name).collect();
                        format!("レイアウトは {} のどれかです: {name}", all.join(" / "))
                    })?;
                theme::apply_layout(layout, cx).map_err(|e| e.to_string())?;
                if layout
                    .chat
                    .is_some_and(|wanted| wanted != self.ai_docked(cx))
                {
                    self.toggle_ai(window, cx);
                }
                cx.notify();
                json!({"applied": layout.name})
            }
            "set_appearance" => self.ai_set_appearance(args, window, cx)?,
            other => return Err(format!("{other} というツールはありません")),
        };
        Ok(value.to_string())
    }

    /// 書き込みで出た失敗を、画面に残さず AI に返す。
    fn take_error(&mut self, what: &str) -> String {
        match self.error.take() {
            Some(error) => error.to_string(),
            None => what.to_owned(),
        }
    }

    /// 一覧と詳細の操作。このペインの中で動かす。
    fn pane_tool(
        &mut self,
        name: &str,
        args: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        let value = match name {
            "navigate" => {
                let to = required(args, "to")?;
                let nav = match to {
                    "home" => Nav::Home,
                    "all" => Nav::All,
                    "unfiled" => Nav::Unfiled,
                    id => {
                        self.db
                            .read(cx)
                            .store
                            .workspace(id)
                            .map_err(|e| e.to_string())?;
                        Nav::Workspace(id.to_owned())
                    }
                };
                self.go_here(nav, cx);
                json!({"shown": nav_json(&self.nav)})
            }
            "open_task" => {
                let id = required(args, "id")?;
                let task = self.db.read(cx).store.task(id).map_err(|e| e.to_string())?;
                self.open_panel(id, window, cx);
                json!({"opened": task.title})
            }
            "close_task" => {
                self.close_panel(cx);
                json!({"closed": true})
            }
            "set_list_view" => {
                self.ai_set_list_view(args, window, cx)?;
                self.list_json()
            }
            "set_columns" => {
                self.ai_set_columns(args, cx)?;
                self.columns_json()
            }
            other => return Err(format!("{other} というツールはありません")),
        };
        Ok(value.to_string())
    }

    fn list_json(&self) -> Value {
        let q = &self.query;
        json!({
            "tab": if q.tab == Tab::Closed { "closed" } else { "open" },
            "query": q.q,
            "status": q.status.map(status_name),
            "assignee": match &q.assignee {
                AssigneeFilter::All => Value::Null,
                AssigneeFilter::Me => json!("me"),
                AssigneeFilter::Unassigned => json!("unassigned"),
                AssigneeFilter::Person(id) => json!(id),
            },
            "priority": q.priority.map(|p| key_of(&PRIORITIES, p)),
            "category": q.category,
            "sort": key_of(&SORTS, q.sort),
            "descending": q.descending,
            "shown_tasks": self.visible.len(),
            "home_mine_only": self.due_mine,
        })
    }

    fn columns_json(&self) -> Value {
        self.columns
            .iter()
            .map(|c| {
                json!({
                    "column": key_of(&COLUMNS, c.column),
                    "label": c.column.label(),
                    "shown": !c.hidden,
                    "width": c.width,
                })
            })
            .collect()
    }

    fn screen_json(&self, cx: &App) -> Value {
        let pane = self.ai_pane_view();
        let (nav, panel, list, columns) = match &pane {
            Some(child) => {
                let c = child.read(cx);
                (
                    c.nav.clone(),
                    c.panel.clone(),
                    c.list_json(),
                    c.columns_json(),
                )
            }
            None => (
                self.nav.clone(),
                self.panel.clone(),
                self.list_json(),
                self.columns_json(),
            ),
        };
        let session = self.ai_session.read(cx);
        let shell = match session.shell_place {
            crate::ai::ShellPlace::Closed => "closed",
            crate::ai::ShellPlace::Docked => "docked",
            crate::ai::ShellPlace::Window(_) => "window",
        };
        let chat = if session.chat_window.is_some() {
            "window"
        } else if self.ai_open {
            "docked"
        } else {
            "closed"
        };
        let terms = crate::term::shared(cx).read(cx);
        let shells: Vec<Value> = terms
            .profiles()
            .iter()
            .map(|p| json!({"id": p.id, "name": p.name}))
            .collect();
        json!({
            "shown": nav_json(&nav),
            "open_task_id": panel,
            "list": list,
            "columns": columns,
            "sidebar": !self.sidebar_hidden,
            "sidebar_width": self.sidebar_width,
            "detail_width": self.panel_width,
            "collapsed_workspace_ids": self.collapsed.iter().collect::<Vec<_>>(),
            "chat": chat,
            "shell": shell,
            "panes": self.layout.panes().len(),
            "pane_zoomed": self.layout.zoomed(),
            "pane_keys": self.pane_keys.code(),
            "saved_pane_layouts": self.pane_presets.iter().map(|(n, _)| n).collect::<Vec<_>>(),
            "pane_layouts": malakt_core::panes::PRESETS.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            "lock_enabled": self.lock_enabled(cx),
            "lock": self.lock_json(cx),
            "reminders": crate::reminders::enabled(cx),
            "autostart": crate::autostart::enabled(cx),
            "auto_update": crate::update::enabled(cx),
            "update_ready": crate::update::ready_version(cx).map(|v| v.to_string()),
            "terminal_shells": shells,
            "dialog_open": self.modal != Modal::None,
        })
    }

    fn ai_set_list_view(
        &mut self,
        args: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let mut q = self.query.clone();
        if bool_arg(args, "clear_filters")? == Some(true) {
            q.clear_filters();
        }
        if let Some(tab) = str_arg(args, "tab") {
            q.tab = match tab {
                "open" => Tab::Open,
                "closed" => Tab::Closed,
                other => return Err(format!("tab は open / closed のどれかです: {other}")),
            };
        }
        if let Some(text) = str_arg(args, "query") {
            q.q = text.to_owned();
        }
        match args.get("status") {
            None => {}
            Some(Value::Null) => q.status = None,
            Some(Value::String(s)) => {
                q.status = Some(
                    Status::parse(s).ok_or_else(|| format!("status が正しくありません: {s}"))?,
                )
            }
            Some(other) => return Err(format!("status が正しくありません: {other}")),
        }
        match args.get("assignee") {
            None => {}
            Some(Value::Null) => q.assignee = AssigneeFilter::All,
            Some(Value::String(s)) => {
                q.assignee = match s.as_str() {
                    "" | "all" => AssigneeFilter::All,
                    "me" => AssigneeFilter::Me,
                    "unassigned" => AssigneeFilter::Unassigned,
                    id => {
                        if !self.people.iter().any(|p| p.id == id) {
                            return Err(format!("担当者が見つかりません: {id}"));
                        }
                        AssigneeFilter::Person(id.to_owned())
                    }
                }
            }
            Some(other) => return Err(format!("assignee が正しくありません: {other}")),
        }
        match args.get("priority") {
            None => {}
            Some(Value::Null) => q.priority = None,
            Some(Value::String(s)) => q.priority = Some(parse_in(&PRIORITIES, s, "priority")?),
            Some(other) => return Err(format!("priority が正しくありません: {other}")),
        }
        match args.get("category") {
            None => {}
            Some(Value::Null) => q.category = None,
            Some(Value::String(s)) if s.is_empty() => q.category = None,
            Some(Value::String(s)) => q.category = Some(s.clone()),
            Some(other) => return Err(format!("category が正しくありません: {other}")),
        }
        if let Some(sort) = str_arg(args, "sort") {
            q.sort = parse_in(&SORTS, sort, "sort")?;
        }
        if let Some(desc) = bool_arg(args, "descending")? {
            q.descending = desc;
        }
        let mine = bool_arg(args, "home_mine_only")?;
        // 検索欄にも、同じキーワードを出す
        if q.q != self.query.q {
            let text = q.q.clone();
            self.inputs
                .search
                .update(cx, |s, cx| s.set_value(text, window, cx));
        }
        self.query = q;
        if let Some(mine) = mine {
            self.due_mine = mine;
        }
        self.reload(cx);
        Ok(())
    }

    fn ai_set_columns(&mut self, args: &Value, cx: &mut Context<Self>) -> Result<(), String> {
        let mut columns = self.columns.clone();
        if let Some(list) = args.get("columns") {
            let list = list.as_array().ok_or("columns が正しくありません")?;
            for item in list {
                let key = item
                    .get("column")
                    .and_then(Value::as_str)
                    .ok_or("column が必要です")?;
                let column = parse_in(&COLUMNS, key, "column")?;
                let width = width_arg(
                    item,
                    "width",
                    crate::state::COLUMN_MIN,
                    crate::state::COLUMN_MAX,
                )?;
                let shown = bool_arg(item, "shown")?;
                let c = columns
                    .iter_mut()
                    .find(|c| c.column == column)
                    .ok_or_else(|| format!("列がありません: {key}"))?;
                if let Some(shown) = shown {
                    c.hidden = !shown;
                }
                if let Some(width) = width {
                    c.width = width.unwrap_or_else(|| column.default_width());
                }
            }
        }
        if let Some(order) = args.get("order") {
            let order = order.as_array().ok_or("order が正しくありません")?;
            let mut wanted = Vec::new();
            for key in order {
                let key = key.as_str().ok_or("order が正しくありません")?;
                let column = parse_in(&COLUMNS, key, "order")?;
                if !wanted.contains(&column) {
                    wanted.push(column);
                }
            }
            columns.sort_by_key(|c| {
                wanted
                    .iter()
                    .position(|w| *w == c.column)
                    .unwrap_or(wanted.len())
            });
        }
        self.columns = columns;
        self.save_columns(cx);
        Ok(())
    }

    fn ai_set_panels(
        &mut self,
        args: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Value, String> {
        use crate::state::{
            PANEL_MAX, PANEL_MIN, PANEL_WIDTH, SIDEBAR_MAX, SIDEBAR_MIN, SIDEBAR_WIDTH,
        };
        let sidebar = bool_arg(args, "sidebar")?;
        let sidebar_width = width_arg(args, "sidebar_width", SIDEBAR_MIN, SIDEBAR_MAX)?;
        let detail_width = width_arg(args, "detail_width", PANEL_MIN, PANEL_MAX)?;
        let chat = bool_arg(args, "chat")?;
        let shell = bool_arg(args, "shell")?;
        let mut expanded = Vec::new();
        if let Some(map) = args.get("expanded") {
            let map = map.as_object().ok_or("expanded が正しくありません")?;
            for (id, open) in map {
                let open = open
                    .as_bool()
                    .ok_or_else(|| format!("expanded の {id} が正しくありません"))?;
                if !self.workspaces.iter().any(|w| w.id == *id) {
                    return Err(format!("ワークスペースが見つかりません: {id}"));
                }
                expanded.push((id.clone(), open));
            }
        }
        if let Some(on) = sidebar {
            self.sidebar_hidden = !on;
            self.sidebar_open = on;
        }
        if let Some(width) = sidebar_width {
            self.set_sidebar_width(width.unwrap_or(SIDEBAR_WIDTH), cx);
        }
        if let Some(width) = detail_width {
            self.set_panel_width(width.unwrap_or(PANEL_WIDTH), cx);
        }
        for (id, open) in expanded {
            if self.collapsed.contains(&id) == open {
                self.toggle_collapsed(&id, cx);
            }
        }
        if let Some(shell) = shell {
            let place = self.ai_session.read(cx).shell_place;
            match (shell, place) {
                (true, crate::ai::ShellPlace::Closed) => self.ai_session.update(cx, |s, cx| {
                    s.shell_place = crate::ai::ShellPlace::Docked;
                    cx.notify();
                }),
                (false, crate::ai::ShellPlace::Docked) => self.ai_session.update(cx, |s, cx| {
                    s.shell_place = crate::ai::ShellPlace::Closed;
                    cx.notify();
                }),
                (false, crate::ai::ShellPlace::Window(handle)) => {
                    // 閉じたあとのウィンドウなら、することがない
                    let _ = handle.update(cx, |_, window, _| window.remove_window());
                    self.ai_session.update(cx, |s, cx| {
                        s.shell_place = crate::ai::ShellPlace::Closed;
                        cx.notify();
                    });
                }
                _ => {}
            }
            // シェルは AI チャットの横に出るので、チャットも出す
            if shell && chat.is_none() && !self.ai_open {
                self.toggle_ai(window, cx);
            }
        }
        if let Some(chat) = chat {
            let window_handle = self.ai_session.read(cx).chat_window;
            if chat {
                // 別ウィンドウに出していれば、それで出ている
                if window_handle.is_none() && !self.ai_open {
                    self.toggle_ai(window, cx);
                }
            } else {
                if let Some(handle) = window_handle {
                    // 閉じたあとのウィンドウなら、することがない
                    let _ = handle.update(cx, |_, window, _| window.remove_window());
                    self.ai_session.update(cx, |s, _| s.chat_window = None);
                }
                if self.ai_open {
                    self.toggle_ai(window, cx);
                }
            }
        }
        cx.notify();
        Ok(json!({
            "sidebar": !self.sidebar_hidden,
            "sidebar_width": self.sidebar_width,
            "detail_width": self.panel_width,
            "chat": self.ai_open,
        }))
    }

    fn ai_pane(
        &mut self,
        args: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Value, String> {
        use malakt_core::panes::{Action, Dir};
        let action = match required(args, "action")? {
            "split_right" => Action::Split(Dir::Right),
            "split_down" => Action::Split(Dir::Down),
            "close" => {
                if self.layout.panes().len() < 2 {
                    return Err("ペインが 1 つだけなので閉じられません".into());
                }
                Action::Close
            }
            "zoom" => Action::Zoom,
            "focus_next" => Action::FocusNext,
            "focus_prev" => Action::FocusPrev,
            "focus_left" => Action::Focus(Dir::Left),
            "focus_right" => Action::Focus(Dir::Right),
            "focus_up" => Action::Focus(Dir::Up),
            "focus_down" => Action::Focus(Dir::Down),
            "swap_left" => Action::Swap(Dir::Left),
            "swap_right" => Action::Swap(Dir::Right),
            "swap_up" => Action::Swap(Dir::Up),
            "swap_down" => Action::Swap(Dir::Down),
            "duplicate" => Action::Duplicate,
            "pop_out" => Action::PopOut,
            "save_layout" => {
                self.save_pane_preset(cx);
                return Ok(
                    json!({"saved_pane_layouts": self.pane_presets.iter().map(|(n, _)| n).collect::<Vec<_>>()}),
                );
            }
            "load_layout" => {
                let name = required(args, "name")?.trim();
                let text = malakt_core::panes::PRESETS
                    .iter()
                    .map(|(n, text)| (n.to_string(), text.to_string()))
                    .chain(self.pane_presets.iter().cloned())
                    .find(|(n, _)| n == name)
                    .map(|(_, text)| text)
                    .ok_or_else(|| format!("「{name}」という配置はありません"))?;
                self.apply_pane_layout(&text, window, cx);
                if let Some(error) = self.error.take() {
                    return Err(error.to_string());
                }
                return Ok(json!({"panes": self.layout.panes().len()}));
            }
            "delete_layout" => {
                let name = required(args, "name")?.trim();
                let at = self
                    .pane_presets
                    .iter()
                    .position(|(n, _)| n == name)
                    .ok_or_else(|| format!("「{name}」という保存した配置はありません"))?;
                self.delete_pane_preset(at, cx);
                return Ok(
                    json!({"saved_pane_layouts": self.pane_presets.iter().map(|(n, _)| n).collect::<Vec<_>>()}),
                );
            }
            other => return Err(format!("action が正しくありません: {other}")),
        };
        self.pane_action(action, window, cx);
        Ok(json!({"panes": self.layout.panes().len(), "zoomed": self.layout.zoomed()}))
    }

    fn ai_open_dialog(
        &mut self,
        args: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Value, String> {
        let name = required(args, "name")?;
        let workspace = || {
            str_arg(args, "workspace_id")
                .filter(|id| !id.is_empty())
                .ok_or_else(|| "workspace_id が必要です".to_owned())
        };
        match name {
            "people" => self.open_people(window, cx),
            "customize" => self.open_appearance(window, cx),
            "lock" => self.open_lock_settings(window, cx),
            "notices" => {
                self.modal = Modal::Notices;
                self.focus.focus(window, cx);
            }
            "keys" => {
                if self.pane_overlay != Some(crate::panes::PaneOverlay::Help) {
                    self.toggle_keys(cx);
                }
            }
            "workspace_settings" | "workspace_icon" => {
                let id = workspace()?;
                self.db
                    .read(cx)
                    .store
                    .workspace(id)
                    .map_err(|e| e.to_string())?;
                if name == "workspace_settings" {
                    self.open_workspace_settings(id, window, cx);
                } else {
                    self.modal = Modal::WorkspaceIcon(id.to_owned());
                    self.focus.focus(window, cx);
                }
            }
            "history" => {
                if self.panel.is_none() {
                    return Err("変更履歴は、タスクを開いているときに出せます".into());
                }
                self.modal = Modal::History;
                self.focus.focus(window, cx);
            }
            "ticket" => {
                let id = str_arg(args, "task_id")
                    .filter(|id| !id.is_empty())
                    .ok_or("task_id が必要です")?;
                self.db.read(cx).store.task(id).map_err(|e| e.to_string())?;
                self.open_ticket_dialog(id, cx);
                self.focus.focus(window, cx);
            }
            other => return Err(format!("name が正しくありません: {other}")),
        }
        cx.notify();
        Ok(json!({"opened": name}))
    }

    fn ai_open_terminal(
        &mut self,
        args: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Value, String> {
        let shell = str_arg(args, "shell").filter(|s| !s.is_empty());
        if let Some(shell) = shell {
            let terms = crate::term::shared(cx).read(cx);
            if !terms.profiles().is_empty() && !terms.profiles().iter().any(|p| p.id == shell) {
                let ids: Vec<_> = terms.profiles().iter().map(|p| p.id.clone()).collect();
                return Err(format!(
                    "shell は {} のどれかです: {shell}",
                    ids.join(" / ")
                ));
            }
        }
        let before = crate::term::shared(cx).read(cx).tabs.len();
        match str_arg(args, "task_id").filter(|id| !id.is_empty()) {
            Some(id) => {
                self.db.read(cx).store.task(id).map_err(|e| e.to_string())?;
                self.open_terminal(Some(id), shell, window, cx);
            }
            None => self.open_terminal(None, shell, window, cx),
        }
        if let Some(error) = self.error.take() {
            return Err(error.to_string());
        }
        let terms = crate::term::shared(cx).read(cx);
        if let Some(error) = &terms.error {
            return Err(error.to_string());
        }
        if terms.tabs.len() <= before {
            return Err("ターミナルを開けませんでした".into());
        }
        Ok(json!({"terminals": terms.tabs.len()}))
    }

    fn lock_json(&self, cx: &App) -> Value {
        json!({
            "enabled": self.lock_enabled(cx),
            "auto_lock_minutes": self.auto_lock(cx).unwrap_or(0),
            "locked_notices": self.locked_notices(cx).code(),
        })
    }

    fn ai_set_lock_options(
        &mut self,
        args: &Value,
        cx: &mut Context<Self>,
    ) -> Result<Value, String> {
        // 先に全部の値を確かめ、1 つでも正しくなければ何も変えない
        let minutes = match args.get("auto_lock_minutes") {
            None | Some(Value::Null) => None,
            Some(value) => {
                let minutes = value
                    .as_u64()
                    .ok_or_else(|| format!("auto_lock_minutes は分数です: {value}"))?;
                let choices = malakt_core::lock::AUTO_CHOICES;
                match u32::try_from(minutes) {
                    Ok(0) => Some(None),
                    Ok(m) if choices.contains(&m) => Some(Some(m)),
                    _ => {
                        return Err(format!(
                            "auto_lock_minutes は 0 / {} のどれかです: {minutes}",
                            choices.map(|c| c.to_string()).join(" / ")
                        ))
                    }
                }
            }
        };
        let notices =
            match str_arg(args, "locked_notices") {
                None => None,
                Some(code) => Some(malakt_core::lock::LockedNotices::parse(code).ok_or_else(
                    || format!("locked_notices は title / hidden のどれかです: {code}"),
                )?),
            };
        if minutes.is_none() && notices.is_none() {
            return Err("auto_lock_minutes か locked_notices を渡してください".into());
        }
        if let Some(minutes) = minutes {
            if !self.set_auto_lock(minutes, cx) {
                return Err(self.take_error("自動でロックするまでの時間を保存できませんでした"));
            }
        }
        if let Some(notices) = notices {
            if !self.set_locked_notices(notices, cx) {
                return Err(self.take_error("ロック中の通知の設定を保存できませんでした"));
            }
        }
        Ok(self.lock_json(cx))
    }

    fn ai_set_options(&mut self, args: &Value, cx: &mut Context<Self>) -> Result<Value, String> {
        let reminders = bool_arg(args, "reminders")?;
        let autostart = bool_arg(args, "autostart")?;
        let auto_update = bool_arg(args, "auto_update")?;
        let keys = match str_arg(args, "pane_keys") {
            None => None,
            Some(code) => Some(
                malakt_core::panes::Keys::ALL
                    .into_iter()
                    .find(|k| k.code() == code)
                    .ok_or_else(|| format!("pane_keys は herdr / tmux のどれかです: {code}"))?,
            ),
        };
        // OS に書くものから先に変える。失敗したら、ほかの項目も変えない
        if let Some(on) = autostart {
            crate::autostart::set_enabled(on, cx)?;
        }
        if let Some(on) = reminders {
            crate::reminders::set_enabled(on, cx)
                .map_err(|e| format!("リマインダーの設定を保存できませんでした: {e}"))?;
        }
        if let Some(on) = auto_update {
            crate::update::set_enabled(on, cx)?;
        }
        if let Some(keys) = keys {
            self.set_pane_keys(keys, cx);
            if self.pane_keys != keys {
                return Err(self.take_error("キー操作を保存できませんでした"));
            }
        }
        cx.notify();
        Ok(json!({
            "reminders": crate::reminders::enabled(cx),
            "autostart": crate::autostart::enabled(cx),
            "auto_update": crate::update::enabled(cx),
            "pane_keys": self.pane_keys.code(),
        }))
    }

    // ---- カスタマイズ ----

    fn find_theme(&self, name: &str, cx: &App) -> Result<Preset, String> {
        let own = theme::own_presets(cx)?;
        let recommended = Preset::recommended_with(&theme::fonts(cx));
        own.iter()
            .chain(&recommended)
            .find(|p| p.name == name)
            .or_else(|| {
                own.iter()
                    .chain(&recommended)
                    .find(|p| p.name.replace(' ', "") == name.replace(' ', ""))
            })
            .cloned()
            .ok_or_else(|| {
                let all: Vec<_> = own
                    .iter()
                    .chain(&recommended)
                    .map(|p| p.name.clone())
                    .collect();
                format!("テーマは {} のどれかです: {name}", all.join(" / "))
            })
    }

    fn ai_store_presets(
        &mut self,
        presets: Vec<Preset>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        theme::set_own_presets(&presets, cx)?;
        self.own_presets = presets;
        self.preset_naming = None;
        cx.notify();
        Ok(())
    }

    /// 見た目を変えたあと、色を選ぶところと一覧の行の高さを合わせる。
    fn after_look_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_color_pickers(window, cx);
        self.size_rows(cx);
        cx.notify();
    }

    fn customization_json(&self, cx: &App) -> Value {
        let look = theme::look(cx);
        let scene = match theme::scene(cx) {
            Scene::File(path) => json!(path.display().to_string()),
            other => json!(SCENES
                .iter()
                .find(|(_, s)| *s == other)
                .map_or("none", |(k, _)| k)),
        };
        let now = theme::current("", cx);
        let own = theme::own_presets(cx).unwrap_or_default();
        let recommended = Preset::recommended_with(&theme::fonts(cx));
        let current_theme = own
            .iter()
            .chain(&recommended)
            .find(|p| p.same_look(&now))
            .map(|p| p.name.clone());
        let chat = self.ai_docked(cx);
        let color = |slot: Slot| look.color(slot).map(|c| c.hex());
        json!({
            "mode": key_of(&MODES, theme::choice(cx)),
            "scene": scene,
            "clear": theme::clear(cx),
            "blur": theme::blur(cx),
            "cards": theme::cards(cx),
            "floating": theme::floating(cx),
            "overlay": theme::overlay(cx),
            "density": key_of(&DENSITIES, theme::density(cx)),
            "feel": key_of(&FEELS, theme::feel(cx)),
            "due_format": key_of(&DUE_FORMATS, theme::due_format(cx)),
            "fonts": crate::fonts::ui_list(cx),
            "mono_fonts": crate::fonts::mono_list(cx),
            "accent": color(Slot::Accent),
            "light_background": color(Slot::Light),
            "dark_background": color(Slot::Dark),
            "current_theme": current_theme,
            "recommended_themes": recommended.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
            "own_themes": own.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
            "layouts": Layout::PATTERNS.iter().map(|l| l.name).collect::<Vec<_>>(),
            "current_layout": Layout::PATTERNS.iter().find(|l| l.shown(chat, cx)).map(|l| l.name),
            "scenes": names(&SCENES),
            "modes": names(&MODES),
            "densities": names(&DENSITIES),
            "feels": names(&FEELS),
            "due_formats": names(&DUE_FORMATS),
        })
    }

    fn ai_set_appearance(
        &mut self,
        args: &Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Value, String> {
        // 先に全部を確かめて、一部だけ変わることがないようにする
        let mode = str_arg(args, "mode")
            .map(|m| parse_in(&MODES, m, "mode"))
            .transpose()?;
        let scene = match str_arg(args, "scene").map(str::trim) {
            None => None,
            Some(key) => match SCENES.iter().find(|(k, _)| *k == key) {
                Some((_, scene)) => Some(scene.clone()),
                None => {
                    let path = malakt_ai::tools::expand_dir(key);
                    if !path.is_file() {
                        return Err(format!(
                            "scene は none / sky / forest / dusk か、画像ファイルの場所です（ファイルが見つかりません: {key}）"
                        ));
                    }
                    Some(Scene::File(path))
                }
            },
        };
        let number = |key: &str, max: u64| -> Result<Option<u8>, String> {
            match args.get(key) {
                None => Ok(None),
                Some(v) => v
                    .as_u64()
                    .filter(|n| *n <= max)
                    .map(|n| Some(n as u8))
                    .ok_or_else(|| format!("{key} は 0〜{max} の整数です: {v}")),
            }
        };
        let clear = number("clear", u64::from(theme::MAX_CLEAR))?;
        let blur = number("blur", 100)?;
        let cards = bool_arg(args, "cards")?;
        let floating = bool_arg(args, "floating")?;
        let overlay = bool_arg(args, "overlay")?;
        let density = str_arg(args, "density")
            .map(|d| parse_in(&DENSITIES, d, "density"))
            .transpose()?;
        let feel = str_arg(args, "feel")
            .map(|f| parse_in(&FEELS, f, "feel"))
            .transpose()?;
        let due_format = str_arg(args, "due_format")
            .map(|f| parse_in(&DUE_FORMATS, f, "due_format"))
            .transpose()?;
        // 書体の名前は、この PC にあるものの綴りにそろえる
        let installed = theme::fonts(cx);
        let find = |name: &str| -> Result<String, String> {
            let name = name.trim();
            installed
                .iter()
                .find(|f| *f == name)
                .or_else(|| installed.iter().find(|f| f.eq_ignore_ascii_case(name)))
                .cloned()
                .ok_or_else(|| {
                    format!("書体「{name}」はこの PC にありません。list_fonts で探してください")
                })
        };
        let list = |key: &str| -> Result<Option<Option<Vec<String>>>, String> {
            match args.get(key) {
                None => Ok(None),
                Some(Value::Null) => Ok(Some(None)),
                Some(Value::Array(items)) => {
                    let names = items
                        .iter()
                        .map(|item| match item {
                            Value::String(name) => find(name),
                            other => Err(format!("{key} が正しくありません: {other}")),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(Some((!names.is_empty()).then_some(names)))
                }
                Some(other) => Err(format!("{key} が正しくありません: {other}")),
            }
        };
        let mut fonts = list("fonts")?;
        match args.get("font") {
            None => {}
            Some(Value::Null) => fonts = Some(None),
            Some(Value::String(s)) if s.trim().is_empty() || s.trim() == theme::DEFAULT_FONT => {
                fonts = Some(None)
            }
            Some(Value::String(s)) => fonts = Some(Some(vec![find(s)?])),
            Some(other) => return Err(format!("font が正しくありません: {other}")),
        }
        let mono_fonts = list("mono_fonts")?;
        let mut colors = Vec::new();
        for (key, slot) in [
            ("accent", Slot::Accent),
            ("light_background", Slot::Light),
            ("dark_background", Slot::Dark),
        ] {
            match args.get(key) {
                None => {}
                Some(Value::Null) => colors.push((slot, None)),
                Some(Value::String(s)) => colors.push((
                    slot,
                    Some(
                        theme::Color::parse(s.trim())
                            .ok_or_else(|| format!("{key} は #rrggbb の色です: {s}"))?,
                    ),
                )),
                Some(other) => return Err(format!("{key} が正しくありません: {other}")),
            }
        }
        let core = |e: malakt_core::Error| e.to_string();
        if let Some(mode) = mode {
            theme::set(mode, window, cx).map_err(core)?;
        }
        if let Some(scene) = scene {
            theme::set_scene(scene, cx)?;
        }
        if let Some(clear) = clear {
            theme::set_clear(clear, cx).map_err(core)?;
        }
        if let Some(blur) = blur {
            theme::set_blur(blur, cx)?;
        }
        if let Some(on) = cards {
            theme::set_cards(on, cx).map_err(core)?;
        }
        if let Some(on) = floating {
            theme::set_floating(on, cx).map_err(core)?;
        }
        if let Some(on) = overlay {
            theme::set_overlay(on, cx).map_err(core)?;
        }
        if let Some(density) = density {
            theme::set_density(density, cx).map_err(core)?;
        }
        if let Some(feel) = feel {
            theme::set_feel(feel, cx).map_err(core)?;
        }
        if let Some(format) = due_format {
            theme::set_due_format(format, cx).map_err(core)?;
        }
        if let Some(fonts) = fonts {
            theme::set_fonts(&fonts.unwrap_or_default(), cx).map_err(core)?;
        }
        if let Some(fonts) = mono_fonts {
            theme::set_mono_fonts(fonts.as_deref(), cx).map_err(core)?;
        }
        for (slot, color) in colors {
            theme::set_color(slot, color, cx).map_err(core)?;
        }
        // ライトとダークで敷く画像が変わる。読めなかったら知らせる
        if let Some(error) = theme::take_failure(cx) {
            self.after_look_change(window, cx);
            return Err(error);
        }
        self.after_look_change(window, cx);
        Ok(self.customization_json(cx))
    }
}

// ---- AI の設定 ----

impl Session {
    /// AI の設定のツールを実行する。
    pub fn run_setting_tool(
        &mut self,
        name: &str,
        args: &Value,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        match name {
            "get_ai_settings" => {}
            "set_ai_settings" => self.ai_set_settings(args, cx)?,
            "check_github" => {
                if !self.watch.enabled {
                    return Err("GitHub の定期チェックがオフです。set_ai_settings の watch_enabled でオンにできます".into());
                }
                if !self.check_github(cx) {
                    return Err("今は GitHub を確認できません（ほかの処理が動いています）".into());
                }
                return Ok(json!({"started": true}).to_string());
            }
            other => return Err(format!("{other} というツールはありません")),
        }
        Ok(self.settings_json().to_string())
    }

    fn settings_json(&self) -> Value {
        json!({
            "provider": self.provider.as_str(),
            "providers": malakt_ai::Provider::ALL.iter().map(|p| json!({"id": p.as_str(), "name": p.label()})).collect::<Vec<_>>(),
            "model": self.model(),
            "listed_models": self.listed.iter().map(|m| m.to_string()).collect::<Vec<_>>(),
            "effort": self.effort.as_str(),
            "efforts": malakt_ai::Effort::ALL.iter().filter(|e| self.effort_allowed(**e)).map(|e| e.as_str()).collect::<Vec<_>>(),
            "routing_model": if self.coordinator_set() { self.coordinator_model() } else { String::new() },
            "arrange": self.arrange,
            "track": self.track,
            "watch_enabled": self.watch.enabled,
            "watch_minutes": self.watch.minutes,
            "watch_instruction": self.watch.instruction,
            "auto_run": self.auto_run,
        })
    }

    fn ai_set_settings(&mut self, args: &Value, cx: &mut Context<Self>) -> Result<(), String> {
        // 先に全部を確かめて、一部だけ変わることがないようにする
        let provider = str_arg(args, "provider")
            .map(|p| {
                let wanted = p.trim().to_lowercase().replace([' ', '-'], "_");
                malakt_ai::Provider::ALL
                    .into_iter()
                    .find(|x| {
                        x.as_str() == wanted || x.label().to_lowercase() == p.trim().to_lowercase()
                    })
                    .ok_or_else(|| {
                        let all: Vec<_> = malakt_ai::Provider::ALL
                            .iter()
                            .map(|x| x.as_str())
                            .collect();
                        format!("provider は {} のどれかです: {p}", all.join(" / "))
                    })
            })
            .transpose()?;
        let effort = str_arg(args, "effort")
            .map(|e| {
                malakt_ai::Effort::parse(e.trim()).ok_or_else(|| {
                    let all: Vec<_> = malakt_ai::Effort::ALL.iter().map(|x| x.as_str()).collect();
                    format!("effort は {} のどれかです: {e}", all.join(" / "))
                })
            })
            .transpose()?;
        let model = str_arg(args, "model").map(str::trim);
        if model == Some("") {
            return Err("model が空です".into());
        }
        let routing = str_arg(args, "routing_model").map(str::trim);
        let arrange = bool_arg(args, "arrange")?;
        let track = bool_arg(args, "track")?;
        let auto_run = bool_arg(args, "auto_run")?;
        let mut watch = self.watch.clone();
        if let Some(on) = bool_arg(args, "watch_enabled")? {
            watch.enabled = on;
        }
        if let Some(v) = args.get("watch_minutes") {
            let minutes = v
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .filter(|n| WATCH_MINUTES.contains(n))
                .ok_or_else(|| {
                    format!("watch_minutes は 60 / 180 / 360 / 720 / 1440 のどれかです: {v}")
                })?;
            watch.minutes = minutes;
        }
        if let Some(text) = str_arg(args, "watch_instruction") {
            if text.trim().is_empty() {
                return Err("watch_instruction が空です".into());
            }
            watch.instruction = text.to_owned();
        }
        self.error = None;
        if let Some(provider) = provider {
            self.set_provider(provider, cx);
        }
        if let Some(model) = model {
            self.set_model(model, cx);
        }
        if let Some(effort) = effort {
            self.set_effort(effort, cx);
        }
        if let Some(routing) = routing {
            self.set_coordinator((!routing.is_empty()).then_some(routing), cx);
        }
        if let Some(on) = arrange {
            self.set_arrange(on, cx);
        }
        if let Some(on) = track {
            self.set_track(on, cx);
        }
        if watch != self.watch {
            self.set_watch(Watch { ..watch }, cx);
        }
        if let Some(on) = auto_run {
            self.set_auto_run(on, cx);
        }
        match self.error.take() {
            Some(error) => Err(error.to_string()),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        confirmation_text, definitions, is_app_tool, key_of, needs_confirmation, parse_in, summary,
        Choice, Column, Density, DueFormat, Feel, Scene, Sort, COLUMNS, DENSITIES, DUE_FORMATS,
        FEELS, MODES, SCENES, SORTS,
    };
    use serde_json::json;

    #[test]
    fn every_app_tool_is_defined_once_with_a_closed_schema_and_a_summary() {
        let tools = definitions();
        let mut all: Vec<_> = tools.iter().map(|t| t.name).collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), tools.len());
        // データベースのツールと名前が重ならない
        let core: Vec<_> = malakt_ai::tools::definitions()
            .iter()
            .map(|t| t.name)
            .collect();
        for tool in &tools {
            assert!(!core.contains(&tool.name), "{}", tool.name);
            assert!(is_app_tool(tool.name), "{}", tool.name);
            assert_eq!(tool.schema["additionalProperties"], false, "{}", tool.name);
            assert!(summary(tool.name, &json!({})).is_some(), "{}", tool.name);
        }
    }

    /// 候補の並びは api/app.tsp に書き、アプリの表と同じであることをここで確かめる
    #[test]
    fn app_enums_match_the_code() {
        let tools = definitions();
        let schema = |tool: &str| {
            tools
                .iter()
                .find(|t| t.name == tool)
                .map(|t| t.schema["properties"].clone())
                .unwrap()
        };
        let list = |v: &serde_json::Value| -> Vec<String> {
            v["enum"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_str().unwrap().to_owned())
                .collect()
        };
        let owned =
            |names: Vec<&str>| -> Vec<String> { names.into_iter().map(String::from).collect() };
        let view = schema("set_list_view");
        assert_eq!(list(&view["sort"]), owned(super::names(&SORTS)));
        let columns = schema("set_columns");
        let column = &columns["columns"]["items"]["properties"]["column"];
        assert_eq!(list(column), owned(super::names(&COLUMNS)));
        assert_eq!(
            list(&columns["order"]["items"]),
            owned(super::names(&COLUMNS))
        );
        let look = schema("set_appearance");
        assert_eq!(list(&look["mode"]), owned(super::names(&MODES)));
        assert_eq!(list(&look["density"]), owned(super::names(&DENSITIES)));
        assert_eq!(list(&look["feel"]), owned(super::names(&FEELS)));
        assert_eq!(list(&look["due_format"]), owned(super::names(&DUE_FORMATS)));
        let clear = &look["clear"];
        assert_eq!(clear["maximum"], json!(crate::theme::MAX_CLEAR));
    }

    #[test]
    fn only_destructive_app_tools_ask_first() {
        assert!(needs_confirmation("delete_theme", &json!({"name": "a"})));
        assert!(needs_confirmation(
            "import_data",
            &json!({"path": "/x.zip"})
        ));
        assert!(needs_confirmation(
            "set_ai_settings",
            &json!({"auto_run": true})
        ));
        assert!(!needs_confirmation(
            "set_ai_settings",
            &json!({"auto_run": false})
        ));
        assert!(!needs_confirmation(
            "set_ai_settings",
            &json!({"model": "m"})
        ));
        assert!(!needs_confirmation("apply_theme", &json!({"name": "標準"})));
        // 書き出しは、同じ名前のファイルがあるときだけ
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("out.zip");
        let args = json!({"path": file.display().to_string()});
        assert!(!needs_confirmation("export_data", &args));
        std::fs::write(&file, b"x").unwrap();
        assert!(needs_confirmation("export_data", &args));
        assert!(confirmation_text("export_data", &args).contains("out.zip"));
    }

    #[test]
    fn names_map_both_ways() {
        for (key, sort) in SORTS {
            assert_eq!(key_of(&SORTS, sort), key);
            assert_eq!(parse_in(&SORTS, key, "sort").unwrap(), sort);
        }
        assert!(parse_in(&SORTS, "random", "sort")
            .unwrap_err()
            .contains("due / priority"));
        assert_eq!(Sort::ALL.len(), SORTS.len());
        assert_eq!(Column::ALL.len(), COLUMNS.len());
        assert_eq!(Density::ALL.len(), DENSITIES.len());
        assert_eq!(Feel::ALL.len(), FEELS.len());
        assert_eq!(DueFormat::ALL.len(), DUE_FORMATS.len());
        for (key, format) in DUE_FORMATS {
            assert_eq!(key, format.key());
        }
        assert_eq!(Choice::ALL.len(), MODES.len());
        assert_eq!(Scene::BUNDLED.len(), SCENES.len());
    }
}
