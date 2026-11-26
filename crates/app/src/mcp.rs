//! MCP サーバー。ターミナルの Claude Code などから、タスクを読み書きできるようにする。
//!
//! ```text
//! malakt-mahoz mcp
//! ```
//!
//! 標準入出力で JSON-RPC を 1 行ずつやり取りする（MCP の stdio）。ネットワークは使わない。
//! ツールは AI チャットと同じもの（[`malakt_ai::tools`]）に、`undo` と `redo` を足したもの。
//! ロックが有効なときは、アプリがロックを解除している間だけ動く（[`malakt_core::lock::allows_outside`]）。

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use malakt_ai::tools;
use malakt_core::{lock, Store};
use serde_json::{json, Value};

/// Claude Code などに登録するときの名前。
pub const NAME: &str = "malakt-mahoz";
/// アプリの中のターミナルやシェルに渡す、この実行ファイルの場所。
pub const ENV_BIN: &str = "MALAKT_MAHOZ_BIN";
/// データベースの場所。
pub const ENV_DB: &str = "MALAKT_MAHOZ_DB";
/// [`config_json`] の中身を書いたファイルの場所。`claude --mcp-config "$MALAKT_MAHOZ_MCP_CONFIG"` で使う。
pub const ENV_CONFIG: &str = "MALAKT_MAHOZ_MCP_CONFIG";

/// 対応している MCP の版。新しい順。
const PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

pub const LOCKED: &str =
    "malakt-mahoz はロックされています。アプリを開いて、ロックを解除してください。";

const INSTRUCTIONS_HEAD: &str = "\
タスク管理アプリ malakt-mahoz のタスク、ワークスペース、担当者、コメントを読み書きします。\
ID は list_workspaces、list_people、list_tasks で調べてから使ってください。\
日付は YYYY-MM-DD、時刻つきなら YYYY-MM-DD HH:MM で渡します。\
変更は undo で 1 つずつ元に戻せます（このサーバーが動いている間）。";

/// サーバーの説明。コメントの決まりは、アプリの中の AI と同じ文を渡す。
fn instructions() -> String {
    format!("{INSTRUCTIONS_HEAD}{}", tools::COMMENT_RULE)
}

const WRITES: &[&str] = &[
    "create_task",
    "update_task",
    "set_assignee",
    "move_task",
    "delete_task",
    "add_comment",
    "create_workspace",
    "create_person",
];

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 外に見せるツールの一覧（`tools/list` の形）。
pub fn tool_list() -> Vec<Value> {
    let mut list: Vec<Value> = tools::definitions()
        .into_iter()
        // GitHub とシェルは、呼ぶ側（Claude Code）が自分で持っている
        .filter(|t| {
            !matches!(
                t.name,
                tools::GITHUB_FETCH | tools::SLACK_FETCH | tools::RUN_SHELL
            )
        })
        .map(|t| {
            let description = if t.name == "delete_task" {
                "タスクを削除する。undo で元に戻せる。"
            } else {
                t.description
            };
            json!({"name": t.name, "description": description, "inputSchema": t.schema})
        })
        .collect();
    let empty = || json!({"type": "object", "properties": {}, "additionalProperties": false});
    list.push(json!({
        "name": "undo",
        "description": "このサーバーからの直前の変更を 1 つ元に戻す。続けて呼ぶと、さらに前の変更を戻す。",
        "inputSchema": empty(),
    }));
    list.push(json!({
        "name": "redo",
        "description": "undo で戻した変更をやり直す。",
        "inputSchema": empty(),
    }));
    list
}

