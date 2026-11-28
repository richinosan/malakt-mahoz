//! プラグインの WASM を動かす（Extism）。
//!
//! プラグインが出す関数は `command` だけ。入力は [`Request`] の JSON、出力は [`Outcome`] の JSON。
//! プラグインからアプリを操作するのは、ホストの関数 `call_tool` だけ。
//! 入力は `{"name": "<ツール>", "args": {…}}`、返事は `{"ok": <結果>}` か `{"error": "<文>"}`。
//! ツールは MCP と同じもの（api/data.tsp）で、使えるかどうかはアプリが権限で決める。
//!
//! ファイルとネットワークには触れられない。WASI は付ける（TinyGo や JavaScript の PDK が使う）が、
//! フォルダーを渡さず、ネットワークも許さないので、使えるのは時計と乱数くらい。HTTP の関数は付けない
//! （extism の http の機能を入れていない）。1 回の呼び出しは
//! [`TIMEOUT`] とメモリ [`MEMORY_PAGES`] までで、超えたら止める。コンパイルしたもののキャッシュは
//! ディスクに置かない。

use std::time::Duration;

use extism::{CurrentPlugin, Function, Manifest as Wasms, PluginBuilder, UserData, Val, Wasm, PTR};
use serde::Serialize;
use serde_json::{json, Value};

use crate::discover::{self, Found};
use crate::manifest;

/// 1 回のコマンドにかけられる時間。
pub const TIMEOUT: Duration = Duration::from_secs(5);
/// 使えるメモリ（64 KiB のページの数）。1024 で 64 MiB。
pub const MEMORY_PAGES: u32 = 1024;
/// プラグインが出す関数。
pub const EXPORT: &str = "command";
/// アプリを操作するホストの関数。
pub const HOST_CALL_TOOL: &str = "call_tool";

/// コマンドを呼ぶときにプラグインへ渡すもの。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Request {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

/// コマンドの結果。どれも省ける。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// 画面に出す知らせ（数秒で消える）
    pub notice: Option<String>,
    /// クリップボードに入れる文字
    pub copy: Option<String>,
}

/// プラグインから呼ばれたツールを実行するもの。結果は AI に渡すのと同じ JSON の文字列。
pub type CallTool = Box<dyn FnMut(&str, &Value) -> Result<String, String> + Send>;

/// 結果の文字の上限。
const MAX_OUTCOME_CHARS: usize = 100_000;

fn outcome(text: &str) -> Result<Outcome, String> {
    if text.trim().is_empty() {
        return Ok(Outcome::default());
    }
    let value: Value =
        serde_json::from_str(text).map_err(|e| format!("結果が JSON ではありません: {e}"))?;
    let object = value
        .as_object()
        .ok_or("結果は JSON のオブジェクトにしてください")?;
    let field = |key: &str| -> Result<Option<String>, String> {
        match object.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if s.chars().count() > MAX_OUTCOME_CHARS => Err(format!(
                "結果の {key} が長すぎます（{MAX_OUTCOME_CHARS} 文字まで）"
            )),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(other) => Err(format!("結果の {key} は文字列にしてください: {other}")),
        }
    };
    if let Some(key) = object
        .keys()
        .find(|k| !["notice", "copy"].contains(&k.as_str()))
    {
        return Err(format!("結果に知らない項目 {key} があります"));
    }
    Ok(Outcome {
        notice: field("notice")?,
        copy: field("copy")?,
    })
}

fn call_tool(
    plugin: &mut CurrentPlugin,
    inputs: &[Val],
    outputs: &mut [Val],
    data: UserData<CallTool>,
) -> Result<(), extism::Error> {
    let input: String = plugin.memory_get_val(&inputs[0])?;
    let reply = match serde_json::from_str::<Value>(&input) {
        Ok(request) => match request.get("name").and_then(Value::as_str) {
            Some(name) => {
                let args = request.get("args").cloned().unwrap_or_else(|| json!({}));
                let call = data.get()?;
                let mut call = call
                    .lock()
                    .map_err(|_| extism::Error::msg("call_tool を呼べません"))?;
                match call(name, &args) {
                    // ツールの結果は JSON の文字列。JSON でなければ文字列のまま渡す
                    Ok(text) => {
                        json!({"ok": serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text))})
                    }
                    Err(error) => json!({"error": error}),
                }
            }
            None => json!({"error": "call_tool の入力に name がありません"}),
        },
        Err(error) => json!({"error": format!("call_tool の入力が JSON ではありません: {error}")}),
    };
    let handle = plugin.memory_new(reply.to_string())?;
    outputs[0] = plugin.memory_to_val(handle);
    Ok(())
}

