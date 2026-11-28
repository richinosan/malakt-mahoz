//! MCP サーバー。ターミナルの Claude Code などから、タスクを読み書きできるようにする。
//!
//! ```text
//! malakt-mahoz mcp
//! ```
//!
//! 標準入出力で JSON-RPC を 1 行ずつやり取りする（MCP の stdio）。ネットワークは使わない。
//! ツールは AI チャットと同じもの（[`malakt_ai::tools`]）に、`undo` と `redo` を足したもの。
//! ロックが有効なときは、アプリがロックを解除している間だけ動く（[`malakt_core::lock::allows_outside`]）。
//! 環境変数 [`ENV_KEY`] でアクセスキーを渡すと、キーごとに読み取りのみ・ロック中も使えるかが決まる
//! （[`malakt_core::access`]）。キーなしの接続をどう扱うかは、アプリの設定で選ぶ。

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use malakt_ai::tools;
use malakt_core::access::{self, Denied};
use malakt_core::Store;
use serde_json::{json, Value};

/// Claude Code などに登録するときの名前。
pub const NAME: &str = "malakt-mahoz";
/// アプリの中のターミナルやシェルに渡す、この実行ファイルの場所。
pub const ENV_BIN: &str = "MALAKT_MAHOZ_BIN";
/// データベースの場所。
pub const ENV_DB: &str = "MALAKT_MAHOZ_DB";
/// アクセスキー。MCP サーバーとコマンドは、ここに入っているキーでつなぐ。
pub const ENV_KEY: &str = "MALAKT_MAHOZ_KEY";
/// [`config_json`] の中身を書いたファイルの場所。`claude --mcp-config "$MALAKT_MAHOZ_MCP_CONFIG"` で使う。
pub const ENV_CONFIG: &str = "MALAKT_MAHOZ_MCP_CONFIG";

/// スタートアップ（OS にログインしたときにアプリを起動するか）を読むツール。
pub const GET_AUTOSTART: &str = "get_autostart";
/// スタートアップに登録する・外すツール。
pub const SET_AUTOSTART: &str = "set_autostart";

/// 対応している MCP の版。新しい順。
const PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

pub const LOCKED: &str =
    "malakt-mahoz はロックされています。アプリを開いて、ロックを解除してください。";
pub const NEEDS_KEY: &str = "malakt-mahoz にはアクセスキーが必要です。アプリの設定の「アクセスキー」でキーを発行して、環境変数 MALAKT_MAHOZ_KEY に入れてください。";
pub const BAD_KEY: &str =
    "アクセスキーが違うか、失効しています。アプリの設定の「アクセスキー」でキーを発行し直してください。";

/// 使えない理由の文。
pub fn denied_text(denied: &Denied) -> String {
    denied.to_string()
}

/// ツールを実行できなかった理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// ロック・アクセスキー（期限・時間帯・読み取りのみ）で使えない。コマンドの終了コードは 3
    Denied(String),
    /// ツールが失敗した。コマンドの終了コードは 1
    Failed(String),
}

impl Refused {
    pub fn text(self) -> String {
        match self {
            Refused::Denied(text) | Refused::Failed(text) => text,
        }
    }
}

/// 読み取りのみのキーで使えるツールか。読むだけのツールは名前が list_ か get_ で始まる。
/// 読み取りのみのときは、ほかに SQLite の `query_only` でも書き込みを止める。
pub fn reads(name: &str) -> bool {
    name.starts_with("list_") || name.starts_with("get_")
}

const INSTRUCTIONS_HEAD: &str = "\
タスク管理アプリ malakt-mahoz のタスク、ワークスペース、担当者、コメントを読み書きします。\
ID は list_workspaces、list_people、list_tasks で調べてから使ってください。\
日付は YYYY-MM-DD、時刻つきなら YYYY-MM-DD HH:MM で渡します。\
変更は undo で 1 つずつ元に戻せます（このサーバーが動いている間）。\
ユーザーが決めた AI のルールは list_ai_rules で読めます。apply が always か chat の有効なルールに従ってください。\
ワークスペースのルールは、そのワークスペースとサブワークスペースのタスクを扱うときに従ってください。";