/// ロックを確かめてから、ツールを 1 つ実行する。コマンド（`malakt-mahoz call`）と共通。
/// 変更は 1 回の呼び出しが 1 つの段になり、`undo` で戻せる。
pub fn call_tool(store: &mut Store, now: i64, name: &str, args: &Value) -> Result<String, String> {
    let core = |e: malakt_core::Error| e.to_string();
    if !lock::allows_outside(store, now).map_err(core)? {
        return Err(LOCKED.to_owned());
    }
    match name {
        "undo" => {
            let done = store.undo().map_err(core)?;
            Ok(json!({"undone": done}).to_string())
        }
        "redo" => {
            let done = store.redo().map_err(core)?;
            Ok(json!({"redone": done}).to_string())
        }
        tools::GITHUB_FETCH | tools::SLACK_FETCH | tools::RUN_SHELL => {
            Err(format!("{name} というツールはありません"))
        }
        _ => {
            // 自分が登録されていれば、コメントの書き手にする（AI チャットと同じ）
            let author = store
                .people()
                .map_err(core)?
                .into_iter()
                .find(|p| p.is_self)
                .map(|p| p.id);
            let write = WRITES.contains(&name);
            if write {
                store.undo_mark().map_err(core)?;
            }
            let result = tools::execute(store, author.as_deref(), name, args);
            if write {
                store.undo_mark().map_err(core)?;
            }
            result
        }
    }
}

/// Claude Code の `.mcp.json` や `--mcp-config` に渡す設定。
/// `db` は、標準でない場所のデータベースを使うときだけ渡す。
pub fn config_json(exe: &Path, db: Option<&Path>) -> Value {
    let mut server = json!({
        "type": "stdio",
        "command": exe.to_string_lossy(),
        "args": ["mcp"],
    });
    if let Some(db) = db {
        server["env"] = json!({ENV_DB: db.to_string_lossy()});
    }
    json!({"mcpServers": {NAME: server}})
}

/// アプリの中から開くシェルやターミナルに渡す環境変数。
/// そこで動く Claude Code などが、このアプリのコマンドとデータベースを見つけられる。
/// `config` は、[`write_config`] が書いたファイル。
pub fn terminal_env(exe: &Path, db: &Path, config: Option<&Path>) -> Vec<(String, String)> {
    let mut env = vec![
        (ENV_BIN.to_owned(), exe.to_string_lossy().into_owned()),
        (ENV_DB.to_owned(), db.to_string_lossy().into_owned()),
    ];
    if let Some(config) = config {
        env.push((ENV_CONFIG.to_owned(), config.to_string_lossy().into_owned()));
    }
    env
}

/// このアプリが開くシェルに渡す環境変数。実行ファイルの場所が分からなければ、何も渡さない。
pub fn shell_env() -> Vec<(String, String)> {
    match std::env::current_exe() {
        Ok(exe) => terminal_env(&exe, &crate::database_path(), None),
        Err(_) => Vec::new(),
    }
}

/// [`config_json`] をファイルに書く。中身が同じなら書かない。
pub fn write_config(path: &Path, exe: &Path, db: Option<&Path>) -> std::io::Result<()> {
    let text = serde_json::to_string_pretty(&config_json(exe, db))? + "\n";
    if std::fs::read_to_string(path).is_ok_and(|old| old == text) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, text)
}

/// アプリが動いている間、ロックしておくファイル。
pub fn running_file(db: &Path) -> PathBuf {
    let mut name = db.file_name().unwrap_or_default().to_os_string();
    name.push(".running");
    db.with_file_name(name)
}

/// アプリが動いている印。持っている間、[`running_file`] をロックする。
/// 捨てるか、アプリのプロセスが終わる（落ちたときも）と、ロックが外れる。
pub struct Running(#[allow(dead_code)] std::fs::File);

/// アプリが動いている印を付ける。MCP サーバーが確かめている一瞬とぶつかったら、少し待ってやり直す。
pub fn mark_running(db: &Path) -> std::io::Result<Running> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(running_file(db))?;
    for _ in 0..50 {
        match file.try_lock() {
            Ok(()) => return Ok(Running(file)),
            Err(std::fs::TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
    }
    Err(std::io::Error::other("ほかのアプリがロックしています"))
}

/// このデータベースを使うアプリが動いているか。
pub fn app_running(db: &Path) -> bool {
    let Ok(file) = std::fs::OpenOptions::new()
        .read(true)
        .open(running_file(db))
    else {
        // 印のファイルがなければ、アプリを起動したことがない
        return false;
    };
    match file.try_lock_shared() {
        // ロックできたなら、アプリは持っていない
        Ok(()) => false,
        Err(std::fs::TryLockError::WouldBlock) => true,
        // ロックを確かめられない場所では、動いているものとして扱う（サーバーを勝手に終わらせない）
        Err(std::fs::TryLockError::Error(_)) => true,
    }
}

/// アプリが閉じられたら、この MCP サーバーのプロセスを終わらせる。入力を待っている間も見張る。
/// 動いたままだと、実行ファイルをつかんだままになり、アプリを新しい版に入れ替えられない。
pub fn exit_when_app_closes(db: PathBuf) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_millis(300));
        if !app_running(&db) {
            std::process::exit(0);
        }
    });
}