/// コマンドを 1 回実行する。`approved` は承認したときのハッシュ。ファイルがそのあとで変わっていれば動かさない。
pub fn run_command(
    found: &Found,
    approved: &str,
    request: &Request,
    call: CallTool,
) -> Result<Outcome, String> {
    let toml = std::fs::read(found.dir.join(manifest::FILE))
        .map_err(|e| format!("{} を読めませんでした: {e}", manifest::FILE))?;
    let wasm = std::fs::read(found.wasm_path())
        .map_err(|e| format!("{} を読めませんでした: {e}", found.manifest.wasm))?;
    if discover::hash(&toml, &wasm) != approved {
        return Err("有効にしたあとで、プラグインのファイルが変わりました。設定の「プラグイン」でもう一度有効にしてください".into());
    }
    let wasms = Wasms::new([Wasm::data(wasm)])
        .with_memory_max(MEMORY_PAGES)
        .with_timeout(TIMEOUT);
    let function = Function::new(HOST_CALL_TOOL, [PTR], [PTR], UserData::new(call), call_tool);
    let mut plugin = PluginBuilder::new(wasms)
        .with_wasi(true)
        .with_cache_disabled()
        .with_functions([function])
        .build()
        .map_err(|e| format!("プラグインを読み込めませんでした: {e}"))?;
    if !plugin.function_exists(EXPORT) {
        return Err(format!("プラグインに関数 {EXPORT} がありません"));
    }
    let input = serde_json::to_string(request).map_err(|e| e.to_string())?;
    let text = plugin
        .call::<&str, &str>(EXPORT, &input)
        .map_err(|e| format!("{e}"))?
        .to_owned();
    outcome(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::{Arc, Mutex};

    /// 見本のプラグイン（examples/plugins/task-markdown）
    fn example() -> Found {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/plugins/task-markdown");
        let found = crate::discover(&[crate::Folder {
            path: dir.parent().unwrap().to_owned(),
            from: crate::Source::Setting,
        }]);
        assert!(found.problems.is_empty(), "{:?}", found.problems);
        found
            .plugins
            .into_iter()
            .find(|p| p.manifest.id == "task-markdown")
            .unwrap()
    }

    fn request(command: &str) -> Request {
        Request {
            command: command.into(),
            task_id: Some("t1".into()),
            workspace_id: Some("w1".into()),
        }
    }

    #[test]
    fn the_example_reads_through_the_app_tools() {
        let found = example();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = calls.clone();
        let call: CallTool = Box::new(move |name, args| {
            seen.lock().unwrap().push((name.to_owned(), args.clone()));
            Ok(json!({
                "title": "告知を出す",
                "status": "in_process",
                "due": "2026-12-01",
                "subtasks": [{"title": "文面", "status": "completed"}],
            })
            .to_string())
        });
        let got = run_command(&found, &found.hash, &request("copy"), call).unwrap();
        assert_eq!(
            got.copy.as_deref(),
            Some("- [ ] 告知を出す（期限 2026-12-01）\n  - [x] 文面")
        );
        assert_eq!(got.notice.as_deref(), Some("2 行をコピーしました"));
        assert_eq!(
            *calls.lock().unwrap(),
            [("get_task".to_owned(), json!({"id": "t1"}))]
        );
    }

    #[test]
    fn the_example_lists_a_workspace() {
        let found = example();
        let call: CallTool = Box::new(|name, args| {
            assert_eq!(name, "list_tasks");
            assert_eq!(*args, json!({"workspace_id": "w1"}));
            Ok(json!({
                "total": 2,
                "shown": 2,
                "tasks": [
                    {"title": "会場", "status": "completed"},
                    {"title": "告知", "status": "not_started", "due": "2026-12-01"},
                ],
            })
            .to_string())
        });
        let got = run_command(&found, &found.hash, &request("copy-workspace"), call).unwrap();
        assert_eq!(
            got.copy.as_deref(),
            Some("- [x] 会場\n- [ ] 告知（期限 2026-12-01）")
        );
    }

    #[test]
    fn tool_failures_and_changed_files_stop_the_command() {
        let found = example();
        let refuse: CallTool = Box::new(|name, _| Err(format!("{name} は使えません")));
        let error = run_command(&found, &found.hash, &request("copy"), refuse).unwrap_err();
        assert!(error.contains("get_task は使えません"), "{error}");
        let never: CallTool = Box::new(|_, _| panic!("呼ばれない"));
        let error = run_command(&found, "other", &request("copy"), never).unwrap_err();
        assert!(error.contains("もう一度有効に"), "{error}");
        let unknown: CallTool = Box::new(|_, _| Ok("{}".into()));
        let error = run_command(&found, &found.hash, &request("nope"), unknown).unwrap_err();
        assert!(error.contains("知らないコマンド"), "{error}");
    }

    #[test]
    fn outcomes_are_read_strictly() {
        assert_eq!(outcome("").unwrap(), Outcome::default());
        let got = outcome(r#"{"notice": "済み", "copy": null}"#).unwrap();
        assert_eq!(got.notice.as_deref(), Some("済み"));
        assert_eq!(got.copy, None);
        assert!(outcome("[]").is_err());
        assert!(outcome(r#"{"notice": 1}"#).is_err());
        assert!(outcome(r#"{"open": "x"}"#).unwrap_err().contains("open"));
    }
}