/// サーバーの説明。コメントの決まりは、アプリの中の AI と同じ文を渡す。
fn instructions() -> String {
    format!("{INSTRUCTIONS_HEAD}{}", tools::COMMENT_RULE)
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 外に見せるツールの一覧（`tools/list` の形）。定義は api/data.tsp と api/mcp.tsp。
pub fn tool_list() -> Vec<Value> {
    use malakt_ai::api::{self, Group};
    let data = api::tools()
        .iter()
        // GitHub とシェルは、呼ぶ側（Claude Code）が自分で持っている
        .filter(|t| t.group == Group::Data && !t.in_app_only)
        .map(|t| {
            // 外のエージェントには確認を出さない。代わりに undo で戻せる
            let description = t.description.replace(
                "元に戻せないので、実行の前にユーザーに確認が出る。",
                "undo で元に戻せる。",
            );
            json!({"name": t.name, "description": description, "inputSchema": t.schema})
        });
    let own = api::tools()
        .iter()
        .filter(|t| t.group == Group::Mcp)
        .map(|t| json!({"name": t.name, "description": t.description, "inputSchema": t.schema}));
    data.chain(own).collect()
}

/// アクセスキーとロックを確かめてから、ツールを 1 つ実行する。コマンド（`malakt-mahoz call`）と共通。
/// `key` は、つないできたものが渡したアクセスキー。キーは呼び出しのたびに確かめるので、
/// 失効させたり、できることを変えたりすると、動いているサーバーにもすぐ効く。
/// 変更は 1 回の呼び出しが 1 つの段になり、`undo` で戻せる。
pub fn call_tool(
    store: &mut Store,
    now: i64,
    key: Option<&str>,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    call_tool_checked(store, now, key, name, args).map_err(Refused::text)
}

/// [`call_tool`] と同じ。使えなかったのか、ツールが失敗したのかを分けて返す。
pub fn call_tool_checked(
    store: &mut Store,
    now: i64,
    key: Option<&str>,
    name: &str,
    args: &Value,
) -> Result<String, Refused> {
    let core = |e: malakt_core::Error| Refused::Failed(e.to_string());
    let grant = access::check(store, key, now)
        .map_err(core)?
        .map_err(|denied| Refused::Denied(denied_text(&denied)))?;
    if grant.read_only {
        if !reads(name) {
            return Err(Refused::Denied(match &grant.key {
                Some(key) => format!(
                    "アクセスキー「{}」は読み取りのみです。{name} は使えません。",
                    key.name
                ),
                None => format!("アクセスキーなしの接続は読み取りのみです。{name} は使えません。"),
            }));
        }
        return store
            .read_only(|store| run_tool(store, name, args))
            .map_err(core)?
            .map_err(Refused::Failed);
    }
    run_tool(store, name, args).map_err(Refused::Failed)
}

fn run_tool(store: &mut Store, name: &str, args: &Value) -> Result<String, String> {
    let core = |e: malakt_core::Error| e.to_string();
    match name {
        "undo" => {
            let done = store.undo().map_err(core)?;
            Ok(json!({"undone": done}).to_string())
        }
        "redo" => {
            let done = store.redo().map_err(core)?;
            Ok(json!({"redone": done}).to_string())
        }
        _ if tools::in_app_only(name) => Err(format!("{name} というツールはありません")),
        GET_AUTOSTART | SET_AUTOSTART => autostart(&crate::autostart::process(), name, args),
        _ => {
            // 自分が登録されていれば、コメントの書き手にする（AI チャットと同じ）
            let author = store
                .people()
                .map_err(core)?
                .into_iter()
                .find(|p| p.is_self)
                .map(|p| p.id);
            let write = tools::writes(name);
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

/// スタートアップのツール。データベースではなく OS の登録を読み書きする。
fn autostart(
    launcher: &crate::autostart::Launcher,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    if name == SET_AUTOSTART {
        let on = args
            .get("enabled")
            .and_then(Value::as_bool)
            .ok_or("enabled（true か false）が必要です")?;
        if !launcher.available {
            return Err("この OS ではスタートアップに登録できません".into());
        }
        (launcher.write)(on)?;
    }
    let on = launcher.available && (launcher.read)()?;
    Ok(json!({"autostart": on, "available": launcher.available}).to_string())
}

/// Claude Code の `.mcp.json` や `--mcp-config` に渡す設定。
/// `db` は、標準でない場所のデータベースを使うときだけ渡す。`key` はアクセスキー。
pub fn config_json(exe: &Path, db: Option<&Path>, key: Option<&str>) -> Value {
    let mut server = json!({
        "type": "stdio",
        "command": exe.to_string_lossy(),
        "args": ["mcp"],
    });
    let mut env = serde_json::Map::new();
    if let Some(db) = db {
        env.insert(ENV_DB.to_owned(), db.to_string_lossy().into());
    }
    if let Some(key) = key {
        env.insert(ENV_KEY.to_owned(), key.into());
    }
    if !env.is_empty() {
        server["env"] = Value::Object(env);
    }
    json!({"mcpServers": {NAME: server}})
}

/// アクセスキーを付けて Claude Code に登録するコマンド（どのフォルダーの claude からも使える）。
pub fn claude_add_command(exe: &Path, key: &str) -> String {
    let exe = exe.to_string_lossy();
    // 空白を含む場所は、どのシェルでも通るように二重引用符で囲む
    let exe = if exe.contains(' ') {
        format!("\"{exe}\"")
    } else {
        exe.into_owned()
    };
    format!("claude mcp add --scope user {NAME} -e {ENV_KEY}={key} -- {exe} mcp")
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
    let text = serde_json::to_string_pretty(&config_json(exe, db, None))? + "\n";
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
    /// 起動したときに渡されたアクセスキー（[`ENV_KEY`]）。
    key: Option<String>,
    now: fn() -> i64,
    /// つないできた相手。`initialize` で名乗った名前で、変更履歴に残す。
    actor: malakt_core::Actor,
}

impl<F: FnOnce() -> malakt_core::Result<Store>> Server<F> {
    /// データベースは、最初のツールの呼び出しで開く。
    pub fn new(open: F) -> Self {
        Self {
            open: Some(open),
            store: None,
            key: None,
            now: unix_now,
            actor: malakt_core::Actor::agent(""),
        }
    }

    /// つなぐのに使うアクセスキー。
    pub fn with_key(mut self, key: Option<String>) -> Self {
        self.key = key;
        self
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
        let actor = self.actor.clone();
        let key = self.key.clone();
        let store = self.store()?;
        store.set_actor(actor);
        call_tool(store, now, key.as_deref(), name, args)
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
                if let Some(name) = params.pointer("/clientInfo/name").and_then(Value::as_str) {
                    self.actor = malakt_core::Actor::agent(name);
                }
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
    use malakt_core::{lock, Status, TaskDraft};

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
                assert!(instructions().contains("list_ai_rules"));
            }
        }
        // 書き込みとして扱うツールは、どれも一覧にある
        let writes = malakt_ai::api::tools()
            .iter()
            .filter(|t| t.group == malakt_ai::api::Group::Data && t.writes);
        for tool in writes {
            assert!(names.contains(&tool.name), "{}", tool.name);
        }
        assert!(!missing.exists());

        let reply = ask(&mut s, "resources/list", json!({}));
        assert_eq!(reply["error"]["code"], -32601);
        let broken: Value = serde_json::from_str(&s.handle("{oops").unwrap()).unwrap();
        assert_eq!(broken["error"]["code"], -32700);
    }

    /// つないできた相手の名前が、作った人・変えた人として残る。名乗らなければ「外部エージェント」。
    #[test]
    fn changes_are_by_the_connected_agent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.sqlite3");
        drop(Store::open(&path).unwrap());
        let mut s = server(&path);
        ask(
            &mut s,
            "initialize",
            json!({"protocolVersion": "2025-03-26", "capabilities": {},
                   "clientInfo": {"name": "claude-code", "version": "2"}}),
        );
        let made = tool(&mut s, "create_task", json!({"title": "告知を出す"}));
        let id = made["id"].as_str().unwrap().to_owned();
        drop(s);
        let store = Store::open(&path).unwrap();
        let task = store.task(&id).unwrap();
        assert_eq!(task.created_by, malakt_core::Actor::agent("claude-code"));
        assert_eq!(
            store.changes(&id).unwrap()[0].actor,
            malakt_core::Actor::agent("claude-code")
        );
        drop(store);

        let mut s = server(&path);
        tool(
            &mut s,
            "update_task",
            json!({"id": id, "status": "in_process"}),
        );
        drop(s);
        let task = Store::open(&path).unwrap().task(&id).unwrap();
        assert_eq!(task.updated_by, malakt_core::Actor::agent(""));
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

    /// スタートアップのツールは、OS の登録を読み書きし、変えたあとの状態を返す。
    #[test]
    fn autostart_is_read_and_switched() {
        use std::sync::{Arc, Mutex};
        let on = Arc::new(Mutex::new(false));
        let (read, write) = (on.clone(), on.clone());
        let launcher = crate::autostart::Launcher {
            available: true,
            read: Arc::new(move || Ok(*read.lock().unwrap())),
            write: Arc::new(move |value| {
                *write.lock().unwrap() = value;
                Ok(())
            }),
        };
        let call = |name, args| -> Value {
            serde_json::from_str(&autostart(&launcher, name, &args).unwrap()).unwrap()
        };
        assert_eq!(call(GET_AUTOSTART, json!({}))["autostart"], false);
        assert_eq!(
            call(SET_AUTOSTART, json!({"enabled": true}))["autostart"],
            true
        );
        assert!(*on.lock().unwrap());
        assert_eq!(call(GET_AUTOSTART, json!({}))["autostart"], true);
        assert_eq!(
            call(SET_AUTOSTART, json!({"enabled": false}))["autostart"],
            false
        );
        assert!(autostart(&launcher, SET_AUTOSTART, &json!({})).is_err());

        // 登録できない OS では、読むと available が false、変えようとすると理由が返る
        let none = crate::autostart::Launcher::none();
        let text = autostart(&none, GET_AUTOSTART, &json!({})).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&text).unwrap(),
            json!({"autostart": false, "available": false})
        );
        assert_eq!(
            autostart(&none, SET_AUTOSTART, &json!({"enabled": true})),
            Err("この OS ではスタートアップに登録できません".into())
        );
        // 失敗は、そのまま返す
        let broken = crate::autostart::Launcher {
            available: true,
            read: Arc::new(|| Ok(false)),
            write: Arc::new(|_| Err("スタートアップに登録できませんでした: 拒否".into())),
        };
        assert_eq!(
            autostart(&broken, SET_AUTOSTART, &json!({"enabled": true})),
            Err("スタートアップに登録できませんでした: 拒否".into())
        );
    }

    fn keyed<'a>(
        path: &'a Path,
        key: &str,
    ) -> Server<impl FnOnce() -> malakt_core::Result<Store> + 'a> {
        server(path).with_key(Some(key.to_owned()))
    }

    /// 読み取りのみのキーでは、読むツールはどれも動き、書くツールと undo は使えない。
    #[test]
    fn a_read_only_key_reads_everything_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.sqlite3");
        let mut app = Store::open(&path).unwrap();
        app.ensure_self().unwrap();
        let w = app.create_workspace(None, "冬のイベント").unwrap();
        let draft = TaskDraft {
            title: "会場を予約する".into(),
            ..Default::default()
        };
        let task = app.create_task(Some(&w.id), &draft).unwrap();
        let key = app.issue_access_key("読むだけ", true, false).unwrap();

        let mut s = keyed(&path, &key.secret);
        let names: Vec<String> = tool_list()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_owned())
            .collect();
        let mut read = 0;
        for name in &names {
            let args = match name.as_str() {
                "get_task" => json!({"id": task.id}),
                "get_due_alerts" => json!({"task_id": task.id}),
                "get_workspace" | "get_workspace_settings" => json!({"id": w.id}),
                _ => json!({}),
            };
            if reads(name) {
                tool(&mut s, name, args);
                read += 1;
            } else {
                let text = failure(&mut s, name, args);
                assert!(
                    text.contains("「読むだけ」は読み取りのみ"),
                    "{name}: {text}"
                );
            }
        }
        assert!(read >= 10, "{read}");
        // 書くツールは、どれも読み取りのみで止まる
        let writes = malakt_ai::api::tools()
            .iter()
            .filter(|t| t.group == malakt_ai::api::Group::Data && t.writes);
        for tool in writes {
            assert!(!reads(tool.name), "{}", tool.name);
        }
        assert_eq!(
            app.tasks(malakt_core::Scope::All, &Default::default())
                .unwrap()
                .len(),
            1
        );
        // 読み取りのみをやめると、同じサーバーのまま書ける
        let patch = malakt_core::access::KeyPatch {
            read_only: Some(false),
            ..Default::default()
        };
        app.update_access_key(&key.key.id, &patch).unwrap();
        tool(&mut s, "create_task", json!({"title": "告知を出す"}));
        // 失効させると、すぐ使えなくなる
        app.revoke_access_key(&key.key.id).unwrap();
        assert_eq!(failure(&mut s, "list_tasks", json!({})), BAD_KEY);
    }

    #[test]
    fn denials_read_the_same_everywhere() {
        assert_eq!(denied_text(&Denied::Locked), LOCKED);
        assert_eq!(denied_text(&Denied::NeedsKey), NEEDS_KEY);
        assert_eq!(denied_text(&Denied::BadKey), BAD_KEY);
    }

    #[test]
    fn keys_and_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.sqlite3");
        let mut app = Store::open(&path).unwrap();
        lock::enable(&mut app).unwrap();
        let plain = app.issue_access_key("ふつう", false, false).unwrap();
        let always = app.issue_access_key("いつでも", false, true).unwrap();

        // ロック中は、ロック中も使えるキーだけが読み書きできる
        let mut s = keyed(&path, &plain.secret);
        assert_eq!(failure(&mut s, "list_tasks", json!({})), LOCKED);
        let mut s = keyed(&path, &always.secret);
        tool(&mut s, "create_task", json!({"title": "夜の作業"}));
        assert_eq!(tool(&mut s, "undo", json!({}))["undone"], true);
        let mut s = server(&path);
        assert_eq!(failure(&mut s, "list_tasks", json!({})), LOCKED);

        // キーなしの接続を使えなくすると、解除していてもキーが要る
        lock::mark_open(&mut app, 1_000).unwrap();
        tool(&mut s, "list_tasks", json!({}));
        access::set_keyless(&mut app, access::Keyless::Off).unwrap();
        assert_eq!(failure(&mut s, "list_tasks", json!({})), NEEDS_KEY);
        let mut s = keyed(&path, &plain.secret);
        tool(&mut s, "list_tasks", json!({}));
        // キーなしを読み取りのみにすると、読むだけできる
        access::set_keyless(&mut app, access::Keyless::ReadOnly).unwrap();
        let mut s = server(&path);
        tool(&mut s, "list_tasks", json!({}));
        let text = failure(&mut s, "create_task", json!({"title": "x"}));
        assert!(
            text.contains("アクセスキーなしの接続は読み取りのみ"),
            "{text}"
        );
        // 違うキーは、キーなしの扱いにかかわらず使えない
        let mut s = keyed(&path, "mmak_nope");
        assert_eq!(failure(&mut s, "list_tasks", json!({})), BAD_KEY);
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

    /// 印を捨てたあと、ロックが外れたと見えるまで待つ（2 秒まで）。
    /// ほかのテストのスレッドがプロセスを起動していると、起動の途中の子プロセスがファイルを
    /// 一瞬だけ持つ（fork から exec まで）。その間はロックが外れないので、すぐには見えないことがある。
    /// MCP サーバーは 300 ミリ秒ごとに確かめるので、アプリでも同じように少しあとで見える。
    fn lets_go(db: &Path) -> bool {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while app_running(db) {
            if std::time::Instant::now() > until {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        true
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
        assert!(lets_go(&db), "捨てた印のロックが外れない");
        // 次に起動したアプリが、また印を付けられる
        let again = mark_running(&db).unwrap();
        assert!(app_running(&db));
        drop(again);
    }

    #[test]
    fn config_and_environment() {
        let exe = Path::new("/opt/malakt-mahoz/malakt-mahoz");
        let config = config_json(exe, None, None);
        let server = &config["mcpServers"][NAME];
        assert_eq!(server["command"], "/opt/malakt-mahoz/malakt-mahoz");
        assert_eq!(server["args"], json!(["mcp"]));
        assert!(server.get("env").is_none());
        let db = Path::new("/data/タスク.sqlite3");
        let config = config_json(exe, Some(db), None);
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

        // アクセスキーは、環境変数で渡す
        let config = config_json(exe, Some(db), Some("mmak_abc"));
        let env = &config["mcpServers"][NAME]["env"];
        assert_eq!(env[ENV_DB], "/data/タスク.sqlite3");
        assert_eq!(env[ENV_KEY], "mmak_abc");
        assert_eq!(
            config_json(exe, None, Some("mmak_abc"))["mcpServers"][NAME]["env"],
            json!({ENV_KEY: "mmak_abc"})
        );
        assert_eq!(
            claude_add_command(exe, "mmak_abc"),
            "claude mcp add --scope user malakt-mahoz -e MALAKT_MAHOZ_KEY=mmak_abc -- /opt/malakt-mahoz/malakt-mahoz mcp"
        );
        assert_eq!(
            claude_add_command(Path::new("C:\\Program Files\\malakt-mahoz.exe"), "k"),
            "claude mcp add --scope user malakt-mahoz -e MALAKT_MAHOZ_KEY=k -- \"C:\\Program Files\\malakt-mahoz.exe\" mcp"
        );

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