static RUNNING: std::sync::Mutex<Option<Running>> = std::sync::Mutex::new(None);
static RUNNING_DB: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// アプリが動いている印を付けて、アプリを閉じるまで持っておく。
pub fn hold_running(db: &Path) -> std::io::Result<()> {
    let running = mark_running(db)?;
    *RUNNING.lock().unwrap_or_else(|e| e.into_inner()) = Some(running);
    // 2 回目からは同じ場所なので、そのままでよい
    let _ = RUNNING_DB.set(db.to_owned());
    Ok(())
}

/// アプリを閉じるときに呼ぶ。印を外すので、動いている MCP サーバーが終わる。印を持っていたら true。
pub fn quitting() -> bool {
    RUNNING
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .is_some()
}

/// 新しい版に入れ替える前に呼ぶ。印を外し、MCP サーバーが終わって実行ファイルを放すまで待つ。
/// 印を持っていたら true。入れ替えに失敗したら [`resume`] で付け直す。
pub fn release_for_install() -> bool {
    let held = quitting();
    if held {
        // サーバーは 0.3 秒ごとに印を見る
        std::thread::sleep(std::time::Duration::from_millis(800));
    }
    held
}

/// 外した印を付け直す。アプリを閉じずに続けるときに呼ぶ。
pub fn resume() -> std::io::Result<()> {
    match RUNNING_DB.get() {
        Some(db) => hold_running(db),
        None => Ok(()),
    }
}

pub struct Server<F> {
    open: Option<F>,
    store: Option<Store>,
    now: fn() -> i64,
}

impl<F: FnOnce() -> malakt_core::Result<Store>> Server<F> {
    /// データベースは、最初のツールの呼び出しで開く。
    pub fn new(open: F) -> Self {
        Self {
            open: Some(open),
            store: None,
            now: unix_now,
        }
    }

    /// 時計を差し替える（テスト用）。
    pub fn with_clock(mut self, now: fn() -> i64) -> Self {
        self.now = now;
        self
    }

    fn store(&mut self) -> Result<&mut Store, String> {
        if self.store.is_none() {
            let open = self.open.take().ok_or(
                "データベースを開けませんでした。malakt-mahoz mcp を起動し直してください。",
            )?;
            self.store = Some(open().map_err(|e| format!("データベースを開けませんでした: {e}"))?);
        }
        Ok(self.store.as_mut().expect("直前に開いた"))
    }

    fn call(&mut self, params: &Value) -> Result<String, String> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or("name が必要です")?;
        let empty = json!({});
        let args = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(args) => args,
        };
        let now = (self.now)();
        call_tool(self.store()?, now, name, args)
    }

    /// 受け取った 1 行に答える。通知には何も返さない。
    pub fn handle(&mut self, line: &str) -> Option<String> {
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(error) => {
                return Some(
                    json!({"jsonrpc": "2.0", "id": null,
                           "error": {"code": -32700, "message": error.to_string()}})
                    .to_string(),
                )
            }
        };
        // id のないものは通知（notifications/initialized など）。返事をしない
        let id = message.get("id").filter(|id| !id.is_null())?.clone();
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let null = Value::Null;
        let params = message.get("params").unwrap_or(&null);
        let result = match method {
            "initialize" => {
                // 相手の版に対応していればその版で、していなければこちらの最新で答える
                let asked = params.get("protocolVersion").and_then(Value::as_str);
                let version = asked
                    .filter(|v| PROTOCOLS.contains(v))
                    .unwrap_or(PROTOCOLS[0]);
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": NAME, "version": env!("CARGO_PKG_VERSION")},
                    "instructions": instructions(),
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tool_list()})),
            "tools/call" => {
                let (text, failed) = match self.call(params) {
                    Ok(text) => (text, false),
                    Err(text) => (text, true),
                };
                Ok(json!({"content": [{"type": "text", "text": text}], "isError": failed}))
            }
            other => {
                Err(json!({"code": -32601, "message": format!("知らないメソッドです: {other}")}))
            }
        };
        let mut reply = json!({"jsonrpc": "2.0", "id": id});
        match result {
            Ok(result) => reply["result"] = result,
            Err(error) => reply["error"] = error,
        }
        Some(reply.to_string())
    }

    /// 入力が閉じるまで、1 行ずつ答える。
    pub fn serve(&mut self, input: &mut dyn BufRead, out: &mut dyn Write) -> std::io::Result<()> {
        let mut line = String::new();
        loop {
            line.clear();
            if input.read_line(&mut line)? == 0 {
                return Ok(());
            }
            if line.trim().is_empty() {
                continue;
            }
            if let Some(reply) = self.handle(line.trim()) {
                writeln!(out, "{reply}")?;
                out.flush()?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use malakt_core::{Status, TaskDraft};

    fn server(path: &Path) -> Server<impl FnOnce() -> malakt_core::Result<Store> + '_> {
        Server::new(move || Store::open(path)).with_clock(|| 1_000)
    }

    fn ask<F: FnOnce() -> malakt_core::Result<Store>>(
        server: &mut Server<F>,
        method: &str,
        params: Value,
    ) -> Value {
        let line = json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params});
        let reply: Value =
            serde_json::from_str(&server.handle(&line.to_string()).unwrap()).unwrap();
        assert_eq!(reply["jsonrpc"], "2.0");
        assert_eq!(reply["id"], 7);
        reply
    }

    /// ツールを呼んで、成功した結果の JSON を返す。
    fn tool<F: FnOnce() -> malakt_core::Result<Store>>(
        server: &mut Server<F>,
        name: &str,
        args: Value,
    ) -> Value {
        let reply = ask(
            server,
            "tools/call",
            json!({"name": name, "arguments": args}),
        );
        let result = &reply["result"];
        let text = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(result["isError"], false, "{text}");
        serde_json::from_str(text).unwrap()
    }

    fn failure<F: FnOnce() -> malakt_core::Result<Store>>(
        server: &mut Server<F>,
        name: &str,
        args: Value,
    ) -> String {
        let reply = ask(
            server,
            "tools/call",
            json!({"name": name, "arguments": args}),
        );
        assert_eq!(reply["result"]["isError"], true);
        reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn handshake_and_tool_list() {
        let dir = tempfile::tempdir().unwrap();
        // 始めのやり取りでは、データベースを開かない
        let missing = dir.path().join("none").join("data.sqlite3");
        let mut s = server(&missing);
        let reply = ask(
            &mut s,
            "initialize",
            json!({"protocolVersion": "2025-03-26", "capabilities": {},
                   "clientInfo": {"name": "test", "version": "0"}}),
        );
        assert_eq!(reply["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(reply["result"]["serverInfo"]["name"], NAME);
        assert!(reply["result"]["capabilities"]["tools"].is_object());
        // 知らない版には、こちらの最新で答える
        let reply = ask(
            &mut s,
            "initialize",
            json!({"protocolVersion": "2099-01-01"}),
        );
        assert_eq!(reply["result"]["protocolVersion"], PROTOCOLS[0]);
        // 通知には返事をしない
        assert_eq!(
            s.handle(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
            None
        );
        assert_eq!(ask(&mut s, "ping", json!({}))["result"], json!({}));

        let reply = ask(&mut s, "tools/list", json!({}));
        let tools = reply["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for name in [
            "list_tasks",
            "get_task",
            "create_task",
            "add_comment",
            "undo",
            "redo",
        ] {
            assert!(names.contains(&name), "{name}");
        }
        // シェルと GitHub は出さない
        assert!(!names.contains(&tools::RUN_SHELL));
        assert!(!names.contains(&tools::GITHUB_FETCH));
        assert!(!names.contains(&tools::SLACK_FETCH));
        for t in tools {
            assert_eq!(t["inputSchema"]["type"], "object", "{t}");
            assert!(!t["description"].as_str().unwrap().is_empty());
            // 削除は undo で戻せるので、「元に戻せない」とは書かない
            assert!(!t["description"].as_str().unwrap().contains("元に戻せない"));
            if t["name"] == "add_comment" {
                // 外のエージェントにも、中身のないコメントを書かせない
                assert!(t["description"].as_str().unwrap().contains("事実だけ"));
                assert!(instructions().contains("していないこと"));
            }
        }
        // 書き込みとして扱うツールは、どれも一覧にある
        for name in WRITES {
            assert!(names.contains(name), "{name}");
        }
        assert!(!missing.exists());

        let reply = ask(&mut s, "resources/list", json!({}));
        assert_eq!(reply["error"]["code"], -32601);
        let broken: Value = serde_json::from_str(&s.handle("{oops").unwrap()).unwrap();
        assert_eq!(broken["error"]["code"], -32700);
    }

    #[test]
    fn reads_writes_and_undo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.sqlite3");
        {
            let mut store = Store::open(&path).unwrap();
            store.ensure_self().unwrap();
            let w = store.create_workspace(None, "冬のイベント").unwrap();
            let draft = TaskDraft {
                title: "会場を予約する".into(),
                ..Default::default()
            };
            store.create_task(Some(&w.id), &draft).unwrap();
        }
        let mut s = server(&path);
        let workspaces = tool(&mut s, "list_workspaces", json!({}));
        assert_eq!(workspaces[0]["name"], "冬のイベント");
        let workspace = workspaces[0]["id"].as_str().unwrap().to_owned();
        let listed = tool(&mut s, "list_tasks", json!({"workspace_id": workspace}));
        assert_eq!(listed["total"], 1);
        let first = listed["tasks"][0]["id"].as_str().unwrap().to_owned();

        // 作る、書き換える、コメントを書く
        let made = tool(
            &mut s,
            "create_task",
            json!({"title": "告知を出す", "workspace_id": workspace, "due": "2026-12-01"}),
        );
        let id = made["id"].as_str().unwrap().to_owned();
        tool(
            &mut s,
            "update_task",
            json!({"id": id, "status": "in_process"}),
        );
        tool(
            &mut s,
            "add_comment",
            json!({"task_id": id, "body": "下書きができた"}),
        );
        let got = tool(&mut s, "get_task", json!({"id": id}));
        assert_eq!(got["status"], "in_process");
        assert_eq!(got["comments"][0]["body"], "下書きができた");
        // コメントの書き手は自分
        assert!(got["comments"][0]["author_id"].is_string());

        // 別の接続（アプリ）からも、すぐ見える
        let app = Store::open(&path).unwrap();
        assert_eq!(app.task(&id).unwrap().status, Status::InProcess);

        // 1 回の呼び出しが 1 つの段。新しい順に戻る
        assert_eq!(tool(&mut s, "undo", json!({}))["undone"], true);
        assert!(app.comments(&id).unwrap().is_empty());
        assert_eq!(tool(&mut s, "undo", json!({}))["undone"], true);
        assert_eq!(app.task(&id).unwrap().status, Status::NeedsAction);
        assert_eq!(tool(&mut s, "redo", json!({}))["redone"], true);
        assert_eq!(app.task(&id).unwrap().status, Status::InProcess);

        // 削除も戻せる
        tool(&mut s, "delete_task", json!({"id": first}));
        assert!(app.task(&first).is_err());
        assert_eq!(tool(&mut s, "undo", json!({}))["undone"], true);
        assert_eq!(app.task(&first).unwrap().title, "会場を予約する");

        // 読むだけの呼び出しは、段を作らない
        tool(&mut s, "list_tasks", json!({}));
        for _ in 0..2 {
            assert_eq!(tool(&mut s, "undo", json!({}))["undone"], true);
        }
        assert!(app.task(&id).is_err());
        assert_eq!(tool(&mut s, "undo", json!({}))["undone"], false);

        // 失敗は isError で返し、サーバーは動き続ける
        let text = failure(&mut s, "get_task", json!({"id": "nope"}));
        assert!(text.contains("見つかりません"), "{text}");
        let text = failure(&mut s, "run_shell", json!({"command": "echo"}));
        assert!(text.contains("ありません"), "{text}");
        let reply = ask(&mut s, "tools/call", json!({}));
        assert_eq!(reply["result"]["isError"], true);
        assert_eq!(tool(&mut s, "list_tasks", json!({}))["total"], 1);
    }

    #[test]
    fn nothing_is_readable_while_the_app_is_locked() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.sqlite3");
        let mut app = Store::open(&path).unwrap();
        let draft = TaskDraft {
            title: "ひみつ".into(),
            ..Default::default()
        };
        app.create_task(None, &draft).unwrap();
        lock::enable(&mut app).unwrap();

        let mut s = server(&path);
        // アプリを開いていない、またはロックしている
        for (name, args) in [
            ("list_tasks", json!({})),
            ("list_workspaces", json!({})),
            ("create_task", json!({"title": "x"})),
            ("undo", json!({})),
        ] {
            let text = failure(&mut s, name, args);
            assert_eq!(text, LOCKED);
            assert!(!text.contains("ひみつ"));
        }
        assert_eq!(
            app.tasks(malakt_core::Scope::All, &Default::default())
                .unwrap()
                .len(),
            1
        );

        // アプリが解除すると、同じサーバーのまま読める
        lock::mark_open(&mut app, 1_000).unwrap();
        assert_eq!(tool(&mut s, "list_tasks", json!({}))["total"], 1);
        // ロックすると、すぐ読めなくなる
        lock::mark_closed(&mut app).unwrap();
        assert_eq!(failure(&mut s, "list_tasks", json!({})), LOCKED);
        // アプリが落ちて印が残っても、古い印では読めない
        lock::mark_open(&mut app, 1_000 - lock::OPEN_FOR - 1).unwrap();
        assert_eq!(failure(&mut s, "list_tasks", json!({})), LOCKED);
    }

    #[test]
    fn serve_answers_line_by_line_and_reports_a_database_that_cannot_open() {
        let dir = tempfile::tempdir().unwrap();
        // フォルダーはデータベースとして開けない
        let mut s = server(dir.path());
        let input = [
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}"#,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
            "",
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"list_tasks"}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_tasks"}}"#,
        ]
        .join("\n");
        let mut out = Vec::new();
        s.serve(&mut input.as_bytes(), &mut out).unwrap();
        let lines: Vec<Value> = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["id"], 1);
        for (line, id) in lines[1..].iter().zip([2, 3]) {
            assert_eq!(line["id"], id);
            assert_eq!(line["result"]["isError"], true);
            let text = line["result"]["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("データベースを開けませんでした"), "{text}");
        }
    }

    #[test]
    fn the_running_mark_is_seen_until_the_app_lets_go() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("data.sqlite3");
        assert_eq!(running_file(&db), dir.path().join("data.sqlite3.running"));
        // アプリを起動したことがなければ、動いていない
        assert!(!app_running(&db));
        let running = mark_running(&db).unwrap();
        assert!(app_running(&db));
        // 何度確かめても、印は付いたまま
        assert!(app_running(&db));
        // もう 1 つのアプリは、印を付けられない
        assert!(mark_running(&db).is_err());
        drop(running);
        assert!(!app_running(&db));
        // 次に起動したアプリが、また印を付けられる
        let again = mark_running(&db).unwrap();
        assert!(app_running(&db));
        drop(again);
    }

    #[test]
    fn config_and_environment() {
        let exe = Path::new("/opt/malakt-mahoz/malakt-mahoz");
        let config = config_json(exe, None);
        let server = &config["mcpServers"][NAME];
        assert_eq!(server["command"], "/opt/malakt-mahoz/malakt-mahoz");
        assert_eq!(server["args"], json!(["mcp"]));
        assert!(server.get("env").is_none());
        let db = Path::new("/data/タスク.sqlite3");
        let config = config_json(exe, Some(db));
        assert_eq!(
            config["mcpServers"][NAME]["env"][ENV_DB],
            "/data/タスク.sqlite3"
        );

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("deep").join("mcp.json");
        write_config(&file, exe, Some(db)).unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(written, config);
        write_config(&file, exe, Some(db)).unwrap();

        let env = terminal_env(exe, db, Some(&file));
        let get = |key: &str| env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        assert_eq!(get(ENV_BIN), Some("/opt/malakt-mahoz/malakt-mahoz"));
        assert_eq!(get(ENV_DB), Some("/data/タスク.sqlite3"));
        assert_eq!(get(ENV_CONFIG), file.to_str());
        assert!(terminal_env(exe, db, None)
            .iter()
            .all(|(k, _)| k != ENV_CONFIG));
    }
}
